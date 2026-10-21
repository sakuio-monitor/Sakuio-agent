use winreg::enums::HKEY_LOCAL_MACHINE;
use winreg::RegKey;

pub fn os_name() -> String {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(key) = hklm.open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") else {
        return "Microsoft Windows".into();
    };
    let Ok(product_name) = key.get_value::<String, _>("ProductName") else {
        return "Microsoft Windows".into();
    };
    if product_name.contains("Server") {
        return product_name;
    }
    if product_name.contains("Windows 11") {
        return product_name;
    }
    if let Ok(build_str) = key.get_value::<String, _>("CurrentBuild") {
        if let Ok(build) = build_str.parse::<u32>() {
            if build >= 22000 {
                if let Some(edition) = product_name.strip_prefix("Windows 10 ") {
                    return format!("Windows 11 {edition}");
                }
                if product_name == "Windows 10" {
                    return "Windows 11".into();
                }
                if !product_name.contains("Windows 11") {
                    return product_name.replace("Windows 10", "Windows 11");
                }
            }
        }
    }
    product_name
}

pub fn kernel_version() -> String {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(key) = hklm.open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") else {
        return "Unknown".into();
    };
    let Ok(build) = key.get_value::<String, _>("CurrentBuild") else {
        return "Unknown".into();
    };
    if let Ok(ubr) = key.get_value::<u32, _>("UBR") {
        format!("{build}.{ubr}")
    } else {
        build
    }
}
