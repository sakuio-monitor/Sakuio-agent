use crate::config::CollectConfig;
use crate::{netstatic, nic};
use parking_lot::Mutex;
use sakuio_protocol::NetworkReport;
use std::collections::HashMap;
use std::time::Instant;

static SPEED_STATE: once_cell::sync::Lazy<Mutex<NetSpeedState>> =
    once_cell::sync::Lazy::new(|| Mutex::new(NetSpeedState::default()));

#[derive(Default)]
struct NetSpeedState {
    total_up: u64,
    total_down: u64,
    sampled_at: Option<Instant>,
}

pub fn network_speed(cfg: &CollectConfig) -> Result<NetworkReport, String> {
    let include = parse_nics(&cfg.include_nics);
    let exclude = parse_nics(&cfg.exclude_nics);

    if cfg.month_rotate != 0 {
        netstatic::ensure_running(cfg);
        let reset = netstatic::last_reset_unix(cfg.month_rotate);
        let now = unix_now();
        let nic_stats = match netstatic::total_traffic_between(cfg, reset, now) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to call GetTotalTrafficBetween, falling back to boot counters");
                return fallback_all(cfg, &include, &exclude);
            }
        };
        let mut total_up = 0u64;
        let mut total_down = 0u64;
        for (name, stats) in nic_stats {
            if should_include(&name, &include, &exclude) {
                total_up = total_up.saturating_add(stats.tx);
                total_down = total_down.saturating_add(stats.rx);
            }
        }
        let (up, down) = fallback_speed(cfg, &include, &exclude)?;
        return Ok(NetworkReport {
            up,
            down,
            total_up,
            total_down,
        });
    }

    fallback_all(cfg, &include, &exclude)
}

fn fallback_all(
    cfg: &CollectConfig,
    include: &HashMap<String, ()>,
    exclude: &HashMap<String, ()>,
) -> Result<NetworkReport, String> {
    let (total_up, total_down) = collect_totals(cfg, include, exclude)?;
    let (up, down) = update_speed_sample(total_up, total_down);
    Ok(NetworkReport {
        up,
        down,
        total_up,
        total_down,
    })
}

fn fallback_speed(
    cfg: &CollectConfig,
    include: &HashMap<String, ()>,
    exclude: &HashMap<String, ()>,
) -> Result<(u64, u64), String> {
    let (total_up, total_down) = collect_totals(cfg, include, exclude)?;
    Ok(update_speed_sample(total_up, total_down))
}

fn collect_totals(
    cfg: &CollectConfig,
    include: &HashMap<String, ()>,
    exclude: &HashMap<String, ()>,
) -> Result<(u64, u64), String> {
    let counters = nic::io_counters(cfg)
        .map_err(|e| format!("failed to get network IO counters: {e}"))?;
    if counters.is_empty() {
        return Err("no network interfaces found".into());
    }
    let mut total_up = 0u64;
    let mut total_down = 0u64;
    for c in &counters {
        if should_include(&c.name, include, exclude) {
            total_up = total_up.saturating_add(c.tx);
            total_down = total_down.saturating_add(c.rx);
        }
    }
    Ok((total_up, total_down))
}

fn update_speed_sample(total_up: u64, total_down: u64) -> (u64, u64) {
    let mut state = SPEED_STATE.lock();
    let now = Instant::now();
    let Some(prev) = state.sampled_at else {
        state.total_up = total_up;
        state.total_down = total_down;
        state.sampled_at = Some(now);
        return (0, 0);
    };
    let elapsed = now.duration_since(prev).as_secs_f64();
    if elapsed <= 0.0 {
        return (0, 0);
    }
    let up_delta = counter_delta(total_up, state.total_up);
    let down_delta = counter_delta(total_down, state.total_down);
    state.total_up = total_up;
    state.total_down = total_down;
    state.sampled_at = Some(now);
    (
        (up_delta as f64 / elapsed) as u64,
        (down_delta as f64 / elapsed) as u64,
    )
}

fn counter_delta(current: u64, previous: u64) -> u64 {
    if current >= previous {
        current - previous
    } else {
        0
    }
}

pub fn parse_nics(nics: &str) -> HashMap<String, ()> {
    let mut set = HashMap::new();
    if nics.trim().is_empty() {
        return set;
    }
    for nic in nics.split(',') {
        let nic = nic.trim();
        if !nic.is_empty() {
            set.insert(nic.to_string(), ());
        }
    }
    set
}

pub fn should_include(
    nic_name: &str,
    include: &HashMap<String, ()>,
    exclude: &HashMap<String, ()>,
) -> bool {
    const LOOPBACK_PREFIX: &[&str] = &[
        "br", "cni", "docker", "podman", "flannel", "lo", "veth", "virbr", "vmbr", "tap", "fwbr",
        "fwpr",
    ];
    for prefix in LOOPBACK_PREFIX {
        if nic_name.starts_with(prefix) {
            return false;
        }
    }
    if !include.is_empty() {
        for pattern in include.keys() {
            if glob_match(pattern, nic_name) {
                return true;
            }
        }
        return false;
    }
    for pattern in exclude.keys() {
        if glob_match(pattern, nic_name) {
            return false;
        }
    }
    true
}

fn glob_match(pattern: &str, name: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    pattern == name
}

pub fn interface_list(cfg: &CollectConfig) -> Result<Vec<String>, String> {
    let include = parse_nics(&cfg.include_nics);
    let exclude = parse_nics(&cfg.exclude_nics);
    Ok(nic::io_counters(cfg)?
        .into_iter()
        .filter(|c| should_include(&c.name, &include, &exclude))
        .map(|c| c.name)
        .collect())
}

fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
