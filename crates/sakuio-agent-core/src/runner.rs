use sakuio_agent_collect::{CollectState, Reporter};
use sakuio_board::{BoardConnectInfo, BoardFrame, BoardSession, BoardTransport};
use sakuio_agent_collect::run_ping;
use sakuio_agent_remote::{execute_file_op, run_relay_pump, ExecBridge, RelayMode, ShellBridge};
use sakuio_protocol::{
    AgentPingParams, ControlMessage, ControlOp, HelloParams, RemoteExecOpen, RemoteFileCommand,
    RemoteFileResult, RemoteShellOpen, RemoteStreamRead, RemoteStreamWrite, SyncCommandItem,
    SyncReqParams, SyncRespParams, TelemetryBatch, WelcomeParams, METHOD_AGENT_PING,
    METHOD_AGENT_PING_RESULT, METHOD_REMOTE_EXEC, METHOD_REMOTE_EXEC_RESULT, METHOD_REMOTE_FILE,
    METHOD_REMOTE_FILE_RESULT, METHOD_REMOTE_SHELL, METHOD_REMOTE_STREAM_READ,
    METHOD_REMOTE_STREAM_WRITE,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};

/// Agent runtime configuration.
/// Agent 运行时配置。
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub node_token: String,
    pub agent_version: String,
    pub telemetry_interval: Duration,
    pub info_report_interval: Duration,
    pub max_pending_batches: u32,
    /// Path used to persist a server-assigned token (auto discovery).
    pub state_path: Option<String>,
}

/// Runs the board session until error or shutdown.
/// 运行板级会话直至错误或关闭。
pub struct AgentRunner {
    transport: Arc<dyn BoardTransport>,
    config: AgentConfig,
}

impl AgentRunner {
    pub fn new(transport: Arc<dyn BoardTransport>, config: AgentConfig) -> Self {
        Self { transport, config }
    }

