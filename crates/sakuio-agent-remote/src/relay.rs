use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub async fn run_relay_pump(
    endpoint: &str,
    token: &str,
    mode: RelayMode,
) -> anyhow::Result<Option<(String, i32)>> {
    let url = if endpoint.contains('?') {
        format!("{endpoint}&token={token}")
    } else {
        format!("{endpoint}?token={token}")
    };
    let (ws, _) = connect_async(&url).await?;
    let (mut tx, mut rx) = ws.split();

    match mode {
        RelayMode::ReadFile(path) => {
            let mut file = File::open(Path::new(&path)).await?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                tx.send(Message::Binary(buf[..n].to_vec())).await?;
            }
        }
        RelayMode::WriteFile(path) => {
            let mut file = File::create(Path::new(&path)).await?;
            while let Some(msg) = rx.next().await {
                match msg? {
                    Message::Binary(b) => file.write_all(&b).await?,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
        RelayMode::Shell(session) => {
            session.run(&mut tx, &mut rx).await?;
        }
        RelayMode::Exec(session) => {
            let summary = tokio::join!(
                session.run(&mut tx),
                async {
                    while let Some(msg) = rx.next().await {
                        if matches!(msg, Ok(Message::Close(_))) {
                            break;
                        }
                    }
                }
            )
            .0?;
            let _ = tx.send(Message::Close(None)).await;
            return Ok(Some(summary));
        }
    }

    let _ = tx.send(Message::Close(None)).await;
    Ok(None)
}

pub enum RelayMode {
    ReadFile(String),
    WriteFile(String),
    Shell(crate::shell::ShellBridge),
    Exec(crate::exec::ExecBridge),
}
