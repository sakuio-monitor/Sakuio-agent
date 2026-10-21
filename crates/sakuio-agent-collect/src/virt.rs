use once_cell::sync::Lazy;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use raw_cpuid::CpuId;
use regex::Regex;
use std::process::Command;

static DOCKER_ID: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)/(?:docker|cri-containerd)[/-]([0-9a-f]{12,64})(?:\.scope)?$").unwrap()
});
static DOCKER_SCOPE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)/docker-[0-9a-f]{12,64}\.scope$").unwrap());
static KUBE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)/kubepods[/.].*([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}).*")
        .unwrap()
});
static PODMAN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)/(?:libpod|podman)[-_]([0-9a-f]{12,64})(?:\.scope)?$").unwrap());
static LXC: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)/lxc/[^/]+$").unwrap());
static CRIO: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)/crio-[0-9a-f]{12,64}\.scope$").unwrap());

pub fn virtualized() -> String {
    #[cfg(windows)]
    {
        return detect_by_cpuid();
    }
    if let Ok(out) = Command::new("systemd-detect-virt").output() {
        if out.status.success() {
            let virt = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !virt.is_empty() {
                return virt;
            }
        }
    }
    if let Some(ct) = detect_container() {
        return ct;
    }
    detect_by_cpuid()
}

fn detect_by_cpuid() -> String {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let cpuid = CpuId::new();
        let Some(info) = cpuid.get_feature_info() else {
            return "none".into();
        };
        if !info.has_hypervisor() {
            return "none".into();
        }
        let vendor = cpuid
            .get_hypervisor_info()
            .map(|hv| hv.as_str().to_lowercase())
            .unwrap_or_default();
        return map_hypervisor_vendor(&vendor);
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        "none".into()
    }
}

fn map_hypervisor_vendor(vendor: &str) -> String {
    const MAP: &[(&str, &[&str])] = &[
        ("kvm", &["kvm"]),
        ("microsoft", &["microsoft", "hyper-v", "msvm", "mshyperv"]),
        ("vmware", &["vmware"]),
        ("xen", &["xen"]),
        ("bhyve", &["bhyve"]),
        ("qemu", &["qemu"]),
        ("parallels", &["parallels"]),
        ("oracle", &["oracle", "virtualbox", "vbox"]),
        ("acrn", &["acrn"]),
    ];
    for (name, keys) in MAP {
        for key in *keys {
            if vendor == *key || vendor.contains(key) {
                return (*name).to_string();
            }
        }
    }
    if vendor.is_empty() {
        "virtualized".into()
    } else {
        vendor.to_string()
    }
}

fn detect_container() -> Option<String> {
    if file_exists("/.dockerenv") {
        return Some("docker".into());
    }
    if file_exists("/run/.containerenv") {
        return parse_cgroup_for_container().or(Some("container".into()));
    }
    if let Some(s) = parse_cgroup_for_container() {
        return Some(s);
    }
    if file_exists("/dev/.lxc-boot-id") {
        return Some("lxc".into());
    }
    if file_exists("/.sakuio-agent-container") {
        return Some("container".into());
    }
    None
}

fn file_exists(p: &str) -> bool {
    std::path::Path::new(p)
        .metadata()
        .map(|m| m.is_file())
        .unwrap_or(false)
}

fn parse_cgroup_for_container() -> Option<String> {
    let content = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let lower = content.to_lowercase();
    if DOCKER_ID.is_match(&lower) || DOCKER_SCOPE.is_match(&lower) {
        return Some("docker".into());
    }
    if PODMAN.is_match(&lower) {
        return Some("podman".into());
    }
    if CRIO.is_match(&lower) {
        return Some("container".into());
    }
    if KUBE.is_match(&lower) {
        return Some("kubernetes".into());
    }
    if LXC.is_match(&lower) {
        return Some("lxc".into());
    }
    None
}
