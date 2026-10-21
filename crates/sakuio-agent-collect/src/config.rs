/// Agent collector configuration (aligned with agent CLI / env flags).
/// Agent 采集配置（与 CLI / 环境变量标志对齐）。
#[derive(Debug, Clone)]
pub struct CollectConfig {
    pub memory_include_cache: bool,
    pub memory_report_raw_used: bool,
    pub include_mountpoints: String,
    pub include_nics: String,
    pub exclude_nics: String,
    pub month_rotate: u32,
    pub enable_gpu: bool,
    pub host_proc: String,
    pub custom_ipv4: String,
    pub custom_ipv6: String,
    pub get_ip_from_nic: bool,
    pub net_static_path: String,
}

fn env_bool(key: &str) -> bool {
    std::env::var(key)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

impl Default for CollectConfig {
    fn default() -> Self {
        Self {
            memory_include_cache: env_bool("SAKUIO_AGENT_MEMORY_INCLUDE_CACHE"),
            memory_report_raw_used: env_bool("SAKUIO_AGENT_MEMORY_REPORT_RAW_USED"),
            include_mountpoints: std::env::var("SAKUIO_AGENT_INCLUDE_MOUNTPOINTS").unwrap_or_default(),
            include_nics: std::env::var("SAKUIO_AGENT_INCLUDE_NICS").unwrap_or_default(),
            exclude_nics: std::env::var("SAKUIO_AGENT_EXCLUDE_NICS").unwrap_or_default(),
            month_rotate: std::env::var("SAKUIO_AGENT_MONTH_ROTATE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            enable_gpu: std::env::var("SAKUIO_AGENT_ENABLE_GPU")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            host_proc: std::env::var("HOST_PROC").unwrap_or_default(),
            custom_ipv4: std::env::var("SAKUIO_AGENT_CUSTOM_IPV4").unwrap_or_default(),
            custom_ipv6: std::env::var("SAKUIO_AGENT_CUSTOM_IPV6").unwrap_or_default(),
            get_ip_from_nic: std::env::var("SAKUIO_AGENT_GET_IP_FROM_NIC")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            net_static_path: std::env::var("SAKUIO_NET_STATIC_PATH")
                .unwrap_or_else(|_| "./net_static.json".into()),
        }
    }
}
