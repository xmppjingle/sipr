use crate::message::{ParseError, SipMessage};
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(#[from] ParseError),
    #[error("Transport not started")]
    NotStarted,
    #[error("Send failed: {0}")]
    SendFailed(String),
}

#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub message: SipMessage,
    pub source: SocketAddr,
}

pub struct SipTransport {
    socket: Arc<UdpSocket>,
    local_addr: SocketAddr,
}

impl SipTransport {
    /// Bind to a local address for UDP transport
    pub async fn bind(addr: &str) -> Result<Self, TransportError> {
        let socket = UdpSocket::bind(addr).await?;
        let local_addr = socket.local_addr()?;
        Ok(Self {
            socket: Arc::new(socket),
            local_addr,
        })
    }

    /// Bind to a specific socket address
    pub async fn bind_addr(addr: SocketAddr) -> Result<Self, TransportError> {
        let socket = UdpSocket::bind(addr).await?;
        let local_addr = socket.local_addr()?;
        Ok(Self {
            socket: Arc::new(socket),
            local_addr,
        })
    }

    /// Get the local address this transport is bound to
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Send a SIP message to a specific address
    pub async fn send_to(
        &self,
        message: &SipMessage,
        addr: SocketAddr,
    ) -> Result<usize, TransportError> {
        let data = message.to_bytes();
        let sent = self.socket.send_to(&data, addr).await?;
        tracing::debug!("Sent {} bytes to {}", sent, addr);
        Ok(sent)
    }

    /// Send raw bytes to a specific address
    pub async fn send_raw(
        &self,
        data: &[u8],
        addr: SocketAddr,
    ) -> Result<usize, TransportError> {
        let sent = self.socket.send_to(data, addr).await?;
        Ok(sent)
    }

    /// Receive a single SIP message
    pub async fn recv(&self) -> Result<IncomingMessage, TransportError> {
        let mut buf = vec![0u8; 65535];
        let (len, source) = self.socket.recv_from(&mut buf).await?;
        let data = String::from_utf8_lossy(&buf[..len]);
        let message = SipMessage::parse(&data)?;

        Ok(IncomingMessage { message, source })
    }

    /// Start receiving messages into a channel
    pub fn start_receiving(
        &self,
        buffer_size: usize,
    ) -> (mpsc::Receiver<IncomingMessage>, mpsc::Sender<()>) {
        let (tx, rx) = mpsc::channel(buffer_size);
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let socket = self.socket.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                tokio::select! {
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((len, source)) => {
                                let data = String::from_utf8_lossy(&buf[..len]);
                                match SipMessage::parse(&data) {
                                    Ok(message) => {
                                        if tx
                                            .send(IncomingMessage {
                                                message,
                                                source,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break; // Channel closed
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Failed to parse SIP message from {}: {}", source, e);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!("UDP receive error: {}", e);
                                break;
                            }
                        }
                    }
                    _ = stop_rx.recv() => {
                        break;
                    }
                }
            }
        });

        (rx, stop_tx)
    }

    /// Get the underlying socket reference (for advanced usage)
    pub fn socket(&self) -> &Arc<UdpSocket> {
        &self.socket
    }
}

/// Parse a SIP URI into host and port
pub fn parse_sip_uri(uri: &str) -> Option<(String, u16)> {
    let uri = uri.strip_prefix("sip:").or_else(|| uri.strip_prefix("sips:"))?;

    // Remove user@ part if present
    let host_part = if let Some(at_pos) = uri.find('@') {
        &uri[at_pos + 1..]
    } else {
        uri
    };

    // Remove any URI parameters
    let host_part = host_part.split(';').next()?;

    // Parse host:port (handle IPv6 bracket notation)
    if host_part.starts_with('[') {
        // IPv6: [host]:port or [host]
        let end_bracket = host_part.find(']')?;
        let host = &host_part[1..end_bracket];
        let after = &host_part[end_bracket + 1..];
        let port = if let Some(port_str) = after.strip_prefix(':') {
            port_str.parse().ok()?
        } else {
            5060
        };
        Some((host.to_string(), port))
    } else if let Some((host, port_str)) = host_part.rsplit_once(':') {
        // Avoid splitting on IPv6 colons (unbracketed)
        if host.contains(':') {
            // Likely bare IPv6 without brackets
            Some((host_part.to_string(), 5060))
        } else {
            let port: u16 = port_str.parse().ok()?;
            Some((host.to_string(), port))
        }
    } else {
        Some((host_part.to_string(), 5060))
    }
}

