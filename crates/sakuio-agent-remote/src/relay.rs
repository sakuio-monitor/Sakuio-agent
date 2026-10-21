use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Derives the `ws(s)://host[:port]` base from the `-e` endpoint argument.
/// Tolerates legacy values that still carry a path (e.g. `ws://host/api/agent/link`).
/// 从 `-e` 参数推导 `ws(s)://host[:port]` base；兼容旧配置携带路径的写法。
pub fn ws_base(endpoint: &str) -> anyhow::Result<String> {
    let trimmed = endpoint.trim();
    let (scheme, rest) = if let Some(r) = trimmed.strip_prefix("wss://") {
        ("wss", r)
    } else if let Some(r) = trimmed.strip_prefix("ws://") {
        ("ws", r)
    } else {
        anyhow::bail!("endpoint must start with ws:// or wss://");
    };
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    if authority.is_empty() {
        anyhow::bail!("endpoint host must not be empty");
    }
    Ok(format!("{scheme}://{authority}"))
}

/// Joins a relay path from the board with the agent's own endpoint base.
/// Absolute URLs (legacy boards) are returned unchanged.
/// 将 board 下发的 relay 相对路径与 agent 自身 endpoint base 拼接；绝对地址（旧版 board）原样返回。
pub fn resolve_relay_url(endpoint_base: &str, relay_path: &str) -> String {
    if relay_path.starts_with("ws://") || relay_path.starts_with("wss://") {
        return relay_path.to_string();
    }
    format!("{endpoint_base}{relay_path}")
}

pub async fn run_relay_pump(
    endpoint: &str,
    token: &str,
    mode: RelayMode,
) -> anyhow::Result<Option<(String, i32)>> {
    let url = if endpoint.contains('?') {
        format!("{endpoint}&token={token}")
    } else {
        format!("{endpoint}?token={token}")
    };
    let (ws, _) = connect_async(&url).await?;
    let (mut tx, mut rx) = ws.split();

    match mode {
        RelayMode::ReadFile(path) => {
            let mut file = File::open(Path::new(&path)).await?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                tx.send(Message::Binary(buf[..n].to_vec())).await?;
            }
        }
        RelayMode::WriteFile(path) => {
            let mut file = File::create(Path::new(&path)).await?;
            while let Some(msg) = rx.next().await {
                match msg? {
                    Message::Binary(b) => file.write_all(&b).await?,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
        RelayMode::Shell(session) => {
            session.run(&mut tx, &mut rx).await?;
        }
        RelayMode::Exec(session) => {
            let summary = tokio::join!(
                session.run(&mut tx),
                async {
                    while let Some(msg) = rx.next().await {
                        if matches!(msg, Ok(Message::Close(_))) {
                            break;
                        }
                    }
                }
            )
            .0?;
            let _ = tx.send(Message::Close(None)).await;
            return Ok(Some(summary));
        }
    }

    let _ = tx.send(Message::Close(None)).await;
    Ok(None)
}

pub enum RelayMode {
    ReadFile(String),
    WriteFile(String),
    Shell(crate::shell::ShellBridge),
    Exec(crate::exec::ExecBridge),
}
