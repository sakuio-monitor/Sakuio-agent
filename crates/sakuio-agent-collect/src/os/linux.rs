use std::process::Command;

pub fn os_name() -> String {
    if let Some(v) = detect_android() {
        return v;
    }
    if let Some(v) = detect_proxmox_ve() {
        return v;
    }
    if let Some(v) = detect_synology() {
        return v;
    }
    if let Some(v) = detect_fnos() {
        return v;
    }
    read_os_release().unwrap_or_else(|| "Linux".into())
}

pub fn kernel_version() -> String {
    Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown".into())
}

fn read_os_release() -> Option<String> {
    let content = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return Some(trim_quotes(v));
        }
    }
    Some("Linux".into())
}

fn trim_quotes(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

fn detect_fnos() -> Option<String> {
    if std::path::Path::new("/usr/trim/BUILD_VERSION").is_file() {
        if let Ok(data) = std::fs::read_to_string("/usr/trim/BUILD_VERSION") {
            let version = data.trim();
            if !version.is_empty() {
                return Some(format!("fnOS {version}"));
            }
        }
    }
    if std::path::Path::new("/usr/trim").is_dir() {
        return Some("fnOS".into());
    }
    None
}

fn detect_synology() -> Option<String> {
    for file in ["/etc/synoinfo.conf", "/etc.defaults/synoinfo.conf"] {
        if std::path::Path::new(file).is_file() {
            if let Some(info) = read_synology_info(file) {
                return Some(info);
            }
        }
    }
    if std::path::Path::new("/usr/syno").is_dir() {
        return Some("Synology DSM".into());
    }
    None
}

fn read_synology_info(filename: &str) -> Option<String> {
    let content = std::fs::read_to_string(filename).ok()?;
    let mut unique = String::new();
    let mut udc_check_state = String::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("unique=") {
            unique = trim_quotes(v);
        } else if let Some(v) = line.strip_prefix("udc_check_state=") {
            udc_check_state = trim_quotes(v);
        }
    }
    if unique.contains("synology_") {
        let parts: Vec<_> = unique.split('_').collect();
        if parts.len() >= 3 {
            let model = parts.last()?.to_uppercase();
            let mut result = format!("Synology {model}");
            if !udc_check_state.is_empty() {
                result.push_str(" DSM ");
                result.push_str(&udc_check_state);
            } else {
                result.push_str(" DSM");
            }
            return Some(result);
        }
    }
    None
}

fn detect_proxmox_ve() -> Option<String> {
    let out = Command::new("pveversion").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let output = String::from_utf8_lossy(&out.stdout);
    let mut version = String::new();
    let mut codename = String::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("pve-manager/") {
            let version_part = rest.split('/').nth(0).unwrap_or(rest);
            version = version_part.split('~').next().unwrap_or(version_part).to_string();
        }
    }
    if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            if let Some(v) = line.strip_prefix("VERSION_CODENAME=") {
                codename = trim_quotes(v);
                break;
            }
        }
    }
    if !version.is_empty() {
        if !codename.is_empty() {
            return Some(format!("Proxmox VE {version} ({codename})"));
        }
        return Some(format!("Proxmox VE {version}"));
    }
    Some("Proxmox VE".into())
}

fn detect_android() -> Option<String> {
    if let Ok(out) = Command::new("getprop").arg("ro.build.version.release").output() {
        if out.status.success() && !out.stdout.is_empty() {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let model = Command::new("getprop")
                .arg("ro.product.model")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let brand = Command::new("getprop")
                .arg("ro.product.brand")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let mut result = format!("Android {version}");
            if !model.is_empty() {
                if !brand.is_empty() && brand != model {
                    result.push_str(&format!(" ({brand} {model})"));
                } else {
                    result.push_str(&format!(" ({model})"));
                }
            }
            return Some(result);
        }
    }
    if std::path::Path::new("/system/build.prop").exists() {
        return Some(read_android_build_prop());
    }
    if is_android_system() {
        return Some("Android".into());
    }
    None
}

fn read_android_build_prop() -> String {
    let Ok(content) = std::fs::read_to_string("/system/build.prop") else {
        return "Android".into();
    };
    let mut version = String::new();
    let mut model = String::new();
    let mut brand = String::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("ro.build.version.release=") {
            version = v.to_string();
        } else if let Some(v) = line.strip_prefix("ro.product.model=") {
            model = v.to_string();
        } else if let Some(v) = line.strip_prefix("ro.product.brand=") {
            brand = v.to_string();
        }
    }
    if version.is_empty() {
        return "Android".into();
    }
    let mut result = format!("Android {version}");
    if !model.is_empty() {
        if !brand.is_empty() && brand != model {
            result.push_str(&format!(" ({brand} {model})"));
        } else {
            result.push_str(&format!(" ({model})"));
        }
    }
    result
}

fn is_android_system() -> bool {
    const DIRS: &[&str] = &["/system/app", "/system/priv-app", "/data/app", "/sdcard"];
    let mut count = 0;
    for dir in DIRS {
        if std::path::Path::new(dir).is_dir() {
            count += 1;
        }
    }
    count >= 2
}