    /// Connects, completes hello/welcome, pushes basic info and telemetry loop.
    /// 连接、完成 hello/welcome、上报静态信息并进入遥测循环。
    pub async fn run(&self) -> anyhow::Result<()> {
        let info = BoardConnectInfo {
            node_token: self.config.node_token.clone(),
            agent_version: self.config.agent_version.clone(),
            capabilities: vec![
                "telemetry".into(),
                "basic_info".into(),
                "ping".into(),
                "remote.file".into(),
                "remote.shell".into(),
                "remote.exec".into(),
            ],
        };
        let mut session = self
            .transport
            .connect(info)
            .await
            .map_err(|e| anyhow::anyhow!("connect: {e}"))?;

        let welcome = self.handshake(&mut session).await?;
        let node_uuid = welcome.node_uuid.clone();
        self.persist_assigned_token(&welcome).await;

        let reporter = Reporter::from_env();
        let static_info = reporter.static_info(&self.config.agent_version);
        session
            .send(BoardFrame::Control(ControlMessage::new(
                ControlOp::BasicInfo,
                serde_json::to_value(static_info.basic)?,
            )))
            .await
            .map_err(|e| anyhow::anyhow!("basic_info send: {e}"))?;

        let mut collect_state = CollectState::default();
        let mut ticker = interval(self.config.telemetry_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut info_ticker = interval(self.config.info_report_interval);
        info_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut batch_seq: u32 = 0;
        let mut pending_acks: u32 = 0;
        let (ping_tx, mut ping_rx) = mpsc::unbounded_channel::<ControlMessage>();
        let mut command_cursor: u64 = 0;
        let mut sync_ticker = interval(Duration::from_secs(2));
        sync_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        self.request_sync(&mut session, command_cursor).await?;

        loop {
            tokio::select! {
                Some(outbound) = ping_rx.recv() => {
                    session.send(BoardFrame::Control(outbound)).await
                        .map_err(|e| anyhow::anyhow!("ping result send: {e}"))?;
                }
                _ = sync_ticker.tick() => {
                    self.request_sync(&mut session, command_cursor).await?;
                }
                _ = info_ticker.tick() => {
                    let static_info = reporter.static_info(&self.config.agent_version);
                    session
                        .send(BoardFrame::Control(ControlMessage::new(
                            ControlOp::BasicInfo,
                            serde_json::to_value(static_info.basic)?,
                        )))
                        .await
                        .map_err(|e| anyhow::anyhow!("basic_info send: {e}"))?;
                }
                _ = ticker.tick() => {
                    if pending_acks >= self.config.max_pending_batches {
                        tracing::warn!(
                            pending = pending_acks,
                            "telemetry backpressure, skipping sample"
                        );
                        continue;
                    }
                    let sample = reporter.sample(&mut collect_state);
                    batch_seq = batch_seq.wrapping_add(1);
                    let batch = TelemetryBatch {
                        node_uuid: node_uuid.clone(),
                        samples: vec![sample],
                    };
                    let mut body = Vec::new();
                    cbor4ii::serde::to_writer(&mut body, &batch)?;
                    session.send(BoardFrame::Telemetry(sakuio_protocol::SkltPayload {
                        batch_seq,
                        body,
                    })).await.map_err(|e| anyhow::anyhow!("telemetry send: {e}"))?;
                    pending_acks += 1;
                }
                frame = session.recv() => {
                    let Some(frame) = frame.map_err(|e| anyhow::anyhow!("recv: {e}"))? else {
                        anyhow::bail!("board session closed");
                    };
                    if let BoardFrame::Control(msg) = frame {
                        command_cursor = self
                            .handle_control(
                                &mut session,
                                msg,
                                &mut pending_acks,
                                &ping_tx,
                                &mut command_cursor,
                            )
                            .await?;
                    }
                }
            }
        }
    }

    /// Persists a server-assigned token (auto discovery) to the state file.
    async fn persist_assigned_token(&self, welcome: &WelcomeParams) {
        let Some(new_token) = welcome.node_token.as_deref().map(str::trim) else {
            return;
        };
        if new_token.is_empty() || new_token == self.config.node_token {
            return;
        }
        let Some(path) = &self.config.state_path else {
            tracing::warn!(
                "server assigned a new node token but no state path is configured; pass --state to enable auto discovery"
            );
            return;
        };
        match tokio::fs::write(path, new_token).await {
            Ok(()) => tracing::info!(
                node_uuid = %welcome.node_uuid,
                state_path = %path,
                "node token assigned by server, saved to state file"
            ),
            Err(e) => tracing::error!(error = %e, path = %path, "failed to save assigned node token"),
        }
    }

    async fn handshake(
        &self,
        session: &mut Box<dyn BoardSession>,
    ) -> anyhow::Result<WelcomeParams> {        let hello = HelloParams {
            agent_version: self.config.agent_version.clone(),
            capabilities: vec![
                "telemetry".into(),
                "basic_info".into(),
                "ping".into(),
                "remote.file".into(),
                "remote.shell".into(),
                "remote.exec".into(),
            ],
        };
        session
            .send(BoardFrame::Control(ControlMessage::new(
                ControlOp::Hello,
                serde_json::to_value(hello)?,
            )))
            .await
            .map_err(|e| anyhow::anyhow!("hello send: {e}"))?;

        loop {
            let Some(frame) = session
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("hello recv: {e}"))?
            else {
                anyhow::bail!("closed before welcome");
            };
            if let BoardFrame::Control(msg) = frame {
                if msg.op == ControlOp::Welcome {
                    let welcome: WelcomeParams = serde_json::from_value(msg.p)?;
                    tracing::info!(
                        session_id = %welcome.session_id,
                        interval_ms = self.config.telemetry_interval.as_millis() as u64,
                        "welcome received"
                    );
                    return Ok(welcome);
                }
            }
        }
    }

    async fn request_sync(
        &self,
        session: &mut Box<dyn BoardSession>,
        since: u64,
    ) -> anyhow::Result<()> {
        session
            .send(BoardFrame::Control(ControlMessage::new(
                ControlOp::SyncReq,
                serde_json::to_value(SyncReqParams { since })?,
            )))
            .await
            .map_err(|e| anyhow::anyhow!("sync_req send: {e}"))
    }

