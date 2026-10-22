use futures_util::SinkExt;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub struct ExecBridge {
    pub command: String,
}

impl ExecBridge {
    pub async fn run<S>(&self, tx: &mut S) -> anyhow::Result<(String, i32)>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let command = self.command.trim();
        if command.is_empty() {
            let msg = "No command provided";
            let _ = tx.send(Message::Binary(msg.as_bytes().to_vec())).await;
            let _ = tx
                .send(Message::Text(r#"{"op":"exit","code":0}"#.into()))
                .await;
            return Ok((msg.to_string(), 0));
        }

        let mut child = build_command().await?;
        let script = build_script(command);
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(script.as_bytes()).await.ok();
        }

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        if let Some(stdout) = child.stdout.take() {
            let stdout_tx = out_tx.clone();
            tokio::spawn(async move {
                pump_stream_to_channel(stdout, stdout_tx).await;
            });
        }
        // Unix merges stderr into stdout at the shell level; Windows pipes it here.
        if let Some(stderr) = child.stderr.take() {
            let err_tx = out_tx.clone();
            tokio::spawn(async move {
                pump_stream_to_channel(stderr, err_tx).await;
            });
        }

        let output = Arc::new(Mutex::new(String::new()));
        while let Some(chunk) = out_rx.recv().await {
            if let Ok(mut guard) = output.lock() {
                guard.push_str(&String::from_utf8_lossy(&chunk));
            }
            if tx.send(Message::Binary(chunk)).await.is_err() {
                break;
            }
        }

        let status = child.wait().await?;
        let exit_code = status.code().unwrap_or(if status.success() { 0 } else { -1 });
        let exit_json = format!(r#"{{"op":"exit","code":{exit_code}}}"#);
        let _ = tx.send(Message::Text(exit_json.into())).await;

        Ok((output.lock().map(|g| g.clone()).unwrap_or_default(), exit_code))
    }
}

async fn pump_stream_to_channel<R>(stream: R, tx: mpsc::UnboundedSender<Vec<u8>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

async fn build_command() -> anyhow::Result<tokio::process::Child> {
    #[cfg(windows)]
    {
        use std::process::Stdio;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let child = Command::new("cmd.exe")
            .arg("/Q")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        Ok(child)
    }
    #[cfg(not(windows))]
    {
        let child = Command::new(find_unix_shell())
            .arg("-s")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(child)
    }
}

/// 机器上有什么 shell 用什么，都没有才退回 sh
/// 候选均为 POSIX 兼容（支持 -s 与花括号块）：bash、zsh、ksh、mksh、dash、ash
#[cfg(not(windows))]
fn find_unix_shell() -> &'static str {
    const CANDIDATES: [&str; 6] = ["bash", "zsh", "ksh", "mksh", "dash", "ash"];
    let Ok(path) = std::env::var("PATH") else {
        return "sh";
    };
    for name in CANDIDATES {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            if std::path::Path::new(dir).join(name).is_file() {
                return name;
            }
        }
    }
    "sh"
}

/// Feeds the command to the child's stdin as a shell script.
#[cfg(not(windows))]
fn build_script(command: &str) -> String {
    format!("{{\n{command}\n}} 2>&1\n")
}

/// Windows: drive interactive `cmd.exe`; keep the exit code of the last command.
#[cfg(windows)]
fn build_script(command: &str) -> String {
    format!("{command}\r\nexit /b %ERRORLEVEL%\r\n")
}
