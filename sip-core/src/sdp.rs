use std::fmt;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaType {
    Audio,
    Video,
    Other(String),
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaType::Audio => write!(f, "audio"),
            MediaType::Video => write!(f, "video"),
            MediaType::Other(s) => write!(f, "{}", s),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportProtocol {
    RtpAvp,
    RtpSavp,
    Other(String),
}

impl fmt::Display for TransportProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportProtocol::RtpAvp => write!(f, "RTP/AVP"),
            TransportProtocol::RtpSavp => write!(f, "RTP/SAVP"),
            TransportProtocol::Other(s) => write!(f, "{}", s),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpMap {
    pub payload_type: u8,
    pub encoding_name: String,
    pub clock_rate: u32,
    pub channels: Option<u32>,
}

impl fmt::Display for RtpMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}/{}", self.payload_type, self.encoding_name, self.clock_rate)?;
        if let Some(ch) = self.channels {
            write!(f, "/{}", ch)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct MediaDescription {
    pub media_type: MediaType,
    pub port: u16,
    pub protocol: TransportProtocol,
    pub formats: Vec<u8>,
    pub rtpmaps: Vec<RtpMap>,
    pub attributes: Vec<(String, Option<String>)>,
}

impl MediaDescription {
    pub fn new_audio(port: u16) -> Self {
        Self {
            media_type: MediaType::Audio,
            port,
            protocol: TransportProtocol::RtpAvp,
            formats: Vec::new(),
            rtpmaps: Vec::new(),
            attributes: Vec::new(),
        }
    }

    pub fn add_codec(&mut self, payload_type: u8, name: &str, clock_rate: u32, channels: Option<u32>) {
        self.formats.push(payload_type);
        self.rtpmaps.push(RtpMap {
            payload_type,
            encoding_name: name.to_string(),
            clock_rate,
            channels,
        });
    }

    pub fn add_attribute(&mut self, name: &str, value: Option<&str>) {
        self.attributes.push((name.to_string(), value.map(|s| s.to_string())));
    }
}

#[derive(Debug, Clone)]
pub struct SdpSession {
    pub version: u32,
    pub origin_username: String,
    pub session_id: String,
    pub session_version: String,
    pub origin_address: String,
    pub session_name: String,
    pub connection_address: Option<String>,
    pub media_descriptions: Vec<MediaDescription>,
    pub attributes: Vec<(String, Option<String>)>,
}

#[derive(Debug, Error)]
pub enum SdpError {
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("invalid SDP line: {0}")]
    InvalidLine(String),
    #[error("invalid media line: {0}")]
    InvalidMedia(String),
}

impl SdpSession {
    pub fn new(address: &str) -> Self {
        let session_id = format!("{}", rand::random::<u32>());
        Self {
            version: 0,
            origin_username: "-".to_string(),
            session_id: session_id.clone(),
            session_version: session_id,
            origin_address: address.to_string(),
            session_name: "sip-rs".to_string(),
            connection_address: Some(address.to_string()),
            media_descriptions: Vec::new(),
            attributes: Vec::new(),
        }
    }

    pub fn add_audio_media(&mut self, port: u16) -> &mut MediaDescription {
        let mut media = MediaDescription::new_audio(port);
        // Add standard codecs
        media.add_codec(0, "PCMU", 8000, None);
        media.add_codec(8, "PCMA", 8000, None);
        media.add_codec(101, "telephone-event", 8000, None);
        media.add_attribute("fmtp", Some("101 0-15"));
        media.add_codec(111, "opus", 48000, Some(2));
        media.add_attribute("sendrecv", None);
        self.media_descriptions.push(media);
        self.media_descriptions.last_mut().unwrap()
    }

