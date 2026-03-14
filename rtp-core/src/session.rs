use crate::codec::{CodecPipeline, CodecType, CodecError};
use crate::jitter::JitterBuffer;
use crate::packet::RtpPacket;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("codec error: {0}")]
    Codec(#[from] CodecError),
    #[error("RTP error: {0}")]
    Rtp(#[from] crate::packet::RtpError),
    #[error("session not started")]
    NotStarted,
    #[error("invalid DTMF digit: {0}")]
    InvalidDtmfDigit(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DtmfEvent {
    pub digit: char,
    pub end: bool,
    pub duration: u16,
    pub volume: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveEvent {
    Audio(Vec<i16>),
    Dtmf(DtmfEvent),
}

#[derive(Debug, Clone)]
struct QueuedDtmf {
    digit: char,
    duration_samples: u16,
}

/// Configuration for an RTP session
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub local_addr: String,
    pub remote_addr: SocketAddr,
    pub codec: CodecType,
    pub ssrc: u32,
    pub jitter_buffer_size: usize,
}

impl SessionConfig {
    pub fn new(local_addr: &str, remote_addr: SocketAddr, codec: CodecType) -> Self {
        Self {
            local_addr: local_addr.to_string(),
            remote_addr,
            codec,
            ssrc: rand::random(),
            jitter_buffer_size: 10,
        }
    }
}

/// RTP session that manages send/receive streams
pub struct RtpSession {
    socket: Arc<UdpSocket>,
    config: SessionConfig,
    codec: CodecPipeline,
    sequence_number: u16,
    timestamp: u32,
    local_addr: SocketAddr,
    dtmf_queue: VecDeque<QueuedDtmf>,
}

impl RtpSession {
    /// Create and bind a new RTP session
    pub async fn new(config: SessionConfig) -> Result<Self, SessionError> {
        let socket = UdpSocket::bind(&config.local_addr).await?;
        let local_addr = socket.local_addr()?;
        let codec = CodecPipeline::new(config.codec);

        Ok(Self {
            socket: Arc::new(socket),
            config,
            codec,
            sequence_number: 0,
            timestamp: 0,
            local_addr,
            dtmf_queue: VecDeque::new(),
        })
    }

    /// Get the local address this session is bound to
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Update the remote address (e.g. after receiving SDP answer)
    pub fn set_remote_addr(&mut self, addr: SocketAddr) {
        self.config.remote_addr = addr;
    }

    /// Send PCM audio samples as an RTP packet
    pub async fn send_audio(&mut self, pcm_samples: &[i16]) -> Result<usize, SessionError> {
        let encoded = self.codec.encode(pcm_samples)?;

        let packet = RtpPacket::new(
            self.config.codec.payload_type(),
            self.sequence_number,
            self.timestamp,
            self.config.ssrc,
        )
        .with_payload(encoded);

        let data = packet.serialize();
        let sent = self.socket.send_to(&data, self.config.remote_addr).await?;

        self.sequence_number = self.sequence_number.wrapping_add(1);
        self.timestamp = self
            .timestamp
            .wrapping_add(self.config.codec.samples_per_frame() as u32);

        Ok(sent)
    }

    /// Send one RFC2833 telephone-event digit.
    ///
    /// `payload_type` is typically negotiated via SDP `a=rtpmap:<pt> telephone-event/8000`.
    pub async fn send_rfc2833_digit(
        &mut self,
        digit: char,
        payload_type: u8,
    ) -> Result<(), SessionError> {
        self.send_rfc2833_digit_with_duration(digit, payload_type, 800).await
    }

    async fn send_rfc2833_digit_with_duration(
        &mut self,
        digit: char,
        payload_type: u8,
        duration_samples: u16,
    ) -> Result<(), SessionError> {
        let event = dtmf_digit_to_event(digit).ok_or(SessionError::InvalidDtmfDigit(digit))?;
        let start_ts = self.timestamp;
        let ramps = [160u16, 320u16, duration_samples];
        for (idx, dur) in ramps.iter().enumerate() {
            let end = idx == ramps.len() - 1;
            let marker = idx == 0;
            let payload = vec![
                event,
                ((end as u8) << 7) | 10u8, // volume 10
                (dur >> 8) as u8,
                (*dur & 0xFF) as u8,
            ];
            let packet = RtpPacket::new(payload_type, self.sequence_number, start_ts, self.config.ssrc)
                .with_marker(marker)
                .with_payload(payload);
            let data = packet.serialize();
            self.socket.send_to(&data, self.config.remote_addr).await?;
            self.sequence_number = self.sequence_number.wrapping_add(1);
        }
        // Advance sender clock by event duration so subsequent media timing moves forward.
        self.timestamp = self.timestamp.wrapping_add(duration_samples as u32);
        Ok(())
    }

    /// Queue DTMF digits for RFC2833 transmission.
    ///
    /// Returns the number of queued digits.
    pub fn queue_rfc2833_digits(&mut self, digits: &str) -> Result<usize, SessionError> {
        let mut queued = 0usize;
        for ch in digits.chars().filter(|c| !c.is_whitespace()) {
            validate_dtmf_digit(ch)?;
            self.dtmf_queue.push_back(QueuedDtmf {
                digit: ch.to_ascii_uppercase(),
                duration_samples: 800,
            });
            queued += 1;
        }
        Ok(queued)
    }

    pub fn queued_rfc2833_digits(&self) -> usize {
        self.dtmf_queue.len()
    }

    /// Send the next queued RFC2833 DTMF digit.
    pub async fn send_next_queued_rfc2833(
        &mut self,
        payload_type: u8,
    ) -> Result<Option<char>, SessionError> {
        let Some(next) = self.dtmf_queue.pop_front() else {
            return Ok(None);
        };
        self.send_rfc2833_digit_with_duration(next.digit, payload_type, next.duration_samples)
            .await?;
        Ok(Some(next.digit))
    }

    /// Flush the queued RFC2833 digits with an inter-digit gap.
    pub async fn flush_queued_rfc2833(
        &mut self,
        payload_type: u8,
        inter_digit_gap_ms: u64,
    ) -> Result<usize, SessionError> {
        let mut sent = 0usize;
        while let Some(_digit) = self.send_next_queued_rfc2833(payload_type).await? {
            sent += 1;
            if !self.dtmf_queue.is_empty() && inter_digit_gap_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(inter_digit_gap_ms)).await;
            }
        }
        Ok(sent)
    }

    /// Send a raw RTP packet
    pub async fn send_packet(&self, packet: &RtpPacket) -> Result<usize, SessionError> {
        let data = packet.serialize();
        let sent = self.socket.send_to(&data, self.config.remote_addr).await?;
        Ok(sent)
    }

    /// Receive a single RTP packet
    pub async fn recv_packet(&self) -> Result<(RtpPacket, SocketAddr), SessionError> {
        let mut buf = vec![0u8; 65535];
        let (len, source) = self.socket.recv_from(&mut buf).await?;
        let packet = RtpPacket::parse(&buf[..len])?;
        Ok((packet, source))
    }

    /// Decode an RTP packet's payload to PCM samples
    pub fn decode_packet(&mut self, packet: &RtpPacket) -> Result<Vec<i16>, SessionError> {
        Ok(self.codec.decode(&packet.payload)?)
    }

    /// Get a silence frame for this session's codec
    pub fn silence_frame(&self) -> Vec<u8> {
        self.codec.silence_frame()
    }

    /// Start a receive loop that feeds packets into a jitter buffer and outputs decoded audio
    pub fn start_receiving(
        &self,
        buffer_size: usize,
    ) -> (
        mpsc::Receiver<Vec<i16>>,
        mpsc::Sender<()>,
    ) {
        let (audio_tx, audio_rx) = mpsc::channel(buffer_size);
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let socket = self.socket.clone();
        let codec_type = self.config.codec;
        let jitter_size = self.config.jitter_buffer_size;
        let expected_source = self.config.remote_addr;

        tokio::spawn(async move {
            let mut codec = CodecPipeline::new(codec_type);
            let mut jitter = JitterBuffer::new(jitter_size);
            let mut buf = vec![0u8; 65535];

            loop {
                tokio::select! {
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((len, source)) => {
                                // Filter packets from unexpected sources
                                if source.ip() != expected_source.ip() {
                                    continue;
                                }
                                match RtpPacket::parse(&buf[..len]) {
                                    Ok(packet) => {
                                        jitter.insert(packet);

                                        // Pop one packet per received packet (don't drain greedily
                                        // as pop() advances seq on missing packets)
                                        if let Some(pkt) = jitter.pop() {
                                            match codec.decode(&pkt.payload) {
                                                Ok(samples) => {
                                                    // Use try_send to avoid blocking the receive loop
                                                    match audio_tx.try_send(samples) {
                                                        Ok(_) => {}
                                                        Err(mpsc::error::TrySendError::Full(_)) => {
                                                            // Drop frame rather than block
                                                        }
                                                        Err(mpsc::error::TrySendError::Closed(_)) => {
                                                            return; // Channel closed
                                                        }
                                                    }
                                                }
                                                Err(_e) => {
                                                    tracing::warn!("RTP decode error: {}", _e);
                                                }
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        // Non-RTP packet or corrupt data, ignore
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!("RTP receive error: {}", e);
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

        (audio_rx, stop_tx)
    }

    /// Start a receive loop that emits both decoded audio and RFC2833 DTMF events.
    pub fn start_receiving_events(
        &self,
        buffer_size: usize,
        dtmf_payload_type: Option<u8>,
    ) -> (
        mpsc::Receiver<ReceiveEvent>,
        mpsc::Sender<()>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(buffer_size);
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let socket = self.socket.clone();
        let codec_type = self.config.codec;
        let jitter_size = self.config.jitter_buffer_size;
        let expected_source = self.config.remote_addr;

        tokio::spawn(async move {
            let mut codec = CodecPipeline::new(codec_type);
            let mut jitter = JitterBuffer::new(jitter_size);
            let mut buf = vec![0u8; 65535];

            loop {
                tokio::select! {
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((len, source)) => {
                                // Filter packets from unexpected sources
                                if source.ip() != expected_source.ip() {
                                    continue;
                                }
                                match RtpPacket::parse(&buf[..len]) {
                                    Ok(packet) => {
                                        if let Some(pt) = dtmf_payload_type {
                                            if packet.payload_type == pt {
                                                if let Some(dtmf) = parse_rfc2833_event(&packet) {
                                                    match event_tx.try_send(ReceiveEvent::Dtmf(dtmf)) {
                                                        Ok(_) => {}
                                                        Err(mpsc::error::TrySendError::Full(_)) => {}
                                                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                                                    }
                                                }
                                                continue;
                                            }
                                        }

                                        jitter.insert(packet);
                                        if let Some(pkt) = jitter.pop() {
                                            match codec.decode(&pkt.payload) {
                                                Ok(samples) => {
                                                    match event_tx.try_send(ReceiveEvent::Audio(samples)) {
                                                        Ok(_) => {}
                                                        Err(mpsc::error::TrySendError::Full(_)) => {}
                                                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                                                    }
                                                }
                                                Err(_e) => {
                                                    tracing::warn!("RTP decode error: {}", _e);
                                                }
                                            }
                                        }
                                    }
                                    Err(_) => {}
                                }
                            }
                            Err(e) => {
                                tracing::error!("RTP receive error: {}", e);
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

        (event_rx, stop_tx)
    }

    /// Get session statistics
    pub fn stats(&self) -> SessionStats {
        SessionStats {
            local_addr: self.local_addr,
            remote_addr: self.config.remote_addr,
            codec: self.config.codec,
            ssrc: self.config.ssrc,
            packets_sent: self.sequence_number as u64,
        }
    }

    /// Get the codec pipeline reference
    pub fn codec(&self) -> &CodecPipeline {
        &self.codec
    }
}

fn validate_dtmf_digit(digit: char) -> Result<(), SessionError> {
    if dtmf_digit_to_event(digit).is_some() {
        Ok(())
    } else {
        Err(SessionError::InvalidDtmfDigit(digit))
    }
}

fn dtmf_digit_to_event(digit: char) -> Option<u8> {
    match digit.to_ascii_uppercase() {
        '0' => Some(0),
        '1' => Some(1),
        '2' => Some(2),
        '3' => Some(3),
        '4' => Some(4),
        '5' => Some(5),
        '6' => Some(6),
        '7' => Some(7),
        '8' => Some(8),
        '9' => Some(9),
        '*' => Some(10),
        '#' => Some(11),
        'A' => Some(12),
        'B' => Some(13),
        'C' => Some(14),
        'D' => Some(15),
        _ => None,
    }
}

fn dtmf_event_to_digit(event: u8) -> Option<char> {
    match event {
        0..=9 => Some((b'0' + event) as char),
        10 => Some('*'),
        11 => Some('#'),
        12 => Some('A'),
        13 => Some('B'),
        14 => Some('C'),
        15 => Some('D'),
        _ => None,
    }
}

fn parse_rfc2833_event(packet: &RtpPacket) -> Option<DtmfEvent> {
    if packet.payload.len() < 4 {
        return None;
    }
    let event = packet.payload[0];
    let e_r_volume = packet.payload[1];
    let end = (e_r_volume & 0x80) != 0;
    let volume = e_r_volume & 0x3F;
    let duration = u16::from_be_bytes([packet.payload[2], packet.payload[3]]);
    let digit = dtmf_event_to_digit(event)?;
    Some(DtmfEvent {
        digit,
        end,
        duration,
        volume,
        sequence_number: packet.sequence_number,
        timestamp: packet.timestamp,
    })
}

#[derive(Debug, Clone)]
pub struct SessionStats {
    pub local_addr: SocketAddr,
    pub remote_addr: SocketAddr,
    pub codec: CodecType,
    pub ssrc: u32,
    pub packets_sent: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn test_session_creation() {
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9999);
        let config = SessionConfig::new("127.0.0.1:0", remote_addr, CodecType::Pcmu);
        let session = RtpSession::new(config).await.unwrap();

        let addr = session.local_addr();
        assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(addr.port() > 0);
    }

    #[tokio::test]
    async fn test_send_and_receive_audio() {
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

        // Create receiver first to get its address
        let recv_config = SessionConfig::new("127.0.0.1:0", remote_addr, CodecType::Pcmu);
        let mut recv_session = RtpSession::new(recv_config).await.unwrap();
        let recv_addr = recv_session.local_addr();

        // Create sender pointed at receiver
        let send_config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcmu);
        let mut send_session = RtpSession::new(send_config).await.unwrap();

        // Send a frame of audio
        let samples: Vec<i16> = (0..160)
            .map(|i| ((i as f64 / 160.0 * std::f64::consts::TAU).sin() * 8000.0) as i16)
            .collect();

        let sent = send_session.send_audio(&samples).await.unwrap();
        assert!(sent > 0);

        // Receive the packet
        let (packet, _source) = recv_session.recv_packet().await.unwrap();
        assert_eq!(packet.payload_type, 0); // PCMU
        assert_eq!(packet.sequence_number, 0);
        assert_eq!(packet.payload.len(), 160);

        // Decode the packet
        let decoded = recv_session.decode_packet(&packet).unwrap();
        assert_eq!(decoded.len(), 160);
    }

    #[tokio::test]
    async fn test_send_multiple_packets() {
        let recv_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = recv_socket.local_addr().unwrap();

        let send_config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcmu);
        let mut send_session = RtpSession::new(send_config).await.unwrap();

        // Send 3 packets
        for _ in 0..3 {
            let samples = vec![0i16; 160];
            send_session.send_audio(&samples).await.unwrap();
        }

        let stats = send_session.stats();
        assert_eq!(stats.packets_sent, 3);
    }

    #[tokio::test]
    async fn test_session_stats() {
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9999);
        let config = SessionConfig::new("127.0.0.1:0", remote_addr, CodecType::Pcmu);
        let session = RtpSession::new(config).await.unwrap();

        let stats = session.stats();
        assert_eq!(stats.codec, CodecType::Pcmu);
        assert_eq!(stats.remote_addr, remote_addr);
        assert_eq!(stats.packets_sent, 0);
    }

    #[tokio::test]
    async fn test_silence_frame() {
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9999);
        let config = SessionConfig::new("127.0.0.1:0", remote_addr, CodecType::Pcmu);
        let session = RtpSession::new(config).await.unwrap();

        let silence = session.silence_frame();
        assert_eq!(silence.len(), 160);
    }

    #[tokio::test]
    async fn test_receive_loop() {
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

        let recv_config = SessionConfig::new("127.0.0.1:0", remote_addr, CodecType::Pcmu);
        let recv_session = RtpSession::new(recv_config).await.unwrap();
        let recv_addr = recv_session.local_addr();

        let (mut audio_rx, stop_tx) = recv_session.start_receiving(16);

        // Create sender
        let send_config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcmu);
        let mut send_session = RtpSession::new(send_config).await.unwrap();

        // Send a few packets
        for _ in 0..3 {
            let samples = vec![1000i16; 160];
            send_session.send_audio(&samples).await.unwrap();
        }

        // Receive decoded audio
        let audio = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            audio_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(audio.len(), 160);

        // Stop the receive loop
        let _ = stop_tx.send(()).await;
    }

    #[tokio::test]
    async fn test_pcma_session() {
        let recv_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = recv_socket.local_addr().unwrap();

        let send_config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcma);
        let mut send_session = RtpSession::new(send_config).await.unwrap();

        let samples = vec![5000i16; 160];
        let sent = send_session.send_audio(&samples).await.unwrap();
        assert!(sent > 0);

        // Verify the packet
        let mut buf = vec![0u8; 65535];
        let (len, _) = recv_socket.recv_from(&mut buf).await.unwrap();
        let packet = RtpPacket::parse(&buf[..len]).unwrap();
        assert_eq!(packet.payload_type, 8); // PCMA
    }

    #[tokio::test]
    async fn test_send_raw_packet() {
        let recv_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = recv_socket.local_addr().unwrap();

        let send_config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcmu);
        let send_session = RtpSession::new(send_config).await.unwrap();

        let packet = RtpPacket::new(0, 42, 6720, 0xBEEF)
            .with_payload(vec![0x7F; 160]);

        let sent = send_session.send_packet(&packet).await.unwrap();
        assert!(sent > 0);

        let mut buf = vec![0u8; 65535];
        let (len, _) = recv_socket.recv_from(&mut buf).await.unwrap();
        let received = RtpPacket::parse(&buf[..len]).unwrap();
        assert_eq!(received.sequence_number, 42);
        assert_eq!(received.ssrc, 0xBEEF);
    }

    #[tokio::test]
    async fn test_send_rfc2833_digit_packet_shape() {
        let recv_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = recv_socket.local_addr().unwrap();
        let config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcmu);
        let mut sender = RtpSession::new(config).await.unwrap();

        sender.send_rfc2833_digit('5', 101).await.unwrap();

        let mut buf = vec![0u8; 65535];
        let (len, _) = recv_socket.recv_from(&mut buf).await.unwrap();
        let pkt = RtpPacket::parse(&buf[..len]).unwrap();
        assert_eq!(pkt.payload_type, 101);
        assert!(pkt.marker);
        assert_eq!(pkt.payload[0], 5);
    }

    #[tokio::test]
    async fn test_queue_and_flush_rfc2833_digits() {
        let recv_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = recv_socket.local_addr().unwrap();
        let config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcmu);
        let mut sender = RtpSession::new(config).await.unwrap();

        let queued = sender.queue_rfc2833_digits("12#").unwrap();
        assert_eq!(queued, 3);
        assert_eq!(sender.queued_rfc2833_digits(), 3);

        let sent = sender.flush_queued_rfc2833(101, 0).await.unwrap();
        assert_eq!(sent, 3);
        assert_eq!(sender.queued_rfc2833_digits(), 0);
    }

    #[tokio::test]
    async fn test_receive_event_reports_dtmf() {
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let recv_config = SessionConfig::new("127.0.0.1:0", remote_addr, CodecType::Pcmu);
        let recv_session = RtpSession::new(recv_config).await.unwrap();
        let recv_addr = recv_session.local_addr();
        let (mut events, stop_tx) = recv_session.start_receiving_events(16, Some(101));

        let send_config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcmu);
        let mut send_session = RtpSession::new(send_config).await.unwrap();
        send_session.send_rfc2833_digit('9', 101).await.unwrap();

        let evt = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        match evt {
            ReceiveEvent::Dtmf(dtmf) => assert_eq!(dtmf.digit, '9'),
            ReceiveEvent::Audio(_) => panic!("expected DTMF event"),
        }

        let _ = stop_tx.send(()).await;
    }
}
