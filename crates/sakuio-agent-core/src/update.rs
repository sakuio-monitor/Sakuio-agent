//! Agent binary self-update.
//! 二进制自更新：每 6 小时检查一次；仅在面板连接存活时执行。
//! 升级目标受面板 welcome 下发的兼容区间约束：最新版不高于区间上界才升级。
//! 替换成功后通过回调请求进程退出（退出码 42），由服务管理器
//! （systemd `Restart=always` / nssm `AppExit Default Restart`）负责拉起新版本。

use anyhow::{anyhow, bail, Context};
use sakuio_protocol::AgentVersionRange;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::MissedTickBehavior;
use tracing::{error, info, warn};
use ureq::{Agent, AgentBuilder};

/// Board link state snapshot shared between the session runner and the update loop.
/// 面板链路状态共享快照：会话循环写入，自更新循环读取。
/// 断开连接时不执行升级；`version_range` 为最近一次 welcome 下发的兼容区间。
#[derive(Debug, Clone, Default)]
pub struct BoardLinkSnapshot {
    pub connected: bool,
    pub version_range: Option<AgentVersionRange>,
}

const GITHUB_API_BASE: &str = "https://api.github.com";
const DEFAULT_REPO: &str = "sakuio-monitor/Sakuio-agent";
const USER_AGENT: &str = "sakuio-agent";
const MAX_ASSET_SIZE: u64 = 128 * 1024 * 1024;
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// 容器内禁止二进制自更新的标记文件。
const CONTAINER_MARKER: &str = "/.sakuio-agent-container";
/// 更新安装成功后的进程退出码，服务管理器据此感知并拉起。
pub const RESTART_EXIT_CODE: i32 = 42;

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: String,
    browser_download_url: String,
}

fn http_client() -> Agent {
    AgentBuilder::new().timeout(HTTP_TIMEOUT).build()
}

/// 解析 `v`/`V` 前缀与预发布/构建元数据后缀，取 major.minor.patch。
/// 容忍两段版本号（缺失段按 0 处理）。
fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let core = value.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor = parts.next().unwrap_or("0").trim().parse().ok()?;
    let patch = parts.next().unwrap_or("0").trim().parse().ok()?;
    Some((major, minor, patch))
}

/// 资产名与发布流水线的命名约定一致：`sakuio-agent-{os}-{arch}[.exe]`。
/// Rust 的 OS 常量 `macos` 对应发布资产里的 `darwin`，架构名也需映射。
fn expected_asset_name() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        "loongarch64" => "loong64",
        "riscv64gc" => "riscv64",
        other => other,
    };
    let exe = if std::env::consts::OS == "windows" {
        ".exe"
    } else {
        ""
    };
    format!("sakuio-agent-{os}-{arch}{exe}")
}

fn is_container_agent() -> bool {
    std::path::Path::new(CONTAINER_MARKER).exists()
}

fn fetch_releases(client: &Agent, repo: &str) -> anyhow::Result<Vec<Release>> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid repo slug {repo:?}, expected owner/name"))?;

    let mut releases = Vec::new();
    for page in 1.. {
        let endpoint = format!("{GITHUB_API_BASE}/repos/{owner}/{name}/releases?per_page=100&page={page}");
        let mut req = client
            .get(&endpoint)
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", USER_AGENT);
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            if !token.trim().is_empty() {
                req = req.set("Authorization", &format!("Bearer {token}"));
            }
        }
        let resp = req
            .call()
            .with_context(|| format!("failed to list GitHub releases (page {page})"))?;

        let page_releases: Vec<Release> = serde_json::from_reader(
            resp.into_reader(),
        )
        .with_context(|| format!("failed to decode GitHub releases response (page {page})"))?;
        let done = page_releases.len() < 100;
        releases.extend(page_releases);
        if done {
            break;
        }
    }
    Ok(releases)
}