    pub fn parse(input: &str) -> Result<Self, SdpError> {
        let mut version = 0u32;
        let mut origin_username = "-".to_string();
        let mut session_id = String::new();
        let mut session_version = String::new();
        let mut origin_address = String::new();
        let mut session_name = String::new();
        let mut connection_address = None;
        let mut media_descriptions: Vec<MediaDescription> = Vec::new();
        let mut session_attributes: Vec<(String, Option<String>)> = Vec::new();
        let mut current_media: Option<MediaDescription> = None;

        for line in input.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if line.len() < 2 || line.as_bytes()[1] != b'=' {
                continue; // Skip malformed lines
            }

            let line_type = line.as_bytes()[0] as char;
            let value = &line[2..];

            match line_type {
                'v' => {
                    version = value.parse().unwrap_or(0);
                }
                'o' => {
                    let parts: Vec<&str> = value.splitn(6, ' ').collect();
                    if parts.len() >= 6 {
                        origin_username = parts[0].to_string();
                        session_id = parts[1].to_string();
                        session_version = parts[2].to_string();
                        origin_address = parts[5].to_string();
                    }
                }
                's' => {
                    session_name = value.to_string();
                }
                'c' => {
                    // c=IN IP4 224.2.17.12
                    let parts: Vec<&str> = value.split(' ').collect();
                    if parts.len() >= 3 {
                        let addr = parts[2].to_string();
                        if current_media.is_some() {
                            // Media-level connection; we'll set it on finalization
                        }
                        connection_address = Some(addr);
                    }
                }
                'm' => {
                    // Finalize previous media
                    if let Some(m) = current_media.take() {
                        media_descriptions.push(m);
                    }

                    // m=audio 49170 RTP/AVP 0 8 111
                    let parts: Vec<&str> = value.split(' ').collect();
                    if parts.len() < 3 {
                        return Err(SdpError::InvalidMedia(value.to_string()));
                    }

                    let media_type = match parts[0] {
                        "audio" => MediaType::Audio,
                        "video" => MediaType::Video,
                        other => MediaType::Other(other.to_string()),
                    };

                    let port: u16 = parts[1].parse().unwrap_or(0);

                    let protocol = match parts[2] {
                        "RTP/AVP" => TransportProtocol::RtpAvp,
                        "RTP/SAVP" => TransportProtocol::RtpSavp,
                        other => TransportProtocol::Other(other.to_string()),
                    };

                    let formats: Vec<u8> = parts[3..]
                        .iter()
                        .filter_map(|s| s.parse().ok())
                        .collect();

                    current_media = Some(MediaDescription {
                        media_type,
                        port,
                        protocol,
                        formats,
                        rtpmaps: Vec::new(),
                        attributes: Vec::new(),
                    });
                }
                'a' => {
                    let (attr_name, attr_value) = if let Some((name, val)) = value.split_once(':') {
                        (name.to_string(), Some(val.to_string()))
                    } else {
                        (value.to_string(), None)
                    };

                    if let Some(ref mut media) = current_media {
                        if attr_name == "rtpmap" {
                            if let Some(val) = &attr_value {
                                if let Some(rtpmap) = parse_rtpmap(val) {
                                    media.rtpmaps.push(rtpmap);
                                }
                            }
                        }
                        media.attributes.push((attr_name, attr_value));
                    } else {
                        session_attributes.push((attr_name, attr_value));
                    }
                }
                _ => {} // Ignore other line types
            }
        }

        // Finalize last media
        if let Some(m) = current_media.take() {
            media_descriptions.push(m);
        }

        Ok(SdpSession {
            version,
            origin_username,
            session_id,
            session_version,
            origin_address,
            session_name,
            connection_address,
            media_descriptions,
            attributes: session_attributes,
        })
    }

    pub fn get_audio_port(&self) -> Option<u16> {
        self.media_descriptions
            .iter()
            .find(|m| m.media_type == MediaType::Audio)
            .map(|m| m.port)
    }

    pub fn get_connection_address(&self) -> Option<&str> {
        self.connection_address.as_deref()
    }

    /// Create an audio media section with a specific direction attribute (for hold/resume).
    pub fn add_audio_media_directed(&mut self, port: u16, direction: &str) -> &mut MediaDescription {
        let mut media = MediaDescription::new_audio(port);
        media.add_codec(0, "PCMU", 8000, None);
        media.add_codec(8, "PCMA", 8000, None);
        media.add_codec(101, "telephone-event", 8000, None);
        media.add_attribute("fmtp", Some("101 0-15"));
        media.add_codec(111, "opus", 48000, Some(2));
        media.add_attribute(direction, None);
        self.media_descriptions.push(media);
        self.media_descriptions.last_mut().unwrap()
    }

    /// Get the media direction attribute (sendrecv, sendonly, recvonly, inactive).
    pub fn get_audio_direction(&self) -> Option<&str> {
        let audio = self.media_descriptions.iter().find(|m| m.media_type == MediaType::Audio)?;
        for (name, _) in &audio.attributes {
            match name.as_str() {
                "sendrecv" | "sendonly" | "recvonly" | "inactive" => return Some(name.as_str()),
                _ => {}
            }
        }
        None
    }

    pub fn get_audio_dtmf_payload_type(&self) -> Option<u8> {
        let audio = self
            .media_descriptions
            .iter()
            .find(|m| m.media_type == MediaType::Audio)?;
        audio
            .rtpmaps
            .iter()
            .find(|rtpmap| rtpmap.encoding_name.eq_ignore_ascii_case("telephone-event"))
            .map(|rtpmap| rtpmap.payload_type)
    }
}

