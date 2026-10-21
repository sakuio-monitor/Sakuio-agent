use crate::session::WsBoardSession;
use async_trait::async_trait;
use http::StatusCode;
use sakuio_board::{BoardConnectInfo, BoardSession, BoardTransport, TransportError};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Error as WsError;
use url::Url;

/// WebSocket implementation of [`BoardTransport`].
/// WebSocket 版 [`BoardTransport`]。
pub struct WsBoardTransport {
    endpoint: Url,
}

impl WsBoardTransport {
    /// Parses `wss://host/path` style endpoint.
    /// 解析 `wss://host/path` 形式的中控地址。
    pub fn new(endpoint: &str) -> Result<Self, TransportError> {
        let url = Url::parse(endpoint).map_err(|e| TransportError::Connect(e.to_string()))?;
        Ok(Self { endpoint: url })
    }
}

fn map_connect_error(err: WsError) -> String {
    if let WsError::Http(resp) = &err {
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            return "board rejected token (HTTP 401): token not found — copy the full 64-character token from admin (check for a missing trailing character)".into();
        }
        if status == StatusCode::FORBIDDEN {
            return "board rejected connection (HTTP 403): client disabled".into();
        }
        return format!("websocket upgrade failed (HTTP {})", status.as_u16());
    }
    err.to_string()
}

#[async_trait]
impl BoardTransport for WsBoardTransport {
    fn name(&self) -> &'static str {
        "websocket"
    }

    async fn connect(
        &self,
        info: BoardConnectInfo,
    ) -> Result<Box<dyn BoardSession>, TransportError> {
        let token = info.node_token.trim();
        if token.is_empty() {
            return Err(TransportError::Connect(
                "token must not be empty; copy the token from admin".into(),
            ));
        }
        if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
            // Not a plain node token: may be an auto-discovery key, the server decides.
            tracing::info!(
                token_len = token.len(),
                "token is not a 64-hex node token; trying auto discovery"
            );
        }
        let mut url = self.endpoint.clone();
        {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            pairs.append_pair("token", token);
        }
        let (socket, _resp) = connect_async(url.as_str())
            .await
            .map_err(|e| TransportError::Connect(map_connect_error(e)))?;
        let session_id = "agent-pending".to_string();
        Ok(Box::new(WsBoardSession::new(session_id, socket)))
    }
}