fn select_candidate<'a>(
    releases: &'a [Release],
    current: (u64, u64, u64),
    asset_name: &str,
    max_allowed: Option<(u64, u64, u64)>,
) -> Option<&'a Release> {
    releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter(|r| {
            parse_version(&r.tag_name).is_some_and(|v| {
                if v <= current {
                    return false;
                }
                match max_allowed {
                    // 面板声明了兼容区间：候选不得越过区间上界
                    Some(max) => v <= max,
                    // 旧面板未声明区间：保守地只升当前 major.minor 线
                    None => v.0 == current.0 && v.1 == current.1,
                }
            })
        })
        .filter(|r| r.assets.iter().any(|a| a.name == asset_name))
        .max_by_key(|r| parse_version(&r.tag_name).unwrap_or((0, 0, 0)))
}

fn download_asset(client: &Agent, asset: &Asset) -> anyhow::Result<Vec<u8>> {
    if asset.size == 0 || asset.size > MAX_ASSET_SIZE {
        bail!(
            "invalid release asset size {} for {} (allowed: 1..={MAX_ASSET_SIZE})",
            asset.size,
            asset.name
        );
    }
    let resp = client
        .get(&asset.browser_download_url)
        .set("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("failed to download release asset {}", asset.name))?;

    let mut data = Vec::with_capacity(asset.size as usize);
    resp.into_reader().take(asset.size + 1).read_to_end(&mut data)?;
    if data.len() as u64 != asset.size {
        bail!(
            "release asset size mismatch for {}: got {} bytes, want {}",
            asset.name,
            data.len(),
            asset.size
        );
    }
    Ok(data)
}

/// 用发布 API 返回的 digest 校验下载内容。
/// 老资产可能没有 digest，此时只告警并放行；digest 存在但不匹配则拒绝安装。
fn verify_asset_digest(asset: &Asset, data: &[u8]) -> anyhow::Result<()> {
    if asset.digest.is_empty() {
        warn!(
            "release asset {} has no digest; skipping integrity check",
            asset.name
        );
        return Ok(());
    }
    let (algorithm, want) = asset
        .digest
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid digest format {:?}", asset.digest))?;
    if !algorithm.eq_ignore_ascii_case("sha256") {
        bail!("unsupported release asset digest {:?}", asset.digest);
    }
    let sum = Sha256::digest(data);
    let mut got = String::with_capacity(sum.len() * 2);
    for byte in sum {
        got.push_str(&format!("{byte:02x}"));
    }
    if !want.eq_ignore_ascii_case(&got) {
        bail!(
            "checksum mismatch for {}: got sha256:{got}, want {}",
            asset.name,
            asset.digest
        );
    }
    Ok(())
}

fn current_executable_path() -> anyhow::Result<PathBuf> {
    let path = std::env::current_exe().context("failed to resolve current executable path")?;
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
}

/// 清理上次更新失败或中断残留的临时文件与 `.old` 备份。
/// 只删匹配自身文件名模式的文件，误删不了正在运行的二进制。
fn cleanup_stale_update_files() {
    let Ok(exe) = current_executable_path() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let Some(name) = exe.file_name() else {
        return;
    };
    let name = name.to_string_lossy();
    let temp_prefix = format!(".{name}.update-");
    let backup = format!("{name}.old");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let candidate = entry.file_name().to_string_lossy().into_owned();
        if candidate.starts_with(&temp_prefix) || candidate == backup {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 替换当前可执行文件：写入同目录临时文件，再原子 rename 到位。
/// Unix 的 rename 对运行中的可执行文件安全（inode 替换）；
/// Windows 运行中的 exe 无法覆盖，先改名为 `.old` 再换入新文件。
fn install_asset(data: &[u8]) -> anyhow::Result<()> {
    let exe = current_executable_path()?;
    let file_name = exe
        .file_name()
        .ok_or_else(|| anyhow!("failed to resolve executable file name"))?
        .to_string_lossy()
        .to_string();
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("failed to resolve executable directory"))?;

    info!("will update {} to a newer release", exe.display());
    let tmp = dir.join(format!(".{file_name}.update-{}", std::process::id()));
    std::fs::write(&tmp, data).with_context(|| format!("failed to write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }

    #[cfg(windows)]
    let old = {
        let old = dir.join(format!("{file_name}.old"));
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old)
            .with_context(|| format!("failed to back up {}", exe.display()))?;
        old
    };

    if let Err(e) = std::fs::rename(&tmp, &exe) {
        #[cfg(windows)]
        {
            let _ = std::fs::rename(&old, &exe);
        }
        bail!("failed to replace {}: {e}", exe.display());
    }

    #[cfg(windows)]
    let _ = std::fs::remove_file(&old);

    info!("binary replaced successfully; restart required");
    Ok(())
}

/// 执行一次升级检查；返回是否安装了新版本（需要重启进程）。
/// `snapshot` 为面板链路状态快照：断开连接时跳过本轮；区间上界约束升级目标。
fn check_and_update(current_version: &str, repo: &str, snapshot: BoardLinkSnapshot) -> anyhow::Result<bool> {
    info!("checking update...");
    cleanup_stale_update_files();
    if !snapshot.connected {
        info!("board link is down; skip update check this round");
        return Ok(false);
    }
    if is_container_agent() {
        info!("agent is running in a container; skip binary self-update, refresh the image instead");
        return Ok(false);
    }

    let Some(current) = parse_version(current_version) else {
        // 无法解析的版本（例如本地 dev 构建）跳过自更新。
        warn!(
            "cannot parse current version {current_version:?}; automatic updates are disabled"
        );
        return Ok(false);
    };
    let max_allowed = snapshot
        .version_range
        .and_then(|r| parse_version(&r.max));

    let client = http_client();
    let releases = fetch_releases(&client, repo)?;
    let asset_name = expected_asset_name();
    let candidate = match select_candidate(&releases, current, &asset_name, max_allowed) {
        Some(c) => c,
        None => {
            info!("current version {current_version} is up-to-date");
            return Ok(false);
        }
    };

    let tag = &candidate.tag_name;
    let asset = candidate
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .expect("candidate was selected with a matching asset");

    info!("downloading update {tag} ({asset_name})...");
    let data = download_asset(&client, asset)
        .with_context(|| format!("failed to download version {tag}"))?;
    verify_asset_digest(asset, &data)
        .with_context(|| format!("refusing to update to version {tag}"))?;
    install_asset(&data).with_context(|| format!("failed to install version {tag}"))?;
    Ok(true)
}

/// 升级检查循环：每 6 小时检查一次；面板断开连接的轮次直接跳过。
/// 每次检查前从共享槽读取链路快照（连接状态 + welcome 兼容区间）。
/// 检查在阻塞线程池执行，避免占用异步运行时；失败只记日志并继续下一轮。
pub async fn run_update_loop(
    current_version: String,
    repo: Option<String>,
    link_state_slot: Arc<std::sync::RwLock<BoardLinkSnapshot>>,
    on_restart: impl FnOnce() + Send + 'static,
) {
    // Self-update only runs in release builds; debug builds are for local
    // development and must never be replaced by a downloaded binary.
    // 仅 release 构建启用自更新；debug 构建用于本地开发，
    // 不允许被下载的二进制覆盖。
    if cfg!(debug_assertions) {
        info!("auto update disabled in debug builds");
        return;
    }
    let repo = repo.unwrap_or_else(|| DEFAULT_REPO.to_string());

    // 阻塞线程池中执行，一次安装成功即结束整个循环。
    async fn once(
        version: &str,
        repo: &str,
        slot: &Arc<std::sync::RwLock<BoardLinkSnapshot>>,
    ) -> bool {
        let v = version.to_string();
        let r = repo.to_string();
        let snapshot = slot.read().map(|guard| guard.clone()).unwrap_or_default();
        match tokio::task::spawn_blocking(move || check_and_update(&v, &r, snapshot)).await {
            Ok(Ok(true)) => true,
            Ok(Ok(false)) => false,
            Ok(Err(e)) => {
                error!("[ERROR] {e:#}");
                false
            }
            Err(e) => {
                error!("[ERROR] update task join failed: {e}");
                false
            }
        }
    }

    // 无法解析的版本只需警告一次，之后静默跳过。
    if parse_version(&current_version).is_none() {
        warn!(
            "cannot parse current version {current_version:?}; automatic updates are disabled"
        );
        return;
    }

    if once(&current_version, &repo, &link_state_slot).await {
        on_restart();
        return;
    }
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await; // 消耗 interval 立即触发的首次 tick
    loop {
        ticker.tick().await;
        if once(&current_version, &repo, &link_state_slot).await {
            on_restart();
            return;
        }
    }
}
