use quick_xml::de::from_str;
use serde::Deserialize;
use std::process::Command;

pub struct DetailedGpu {
    pub name: String,
    pub memory_total: u64,
    pub memory_used: u64,
    pub utilization: f64,
    pub temperature: u64,
}

pub fn detailed_gpus() -> Result<Vec<DetailedGpu>, String> {
    let xml = nvidia_xml().ok_or("nvidia-smi unavailable")?;
    let parsed = parse_nvidia_xml(&xml)?;
    Ok(parsed
        .gpus
        .into_iter()
        .map(|g| DetailedGpu {
            name: g.product_name,
            memory_total: parse_mib(&g.fb_memory_usage.total),
            memory_used: parse_mib(&g.fb_memory_usage.used),
            utilization: parse_pct(&g.utilization.gpu_util),
            temperature: g
                .temperature
                .gpu_temp
                .trim()
                .trim_end_matches('C')
                .trim()
                .parse()
                .unwrap_or(0),
        })
        .collect())
}

pub fn gather_usage() -> Result<Vec<f64>, String> {
    let list = detailed_gpus()?;
    Ok(list.into_iter().map(|g| g.utilization).collect())
}

pub fn gpu_models() -> Result<Vec<String>, String> {
    let xml = nvidia_xml().ok_or("nvidia-smi unavailable")?;
    let parsed = parse_nvidia_xml(&xml)?;
    Ok(parsed
        .gpus
        .into_iter()
        .map(|g| g.product_name)
        .filter(|s| !s.is_empty())
        .collect())
}

fn nvidia_bin() -> Option<String> {
    for candidate in ["/usr/bin/nvidia-smi", "/usr/local/bin/nvidia-smi"] {
        if std::path::Path::new(candidate).exists() {
            return Some(candidate.into());
        }
    }
    #[cfg(windows)]
    {
        if std::path::Path::new(r"C:\Windows\System32\nvidia-smi.exe").exists() {
            return Some(r"C:\Windows\System32\nvidia-smi.exe".into());
        }
    }
    let out = Command::new(if cfg!(windows) { "where" } else { "sh" })
        .args(if cfg!(windows) {
            vec!["nvidia-smi"]
        } else {
            vec!["-c", "command -v nvidia-smi"]
        })
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

fn nvidia_xml() -> Option<String> {
    let bin = nvidia_bin()?;
    let out = Command::new(bin).args(["-q", "-x"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into())
}

#[derive(Debug, Deserialize)]
struct NvidiaXml {
    #[serde(rename = "gpu", default)]
    gpus: Vec<NvidiaGpuXml>,
}

#[derive(Debug, Deserialize)]
struct NvidiaGpuXml {
    #[serde(rename = "product_name", default)]
    product_name: String,
    #[serde(rename = "fb_memory_usage", default)]
    fb_memory_usage: NvidiaMem,
    #[serde(rename = "utilization", default)]
    utilization: NvidiaUtil,
    #[serde(rename = "temperature", default)]
    temperature: NvidiaTemp,
}

#[derive(Debug, Default, Deserialize)]
struct NvidiaMem {
    #[serde(rename = "total", default)]
    total: String,
    #[serde(rename = "used", default)]
    used: String,
}

#[derive(Debug, Default, Deserialize)]
struct NvidiaUtil {
    #[serde(rename = "gpu_util", default)]
    gpu_util: String,
}

#[derive(Debug, Default, Deserialize)]
struct NvidiaTemp {
    #[serde(rename = "gpu_temp", default)]
    gpu_temp: String,
}

fn parse_nvidia_xml(xml: &str) -> Result<NvidiaXml, String> {
    from_str(xml).map_err(|e| e.to_string())
}

fn parse_mib(s: &str) -> u64 {
    s.trim()
        .trim_end_matches("MiB")
        .trim()
        .parse::<f64>()
        .map(|v| (v * 1024.0 * 1024.0) as u64)
        .unwrap_or(0)
}

fn parse_pct(s: &str) -> f64 {
    s.trim()
        .trim_end_matches('%')
        .trim()
        .parse()
        .unwrap_or(0.0)
}
