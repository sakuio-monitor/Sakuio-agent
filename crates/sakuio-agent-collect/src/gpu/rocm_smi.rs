use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct DetailedGpu {
    pub name: String,
    pub memory_total: u64,
    pub memory_used: u64,
    pub utilization: f64,
    pub temperature: u64,
}

pub fn detailed_gpus() -> Result<Vec<DetailedGpu>, String> {
    let data = poll_rocm_smi()?;
    gather_detailed(&data)
}

pub fn gpu_models() -> Result<Vec<String>, String> {
    let data = poll_rocm_smi()?;
    gather_models(&data)
}

pub fn gather_usage() -> Result<Vec<f64>, String> {
    let data = poll_rocm_smi()?;
    gather_usage_from_json(&data)
}

fn bin_path() -> Result<String, String> {
    const DEFAULT: &str = "/opt/rocm/bin/rocm-smi";
    if Path::new(DEFAULT).is_file() {
        return Ok(DEFAULT.into());
    }
    which_rocm().ok_or_else(|| "rocm-smi tool not found".into())
}

fn which_rocm() -> Option<String> {
    Command::new("which")
        .arg("rocm-smi")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("PATH")
                .ok()
                .and_then(|_| {
                    Command::new("rocm-smi")
                        .arg("--version")
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|_| "rocm-smi".into())
                })
        })
}

fn poll_rocm_smi() -> Result<Vec<u8>, String> {
    let bin = bin_path()?;
    let out = Command::new(&bin)
        .args(["--showallinfo", "--json"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "rocm-smi failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    if out.stdout.is_empty() {
        return Err("rocm-smi returned empty output".into());
    }
    Ok(out.stdout)
}

fn gather_models(data: &[u8]) -> Result<Vec<String>, String> {
    let root: serde_json::Value =
        serde_json::from_slice(data).map_err(|e| format!("rocm json: {e}"))?;
    let Some(obj) = root.as_object() else {
        return Ok(Vec::new());
    };
    let mut models = Vec::new();
    for (key, value) in obj {
        if !key.starts_with("card") {
            continue;
        }
        if let Some(name) = value
            .get("Card series")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            models.push(name.to_string());
        }
    }
    Ok(models)
}

fn gather_usage_from_json(data: &[u8]) -> Result<Vec<f64>, String> {
    let root: serde_json::Value =
        serde_json::from_slice(data).map_err(|e| format!("rocm json: {e}"))?;
    let Some(obj) = root.as_object() else {
        return Ok(Vec::new());
    };
    let mut usage = Vec::new();
    for (key, value) in obj {
        if !key.starts_with("card") {
            continue;
        }
        let mut u = 0.0;
        if let Some(s) = value.get("GPU use (%)").and_then(|v| v.as_str()) {
            u = parse_percentage(s).unwrap_or(0.0);
        }
        usage.push(u);
    }
    Ok(usage)
}

fn gather_detailed(data: &[u8]) -> Result<Vec<DetailedGpu>, String> {
    let root: serde_json::Value =
        serde_json::from_slice(data).map_err(|e| format!("rocm json: {e}"))?;
    let Some(obj) = root.as_object() else {
        return Ok(Vec::new());
    };
    let mut gpus = Vec::new();
    for (key, value) in obj {
        if !key.starts_with("card") {
            continue;
        }
        let name = value
            .get("Card series")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let utilization = value
            .get("GPU use (%)")
            .and_then(|v| v.as_str())
            .and_then(|s| parse_percentage(s).ok())
            .unwrap_or(0.0);
        let memory_used = value
            .get("VRAM Total Used Memory (B)")
            .and_then(|v| v.as_str())
            .and_then(|s| parse_memory_bytes(s).ok())
            .unwrap_or(0);
        let memory_total = value
            .get("VRAM Total Memory (B)")
            .and_then(|v| v.as_str())
            .and_then(|s| parse_memory_bytes(s).ok())
            .unwrap_or(0);
        let temperature = value
            .get("Temperature (Sensor junction) (C)")
            .and_then(|v| v.as_str())
            .and_then(|s| parse_temperature(s).ok())
            .unwrap_or(0);
        gpus.push(DetailedGpu {
            name,
            memory_total,
            memory_used,
            utilization,
            temperature,
        });
    }
    if gpus.is_empty() {
        Err("no AMD GPU data in rocm-smi output".into())
    } else {
        Ok(gpus)
    }
}

fn parse_percentage(value: &str) -> Result<f64, String> {
    let cleaned = value.trim().trim_end_matches('%').trim();
    if cleaned.is_empty() {
        return Ok(0.0);
    }
    cleaned.parse().map_err(|e: std::num::ParseFloatError| e.to_string())
}

fn parse_memory_bytes(value: &str) -> Result<u64, String> {
    let cleaned = value.trim();
    if cleaned.is_empty() {
        return Ok(0);
    }
    cleaned.parse().map_err(|e: std::num::ParseIntError| e.to_string())
}

fn parse_temperature(value: &str) -> Result<u64, String> {
    let cleaned = value.trim().trim_end_matches('C').trim();
    if cleaned.is_empty() {
        return Ok(0);
    }
    cleaned.parse().map_err(|e: std::num::ParseIntError| e.to_string())
}
