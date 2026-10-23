use sakuio_protocol::{GpuDetailedReport, GpuDeviceReport, GpuModelsReport};
use serde_json::Value;

#[cfg(target_os = "linux")]
mod rocm_smi;

pub fn gpu_name() -> String {
    platform_impl::gpu_name().unwrap_or_else(|| "None".into())
}

pub fn gpu_payload(enable_gpu: bool) -> Option<Value> {
    if !enable_gpu {
        return None;
    }
    if let Ok(detailed) = platform_impl::detailed_gpu() {
        if !detailed.is_empty() {
            let mut total = 0.0;
            let devices: Vec<GpuDeviceReport> = detailed
                .into_iter()
                .map(|g| {
                    total += g.utilization;
                    GpuDeviceReport {
                        name: g.name,
                        memory_total: g.memory_total,
                        memory_used: g.memory_used,
                        utilization: g.utilization,
                        temperature: g.temperature,
                    }
                })
                .collect();
            let count = devices.len() as i32;
            let report = GpuDetailedReport {
                count,
                average_usage: if count > 0 { total / count as f64 } else { 0.0 },
                detailed_info: devices,
            };
            return serde_json::to_value(report).ok();
        }
    }
    if let Ok(models) = platform_impl::gpu_models() {
        if !models.is_empty() {
            let report = GpuModelsReport { models };
            return serde_json::to_value(report).ok();
        }
    }
    None
}

struct DetailedGpu {
    name: String,
    memory_total: u64,
    memory_used: u64,
    utilization: f64,
    temperature: u64,
}

#[cfg(target_os = "linux")]
mod platform_impl {
    use super::rocm_smi;
    use super::DetailedGpu;
    use crate::nvidia_smi;
    use once_cell::sync::Lazy;
    use regex::Regex;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const VENDOR_AMD: u8 = 1;
    const VENDOR_NVIDIA: u8 = 2;

    static VENDOR: Lazy<u8> = Lazy::new(|| {
        if nvidia_smi::gather_usage().is_ok() {
            VENDOR_NVIDIA
        } else {
            VENDOR_AMD
        }
    });

    pub fn gpu_name() -> Option<String> {
        let name = from_lspci().or_else(from_sysfs_drm)?;
        if name == "None" {
            None
        } else {
            Some(name)
        }
    }

    pub fn detailed_gpu() -> Result<Vec<DetailedGpu>, String> {
        match *VENDOR {
            VENDOR_NVIDIA => nvidia_smi::detailed_gpus().map(map_nvidia),
            VENDOR_AMD => amd_detailed(),
            _ => Err("invalid GPU vendor".into()),
        }
    }

    pub fn gpu_models() -> Result<Vec<String>, String> {
        match *VENDOR {
            VENDOR_NVIDIA => nvidia_smi::gpu_models(),
            VENDOR_AMD => amd_models(),
            _ => Err("invalid GPU vendor".into()),
        }
    }

    fn map_nvidia(list: Vec<nvidia_smi::DetailedGpu>) -> Vec<DetailedGpu> {
        list.into_iter()
            .map(|g| DetailedGpu {
                name: g.name,
                memory_total: g.memory_total,
                memory_used: g.memory_used,
                utilization: g.utilization,
                temperature: g.temperature,
            })
            .collect()
    }

    fn from_lspci() -> Option<String> {
        let out = Command::new("lspci").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let exclude_patterns = [
            r"(?i)^1111",
            r"(?i)^cirrus logic (cl[-\s]?)?gd 5",
            r"(?i)virtio",
            r"(?i)vmware",
            r"(?i)qxl",
            r"(?i)hyper-v",
        ];
        let priority = [
            "nvidia",
            "amd",
            "radeon",
            "intel",
            "arc",
            "snap",
            "qualcomm",
            "snapdragon",
        ];
        let mut result = Vec::new();
        let append = |name: &str, result: &mut Vec<String>| {
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            for pat in exclude_patterns {
                if Regex::new(pat).ok().is_some_and(|re| re.is_match(name)) {
                    return;
                }
            }
            result.push(name.to_string());
        };
        let extract = |line: &str| -> String {
            let Some(idx) = line.rfind(':') else {
                return String::new();
            };
            let mut name = line[idx + 1..].trim();
            if let Some(p) = name.find('(') {
                name = name[..p].trim();
            }
            if name.is_empty() {
                return String::new();
            }
            name.to_string()
        };
        for line in text.lines() {
            let lower = line.to_lowercase();
            if !lower.contains("vga") && !lower.contains("3d") && !lower.contains("display") {
                continue;
            }
            for vendor in priority {
                if lower.contains(vendor) {
                    append(&extract(line), &mut result);
                    break;
                }
            }
        }
        if result.is_empty() {
            for line in text.lines() {
                let lower = line.to_lowercase();
                if lower.contains("vga") || lower.contains("3d") || lower.contains("display") {
                    append(&extract(line), &mut result);
                }
            }
        }
        if result.is_empty() {
            Some("None".into())
        } else {
            Some(format_gpu_names(&result))
        }
    }

