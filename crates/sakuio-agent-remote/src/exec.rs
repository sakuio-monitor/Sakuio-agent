use futures_util::SinkExt;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
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
        let script = format!("{{\n{command}\n}} 2>&1\n");
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(script.as_bytes()).await.ok();
        }

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                pump_stream_to_channel(stdout, out_tx).await;
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
        anyhow::bail!("remote exec on Windows agent is not supported yet");
    }
    #[cfg(not(windows))]
    {
        let child = Command::new("sh")
            .arg("-s")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(child)
    }
}
