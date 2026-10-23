//! Public IPv4/IPv6 detection via family-scoped HTTP clients and NIC fallback.
//! 公网 IPv4/IPv6 探测（按地址族隔离 HTTP 客户端，可选网卡来源）。

use crate::config::CollectConfig;
use crate::net::interface_list;
use once_cell::sync::Lazy;
use regex::Regex;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;
use ureq::{Agent, Resolver};

const USER_AGENT: &str = "curl/8.0.1";

struct Ipv4Resolver;

impl Resolver for Ipv4Resolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        filter_socket_addrs(netloc, |addr| addr.is_ipv4())
    }
}

struct Ipv6Resolver;

impl Resolver for Ipv6Resolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        filter_socket_addrs(netloc, |addr| addr.is_ipv6())
    }
}

fn filter_socket_addrs<F>(netloc: &str, keep: F) -> io::Result<Vec<SocketAddr>>
where
    F: Fn(&SocketAddr) -> bool,
{
    ToSocketAddrs::to_socket_addrs(netloc).map(|iter| iter.filter(keep).collect())
}

static IPV4_HTTP_CLIENT: Lazy<Agent> = Lazy::new(|| {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .resolver(Ipv4Resolver)
        .build()
});

static IPV6_HTTP_CLIENT: Lazy<Agent> = Lazy::new(|| {
    ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .resolver(Ipv6Resolver)
        .build()
});

static RE_IPV4: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}").expect("ipv4 re"));

static RE_IPV6: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(([0-9A-Fa-f]{1,4}:){7})([0-9A-Fa-f]{1,4})|(([0-9A-Fa-f]{1,4}:){1,6}:)(([0-9A-Fa-f]{1,4}:){0,4})([0-9A-Fa-f]{0,4})",
    )
    .expect("ipv6 re")
});

pub fn ip_addresses(cfg: &CollectConfig) -> (String, String) {
    if cfg.get_ip_from_nic {
        if let Ok(allow_nics) = interface_list(cfg) {
            let (ipv4, ipv6) = get_ip_from_interfaces(&allow_nics);
            if !ipv4.is_empty() || !ipv6.is_empty() {
                tracing::info!(ipv4 = %ipv4, ipv6 = %ipv6, "got ip from nic");
                return (ipv4, ipv6);
            }
        }
    }

    let ipv4 = if !cfg.custom_ipv4.is_empty() {
        cfg.custom_ipv4.clone()
    } else {
        get_ipv4_address().unwrap_or_default()
    };

    let ipv6 = if !cfg.custom_ipv6.is_empty() {
        cfg.custom_ipv6.clone()
    } else {
        get_ipv6_address().unwrap_or_default()
    };

    (ipv4, ipv6)
}

fn get_ipv4_address() -> Option<String> {
    const WEB_APIS: &[&str] = &[
        "https://api.ipify.org?format=json",
        "http://ipv4.ip.sb",
        "https://edge-ip.html.zone/geo",
        "https://vercel-ip.html.zone/geo",
        "https://www.visa.cn/cdn-cgi/trace",
        "https://www.qualcomm.cn/cdn-cgi/trace",
        "https://www.toutiao.com/stream/widget/local_weather/data/",
    ];
    // Query all providers concurrently; the first valid answer wins. A slow or
    // blocked provider no longer delays the others.
    let (tx, rx) = std::sync::mpsc::channel();
    for api in WEB_APIS {
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Some(ipv4) = fetch_ipv4(api) {
                let _ = tx.send(ipv4);
            }
        });
    }
    drop(tx);
    rx.recv().ok()
}

fn fetch_ipv4(api: &str) -> Option<String> {
    let resp = IPV4_HTTP_CLIENT.get(api).call().ok()?;
    let body = resp.into_string().ok()?;
    let ipv4 = RE_IPV4.find(&body)?.as_str().to_string();
    tracing::info!(ipv4 = %ipv4, "get ipv4 success");
    Some(ipv4)
}

fn get_ipv6_address() -> Option<String> {
    const WEB_APIS: &[&str] = &[
        "https://v6.ip.zxinc.org/info.php?type=json",
        "https://api6.ipify.org?format=json",
        "https://ipv6.icanhazip.com",
        "http://api-ipv6.ip.sb/geoip",
    ];
    let (tx, rx) = std::sync::mpsc::channel();
    for api in WEB_APIS {
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Some(ipv6) = fetch_ipv6(api) {
                let _ = tx.send(ipv6);
            }
        });
    }
    drop(tx);
    rx.recv().ok()
}

fn fetch_ipv6(api: &str) -> Option<String> {
    let resp = IPV6_HTTP_CLIENT.get(api).call().ok()?;
    let body = resp.into_string().ok()?;
    let ipv6 = RE_IPV6.find(&body)?.as_str().to_string();
    tracing::info!(ipv6 = %ipv6, "get ipv6 success");
    Some(ipv6)
}

fn get_ip_from_interfaces(nic_names: &[String]) -> (String, String) {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return (String::new(), String::new());
    };

    let mut ipv4 = String::new();
    let mut ipv6 = String::new();

    for iface in interfaces {
        if !nic_names.iter().any(|n| n == &iface.name) {
            continue;
        }
        if iface.is_loopback() {
            continue;
        }

        let ip = iface.addr.ip();
        if ip.is_loopback() {
            continue;
        }

        match ip {
            IpAddr::V4(v4) if ipv4.is_empty() => ipv4 = v4.to_string(),
            IpAddr::V6(v6) if ipv6.is_empty() && !v6.is_unicast_link_local() => {
                ipv6 = v6.to_string();
            }
            _ => {}
        }

        if !ipv4.is_empty() && !ipv6.is_empty() {
            break;
        }
    }

    (ipv4, ipv6)
}