    fn from_sysfs_drm() -> Option<String> {
        let paths: Vec<PathBuf> = glob::glob("/sys/class/drm/card*").ok()?.flatten().collect();
        let excluded: HashMap<&str, bool> = [
            ("virtio-pci", true),
            ("virtio_gpu", true),
            ("bochs-drm", true),
            ("qxl", true),
            ("vmwgfx", true),
            ("cirrus", true),
            ("vboxvideo", true),
            ("hyperv_fb", true),
            ("simpledrm", true),
            ("simplefb", true),
            ("cirrus-qemu", true),
        ]
        .into_iter()
        .collect();
        let mut result = Vec::new();
        for path in paths {
            if !is_drm_card_path(&path) {
                continue;
            }
            let device_dir = path.join("device");
            let Ok(driver_link) = std::fs::read_link(device_dir.join("driver")) else {
                continue;
            };
            let driver = driver_link
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if excluded.contains_key(driver) {
                continue;
            }
            if let Ok(compatible) = std::fs::read(device_dir.join("of_node/compatible")) {
                if let Some(model) = parse_soc_model(driver, &compatible) {
                    result.push(model);
                    continue;
                }
            }
            if let Ok(name) = std::fs::read_to_string(device_dir.join("product_name")) {
                let name = name.trim().to_string();
                if !name.is_empty() && !is_excluded_sysfs_name(&name) {
                    result.push(name);
                    continue;
                }
            }
            let mapped = match driver {
                "vc4" | "vc4-drm" => {
                    std::borrow::Cow::<str>::Borrowed("Broadcom VideoCore IV/VI (Raspberry Pi)")
                }
                "v3d" | "v3d-drm" => {
                    std::borrow::Cow::<str>::Borrowed("Broadcom V3D (Raspberry Pi 4/5)")
                }
                "msm" | "msm_drm" => {
                    std::borrow::Cow::<str>::Borrowed("Qualcomm Adreno (Unknown Model)")
                }
                "panfrost" => std::borrow::Cow::<str>::Borrowed("ARM Mali (Panfrost)"),
                "lima" => std::borrow::Cow::<str>::Borrowed("ARM Mali (Lima)"),
                "sun4i-drm" | "sunxi-drm" => {
                    std::borrow::Cow::<str>::Borrowed("Allwinner Display Engine")
                }
                "tegra" => std::borrow::Cow::<str>::Borrowed("NVIDIA Tegra"),
                "ast" => std::borrow::Cow::<str>::Borrowed(
                    "ASPEED Technology, Inc. ASPEED Graphics Family",
                ),
                "i915" | "i915-drm" => {
                    std::borrow::Cow::<str>::Borrowed("Intel Integrated Graphics")
                }
                "mgag200" => std::borrow::Cow::<str>::Borrowed("Matrox G200 Series"),
                _ if !driver.is_empty() => {
                    std::borrow::Cow::Owned(format!("Direct Render Manager {driver}"))
                }
                _ => continue,
            };
            result.push(mapped.to_string());
        }
        if !result.is_empty() {
            return Some(format_gpu_names(&result));
        }
        if let Ok(model) = std::fs::read_to_string("/sys/firmware/devicetree/base/model") {
            if model.contains("Raspberry Pi") {
                return Some("Broadcom VideoCore (Integrated)".into());
            }
        }
        Some("None".into())
    }

