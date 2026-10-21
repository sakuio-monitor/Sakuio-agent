use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use sakuio_board::{BoardFrame, BoardSession};
use sakuio_protocol::{decode_sktl, encode_sktl, ControlMessage, TelemetryBatch};
use tokio_tungstenite::tungstenite::Message;

/// Outbound WebSocket board session.
/// 出站 WebSocket 板级会话。
pub struct WsBoardSession {
    session_id: String,
    write: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >,
    read: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
}

impl WsBoardSession {
    pub fn new(
        session_id: String,
        socket: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Self {
        let (write, read) = socket.split();
        Self {
            session_id,
            write,
            read,
        }
    }
}

#[async_trait]
impl BoardSession for WsBoardSession {
    async fn send(&mut self, frame: BoardFrame) -> Result<(), String> {
        let msg = match frame {
            BoardFrame::Control(c) => {
                let bytes = c.to_json_bytes().map_err(|e| e.to_string())?;
                Message::Text(String::from_utf8(bytes).map_err(|e| e.to_string())?)
            }
            BoardFrame::Telemetry(payload) => {
                let batch: TelemetryBatch = cbor4ii::serde::from_reader(payload.body.as_slice())
                    .map_err(|e| e.to_string())?;
                let raw = encode_sktl(payload.batch_seq, &batch).map_err(|e| e.to_string())?;
                Message::Binary(raw)
            }
        };
        self.write.send(msg).await.map_err(|e| e.to_string())
    }

    async fn recv(&mut self) -> Result<Option<BoardFrame>, String> {
        loop {
            let Some(next) = self.read.next().await else {
                return Ok(None);
            };
            let msg = next.map_err(|e| e.to_string())?;
            match msg {
                Message::Text(text) => {
                    let control =
                        ControlMessage::from_json_bytes(text.as_bytes()).map_err(|e| e.to_string())?;
                    return Ok(Some(BoardFrame::Control(control)));
                }
                Message::Binary(bin) => {
                    let (batch_seq, batch) = decode_sktl(&bin).map_err(|e| e.to_string())?;
                    let mut body = Vec::new();
                    cbor4ii::serde::to_writer(&mut body, &batch).map_err(|e| e.to_string())?;
                    return Ok(Some(BoardFrame::Telemetry(sakuio_protocol::SkltPayload {
                        batch_seq,
                        body,
                    })));
                }
                Message::Ping(payload) => {
                    self.write
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|e| e.to_string())?;
                    continue;
                }
                Message::Pong(_) | Message::Close(_) => return Ok(None),
                _ => continue,
            }
        }
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }
}
