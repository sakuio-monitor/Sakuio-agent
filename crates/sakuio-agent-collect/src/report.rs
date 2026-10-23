use crate::config::CollectConfig;
use crate::{connections, cpu, disk, gpu, ip, load, mem, net, os, process, virt};
use sakuio_protocol::{
    BasicInfoParams, ConnectionsReport, CpuReport, NetworkReport, TelemetrySample, UsageReport,
};
use sysinfo::System;

/// Stateful collector (CPU refresh + network rate baseline).
/// 有状态采集器（CPU 刷新与网络速率基线）。
pub struct CollectState {
    pub sys: System,
}

impl Default for CollectState {
    fn default() -> Self {
        // Only CPU state is needed for per-sample usage; `new_all()` would
        // retain every process/disk/user entry for the whole agent lifetime.
        // 采样只需要 CPU 状态；`new_all()` 会把全部进程/磁盘/用户条目常驻内存。
        let mut sys = System::new();
        sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing().with_cpu_usage());
        Self { sys }
    }
}

/// Host static profile for control-plane `basic_info`.
/// 控制面 `basic_info` 用的静态主机信息。
pub struct HostStaticInfo {
    pub basic: BasicInfoParams,
}

pub struct Reporter {
    cfg: CollectConfig,
}

impl Reporter {
    pub fn new(cfg: CollectConfig) -> Self {
        Self { cfg }
    }

    pub fn from_env() -> Self {
        Self::new(CollectConfig::default())
    }

    pub fn static_info(&self, agent_version: &str) -> HostStaticInfo {
        let cpu = cpu::cpu_static();
        let ram = mem::ram(&self.cfg);
        let swap = mem::swap(&self.cfg);
        let disk = disk::disk(&self.cfg);
        let (ipv4, ipv6) = ip::ip_addresses(&self.cfg);
        let hostname = sysinfo::System::host_name();
        HostStaticInfo {
            basic: BasicInfoParams {
                hostname: hostname.filter(|s| !s.trim().is_empty()),
                cpu_name: Some(cpu.cpu_name),
                cpu_cores: Some(cpu.cpu_cores),
                cpu_physical_cores: Some(cpu.cpu_physical_cores),
                arch: Some(cpu.cpu_architecture),
                os: Some(os::os_name()),
                kernel_version: os::kernel_version(),
                ipv4: if ipv4.is_empty() { None } else { Some(ipv4) },
                ipv6: if ipv6.is_empty() { None } else { Some(ipv6) },
                mem_total: Some(ram.total as i64),
                swap_total: Some(swap.total as i64),
                disk_total: Some(disk.total as i64),
                gpu_name: Some(gpu::gpu_name()),
                virtualization: Some(virt::virtualized()),
                version: Some(agent_version.to_string()),
            },
        }
    }

    pub fn sample(&self, state: &mut CollectState) -> TelemetrySample {
        let mut message = String::new();
        let usage = cpu::cpu_usage(&mut state.sys);
        let ram = mem::ram(&self.cfg);
        let swap = mem::swap(&self.cfg);
        let load = load::load_averages();
        let disk = disk::disk(&self.cfg);

        let network = match net::network_speed(&self.cfg) {
            Ok(n) => n,
            Err(e) => {
                message.push_str(&format!("failed to get network speed: {e}\n"));
                NetworkReport::default()
            }
        };

        let connections = match connections::connections_count(&self.cfg) {
            Ok(c) => c,
            Err(e) => {
                message.push_str(&format!("failed to get connections: {e}\n"));
                ConnectionsReport::default()
            }
        };

        let uptime = sysinfo::System::uptime();
        let process = process::process_count(&self.cfg);
        let gpu = gpu::gpu_payload(self.cfg.enable_gpu);
        if self.cfg.enable_gpu && gpu.is_none() {
            message.push_str("failed to get detailed GPU info\n");
        }

        TelemetrySample {
            observed_at_ms: unix_millis(),
            cpu: CpuReport { usage },
            ram: UsageReport {
                total: ram.total,
                used: ram.used,
            },
            swap: UsageReport {
                total: swap.total,
                used: swap.used,
            },
            load,
            disk: UsageReport {
                total: disk.total,
                used: disk.used,
            },
            network,
            connections,
            gpu,
            uptime,
            process,
            message,
        }
    }
}

fn unix_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