fn parse_rtpmap(value: &str) -> Option<RtpMap> {
    // Format: "111 opus/48000/2" or "0 PCMU/8000"
    let parts: Vec<&str> = value.splitn(2, ' ').collect();
    if parts.len() != 2 {
        return None;
    }

    let payload_type: u8 = parts[0].parse().ok()?;
    let codec_parts: Vec<&str> = parts[1].split('/').collect();
    if codec_parts.len() < 2 {
        return None;
    }

    let encoding_name = codec_parts[0].to_string();
    let clock_rate: u32 = codec_parts[1].parse().ok()?;
    let channels = codec_parts.get(2).and_then(|s| s.parse().ok());

    Some(RtpMap {
        payload_type,
        encoding_name,
        clock_rate,
        channels,
    })
}

impl fmt::Display for SdpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "v={}", self.version)?;
        writeln!(
            f,
            "o={} {} {} IN IP4 {}",
            self.origin_username, self.session_id, self.session_version, self.origin_address
        )?;
        writeln!(f, "s={}", self.session_name)?;
        if let Some(addr) = &self.connection_address {
            writeln!(f, "c=IN IP4 {}", addr)?;
        }
        writeln!(f, "t=0 0")?;

        for (name, value) in &self.attributes {
            if let Some(val) = value {
                writeln!(f, "a={}:{}", name, val)?;
            } else {
                writeln!(f, "a={}", name)?;
            }
        }

        for media in &self.media_descriptions {
            let formats: Vec<String> = media.formats.iter().map(|f| f.to_string()).collect();
            writeln!(
                f,
                "m={} {} {} {}",
                media.media_type,
                media.port,
                media.protocol,
                formats.join(" ")
            )?;

            for rtpmap in &media.rtpmaps {
                writeln!(f, "a=rtpmap:{}", rtpmap)?;
            }

            for (name, value) in &media.attributes {
                if name == "rtpmap" {
                    continue; // Already handled above
                }
                if let Some(val) = value {
                    writeln!(f, "a={}:{}", name, val)?;
                } else {
                    writeln!(f, "a={}", name)?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SDP: &str = "v=0\r\n\
        o=- 123456 654321 IN IP4 192.168.1.100\r\n\
        s=sip-rs\r\n\
        c=IN IP4 192.168.1.100\r\n\
        t=0 0\r\n\
        m=audio 49170 RTP/AVP 0 8 101 111\r\n\
        a=rtpmap:0 PCMU/8000\r\n\
        a=rtpmap:8 PCMA/8000\r\n\
        a=rtpmap:101 telephone-event/8000\r\n\
        a=fmtp:101 0-15\r\n\
        a=rtpmap:111 opus/48000/2\r\n\
        a=sendrecv\r\n";

    #[test]
    fn test_parse_sdp() {
        let sdp = SdpSession::parse(SAMPLE_SDP).unwrap();
        assert_eq!(sdp.version, 0);
        assert_eq!(sdp.origin_username, "-");
        assert_eq!(sdp.session_id, "123456");
        assert_eq!(sdp.connection_address, Some("192.168.1.100".to_string()));
        assert_eq!(sdp.media_descriptions.len(), 1);

        let audio = &sdp.media_descriptions[0];
        assert_eq!(audio.media_type, MediaType::Audio);
        assert_eq!(audio.port, 49170);
        assert_eq!(audio.protocol, TransportProtocol::RtpAvp);
        assert_eq!(audio.formats, vec![0, 8, 101, 111]);
        assert_eq!(audio.rtpmaps.len(), 4);

        assert_eq!(audio.rtpmaps[0].encoding_name, "PCMU");
        assert_eq!(audio.rtpmaps[0].clock_rate, 8000);
        assert_eq!(audio.rtpmaps[1].encoding_name, "PCMA");
        assert_eq!(audio.rtpmaps[2].encoding_name, "telephone-event");
        assert_eq!(audio.rtpmaps[2].clock_rate, 8000);
        assert_eq!(audio.rtpmaps[3].encoding_name, "opus");
        assert_eq!(audio.rtpmaps[3].clock_rate, 48000);
        assert_eq!(audio.rtpmaps[3].channels, Some(2));
    }

    #[test]
    fn test_sdp_get_audio_port() {
        let sdp = SdpSession::parse(SAMPLE_SDP).unwrap();
        assert_eq!(sdp.get_audio_port(), Some(49170));
    }

    #[test]
    fn test_sdp_get_connection_address() {
        let sdp = SdpSession::parse(SAMPLE_SDP).unwrap();
        assert_eq!(sdp.get_connection_address(), Some("192.168.1.100"));
    }

    #[test]
    fn test_create_sdp() {
        let mut sdp = SdpSession::new("10.0.0.1");
        sdp.add_audio_media(5004);

        let output = sdp.to_string();
        assert!(output.contains("v=0"));
        assert!(output.contains("c=IN IP4 10.0.0.1"));
        assert!(output.contains("m=audio 5004 RTP/AVP 0 8 101 111"));
        assert!(output.contains("a=rtpmap:0 PCMU/8000"));
        assert!(output.contains("a=rtpmap:8 PCMA/8000"));
        assert!(output.contains("a=rtpmap:101 telephone-event/8000"));
        assert!(output.contains("a=fmtp:101 0-15"));
        assert!(output.contains("a=rtpmap:111 opus/48000/2"));
        assert!(output.contains("a=sendrecv"));
    }

    #[test]
    fn test_sdp_roundtrip() {
        let mut sdp = SdpSession::new("192.168.1.50");
        sdp.add_audio_media(8000);

        let serialized = sdp.to_string();
        let parsed = SdpSession::parse(&serialized).unwrap();

        assert_eq!(parsed.version, 0);
        assert_eq!(parsed.connection_address, Some("192.168.1.50".to_string()));
        assert_eq!(parsed.get_audio_port(), Some(8000));
        assert_eq!(parsed.media_descriptions[0].rtpmaps.len(), 4);
        assert_eq!(parsed.get_audio_dtmf_payload_type(), Some(101));
    }

    #[test]
    fn test_parse_rtpmap() {
        let rtpmap = parse_rtpmap("111 opus/48000/2").unwrap();
        assert_eq!(rtpmap.payload_type, 111);
        assert_eq!(rtpmap.encoding_name, "opus");
        assert_eq!(rtpmap.clock_rate, 48000);
        assert_eq!(rtpmap.channels, Some(2));

        let rtpmap = parse_rtpmap("0 PCMU/8000").unwrap();
        assert_eq!(rtpmap.payload_type, 0);
        assert_eq!(rtpmap.encoding_name, "PCMU");
        assert_eq!(rtpmap.clock_rate, 8000);
        assert_eq!(rtpmap.channels, None);
    }

    #[test]
    fn test_media_description_add_codec() {
        let mut media = MediaDescription::new_audio(5000);
        media.add_codec(96, "telephone-event", 8000, None);
        assert_eq!(media.formats, vec![96]);
        assert_eq!(media.rtpmaps[0].encoding_name, "telephone-event");
    }

    #[test]
    fn test_sdp_no_media() {
        let sdp_str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=test\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n";
        let sdp = SdpSession::parse(sdp_str).unwrap();
        assert!(sdp.media_descriptions.is_empty());
        assert_eq!(sdp.get_audio_port(), None);
    }

    #[test]
    fn test_rtpmap_display() {
        let rtpmap = RtpMap {
            payload_type: 111,
            encoding_name: "opus".to_string(),
            clock_rate: 48000,
            channels: Some(2),
        };
        assert_eq!(rtpmap.to_string(), "111 opus/48000/2");

        let rtpmap = RtpMap {
            payload_type: 0,
            encoding_name: "PCMU".to_string(),
            clock_rate: 8000,
            channels: None,
        };
        assert_eq!(rtpmap.to_string(), "0 PCMU/8000");
    }

    #[test]
    fn test_add_audio_media_directed_sendonly() {
        let mut sdp = SdpSession::new("192.168.1.1");
        sdp.add_audio_media_directed(4000, "sendonly");
        let direction = sdp.get_audio_direction();
        assert_eq!(direction, Some("sendonly"));
        let s = sdp.to_string();
        assert!(s.contains("a=sendonly"));
        assert!(!s.contains("a=sendrecv"));
    }

    #[test]
    fn test_add_audio_media_directed_recvonly() {
        let mut sdp = SdpSession::new("10.0.0.1");
        sdp.add_audio_media_directed(5000, "recvonly");
        assert_eq!(sdp.get_audio_direction(), Some("recvonly"));
    }

    #[test]
    fn test_add_audio_media_directed_inactive() {
        let mut sdp = SdpSession::new("10.0.0.1");
        sdp.add_audio_media_directed(5000, "inactive");
        assert_eq!(sdp.get_audio_direction(), Some("inactive"));
    }

    #[test]
    fn test_add_audio_media_directed_sendrecv() {
        let mut sdp = SdpSession::new("10.0.0.1");
        sdp.add_audio_media_directed(5000, "sendrecv");
        assert_eq!(sdp.get_audio_direction(), Some("sendrecv"));
    }

    #[test]
    fn test_get_audio_direction_default_is_none() {
        // Standard add_audio_media uses sendrecv
        let mut sdp = SdpSession::new("10.0.0.1");
        sdp.add_audio_media(5000);
        // sendrecv is added by add_audio_media
        assert_eq!(sdp.get_audio_direction(), Some("sendrecv"));
    }

    #[test]
    fn test_parse_sdp_with_direction() {
        let sdp_text = "v=0\r\n\
o=- 0 0 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 0\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=sendonly\r\n";
        let sdp = SdpSession::parse(sdp_text).unwrap();
        assert_eq!(sdp.get_audio_direction(), Some("sendonly"));
        assert_eq!(sdp.get_audio_port(), Some(4000));
    }
}
