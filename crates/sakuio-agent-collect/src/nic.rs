use crate::config::CollectConfig;

/// Kernel byte counters of one interface since boot.
/// 单个网卡自开机以来的内核字节计数。
#[derive(Debug, Clone)]
pub struct NicCounter {
    pub name: String,
    pub rx: u64,
    pub tx: u64,
}

/// Per-interface counters, same source as gopsutil `net.IOCounters(true)`.
/// 按网卡返回计数，数据源与 gopsutil `net.IOCounters(true)` 一致。
pub fn io_counters(cfg: &CollectConfig) -> Result<Vec<NicCounter>, String> {
    platform::io_counters(cfg)
}

#[cfg(target_os = "linux")]
mod platform {
    use super::NicCounter;
    use crate::config::CollectConfig;

    pub fn io_counters(cfg: &CollectConfig) -> Result<Vec<NicCounter>, String> {
        let root = if cfg.host_proc.is_empty() {
            "/proc"
        } else {
            cfg.host_proc.as_str()
        };
        let path = format!("{root}/net/dev");
        let content = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        let mut out = Vec::new();
        for line in content.lines().skip(2) {
            let Some(pos) = line.rfind(':') else {
                continue;
            };
            let name = line[..pos].trim();
            if name.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line[pos + 1..].split_whitespace().collect();
            if fields.len() < 9 {
                return Err(format!("{path}: malformed line {line:?}"));
            }
            let rx = fields[0]
                .parse::<u64>()
                .map_err(|e| format!("{path}: {e}"))?;
            let tx = fields[8]
                .parse::<u64>()
                .map_err(|e| format!("{path}: {e}"))?;
            out.push(NicCounter {
                name: name.to_string(),
                rx,
                tx,
            });
        }
        Ok(out)
    }
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
mod platform {
    use super::NicCounter;
    use crate::config::CollectConfig;
    use std::collections::HashSet;
    use std::process::Command;

    struct Row {
        link: bool,
        counter: NicCounter,
    }

    fn netstat(extra: Option<&str>) -> Result<String, String> {
        let mut cmd = Command::new("netstat");
        match extra {
            Some(iface) => cmd.arg(format!("-ibdnWI{iface}")),
            None => cmd.arg("-ibdnW"),
        };
        let out = cmd.output().map_err(|e| format!("netstat: {e}"))?;
        if !out.status.success() {
            return Err(format!("netstat exited with {}", out.status));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn parse_value(v: &str) -> Result<u64, String> {
        if v == "-" {
            return Ok(0);
        }
        v.parse::<u64>().map_err(|e| format!("netstat value {v:?}: {e}"))
    }

    fn parse_line(line: &str) -> Result<Option<Row>, String> {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.is_empty() || cols[0] == "Name" {
            return Ok(None);
        }
        if cols.len() < 11 || cols.len() > 13 {
            return Err(format!("netstat line {line:?} has {} columns", cols.len()));
        }
        let link = cols[2].starts_with("<Link#") && cols[2].ends_with('>');
        let base = if cols.len() < 12 { 0 } else { 1 };
        let rx = parse_value(cols[base + 5])?;
        let tx = parse_value(cols[base + 8])?;
        Ok(Some(Row {
            link,
            counter: NicCounter {
                name: cols[0].trim_matches('*').to_string(),
                rx,
                tx,
            },
        }))
    }

    fn parse_output(text: &str) -> Result<Vec<Row>, String> {
        let mut rows = Vec::new();
        for line in text.trim_matches('\n').lines().skip(1) {
            if let Some(row) = parse_line(line)? {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    pub fn io_counters(_cfg: &CollectConfig) -> Result<Vec<NicCounter>, String> {
        let rows = parse_output(&netstat(None)?)?;

        let mut seen = HashSet::new();
        let truncated = rows
            .iter()
            .filter(|r| r.link)
            .any(|r| !seen.insert(r.counter.name.as_str()));

        if !truncated {
            return Ok(rows
                .into_iter()
                .filter(|r| r.link)
                .map(|r| r.counter)
                .collect());
        }

        let out = Command::new("ifconfig")
            .arg("-l")
            .output()
            .map_err(|e| format!("ifconfig -l: {e}"))?;
        let names = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut result = Vec::new();
        for iface in names.split_whitespace() {
            if let Some(row) = rows.iter().find(|r| r.link && r.counter.name == iface) {
                result.push(row.counter.clone());
                continue;
            }
            let per_iface = parse_output(&netstat(Some(iface))?)?;
            if let Some(row) = per_iface.into_iter().find(|r| r.link) {
                result.push(row.counter);
            }
        }
        Ok(result)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
mod platform {
    use super::NicCounter;
    use crate::config::CollectConfig;
    use sysinfo::Networks;

    pub fn io_counters(_cfg: &CollectConfig) -> Result<Vec<NicCounter>, String> {
        let networks = Networks::new_with_refreshed_list();
        Ok(networks
            .iter()
            .map(|(name, data)| NicCounter {
                name: name.clone(),
                rx: data.total_received(),
                tx: data.total_transmitted(),
            })
            .collect())
    }
}