/// Resolve a SIP URI to a socket address
pub fn resolve_sip_uri(uri: &str) -> Option<SocketAddr> {
    let (host, port) = parse_sip_uri(uri)?;
    // For simplicity, try to parse as IP directly
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        Some(SocketAddr::new(ip, port))
    } else {
        // DNS resolution would happen here in production
        // For now, return None for hostnames
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sip_uri() {
        assert_eq!(
            parse_sip_uri("sip:bob@192.168.1.100:5060"),
            Some(("192.168.1.100".to_string(), 5060))
        );
        assert_eq!(
            parse_sip_uri("sip:bob@biloxi.com"),
            Some(("biloxi.com".to_string(), 5060))
        );
        assert_eq!(
            parse_sip_uri("sip:alice@10.0.0.1:5080"),
            Some(("10.0.0.1".to_string(), 5080))
        );
        assert_eq!(
            parse_sip_uri("sip:registrar.example.com"),
            Some(("registrar.example.com".to_string(), 5060))
        );
    }

    #[test]
    fn test_parse_sip_uri_with_params() {
        assert_eq!(
            parse_sip_uri("sip:bob@192.168.1.100:5060;transport=udp"),
            Some(("192.168.1.100".to_string(), 5060))
        );
    }

    #[test]
    fn test_parse_sip_uri_invalid() {
        assert_eq!(parse_sip_uri("http://example.com"), None);
        assert_eq!(parse_sip_uri("not-a-uri"), None);
    }

    #[test]
    fn test_resolve_sip_uri_ip() {
        let addr = resolve_sip_uri("sip:bob@192.168.1.100:5060").unwrap();
        assert_eq!(addr.ip().to_string(), "192.168.1.100");
        assert_eq!(addr.port(), 5060);
    }

    #[test]
    fn test_resolve_sip_uri_hostname() {
        // Hostname resolution returns None in this simple implementation
        assert!(resolve_sip_uri("sip:bob@biloxi.com").is_none());
    }

    #[tokio::test]
    async fn test_transport_bind() {
        let transport = SipTransport::bind("127.0.0.1:0").await.unwrap();
        let addr = transport.local_addr();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(addr.port() > 0);
    }

    #[tokio::test]
    async fn test_transport_send_receive() {
        let t1 = SipTransport::bind("127.0.0.1:0").await.unwrap();
        let t2 = SipTransport::bind("127.0.0.1:0").await.unwrap();

        let request = crate::message::RequestBuilder::new(
            crate::message::SipMethod::Register,
            "sip:registrar.example.com",
        )
        .header(
            HeaderName::Via,
            format!(
                "SIP/2.0/UDP {};branch=z9hG4bKtest123",
                t1.local_addr()
            ),
        )
        .header(HeaderName::From, "<sip:alice@example.com>;tag=abc")
        .header(HeaderName::To, "<sip:alice@example.com>")
        .header(HeaderName::CallId, "test-transport-call")
        .header(HeaderName::CSeq, "1 REGISTER")
        .build();

        // Send from t1 to t2
        t1.send_to(&request, t2.local_addr()).await.unwrap();

        // Receive on t2
        let incoming = t2.recv().await.unwrap();
        assert!(incoming.message.is_request());
        assert_eq!(incoming.source, t1.local_addr());
        assert_eq!(incoming.message.call_id().unwrap(), "test-transport-call");
    }

    #[tokio::test]
    async fn test_transport_channel_receive() {
        let t1 = SipTransport::bind("127.0.0.1:0").await.unwrap();
        let t2 = SipTransport::bind("127.0.0.1:0").await.unwrap();

        let (mut rx, _stop_tx) = t2.start_receiving(16);

        let request = crate::message::RequestBuilder::new(
            crate::message::SipMethod::Options,
            "sip:bob@example.com",
        )
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch=z9hG4bKchan", t1.local_addr()),
        )
        .header(HeaderName::From, "<sip:alice@example.com>;tag=ch1")
        .header(HeaderName::To, "<sip:bob@example.com>")
        .header(HeaderName::CallId, "channel-test")
        .header(HeaderName::CSeq, "1 OPTIONS")
        .build();

        t1.send_to(&request, t2.local_addr()).await.unwrap();

        let incoming = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(incoming.message.call_id().unwrap(), "channel-test");
    }

    use crate::header::HeaderName;
}
