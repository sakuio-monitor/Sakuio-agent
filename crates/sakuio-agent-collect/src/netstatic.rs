use crate::config::CollectConfig;
use crate::net::{parse_nics, should_include};
use crate::nic;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_DATA_PRESERVE_DAY: f64 = 30.0;
const DEFAULT_DETECT_INTERVAL: f64 = 2.0;
const DEFAULT_SAVE_INTERVAL: f64 = 600.0;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TrafficData {
    timestamp: u64,
    tx: u64,
    rx: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NetStaticConfig {
    #[serde(default = "default_preserve")]
    data_preserve_day: f64,
    #[serde(default = "default_detect")]
    detect_interval: f64,
    #[serde(default = "default_save")]
    save_interval: f64,
    #[serde(default)]
    nics: Vec<String>,
}

fn default_preserve() -> f64 {
    DEFAULT_DATA_PRESERVE_DAY
}
fn default_detect() -> f64 {
    DEFAULT_DETECT_INTERVAL
}
fn default_save() -> f64 {
    DEFAULT_SAVE_INTERVAL
}

impl Default for NetStaticConfig {
    fn default() -> Self {
        Self {
            data_preserve_day: DEFAULT_DATA_PRESERVE_DAY,
            detect_interval: DEFAULT_DETECT_INTERVAL,
            save_interval: DEFAULT_SAVE_INTERVAL,
            nics: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct NetStaticFile {
    #[serde(default)]
    interfaces: HashMap<String, Vec<TrafficData>>,
    #[serde(default)]
    config: NetStaticConfig,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NicTraffic {
    pub tx: u64,
    pub rx: u64,
}

struct Counter {
    tx: u64,
    rx: u64,
}

struct NetStaticRuntime {
    collect: CollectConfig,
    path: PathBuf,
    store: NetStaticFile,
    static_cache: HashMap<String, Vec<TrafficData>>,
    last_counters: HashMap<String, Counter>,
    running: bool,
}

static RUNTIME: once_cell::sync::Lazy<Mutex<Option<NetStaticRuntime>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

pub fn ensure_running(cfg: &CollectConfig) {
    let mut guard = RUNTIME.lock();
    if guard.as_ref().is_some_and(|r| r.running) {
        return;
    }
    let path = PathBuf::from(&cfg.net_static_path);
    let mut runtime = NetStaticRuntime {
        collect: cfg.clone(),
        path: path.clone(),
        store: load_from_file(&path),
        static_cache: HashMap::new(),
        last_counters: HashMap::new(),
        running: true,
    };
    merge_nics_from_collect_cfg(&mut runtime.store.config, cfg);
    purge_expired(&mut runtime.store);
    *guard = Some(runtime);
    let detect_secs = guard
        .as_ref()
        .map(|r| r.store.config.detect_interval)
        .unwrap_or(DEFAULT_DETECT_INTERVAL);
    let save_secs = guard
        .as_ref()
        .map(|r| r.store.config.save_interval)
        .unwrap_or(DEFAULT_SAVE_INTERVAL);
    drop(guard);
    std::thread::spawn(move || background_loop(path, detect_secs, save_secs));
}

fn merge_nics_from_collect_cfg(cfg: &mut NetStaticConfig, collect: &CollectConfig) {
    let include = parse_nics(&collect.include_nics);
    if !include.is_empty() {
        cfg.nics = include.keys().cloned().collect();
    }
}

fn background_loop(path: PathBuf, detect_secs: f64, save_secs: f64) {
    let detect = Duration::from_secs_f64(detect_secs.max(0.5));
    let save_every = Duration::from_secs_f64(save_secs.max(1.0));
    let mut last_save = SystemTime::now();
    loop {
        std::thread::sleep(detect);
        {
            let mut guard = RUNTIME.lock();
            let Some(rt) = guard.as_mut() else {
                break;
            };
            sample_once(rt);
        }
        if last_save.elapsed().unwrap_or(Duration::ZERO) >= save_every {
            let mut guard = RUNTIME.lock();
            if let Some(rt) = guard.as_mut() {
                flush_cache(rt, unix_now());
                purge_expired(&mut rt.store);
                let _ = save_to_file(&rt.path, &rt.store);
            }
            last_save = SystemTime::now();
        }
    }
}

fn is_nic_allowed(name: &str, cfg: &NetStaticConfig) -> bool {
    if cfg.nics.is_empty() {
        return true;
    }
    cfg.nics.iter().any(|n| n == name)
}

fn sample_once(rt: &mut NetStaticRuntime) {
    let counters = match nic::io_counters(&rt.collect) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "netstatic: failed to read interface counters");
            return;
        }
    };
    let ts = unix_now();
    let cfg = rt.store.config.clone();
    for c in counters {
        let name = &c.name;
        if !is_nic_allowed(name, &cfg) {
            continue;
        }
        let cur_tx = c.tx;
        let cur_rx = c.rx;
        if let Some(prev) = rt.last_counters.get(name) {
            let dtx = safe_delta(cur_tx, prev.tx);
            let drx = safe_delta(cur_rx, prev.rx);
            if dtx > 0 || drx > 0 {
                rt.static_cache
                    .entry(name.clone())
                    .or_default()
                    .push(TrafficData {
                        timestamp: ts,
                        tx: dtx,
                        rx: drx,
                    });
            }
        }
        rt.last_counters.insert(
            name.clone(),
            Counter {
                tx: cur_tx,
                rx: cur_rx,
            },
        );
    }
}

fn flush_cache(rt: &mut NetStaticRuntime, ts: u64) {
    if rt.static_cache.is_empty() {
        return;
    }
    for (name, arr) in rt.static_cache.drain() {
        let mut sum_tx = 0u64;
        let mut sum_rx = 0u64;
        for td in &arr {
            sum_tx = sum_tx.saturating_add(td.tx);
            sum_rx = sum_rx.saturating_add(td.rx);
        }
        if sum_tx > 0 || sum_rx > 0 {
            rt.store
                .interfaces
                .entry(name)
                .or_default()
                .push(TrafficData {
                    timestamp: ts,
                    tx: sum_tx,
                    rx: sum_rx,
                });
        }
    }
}

fn safe_delta(cur: u64, prev: u64) -> u64 {
    if cur >= prev {
        cur - prev
    } else {
        0
    }
}

pub fn total_traffic_between(
    cfg: &CollectConfig,
    from: u64,
    to: u64,
) -> Result<HashMap<String, NicTraffic>, String> {
    ensure_running(cfg);
    let guard = RUNTIME.lock();
    let rt = guard.as_ref().ok_or("netstatic not running")?;
    let include = parse_nics(&cfg.include_nics);
    let exclude = parse_nics(&cfg.exclude_nics);
    let in_range = |ts: u64| (from == 0 || ts >= from) && (to == 0 || ts <= to);
    let mut out = HashMap::new();

    let mut add_nic = |name: &str, tx: u64, rx: u64| {
        if tx == 0 && rx == 0 {
            return;
        }
        if !should_include(name, &include, &exclude) {
            return;
        }
        let entry = out.entry(name.to_string()).or_insert(NicTraffic {
            tx: 0,
            rx: 0,
        });
        entry.tx = entry.tx.saturating_add(tx);
        entry.rx = entry.rx.saturating_add(rx);
    };

    for (nic, rows) in &rt.store.interfaces {
        let mut tx = 0u64;
        let mut rx = 0u64;
        for row in rows {
            if in_range(row.timestamp) {
                tx = tx.saturating_add(row.tx);
                rx = rx.saturating_add(row.rx);
            }
        }
        add_nic(nic, tx, rx);
    }
    for (nic, rows) in &rt.static_cache {
        let mut tx = 0u64;
        let mut rx = 0u64;
        for row in rows {
            if in_range(row.timestamp) {
                tx = tx.saturating_add(row.tx);
                rx = rx.saturating_add(row.rx);
            }
        }
        add_nic(nic, tx, rx);
    }
    Ok(out)
}

pub fn last_reset_unix(month_rotate: u32) -> u64 {
    use chrono::{Datelike, Local, TimeZone};
    let now = Local::now();
    let day = month_rotate.max(1).min(28);
    let mut reset = Local
        .with_ymd_and_hms(now.year(), now.month(), day, 0, 0, 0)
        .single()
        .unwrap_or(now);
    if reset > now {
        reset = Local
            .with_ymd_and_hms(
                if now.month() == 1 {
                    now.year() - 1
                } else {
                    now.year()
                },
                if now.month() == 1 { 12 } else { now.month() - 1 },
                day,
                0,
                0,
                0,
            )
            .single()
            .unwrap_or(reset);
    }
    reset.timestamp() as u64
}

fn load_from_file(path: &Path) -> NetStaticFile {
    let Ok(data) = std::fs::read(path) else {
        return NetStaticFile::default();
    };
    if data.is_empty() {
        return NetStaticFile::default();
    }
    match serde_json::from_slice::<NetStaticFile>(&data) {
        Ok(mut ns) => {
            ns.config = config_or_default(ns.config);
            ns
        }
        Err(_) => {
            let bak = path.with_extension("json.bak");
            let _ = std::fs::rename(path, bak);
            NetStaticFile::default()
        }
    }
}

fn config_or_default(c: NetStaticConfig) -> NetStaticConfig {
    let mut c = c;
    if c.data_preserve_day == 0.0 {
        c.data_preserve_day = DEFAULT_DATA_PRESERVE_DAY;
    }
    if c.detect_interval == 0.0 {
        c.detect_interval = DEFAULT_DETECT_INTERVAL;
    }
    if c.save_interval == 0.0 {
        c.save_interval = DEFAULT_SAVE_INTERVAL;
    }
    c
}

fn purge_expired(store: &mut NetStaticFile) {
    let ttl_secs = (store.config.data_preserve_day * 86400.0) as u64;
    let cutoff = unix_now().saturating_sub(ttl_secs);
    store.interfaces.retain(|_, arr| {
        arr.retain(|td| td.timestamp >= cutoff);
        !arr.is_empty()
    });
}

fn save_to_file(path: &Path, store: &NetStaticFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let json = serde_json::to_vec(store).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
