#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(windows)]
mod windows;
#[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
mod bsd;

pub fn os_name() -> String {
    #[cfg(target_os = "linux")]
    {
        return linux::os_name();
    }
    #[cfg(target_os = "macos")]
    {
        return darwin::os_name();
    }
    #[cfg(windows)]
    {
        return windows::os_name();
    }
    #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        return bsd::os_name();
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        windows,
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    {
        sysinfo::System::name().unwrap_or_else(|| std::env::consts::OS.to_string())
    }
}

pub fn kernel_version() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        return Some(linux::kernel_version());
    }
    #[cfg(target_os = "macos")]
    {
        return Some(darwin::kernel_version());
    }
    #[cfg(windows)]
    {
        return Some(windows::kernel_version());
    }
    #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        return Some(bsd::kernel_version());
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        windows,
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    {
        sysinfo::System::kernel_version()
    }
}