    fn is_drm_card_path(path: &Path) -> bool {
        path.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
            s.starts_with("card")
                && !s.contains('-')
                && s.chars().skip(4).all(|c| c.is_ascii_digit())
        })
    }

    fn is_excluded_sysfs_name(name: &str) -> bool {
        let lower = name.to_lowercase();
        lower.contains("virtio")
            || lower.contains("vmware")
            || lower.contains("qxl")
            || lower.contains("hyper-v")
            || lower.contains("cirrus")
    }

    fn parse_soc_model(driver: &str, raw: &[u8]) -> Option<String> {
        let content = String::from_utf8_lossy(raw).replace('\0', " ");
        let lower = content.to_lowercase();
        if driver == "msm" || lower.contains("adreno") {
            let re = Regex::new(r"adreno[-_](\d+)").ok()?;
            if let Some(caps) = re.captures(&lower) {
                return Some(format!("Qualcomm Adreno {}", &caps[1]));
            }
            return Some("Qualcomm Adreno".into());
        }
        if driver == "panfrost" || driver == "lima" || lower.contains("mali") {
            let re = Regex::new(r"mali[-_]([a-z]\d+)").ok()?;
            if let Some(caps) = re.captures(&lower) {
                return Some(format!("ARM Mali {}", caps[1].to_uppercase()));
            }
            return Some("ARM Mali".into());
        }
        if driver == "vc4" || driver == "vc4-drm" || driver == "v3d" {
            if lower.contains("bcm2712") {
                return Some("Broadcom VideoCore VII (Pi 5)".into());
            }
            if lower.contains("bcm2711") {
                return Some("Broadcom VideoCore VI (Pi 4)".into());
            }
            if lower.contains("bcm2837") || lower.contains("bcm2835") {
                return Some("Broadcom VideoCore IV".into());
            }
        }
        if lower.contains("allwinner") || lower.contains("sun50i") || lower.contains("sun8i") {
            let re = Regex::new(r"sun\d+i-([a-z0-9]+)").ok()?;
            if let Some(caps) = re.captures(&lower) {
                return Some(format!("Allwinner {}", caps[1].to_uppercase()));
            }
            return Some("Allwinner Display Engine".into());
        }
        if driver == "tegra" {
            if lower.contains("tegra194") {
                return Some("NVIDIA Tegra Xavier".into());
            }
            if lower.contains("tegra234") {
                return Some("NVIDIA Orin".into());
            }
            if lower.contains("tegra210") {
                return Some("NVIDIA Tegra X1".into());
            }
        }
        None
    }

    fn format_gpu_names(names: &[String]) -> String {
        let mut counts = HashMap::new();
        for n in names {
            *counts.entry(n.clone()).or_insert(0usize) += 1;
        }
        let mut parts = Vec::new();
        for (name, count) in counts {
            if count > 1 {
                parts.push(format!("{name} x {count}"));
            } else {
                parts.push(name);
            }
        }
        parts.join(", ")
    }

    fn amd_detailed() -> Result<Vec<DetailedGpu>, String> {
        if let Ok(list) = rocm_smi::detailed_gpus() {
            if !list.is_empty() {
                return Ok(list
                    .into_iter()
                    .map(|g| DetailedGpu {
                        name: g.name,
                        memory_total: g.memory_total,
                        memory_used: g.memory_used,
                        utilization: g.utilization,
                        temperature: g.temperature,
                    })
                    .collect());
            }
        }
        let cards = list_amd_sysfs_cards()?;
        let fallback = gpu_name();
        Ok(cards
            .into_iter()
            .map(|c| {
                let mut name = c.name;
                if name.is_empty() || name == "None" {
                    name = fallback.clone().unwrap_or_default();
                }
                if name.is_empty() || name == "None" {
                    name = "AMD GPU".into();
                }
                DetailedGpu {
                    name,
                    memory_total: c.memory_total,
                    memory_used: c.memory_used,
                    utilization: c.utilization,
                    temperature: c.temperature,
                }
            })
            .collect())
    }

    fn amd_models() -> Result<Vec<String>, String> {
        if let Ok(models) = rocm_smi::gpu_models() {
            if !models.is_empty() {
                return Ok(models);
            }
        }
        let names: Vec<String> = amd_detailed()?
            .into_iter()
            .map(|g| g.name)
            .filter(|s| !s.is_empty())
            .collect();
        if names.is_empty() {
            Err("no AMD GPU found".into())
        } else {
            Ok(names)
        }
    }

    struct AmdCard {
        name: String,
        memory_total: u64,
        memory_used: u64,
        utilization: f64,
        temperature: u64,
    }

    fn list_amd_sysfs_cards() -> Result<Vec<AmdCard>, String> {
        let mut cards = Vec::new();
        let Ok(paths) = glob::glob("/sys/class/drm/card*") else {
            return Err("glob failed".into());
        };
        for path in paths.flatten() {
            if !is_drm_card_path(&path) {
                continue;
            }
            let device_dir = path.join("device");
            let Ok(driver_link) = std::fs::read_link(device_dir.join("driver")) else {
                continue;
            };
            if driver_link.file_name().and_then(|s| s.to_str()) != Some("amdgpu") {
                continue;
            }
            let Ok(busy) = read_sysfs_float(device_dir.join("gpu_busy_percent")) else {
                continue;
            };
            cards.push(AmdCard {
                name: read_amd_name(&device_dir),
                memory_total: read_sysfs_uint(device_dir.join("mem_info_vram_total")).unwrap_or(0),
                memory_used: read_sysfs_uint(device_dir.join("mem_info_vram_used")).unwrap_or(0),
                utilization: busy,
                temperature: read_amd_temp(&device_dir),
            });
        }
        if cards.is_empty() {
            Err("no amdgpu sysfs metrics found".into())
        } else {
            Ok(cards)
        }
    }

    fn read_amd_name(device_dir: &std::path::Path) -> String {
        for name in ["product_name", "product_description"] {
            if let Ok(raw) = std::fs::read_to_string(device_dir.join(name)) {
                let value = raw.trim();
                if !value.is_empty() {
                    return value.to_string();
                }
            }
        }
        String::new()
    }

    fn read_amd_temp(device_dir: &std::path::Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(device_dir.join("hwmon")) else {
            return 0;
        };
        let mut fallback = 0u64;
        for entry in entries.flatten() {
            let hwmon = entry.path();
            if let Ok(labels) = glob::glob(&format!("{}/temp*_label", hwmon.display())) {
                for label_path in labels.flatten() {
                    let Ok(label) = std::fs::read_to_string(&label_path) else {
                        continue;
                    };
                    let label = label.to_lowercase();
                    let fname = label_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .replace("_label", "_input");
                    let input = label_path.with_file_name(fname);
                    if let Ok(milli) = read_sysfs_uint(input) {
                        let celsius = milli / 1000;
                        if label.contains("junction") || label.contains("edge") {
                            return celsius;
                        }
                        if fallback == 0 {
                            fallback = celsius;
                        }
                    }
                }
            }
        }
        fallback
    }

    fn read_sysfs_uint(path: std::path::PathBuf) -> Result<u64, String> {
        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        data.trim()
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())
    }

    fn read_sysfs_float(path: std::path::PathBuf) -> Result<f64, String> {
        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        data.trim()
            .parse()
            .map_err(|e: std::num::ParseFloatError| e.to_string())
    }
}

