use clap::Parser;
use sakuio_agent_core::{AgentConfig, AgentRunner};
use sakuio_agent_remote::ws_base;
use sakuio_agent_ws::WsBoardTransport;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_ENDPOINT: &str = "ws://127.0.0.1:8080";
const DEFAULT_STATE_FILE: &str = "sakuio-agent-token";

/// Sakuio board agent: connects with token and pushes host metrics.
#[derive(Debug, Parser)]
#[command(name = "sakuio-agent", version = VERSION, about)]
struct Cli {
    /// Board base URL, e.g. `ws://192.168.1.10:8080` or `wss://panel.example.com`.
    /// Board WebSocket base（`ws(s)://host[:port]`，路径由 agent 内部处理）。
    #[arg(long, short = 'e', env = "SAKUIO_BOARD_ENDPOINT", default_value = DEFAULT_ENDPOINT)]
    endpoint: String,

    /// Node token from the admin console, or the auto-discovery key.
    #[arg(long, short = 't', alias = "auto-discovery", env = "SAKUIO_NODE_TOKEN")]
    token: Option<String>,

    /// State file used to persist a server-assigned token (auto discovery).
    #[arg(long, env = "SAKUIO_AGENT_STATE", default_value = DEFAULT_STATE_FILE)]
    state: PathBuf,

    /// Telemetry collect and report interval in seconds.
    #[arg(long, short = 'i', env = "SAKUIO_AGENT_INTERVAL", default_value_t = 3.0)]
    interval: f64,

    /// Reconnect interval in seconds after the board session drops.
    #[arg(long, short = 'c', env = "SAKUIO_AGENT_RECONNECT_INTERVAL", default_value_t = 5)]
    reconnect_interval: u64,

    /// Interval in minutes for re-reporting basic host info.
    #[arg(long, env = "SAKUIO_AGENT_INFO_REPORT_INTERVAL", default_value_t = 5)]
    info_report_interval: u64,

    /// Disable remote control capabilities (terminal / file / shell / exec).
    /// 禁用远程控制能力（终端 / 文件 / shell / exec）。
    #[arg(long)]
    disable_remote_control: bool,

    /// Disable built-in auto update (reserved; current builds do not self-update).
    /// 禁用自动更新（预留；当前版本尚未内置自更新）。
    #[arg(long)]
    disable_auto_update: bool,

    /// Skip TLS certificate verification for wss:// endpoints (self-signed setups).
    /// 忽略 TLS 证书校验（自签名部署场景）。
    #[arg(long)]
    insecure: bool,

    /// Include buffer/cache memory in used-memory accounting.
    /// 内存占用统计包含缓冲区 / 缓存。
    #[arg(long)]
    include_buffer_memory: bool,

    /// Prefer the NIC address as the reported public IP.
    /// 优先从网卡获取 IP 地址上报。
    #[arg(long)]
    ip_from_nic: bool,

    /// Enable detailed GPU monitoring.
    /// 启用详细 GPU 监控。
    #[arg(long)]
    gpu_detail: bool,

    /// Comma-separated list of NICs to include for traffic stats.
    /// 只监测特定网卡（逗号分隔）。
    #[arg(long)]
    include_nics: Option<String>,

    /// Comma-separated list of NICs to exclude from traffic stats.
    /// 排除特定网卡（逗号分隔）。
    #[arg(long)]
    exclude_nics: Option<String>,

    /// Comma-separated list of mount points to include for disk stats.
    /// 只监测特定挂载点（逗号分隔）。
    #[arg(long)]
    include_mountpoints: Option<String>,

    /// Day of month (1-31) to rotate network statistics counters.
    /// 网络统计月重置日（1-31）。
    #[arg(long)]
    monthly_reset_day: Option<u32>,
}

