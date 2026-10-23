use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(3);
const HIGH_LATENCY_MS: i64 = 1000;
const TCP_RETRY_DROP_MS: i64 = 800;
const MAX_HIGH_RETRIES: usize = 3;

pub fn run_ping(ping_type: &str, target: &str) -> i32 {
    let result = match ping_type {
        "icmp" => measure_icmp(target),
        "tcp" => measure_tcp(target),
        "http" => measure_http(target),
        _ => Err("unsupported ping type".into()),
    };
    match result {
        Ok(ms) if ms >= 0 => ms as i32,
        _ => -1,
    }
}

fn measure_with_retries(ping_type: &str, target: &str, measure: fn(&str) -> Result<i64, String>) -> Result<i64, String> {
    let latency = measure(target)?;
    if latency <= HIGH_LATENCY_MS {
        return Ok(latency);
    }
    let first = latency;
    for i in 0..MAX_HIGH_RETRIES {
        match measure(target) {
            Ok(second) if second <= HIGH_LATENCY_MS => {
                if ping_type == "tcp" && first - second > TCP_RETRY_DROP_MS {
                    return Err("suspicious retransmission detected in tcp handshake".into());
                }
                return Ok(second);
            }
            Ok(_) if i == MAX_HIGH_RETRIES - 1 => {
                return Err("latency remains high after retries".into());
            }
            Err(e) => return Err(e),
            _ => {}
        }
    }
    Ok(latency)
}

fn measure_icmp(target: &str) -> Result<i64, String> {
    measure_with_retries("icmp", target, icmp_once)
}

fn ping_binary() -> &'static str {
    if cfg!(windows) {
        return "ping";
    }
    for candidate in ["/sbin/ping", "/bin/ping", "/usr/bin/ping"] {
        if std::path::Path::new(candidate).exists() {
            return candidate;
        }
    }
    "ping"
}

fn bsd_ping() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))
}

fn ping_args(host: &str, timeout: Duration) -> Vec<String> {
    if cfg!(windows) {
        let mut args = vec!["-n".to_string(), "1".to_string(), "-w".to_string(), timeout.as_millis().to_string()];
        if host.contains(':') {
            args.insert(0, "-6".to_string());
        } else {
            args.insert(0, "-4".to_string());
        }
        args.push(host.to_string());
        return args;
    }
    if bsd_ping() {
        return vec![
            "-c".into(), "1".into(), "-W".into(),
            timeout.as_millis().to_string(), host.into(),
        ];
    }
    // Linux / BusyBox: -W is seconds, while -w is the overall deadline.
    let seconds = timeout.as_secs().max(1).to_string();
    let deadline = (timeout.as_secs() + 2).to_string();
    vec![
        "-c".into(), "1".into(), "-W".into(), seconds,
        "-w".into(), deadline, host.into(),
    ]
}

fn parse_latency(text: &str) -> Option<i64> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let Some((index, marker_len)) = ["time=", "time<", "时间=", "时间<", "時間=", "時間<"]
            .iter()
            .find_map(|needle| {
                lower
                    .find(needle)
                    .or_else(|| line.find(needle))
                    .map(|index| (index, needle.len()))
            })
        else {
            continue;
        };
        let mut number = line[index + marker_len..].trim_start();
        if let Some(value) = number.strip_prefix('<') {
            number = value;
        }
        let ms: String = number
            .chars()
            .take_while(|c| *c == '.' || c.is_ascii_digit())
            .collect();
        if let Ok(value) = ms.parse::<f64>() {
            return Some(value.round().max(0.0) as i64);
        }
    }
    // Windows may localize "time="; the numeric suffix is still ASCII.
    for line in text.lines() {
        let Some(ms_index) = line.to_ascii_lowercase().find("ms") else {
            continue;
        };
        let prefix = &line[..ms_index];
        let Some(separator) = prefix.rfind(['=', '<']) else { continue; };
        let number = prefix[separator + 1..].trim();
        let numeric: String = number.chars().take_while(|c| *c == '.' || c.is_ascii_digit()).collect();
        if let Ok(value) = numeric.parse::<f64>() {
            return Some(value.round().max(0.0) as i64);
        }
    }
    // Linux/macOS summary line: rtt min/avg/max/mdev = 1.2/2.3/3.4/0.5 ms
    for line in text.lines() {
        if !(line.contains("round-trip") || line.contains("rtt") || line.contains("平均")) {
            continue;
        }
        let Some(part) = line.split('=').nth(1) else { continue; };
        let avg = part.split('/').nth(1).unwrap_or("").trim();
        let numeric: String = avg.chars().take_while(|c| *c == '.' || c.is_ascii_digit()).collect();
        if let Ok(value) = numeric.parse::<f64>() {
            return Some(value.round().max(0.0) as i64);
        }
    }
    None
}

