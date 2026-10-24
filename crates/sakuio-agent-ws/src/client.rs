use crate::session::WsBoardSession;
use async_trait::async_trait;
use http::StatusCode;
use sakuio_board::{BoardConnectInfo, BoardSession, BoardTransport, TransportError};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::Error as WsError;
use url::Url;

/// Verifier that accepts any server certificate (only used with --insecure).
/// 接受任意服务器证书的校验器（仅 --insecure 时使用）。
#[derive(Debug)]
struct NoCertVerifier;

impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// WebSocket implementation of [`BoardTransport`].
/// WebSocket 版 [`BoardTransport`]。
pub struct WsBoardTransport {
    endpoint: Url,
    /// Skip TLS certificate verification (self-signed deployments).
    /// 跳过 TLS 证书校验（自签名部署）。
    insecure_tls: bool,
}

impl WsBoardTransport {
    /// Parses `wss://host/path` style endpoint.
    /// 解析 `wss://host/path` 形式的中控地址。
    pub fn new(endpoint: &str) -> Result<Self, TransportError> {
        let url = Url::parse(endpoint).map_err(|e| TransportError::Connect(e.to_string()))?;
        Ok(Self {
            endpoint: url,
            insecure_tls: false,
        })
    }

    /// Enables skipping of TLS certificate verification.
    /// 开启后跳过 TLS 证书校验。
    pub fn with_insecure(mut self, insecure: bool) -> Self {
        self.insecure_tls = insecure;
        self
    }

    fn dangerous_tls_connector() -> Connector {
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("supported tls protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(NoCertVerifier))
            .with_no_client_auth();
        Connector::Rustls(std::sync::Arc::new(config))
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
        // Node token is a header, never a query string: proxy access logs must
        // not contain a reusable agent credential.
        let mut request = self
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| TransportError::Connect(e.to_string()))?,
        );
        let connect_result = if self.insecure_tls && self.endpoint.scheme() == "wss" {
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                None,
                false,
                Some(Self::dangerous_tls_connector()),
            )
            .await
        } else {
            connect_async(request).await
        };
        let (socket, _resp) =
            connect_result.map_err(|e| TransportError::Connect(map_connect_error(e)))?;
        let session_id = "agent-pending".to_string();
        Ok(Box::new(WsBoardSession::new(session_id, socket)))
    }
}
