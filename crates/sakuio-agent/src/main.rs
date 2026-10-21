use clap::Parser;
use sakuio_agent_core::{AgentConfig, AgentRunner};
use sakuio_agent_ws::WsBoardTransport;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_ENDPOINT: &str = "ws://127.0.0.1:8080/api/agent/link";
const DEFAULT_STATE_FILE: &str = "sakuio-agent-token";

/// Sakuio board agent: connects with token and pushes host metrics.
#[derive(Debug, Parser)]
#[command(name = "sakuio-agent", version = VERSION, about)]
struct Cli {
    /// Board WebSocket URL (query `token` is appended automatically).
    #[arg(long, short = 'e', env = "SAKUIO_BOARD_ENDPOINT", default_value = DEFAULT_ENDPOINT)]
    endpoint: String,

    /// Node token from the admin console, or the auto-discovery key.
    #[arg(long, short = 't', env = "SAKUIO_NODE_TOKEN")]
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
}

/// Reads the persisted server-assigned token from the state file, if any.
fn stored_token(state: &PathBuf) -> Option<String> {
    std::fs::read_to_string(state)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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

    let transport = Arc::new(
        WsBoardTransport::new(endpoint).map_err(|e| anyhow::anyhow!("endpoint: {e}"))?,
    );

    if !cli.interval.is_finite() || cli.interval <= 0.0 {
        anyhow::bail!("--interval must be a positive number of seconds");
    }

    let reconnect = Duration::from_secs(cli.reconnect_interval.max(1));
    let mut first_attempt = true;

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
            telemetry_interval: Duration::from_secs_f64(cli.interval),
            info_report_interval: Duration::from_secs(cli.info_report_interval.max(1) * 60),
            max_pending_batches: 8,
            state_path: Some(cli.state.to_string_lossy().to_string()),
        };
        let runner = AgentRunner::new(transport.clone(), config);
        match runner.run().await {
            Ok(()) => tracing::info!("agent session ended cleanly"),
            Err(e) => tracing::warn!(error = %e, "agent session error, reconnecting"),
        }
        tokio::time::sleep(reconnect).await;
    }
}