    async fn handle_control(
        &self,
        session: &mut Box<dyn BoardSession>,
        msg: ControlMessage,
        pending_acks: &mut u32,
        ping_tx: &mpsc::UnboundedSender<ControlMessage>,
        command_cursor: &mut u64,
    ) -> anyhow::Result<u64> {
        match msg.op {
            ControlOp::TelemetryAck => {
                *pending_acks = pending_acks.saturating_sub(1);
            }
            ControlOp::Heartbeat => {
                session
                    .send(BoardFrame::Control(ControlMessage::new(
                        ControlOp::Heartbeat,
                        serde_json::json!({}),
                    )))
                    .await
                    .map_err(|e| anyhow::anyhow!("heartbeat send: {e}"))?;
            }
            ControlOp::SyncReq => {}
            ControlOp::SyncResp => {
                let resp: SyncRespParams = serde_json::from_value(msg.p)?;
                for item in resp.commands {
                    *command_cursor = (*command_cursor).max(item.cursor);
                    if item.method == METHOD_AGENT_PING {
                        self.spawn_ping(ping_tx.clone(), item)?;
                    } else if item.method == METHOD_REMOTE_FILE {
                        self.spawn_remote_file(ping_tx.clone(), item)?;
                    } else if item.method == METHOD_REMOTE_SHELL {
                        self.spawn_remote_shell(item)?;
                    } else if item.method == METHOD_REMOTE_STREAM_READ {
                        self.spawn_stream_read(item)?;
                    } else if item.method == METHOD_REMOTE_STREAM_WRITE {
                        self.spawn_stream_write(item)?;
                    } else if item.method == METHOD_REMOTE_EXEC {
                        self.spawn_remote_exec(ping_tx.clone(), item)?;
                    }
                }
            }
            ControlOp::Cmd => {
                let method = msg.p.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let payload = msg
                    .p
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| msg.p.clone());
                if method == METHOD_AGENT_PING {
                    let item = SyncCommandItem {
                        cursor: 0,
                        method: method.to_string(),
                        payload,
                    };
                    self.spawn_ping(ping_tx.clone(), item)?;
                } else if method == METHOD_REMOTE_FILE {
                    let item = SyncCommandItem {
                        cursor: 0,
                        method: method.to_string(),
                        payload,
                    };
                    self.spawn_remote_file(ping_tx.clone(), item)?;
                } else if method == METHOD_REMOTE_SHELL {
                    let item = SyncCommandItem {
                        cursor: 0,
                        method: method.to_string(),
                        payload,
                    };
                    self.spawn_remote_shell(item)?;
                } else if method == METHOD_REMOTE_STREAM_READ {
                    let item = SyncCommandItem {
                        cursor: 0,
                        method: method.to_string(),
                        payload,
                    };
                    self.spawn_stream_read(item)?;
                } else if method == METHOD_REMOTE_STREAM_WRITE {
                    let item = SyncCommandItem {
                        cursor: 0,
                        method: method.to_string(),
                        payload,
                    };
                    self.spawn_stream_write(item)?;
                } else if method == METHOD_REMOTE_EXEC {
                    let item = SyncCommandItem {
                        cursor: 0,
                        method: method.to_string(),
                        payload,
                    };
                    self.spawn_remote_exec(ping_tx.clone(), item)?;
                }
            }
            other => {
                tracing::debug!(?other, "ignored control message");
            }
        }
        Ok(*command_cursor)
    }

    fn spawn_ping(
        &self,
        ping_tx: mpsc::UnboundedSender<ControlMessage>,
        item: SyncCommandItem,
    ) -> anyhow::Result<()> {
        let params: AgentPingParams = serde_json::from_value(item.payload)?;
        tokio::spawn(async move {
            let ping_type = params.ping_type.clone();
            let target = params.target.clone();
            let task_id = params.task_id;
            let ping_type_for_probe = ping_type.clone();
            let value = tokio::task::spawn_blocking(move || {
                run_ping(&ping_type_for_probe, &target)
            })
                .await
                .unwrap_or(-1);
            let finished_at = chrono::Utc::now().to_rfc3339();
            let msg = ControlMessage::new(
                ControlOp::CmdResult,
                serde_json::json!({
                    "method": METHOD_AGENT_PING_RESULT,
                    "task_id": task_id,
                    "ping_type": ping_type,
                    "value": value,
                    "finished_at": finished_at,
                }),
            );
            let _ = ping_tx.send(msg);
        });
        Ok(())
    }

    fn spawn_remote_file(
        &self,
        result_tx: mpsc::UnboundedSender<ControlMessage>,
        item: SyncCommandItem,
    ) -> anyhow::Result<()> {
        let cmd: RemoteFileCommand = serde_json::from_value(item.payload)?;
        tokio::task::spawn_blocking(move || {
            let (ok, result, error) = match execute_file_op(&cmd.op, &cmd.args) {
                Ok(value) => (true, value, None),
                Err(message) => (false, serde_json::Value::Null, Some(message)),
            };
            let body = RemoteFileResult {
                req_id: cmd.req_id,
                ok,
                result,
                error,
            };
            let msg = ControlMessage::new(
                ControlOp::CmdResult,
                serde_json::json!({
                    "method": METHOD_REMOTE_FILE_RESULT,
                    "payload": body,
                }),
            );
            let _ = result_tx.send(msg);
        });
        Ok(())
    }

    fn spawn_stream_read(&self, item: SyncCommandItem) -> anyhow::Result<()> {
        let req: RemoteStreamRead = serde_json::from_value(item.payload)?;
        let endpoint = req.relay.path.clone();
        let token = req.relay.token.clone();
        let path = req.path.clone();
        tokio::spawn(async move {
            let _ = run_relay_pump(&endpoint, &token, RelayMode::ReadFile(path)).await;
        });
        Ok(())
    }

    fn spawn_stream_write(&self, item: SyncCommandItem) -> anyhow::Result<()> {
        let req: RemoteStreamWrite = serde_json::from_value(item.payload)?;
        let endpoint = req.relay.path.clone();
        let token = req.relay.token.clone();
        let path = req.path.clone();
        tokio::spawn(async move {
            let _ = run_relay_pump(&endpoint, &token, RelayMode::WriteFile(path)).await;
        });
        Ok(())
    }

    fn spawn_remote_shell(&self, item: SyncCommandItem) -> anyhow::Result<()> {
        let open: RemoteShellOpen = serde_json::from_value(item.payload)?;
        let endpoint = open.relay.path.clone();
        let token = open.relay.token.clone();
        let bridge = ShellBridge {
            cols: open.cols,
            rows: open.rows,
        };
        tokio::spawn(async move {
            let _ = run_relay_pump(
                &endpoint,
                &token,
                RelayMode::Shell(bridge),
            )
            .await;
        });
        Ok(())
    }

    fn spawn_remote_exec(
        &self,
        result_tx: mpsc::UnboundedSender<ControlMessage>,
        item: SyncCommandItem,
    ) -> anyhow::Result<()> {
        let open: RemoteExecOpen = serde_json::from_value(item.payload)?;
        let endpoint = open.relay.path.clone();
        let token = open.relay.token.clone();
        let bridge = ExecBridge {
            command: open.command.clone(),
        };
        let task_id = open.task_id.clone();
        let client_uuid = open.client_uuid.clone();
        tokio::spawn(async move {
            let (output, exit_code) = match run_relay_pump(
                &endpoint,
                &token,
                RelayMode::Exec(bridge),
            )
            .await
            {
                Ok(Some(v)) => v,
                _ => (String::new(), -1),
            };
            let finished_at = chrono::Utc::now().to_rfc3339();
            let msg = ControlMessage::new(
                ControlOp::CmdResult,
                serde_json::json!({
                    "method": METHOD_REMOTE_EXEC_RESULT,
                    "task_id": task_id,
                    "client_uuid": client_uuid,
                    "result": output,
                    "exit_code": exit_code,
                    "finished_at": finished_at,
                }),
            );
            let _ = result_tx.send(msg);
        });
        Ok(())
    }
}
