use crate::config::CollectConfig;
use sakuio_protocol::UsageReport;

#[cfg(target_os = "linux")]
mod linux {
    use super::UsageReport;

    pub struct ProcMemInfo {
        pub mem_total: u64,
        pub mem_free: u64,
        pub mem_available: u64,
        pub buffers: u64,
        pub cached: u64,
        pub swap_total: u64,
        pub swap_free: u64,
        pub swap_cached: u64,
        pub shmem: u64,
        pub sreclaimable: u64,
    }

    pub fn read_proc_meminfo() -> Option<ProcMemInfo> {
        let content = std::fs::read_to_string("/proc/meminfo").ok()?;
        let mut info = ProcMemInfo {
            mem_total: 0,
            mem_free: 0,
            mem_available: 0,
            buffers: 0,
            cached: 0,
            swap_total: 0,
            swap_free: 0,
            swap_cached: 0,
            shmem: 0,
            sreclaimable: 0,
        };
        for line in content.lines() {
            let mut parts = line.split_whitespace();
            let key = parts.next()?.trim_end_matches(':');
            let val: u64 = parts.next()?.parse().ok()?;
            let bytes = val.saturating_mul(1024);
            match key {
                "MemTotal" => info.mem_total = bytes,
                "MemFree" => info.mem_free = bytes,
                "MemAvailable" => info.mem_available = bytes,
                "Buffers" => info.buffers = bytes,
                "Cached" => info.cached = bytes,
                "SwapTotal" => info.swap_total = bytes,
                "SwapFree" => info.swap_free = bytes,
                "SwapCached" => info.swap_cached = bytes,
                "Shmem" => info.shmem = bytes,
                "SReclaimable" => info.sreclaimable = bytes,
                _ => {}
            }
        }
        Some(info)
    }

    pub fn mem_htop_like(info: &ProcMemInfo) -> UsageReport {
        let mut used = if info.mem_total >= info.mem_free + info.cached + info.sreclaimable + info.buffers
        {
            info.mem_total - (info.mem_free + info.cached + info.sreclaimable + info.buffers)
        } else {
            info.mem_total - info.mem_free
        };
        used = used.saturating_add(info.shmem);
        UsageReport {
            total: info.mem_total,
            used,
        }
    }

    pub fn swap_from_proc(info: &ProcMemInfo) -> UsageReport {
        let deductions = info.swap_free + info.swap_cached;
        let used = if info.swap_total >= deductions {
            info.swap_total - deductions
        } else {
            info.swap_total - info.swap_free
        };
        UsageReport {
            total: info.swap_total,
            used,
        }
    }
}

#[cfg(target_os = "macos")]
mod darwin {
    use super::UsageReport;
    use std::ffi::CStr;
    use std::mem::{size_of, zeroed};

    unsafe extern "C" {
        static vm_kernel_page_size: libc::vm_size_t;
    }

    pub struct VmInfo {
        pub total: u64,
        pub free: u64,
        pub available: u64,
    }

    unsafe fn sysctl_value<T>(name: &CStr, out: &mut T) -> bool {
        let mut len = size_of::<T>();
        // SAFETY: raw sysctl query; caller guarantees `out` is a valid `T`.
        unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                out as *mut T as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            ) == 0
        } && len == size_of::<T>()
    }

    /// Same accounting as gopsutil `mem.VirtualMemory()` on darwin.
    pub fn virtual_memory() -> Option<VmInfo> {
        unsafe {
            let mut total: u64 = 0;
            if !sysctl_value(c"hw.memsize", &mut total) || total == 0 {
                return None;
            }
            let mut stat: libc::vm_statistics64 = zeroed();
            let mut count = libc::HOST_VM_INFO64_COUNT;
            #[allow(deprecated)]
            let host = libc::mach_host_self();
            let kr = libc::host_statistics64(
                host,
                libc::HOST_VM_INFO64,
                &mut stat as *mut libc::vm_statistics64 as libc::host_info64_t,
                &mut count,
            );
            if kr != libc::KERN_SUCCESS {
                return None;
            }
            let page = vm_kernel_page_size as u64;
            let available = (u64::from(stat.inactive_count) + u64::from(stat.free_count))
                .saturating_mul(page)
                .min(total);
            let free = u64::from(stat.free_count).saturating_mul(page).min(total);
            Some(VmInfo {
                total,
                free,
                available,
            })
        }
    }

    pub fn swap() -> Option<UsageReport> {
        unsafe {
            let mut xs: libc::xsw_usage = zeroed();
            if !sysctl_value(c"vm.swapusage", &mut xs) {
                return None;
            }
            Some(UsageReport {
                total: xs.xsu_total,
                used: xs.xsu_used,
            })
        }
    }
}

pub fn ram(cfg: &CollectConfig) -> UsageReport {
    if cfg.memory_include_cache {
        #[cfg(target_os = "macos")]
        {
            return darwin::virtual_memory()
                .map(|v| UsageReport {
                    total: v.total,
                    used: v.total - v.free,
                })
                .unwrap_or_default();
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut sys = sysinfo::System::new();
            sys.refresh_memory();
            let total = sys.total_memory();
            let used = total.saturating_sub(sys.free_memory());
            return UsageReport { total, used };
        }
    }

    if cfg.memory_report_raw_used {
        #[cfg(target_os = "linux")]
        if let Some(info) = linux::read_proc_meminfo() {
            if info.mem_total > 0 {
                return linux::mem_htop_like(&info);
            }
        }
        return gopsutil_available();
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(info) = linux::read_proc_meminfo() {
            if info.mem_total > 0 {
                return linux::mem_htop_like(&info);
            }
        }
    }

    gopsutil_available()
}

#[cfg(target_os = "macos")]
fn gopsutil_available() -> UsageReport {
    darwin::virtual_memory()
        .map(|v| UsageReport {
            total: v.total,
            used: v.total - v.available,
        })
        .unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn gopsutil_available() -> UsageReport {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    UsageReport {
        total: sys.total_memory(),
        used: sys.total_memory().saturating_sub(sys.available_memory()),
    }
}

/// Same as reference agent `CallFree()` (`free -b` on Linux/FreeBSD).
pub fn call_free() -> UsageReport {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        use std::process::Command;
        if let Ok(out) = Command::new("free").arg("-b").output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                for (i, line) in text.lines().enumerate() {
                    if i == 0 {
                        continue;
                    }
                    if line.starts_with("Mem:") {
                        let fields: Vec<_> = line.split_whitespace().collect();
                        if fields.len() >= 3 {
                            if let (Ok(total), Ok(used)) =
                                (fields[1].parse::<u64>(), fields[2].parse::<u64>())
                            {
                                return UsageReport { total, used };
                            }
                        }
                    }
                }
            }
        }
    }
    UsageReport::default()
}

pub fn swap(cfg: &CollectConfig) -> UsageReport {
    let _ = cfg;
    #[cfg(target_os = "linux")]
    if let Some(info) = linux::read_proc_meminfo() {
        return linux::swap_from_proc(&info);
    }
    #[cfg(target_os = "macos")]
    if let Some(s) = darwin::swap() {
        return s;
    }
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    UsageReport {
        total: sys.total_swap(),
        used: sys.used_swap(),
    }
}