fn icmp_once(target: &str) -> Result<i64, String> {
    let host = strip_host_port(target);
    let mut command = Command::new(ping_binary());
    command.args(ping_args(&host, TIMEOUT));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let out = command.output().map_err(|error| format!("启动 ping 失败：{error}"))?;
    if !out.status.success() {
        return Err(format!(
            "ping 失败：{}",
            String::from_utf8_lossy(&out.stderr).chars().take(160).collect::<String>()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_latency(&text).ok_or_else(|| "解析 ping 输出失败".to_string())
}

fn measure_tcp(target: &str) -> Result<i64, String> {
    measure_with_retries("tcp", target, tcp_once)
}

fn tcp_once(target: &str) -> Result<i64, String> {
    let (host, port) = split_host_port(target, "80");
    let ip = resolve_host(&host)?;
    let addr = format!("{ip}:{port}");
    let socket = addr
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "resolve failed".to_string())?;
    let start = Instant::now();
    TcpStream::connect_timeout(&socket, TIMEOUT).map_err(|e| e.to_string())?;
    Ok(start.elapsed().as_millis() as i64)
}

fn measure_http(target: &str) -> Result<i64, String> {
    let mut url = target.to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("http://{url}");
    }
    let start = Instant::now();
    let resp = ureq::get(&url).timeout(TIMEOUT).call().map_err(|e| e.to_string())?;
    let latency = start.elapsed().as_millis() as i64;
    let status = resp.status();
    if (200..400).contains(&status) {
        Ok(latency)
    } else {
        Err(format!("http status {status}"))
    }
}

fn resolve_host(host: &str) -> Result<String, String> {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(host.to_string());
    }
    let addrs: Vec<_> = (host, 0)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    addrs
        .first()
        .map(|a| a.ip().to_string())
        .ok_or_else(|| "failed to resolve target".into())
}

fn strip_host_port(target: &str) -> String {
    split_host_port(target, "").0
}

fn split_host_port(target: &str, default_port: &str) -> (String, String) {
    if let Some((h, p)) = target.rsplit_once(':') {
        if !h.contains(':') && p.chars().all(|c| c.is_ascii_digit()) {
            return (h.trim_matches('[').trim_matches(']').to_string(), p.to_string());
        }
    }
    (
        target.trim_matches('[').trim_matches(']').to_string(),
        default_port.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linux_output() {
        let text = "64 bytes from 1.1.1.1: icmp_seq=1 ttl=57 time=12.4 ms";
        assert_eq!(parse_latency(text), Some(12));
    }

    #[test]
    fn parses_macos_output() {
        let text = "64 bytes from 1.1.1.1: icmp_seq=0 ttl=57 time=8.123 ms";
        assert_eq!(parse_latency(text), Some(8));
    }

    #[test]
    fn parses_windows_output() {
        let text = "Reply from 1.1.1.1: bytes=32 time=15ms TTL=57";
        assert_eq!(parse_latency(text), Some(15));
        let localised = "来自 1.1.1.1 的回复: 字节=32 时间=16ms TTL=57";
        assert_eq!(parse_latency(localised), Some(16));
    }

    #[test]
    fn parses_summary_only_output() {
        let text = "rtt min/avg/max/mdev = 10.1/12.2/14.3/0.5 ms";
        assert_eq!(parse_latency(text), Some(10));
    }

    #[test]
    fn linux_uses_seconds_for_timeout() {
        let args = ping_args("1.1.1.1", Duration::from_secs(3));
        assert!(args.contains(&"-W".to_string()));
        if bsd_ping() {
            assert!(args.contains(&"3000".to_string()));
        } else {
            assert!(args.contains(&"3".to_string()));
        }
    }
}
