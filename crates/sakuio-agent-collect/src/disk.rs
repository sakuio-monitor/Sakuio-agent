use crate::config::CollectConfig;
use sakuio_protocol::UsageReport;
use sysinfo::Disks;

struct PartitionLike {
    device: String,
    mountpoint: String,
    fstype: String,
}

pub fn disk(cfg: &CollectConfig) -> UsageReport {
    let mut out = UsageReport::default();
    if !cfg.include_mountpoints.is_empty() {
        for mount in cfg.include_mountpoints.split(';') {
            let mount = mount.trim();
            if mount.is_empty() {
                continue;
            }
            if let Some((total, used)) = usage_at_mount(mount) {
                out.total = out.total.saturating_add(total);
                out.used = out.used.saturating_add(used);
            }
        }
        return out;
    }

    let disks = Disks::new_with_refreshed_list();
    let mut device_map: std::collections::HashMap<String, (u64, u64)> =
        std::collections::HashMap::new();

    for d in disks.iter() {
        let mount = d.mount_point().to_string_lossy().to_string();
        let part = PartitionLike {
            device: d.name().to_string_lossy().to_string(),
            mountpoint: mount,
            fstype: d.file_system().to_string_lossy().to_string(),
        };
        if !is_physical_disk(&part) {
            continue;
        }
        let total = d.total_space();
        let used = d.total_space().saturating_sub(d.available_space());
        let mut device_id = part.device.clone();
        if part.fstype.eq_ignore_ascii_case("zfs") {
            if let Some(idx) = device_id.find('/') {
                device_id.truncate(idx);
            }
        }
        device_map
            .entry(device_id)
            .and_modify(|(t, u)| {
                if total > *t {
                    *t = total;
                    *u = used;
                }
            })
            .or_insert((total, used));
    }

    for (total, used) in device_map.into_values() {
        out.total = out.total.saturating_add(total);
        out.used = out.used.saturating_add(used);
    }
    out
}

fn usage_at_mount(mount: &str) -> Option<(u64, u64)> {
    let disks = Disks::new_with_refreshed_list();
    for d in disks.iter() {
        if d.mount_point().to_string_lossy() == mount {
            let total = d.total_space();
            let used = d.total_space().saturating_sub(d.available_space());
            return Some((total, used));
        }
    }
    None
}

fn is_physical_disk(part: &PartitionLike) -> bool {
    if part.mountpoint == "/" {
        return true;
    }
    let mountpoint = part.mountpoint.to_lowercase();
    let exclude_prefix = [
        "/tmp",
        "/var/tmp",
        "/dev",
        "/run",
        "/var/lib/containers",
        "/var/lib/docker",
        "/proc",
        "/sys",
        "/sys/fs/cgroup",
        "/etc/resolv.conf",
        "/etc/host",
        "/nix/store",
    ];
    for mp in exclude_prefix {
        if mountpoint == mp || mountpoint.starts_with(mp) {
            return false;
        }
    }

    let fstype = part.fstype.to_lowercase();
    if fstype == "autofs" && !part.device.starts_with("/dev/") {
        return false;
    }
    if fstype == "fuseblk" {
        return true;
    }
    if part.device == "/dev/fuse" && fstype == "fuse" && mountpoint == "/sdcard" {
        return true;
    }

    let exclude_fs = [
        "tmpfs",
        "devtmpfs",
        "udev",
        "nfs",
        "cifs",
        "smb",
        "vboxsf",
        "9p",
        "fuse",
        "overlay",
        "proc",
        "devpts",
        "sysfs",
        "cgroup",
        "mqueue",
        "hugetlbfs",
        "debugfs",
        "binfmt_misc",
        "securityfs",
        "nullfs",
    ];
    for fs in exclude_fs {
        if fstype == fs || fstype.starts_with(fs) {
            return false;
        }
    }

    if part.device.starts_with("/dev/loop") {
        return false;
    }
    true
}