#[cfg(target_os = "windows")]
mod platform_impl {
    // 照参考实现 gpu_windows.go 方案：CreateDXGIFactory1 枚举 DXGI 适配器，
    // 跳过软件适配器、按 LUID 去重，不依赖 nvidia-smi
    use super::DetailedGpu;
    use crate::nvidia_smi;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    fn dxgi_adapters() -> Vec<DXGI_ADAPTER_DESC1> {
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
            return Vec::new();
        };
        let mut adapters = Vec::new();
        for index in 0..32u32 {
            // DXGI_ERROR_NOT_FOUND 或其他错误即结束枚举
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            let mut desc = DXGI_ADAPTER_DESC1::default();
            if unsafe { adapter.GetDesc1(&mut desc) }.is_err() {
                break;
            }
            adapters.push(desc);
        }
        adapters
    }

    fn adapter_name(desc: &DXGI_ADAPTER_DESC1) -> String {
        let end = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        String::from_utf16_lossy(&desc.Description[..end])
    }

    // 参考实现 GpuName：跳过软件适配器、按 LUID 去重后拼接型号列表
    fn dxgi_gpu_names() -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut names = Vec::new();
        for desc in dxgi_adapters() {
            if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                continue;
            }
            let key = (desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart);
            if seen.insert(key) {
                let name = adapter_name(&desc);
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
        names
    }

    fn format_gpu_name_list(names: Vec<String>) -> Option<String> {
        // 照参考实现 formatGPUNameList：保持顺序、去重计数
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for name in names {
            let name = name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            if counts.get(&name).copied().unwrap_or(0) == 0 {
                order.push(name.clone());
            }
            *counts.entry(name).or_insert(0) += 1;
        }
        if order.is_empty() {
            return None;
        }
        Some(
            order
                .into_iter()
                .map(|name| {
                    let count = counts.get(&name).copied().unwrap_or(1);
                    if count > 1 {
                        format!("{name} x {count}")
                    } else {
                        name
                    }
                })
                .collect::<Vec<String>>()
                .join(", "),
        )
    }

    pub fn gpu_name() -> Option<String> {
        format_gpu_name_list(dxgi_gpu_names()).or_else(|| {
            nvidia_smi::gpu_models()
                .ok()
                .and_then(|m| m.first().cloned())
        })
    }

    pub fn detailed_gpu() -> Result<Vec<DetailedGpu>, String> {
        Ok(nvidia_smi::detailed_gpus()?
            .into_iter()
            .map(|g| DetailedGpu {
                name: g.name,
                memory_total: g.memory_total,
                memory_used: g.memory_used,
                utilization: g.utilization,
                temperature: g.temperature,
            })
            .collect())
    }

    pub fn gpu_models() -> Result<Vec<String>, String> {
        let names = dxgi_gpu_names();
        if !names.is_empty() {
            return Ok(names);
        }
        nvidia_smi::gpu_models()
    }
}