/// Reads the persisted server-assigned token from the state file, if any.
fn stored_token(state: &PathBuf) -> Option<String> {
    std::fs::read_to_string(state)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Maps install-time flags onto the `SAKUIO_AGENT_*` environment variables
/// consumed by the collector (`CollectConfig::default`).
/// 把安装选项映射到采集器读取的 `SAKUIO_AGENT_*` 环境变量。
fn apply_collect_env(cli: &Cli) {
    // SAFETY: called once from main before any worker thread reads these
    // variables (edition 2024 marks `set_var` unsafe because of env races).
    unsafe {
        if cli.include_buffer_memory {
            std::env::set_var("SAKUIO_AGENT_MEMORY_INCLUDE_CACHE", "1");
        }
        if cli.ip_from_nic {
            std::env::set_var("SAKUIO_AGENT_GET_IP_FROM_NIC", "1");
        }
        if cli.gpu_detail {
            std::env::set_var("SAKUIO_AGENT_ENABLE_GPU", "1");
        }
        if let Some(v) = cli.include_nics.as_deref().filter(|v| !v.trim().is_empty()) {
            std::env::set_var("SAKUIO_AGENT_INCLUDE_NICS", v.trim());
        }
        if let Some(v) = cli.exclude_nics.as_deref().filter(|v| !v.trim().is_empty()) {
            std::env::set_var("SAKUIO_AGENT_EXCLUDE_NICS", v.trim());
        }
        if let Some(v) = cli
            .include_mountpoints
            .as_deref()
            .filter(|v| !v.trim().is_empty())
        {
            std::env::set_var("SAKUIO_AGENT_INCLUDE_MOUNTPOINTS", v.trim());
        }
        if let Some(d) = cli.monthly_reset_day {
            std::env::set_var("SAKUIO_AGENT_MONTH_ROTATE", d.to_string());
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let cli = Cli::parse();
    let endpoint = cli.endpoint.trim();
    if endpoint.is_empty() {
        anyhow::bail!("endpoint must not be empty");
    }
    // First start: explicit token (node token or auto-discovery key) wins,
    // otherwise fall back to the persisted token from a previous registration.
    let mut token = cli
        .token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| stored_token(&cli.state))
        .ok_or_else(|| {
            anyhow::anyhow!("--token / -t or SAKUIO_NODE_TOKEN is required on first start")
        })?;

    tracing::info!(endpoint = %endpoint, "connecting to board");

    let endpoint_base = ws_base(endpoint)?;
    let transport = Arc::new(
        WsBoardTransport::new(&format!("{endpoint_base}/api/agent/link"))
            .map_err(|e| anyhow::anyhow!("endpoint: {e}"))?
            .with_insecure(cli.insecure),
    );

    if !cli.interval.is_finite() || cli.interval <= 0.0 {
        anyhow::bail!("--interval must be a positive number of seconds");
    }
    apply_collect_env(&cli);

    let reconnect = Duration::from_secs(cli.reconnect_interval.max(1));
    let mut first_attempt = true;
    let mut attempt: u64 = 0;

    // 自动更新：每 6 小时检查一次；面板断开连接的轮次跳过。
    // 安装成功后退出进程（退出码 42），由服务管理器（systemd / nssm）拉起新版本。
    // 链路快照（存活状态 + 兼容区间）由会话循环写入、更新循环读取。
    // 仅 release 构建启动更新循环；debug 构建下不启动。
    // 注意：watch 的所有 Sender 都 drop 后 `changed()` 会立即返回，
    // 必须保活 sender，否则重连循环会被 restart 分支空转打断。
    let link_state_slot = Arc::new(std::sync::RwLock::new(Default::default()));
    let (restart_tx, mut restart_rx) = tokio::sync::watch::channel(false);
    let _keep_restart_tx = if !cli.disable_auto_update && !cfg!(debug_assertions) {
        tokio::spawn(sakuio_agent_core::run_update_loop(
            VERSION.to_string(),
            None,
            Arc::clone(&link_state_slot),
            move || {
                let _ = restart_tx.send(true);
            },
        ));
        None
    } else {
        Some(restart_tx)
    };

    loop {
        if !first_attempt {
            // Auto discovery may have replaced the token while running.
            if let Some(saved) = stored_token(&cli.state) {
                if saved != token {
                    tracing::info!("using server-assigned node token from state file");
                    token = saved;
                }
            }
        }
        first_attempt = false;

        let config = AgentConfig {
            node_token: token.clone(),
            agent_version: VERSION.to_string(),
            endpoint_base: endpoint_base.clone(),
            link_state_slot: Arc::clone(&link_state_slot),
            telemetry_interval: Duration::from_secs_f64(cli.interval),
            info_report_interval: Duration::from_secs(cli.info_report_interval.max(1) * 60),
            max_pending_batches: 8,
            state_path: Some(cli.state.to_string_lossy().to_string()),
            disable_remote_control: cli.disable_remote_control,
        };
        let runner = AgentRunner::new(transport.clone(), config);
        let started = std::time::Instant::now();
        tokio::select! {
            result = runner.run() => {
                match result {
                    Ok(()) => tracing::info!(
                        uptime_secs = started.elapsed().as_secs(),
                        "board session ended"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        uptime_secs = started.elapsed().as_secs(),
                        "board connection lost"
                    ),
                }
            }
            _ = restart_rx.changed() => {
                if *restart_rx.borrow() {
                    tracing::info!("update installed; exiting for service manager restart");
                    std::process::exit(sakuio_agent_core::RESTART_EXIT_CODE);
                }
            }
        }
        // A cycle that ran for a while counts as a healthy connection;
        // consecutive short-lived failures bump the retry attempt counter.
        // 持续了一段时间的会话视为一次健康连接；短暂即失败的轮次累计重试次数。
        if started.elapsed() >= Duration::from_secs(30) {
            attempt = 0;
        }
        attempt += 1;
        tracing::info!(
            attempt,
            delay_secs = reconnect.as_secs(),
            "retrying connection to board"
        );
        tokio::time::sleep(reconnect).await;
    }
}
