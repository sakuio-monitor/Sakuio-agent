use crate::config::CollectConfig;
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
use std::process::Command;

pub fn process_count(cfg: &CollectConfig) -> i32 {
    #[cfg(not(target_os = "linux"))]
    let _ = cfg;
    #[cfg(target_os = "linux")]
    {
        return process_count_linux(cfg);
    }
    #[cfg(target_os = "macos")]
    {
        return process_count_darwin();
    }
    #[cfg(target_os = "freebsd")]
    {
        return process_count_freebsd();
    }
    #[cfg(windows)]
    {
        return process_count_windows();
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    )))]
    {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        return i32::try_from(sys.processes().len()).unwrap_or(0);
    }
}

#[cfg(target_os = "linux")]
fn process_count_linux(cfg: &CollectConfig) -> i32 {
    let proc_dir = if !cfg.host_proc.is_empty() {
        cfg.host_proc.as_str()
    } else {
        "/proc"
    };
    let Ok(entries) = std::fs::read_dir(proc_dir) else {
        return 0;
    };
    let mut count = 0i32;
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            count += 1;
        }
    }
    count
}

#[cfg(target_os = "macos")]
fn process_count_darwin() -> i32 {
    let Ok(out) = Command::new("ps").arg("-A").output() else {
        return 0;
    };
    if !out.status.success() {
        return 0;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line_count = text.lines().count();
    i32::try_from(line_count.saturating_sub(1)).unwrap_or(0)
}

#[cfg(target_os = "freebsd")]
fn process_count_freebsd() -> i32 {
    let Ok(out) = Command::new("ps").arg("-ax").output() else {
        return 0;
    };
    if !out.status.success() {
        return 0;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line_count = text.lines().count();
    i32::try_from(line_count.saturating_sub(1)).unwrap_or(0)
}

#[cfg(windows)]
fn process_count_windows() -> i32 {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
    const MAX: usize = 1024;
    let mut pids = [0u32; MAX];
    let mut bytes_returned = 0u32;
    let ok = unsafe {
        K32EnumProcesses(
            pids.as_mut_ptr(),
            (MAX * std::mem::size_of::<u32>()) as u32,
            &mut bytes_returned,
        )
    };
    if ok == 0 {
        let _ = unsafe { GetLastError() };
        return 0;
    }
    let mut count = (bytes_returned as usize) / std::mem::size_of::<u32>();
    if count > MAX {
        count = MAX;
    }
    i32::try_from(count).unwrap_or(0)
}
