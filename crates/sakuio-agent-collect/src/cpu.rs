use sysinfo::{CpuRefreshKind, System};

pub struct CpuStatic {
    pub cpu_name: String,
    pub cpu_architecture: String,
    pub cpu_cores: i32,
    pub cpu_physical_cores: i32,
}

pub fn cpu_static() -> CpuStatic {
    let mut sys = System::new();
    sys.refresh_cpu_list(CpuRefreshKind::everything());
    let cpu_name = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown".into());

    #[cfg(target_os = "linux")]
    let cpu_name = {
        if cpu_name == "Unknown" {
            read_cpu_name_from_proc().unwrap_or(cpu_name)
        } else {
            cpu_name
        }
    };

    let cpu_cores = i32::try_from(sys.cpus().len()).unwrap_or(1).max(1);
    let cpu_physical_cores = sys
        .physical_core_count()
        .and_then(|n| i32::try_from(n).ok())
        .unwrap_or(0);

    CpuStatic {
        cpu_name,
        cpu_architecture: std::env::consts::ARCH.to_string(),
        cpu_cores,
        cpu_physical_cores,
    }
}

pub fn cpu_usage(sys: &mut System) -> f64 {
    sys.refresh_cpu_usage();
    let usage = sys.global_cpu_usage();
    if f64::from(usage) <= 0.001 {
        0.001
    } else {
        f64::from(usage)
    }
}

#[cfg(target_os = "linux")]
fn read_cpu_name_from_proc() -> Option<String> {
    let content = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in content.lines() {
        if line.starts_with("Model\t")
            || line.starts_with("Hardware\t")
            || line.starts_with("Processor\t")
        {
            let mut parts = line.splitn(2, ':');
            parts.next()?;
            return Some(parts.next()?.trim().to_string());
        }
    }
    None
}

// Fallback stub keeps the call site (Linux-only) compiling on all targets.
// 保留的空实现：调用点仅在 Linux 分支，此桩保证跨平台编译。
#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn read_cpu_name_from_proc() -> Option<String> {
    None
}
