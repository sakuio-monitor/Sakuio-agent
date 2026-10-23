use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Deserialize)]
struct ResizeMsg {
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

pub struct ShellBridge {
    pub cols: u16,
    pub rows: u16,
}

impl ShellBridge {
    pub async fn run<S, R>(&self, tx: &mut S, rx: &mut R) -> anyhow::Result<()>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
        R: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: self.rows.max(2),
                cols: self.cols.max(40),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("open pty")?;

        let master = Arc::new(Mutex::new(pair.master));
        let shell = default_shell();
        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(cmd).context("spawn shell")?;
        drop(pair.slave);

        let mut reader = master
            .lock()
            .map_err(|_| anyhow::anyhow!("pty lock"))?
            .try_clone_reader()
            .context("pty reader")?;
        let mut writer = master
            .lock()
            .map_err(|_| anyhow::anyhow!("pty lock"))?
            .take_writer()
            .context("pty writer")?;

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if out_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let master_resize = master.clone();
        loop {
            tokio::select! {
                chunk = out_rx.recv() => {
                    match chunk {
                        Some(bytes) => {
                            if tx.send(Message::Binary(bytes)).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                msg = rx.next() => {
                    match msg {
                        Some(Ok(Message::Binary(b))) => {
                            writer.write_all(&b).ok();
                        }
                        Some(Ok(Message::Text(t))) => {
                            if let Ok(msg) = serde_json::from_str::<ResizeMsg>(t.trim()) {
                                let is_resize = msg.op.as_deref() == Some("resize")
                                    || msg.r#type.as_deref() == Some("resize");
                                if is_resize {
                                    let cols = msg.cols.unwrap_or(80).max(40);
                                    let rows = msg.rows.unwrap_or(24).max(2);
                                    if let Ok(guard) = master_resize.lock() {
                                        let _ = guard.resize(PtySize {
                                            cols,
                                            rows,
                                            pixel_width: 0,
                                            pixel_height: 0,
                                        });
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        _ => {}
                    }
                }
            }
        }

        let _ = child.kill();
        Ok(())
    }
}

fn is_executable(path: &str) -> bool {
    std::fs::metadata(path).map(|meta| meta.is_file()).unwrap_or(false)
}

fn shell_from_passwd() -> Option<String> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()?;
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.first().copied() != Some(user.as_str()) {
            return None;
        }
        let shell = fields.get(6).copied()?.trim();
        (is_executable(shell) && shell != "/bin/false" && shell != "/usr/sbin/nologin")
            .then(|| shell.to_string())
    })
}

fn first_usable_shell() -> Option<String> {
    for shell in [
        "/bin/bash",
        "/usr/bin/bash",
        "/bin/zsh",
        "/usr/bin/zsh",
        "/bin/sh",
        "/usr/bin/sh",
        "/bin/ash",
    ] {
        if is_executable(shell) {
            return Some(shell.to_string());
        }
    }
    None
}

#[cfg(windows)]
fn find_on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(windows)]
fn version_key(name: &str) -> Vec<u32> {
    name.split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u32>().ok())
        .collect()
}

/// PowerShell 7+ (`pwsh.exe`), newest installed copy first.
#[cfg(windows)]
fn find_pwsh() -> Option<String> {
    let mut candidates: Vec<(Vec<u32>, String)> = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        let root = std::path::PathBuf::from(program_files).join("PowerShell");
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.contains("preview") {
                    continue;
                }
                let executable = entry.path().join("pwsh.exe");
                if executable.is_file() {
                    candidates.push((
                        version_key(&name),
                        executable.to_string_lossy().into_owned(),
                    ));
                }
            }
        }
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    if let Some((_, executable)) = candidates.into_iter().next() {
        return Some(executable);
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let store = std::path::PathBuf::from(local_app_data)
            .join("Microsoft")
            .join("WindowsApps")
            .join("pwsh.exe");
        if store.is_file() {
            return Some(store.to_string_lossy().into_owned());
        }
    }
    find_on_path("pwsh.exe")
}

#[cfg(windows)]
fn find_windows_powershell() -> Option<String> {
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let candidate = std::path::PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    find_on_path("powershell.exe")
}

fn default_shell() -> String {
    #[cfg(windows)]
    {
        // Prefer PowerShell 7 (pwsh), then Windows PowerShell 5.1, and only
        // fall back to cmd.exe when neither is installed.
        if let Some(shell) = find_pwsh() {
            return shell;
        }
        if let Some(shell) = find_windows_powershell() {
            return shell;
        }
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(not(windows))]
    {
        // systemd often sets SHELL=/bin/sh even when the account uses bash.
        // Prefer the account shell from passwd, then the environment shell,
        // then common interactive shells.
        if let Some(shell) = shell_from_passwd() {
            return shell;
        }
        if let Ok(shell) = std::env::var("SHELL") {
            if is_executable(&shell) && shell != "/bin/sh" {
                return shell;
            }
        }
        first_usable_shell().unwrap_or_else(|| "/bin/sh".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_missing_shell_paths() {
        assert!(!is_executable("/definitely/not/a/shell"));
    }

    #[test]
    fn has_a_shell_on_this_platform() {
        #[cfg(not(windows))]
        assert!(is_executable(&default_shell()));
    }
}
