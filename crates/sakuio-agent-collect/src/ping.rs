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

fn icmp_once(target: &str) -> Result<i64, String> {
    let host = strip_host_port(target);
    #[cfg(unix)]
    {
        let timeout_ms = TIMEOUT.as_millis().to_string();
        let out = Command::new("/sbin/ping")
            .args(["-c", "1", "-W", &timeout_ms, &host])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("ping failed".into());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("time=") {
                if let Some(idx) = line.find("time=") {
                    let rest = &line[idx + 5..];
                    let ms_str: String = rest.chars().take_while(|c| *c == '.' || c.is_ascii_digit()).collect();
                    if let Ok(ms) = ms_str.parse::<f64>() {
                        return Ok(ms.round() as i64);
                    }
                }
            }
            if line.contains("round-trip") {
                if let Some(part) = line.split('=').nth(1) {
                    let avg = part.split('/').nth(1).unwrap_or("").trim();
                    if let Ok(ms) = avg.parse::<f64>() {
                        return Ok(ms.round() as i64);
                    }
                }
            }
        }
        return Err("parse ping output failed".into());
    }
    #[cfg(not(unix))]
    {
        let _ = host;
        Err("icmp not supported on this platform".into())
    }
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