#[cfg(target_os = "macos")]
mod platform_impl {
    use super::DetailedGpu;
    use std::process::Command;

    pub fn gpu_name() -> Option<String> {
        // `system_profiler` is slow and allocates a large output buffer; the
        // model list is static, so detect it once per process.
        // `system_profiler` 慢且输出较大；型号是静态信息，进程内只检测一次。
        static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        CACHE.get_or_init(detect_gpu_name).clone()
    }

    fn detect_gpu_name() -> Option<String> {
        let out = Command::new("system_profiler")
            .arg("SPDisplaysDataType")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut names = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("Chipset Model:") {
                names.push(rest.trim().to_string());
            }
        }
        if names.is_empty() {
            None
        } else {
            Some(names.join(", "))
        }
    }

    pub fn detailed_gpu() -> Result<Vec<DetailedGpu>, String> {
        Err("detailed GPU monitoring not supported on this platform".into())
    }

    pub fn gpu_models() -> Result<Vec<String>, String> {
        gpu_name()
            .map(|n| vec![n])
            .ok_or_else(|| "detailed GPU monitoring not supported on this platform".into())
    }
}

#[cfg(target_os = "freebsd")]
mod platform_impl {
    use super::DetailedGpu;
    use std::process::Command;

    pub fn gpu_name() -> Option<String> {
        let out = Command::new("pciconf").arg("-lv").output().ok()?;
        if !out.status.success() {
            return Some("Unknown".into());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut names = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.contains("VGA") || line.contains("Display") {
                names.push(line.to_string());
            }
        }
        if names.is_empty() {
            Some("Unknown".into())
        } else {
            Some(names.join(", "))
        }
    }

    pub fn detailed_gpu() -> Result<Vec<DetailedGpu>, String> {
        Err("detailed GPU monitoring not supported on this platform".into())
    }

    pub fn gpu_models() -> Result<Vec<String>, String> {
        gpu_name()
            .map(|n| vec![n])
            .ok_or_else(|| "detailed GPU monitoring not supported on this platform".into())
    }
}

#[cfg(all(
    not(target_os = "linux"),
    not(target_os = "windows"),
    not(target_os = "macos"),
    not(target_os = "freebsd")
))]
mod platform_impl {
    use super::DetailedGpu;

    pub fn gpu_name() -> Option<String> {
        None
    }

    pub fn detailed_gpu() -> Result<Vec<DetailedGpu>, String> {
        Err("detailed GPU monitoring not supported on this platform".into())
    }

    pub fn gpu_models() -> Result<Vec<String>, String> {
        Err("detailed GPU monitoring not supported on this platform".into())
    }
}
