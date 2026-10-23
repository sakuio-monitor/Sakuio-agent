use crate::config::CollectConfig;
use sakuio_protocol::ConnectionsReport;

pub fn connections_count(cfg: &CollectConfig) -> Result<ConnectionsReport, String> {
    #[cfg(not(target_os = "linux"))]
    let _ = cfg;
    #[cfg(target_os = "linux")]
    {
        match proc_net_count(cfg) {
            Ok(r) => return Ok(r),
            Err(proc_err) => match netstat_fallback() {
                Ok(r) => Ok(r),
                Err(fallback_err) => Err(format!(
                    "proc net fast path failed: {proc_err}; gopsutil fallback failed: {fallback_err}"
                )),
            },
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        netstat_fallback()
    }
}

#[cfg(target_os = "linux")]
fn proc_root(cfg: &CollectConfig) -> String {
    if !cfg.host_proc.is_empty() {
        return cfg.host_proc.clone();
    }
    "/proc".into()
}

#[cfg(target_os = "linux")]
fn proc_net_count(cfg: &CollectConfig) -> Result<ConnectionsReport, String> {
    let root = proc_root(cfg);
    let tcp = count_proc_net_files(&root, &["tcp", "tcp6"])?;
    let udp = count_proc_net_files(&root, &["udp", "udp6"])?;
    Ok(ConnectionsReport {
        tcp: i32::try_from(tcp).unwrap_or(i32::MAX),
        udp: i32::try_from(udp).unwrap_or(i32::MAX),
    })
}

#[cfg(target_os = "linux")]
fn count_proc_net_files(root: &str, names: &[&str]) -> Result<usize, String> {
    let mut total = 0usize;
    let mut read_any = false;
    for name in names {
        let path = format!("{root}/net/{name}");
        match count_proc_net_file(&path) {
            Ok(n) => {
                total += n;
                read_any = true;
            }
            Err(e) if e.contains("No such file") => {}
            Err(e) => return Err(e),
        }
    }
    if !read_any {
        return Err(format!("no proc net files found under {root}/net"));
    }
    Ok(total)
}

#[cfg(target_os = "linux")]
fn count_proc_net_file(path: &str) -> Result<usize, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut count = 0usize;
    for (i, line) in content.lines().enumerate() {
        if i == 0 {
            continue;
        }
        if !line.trim().is_empty() {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(not(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly")))]
fn netstat_fallback() -> Result<ConnectionsReport, String> {
    use netstat2::{get_sockets_info, AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo};
    let af = AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6;
    let tcp_flags = ProtocolFlags::TCP;
    let udp_flags = ProtocolFlags::UDP;
    let tcp = get_sockets_info(af, tcp_flags).map_err(|e| e.to_string())?;
    let udp = get_sockets_info(af, udp_flags).map_err(|e| e.to_string())?;
    let tcp_count = tcp
        .iter()
        .filter(|s| matches!(s.protocol_socket_info, ProtocolSocketInfo::Tcp(_)))
        .count();
    let udp_count = udp
        .iter()
        .filter(|s| matches!(s.protocol_socket_info, ProtocolSocketInfo::Udp(_)))
        .count();
    Ok(ConnectionsReport {
        tcp: i32::try_from(tcp_count).unwrap_or(i32::MAX),
        udp: i32::try_from(udp_count).unwrap_or(i32::MAX),
    })
}

#[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
fn netstat_fallback() -> Result<ConnectionsReport, String> {
    fn count(proto: &str) -> Result<i32, String> {
        let output = std::process::Command::new("netstat")
            .args(["-an", "-p", proto])
            .output()
            .map_err(|e| format!("netstat -p {proto}: {e}"))?;
        if !output.status.success() {
            return Err(format!("netstat -p {proto} exited with {}", output.status));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .lines()
            .filter(|line| {
                let line = line.trim();
                !line.is_empty() && !line.starts_with("Active") && !line.starts_with("Proto")
            })
            .count() as i32)
    }
    Ok(ConnectionsReport {
        tcp: count("tcp")?,
        udp: count("udp")?,
    })
}
