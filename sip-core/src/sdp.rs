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

    /// `a=fmtp:<pt> ...` value for this payload type, if present.
    pub fn fmtp_for(&self, payload_type: u8) -> Option<&str> {
        let prefix = format!("{} ", payload_type);
        for (name, value) in &self.attributes {
            if name != "fmtp" {
                continue;
            }
            if let Some(val) = value {
                if val.starts_with(&prefix) {
                    return Some(val[prefix.len()..].trim());
                }
            }
        }
        None
    }
}

/// AMR-WB `octet-align` in `a=fmtp` (RFC 4867). Omitted means bandwidth-efficient (0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OctetAlign {
    /// `octet-align=1` (octet-aligned)
    #[default]
    One,
    /// `octet-align=0` (bandwidth-efficient)
    Zero,
    /// No `octet-align` token (RFC 4867 default = 0)
    Omitted,
}

impl OctetAlign {
    pub fn parse_token(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1" | "one" | "oa1" | "octet" | "octet-aligned" => Ok(OctetAlign::One),
            "0" | "zero" | "oa0" | "be" | "bandwidth-efficient" => Ok(OctetAlign::Zero),
            "omit" | "omitted" | "none" | "rfc" => Ok(OctetAlign::Omitted),
            other => Err(format!(
                "Unknown octet-align '{}'. Use 1, 0, or omit",
                other
            )),
        }
    }

    /// Parse from an AMR/AMR-WB fmtp body (`octet-align=1; mode-set=8`).
    pub fn from_fmtp(fmtp: &str) -> Self {
        for part in fmtp.split(';') {
            let part = part.trim();
            if let Some(val) = part
                .strip_prefix("octet-align=")
                .or_else(|| part.strip_prefix("octet-align ="))
            {
                return match val.trim() {
                    "1" => OctetAlign::One,
                    "0" => OctetAlign::Zero,
                    _ => OctetAlign::Omitted,
                };
            }
        }
        OctetAlign::Omitted
    }

    /// Token for `a=fmtp` (`None` means omit the parameter).
    pub fn fmtp_token(self) -> Option<&'static str> {
        match self {
            OctetAlign::One => Some("octet-align=1"),
            OctetAlign::Zero => Some("octet-align=0"),
            OctetAlign::Omitted => None,
        }
    }
}

impl fmt::Display for OctetAlign {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OctetAlign::One => write!(f, "1"),
            OctetAlign::Zero => write!(f, "0"),
            OctetAlign::Omitted => write!(f, "omitted"),
        }
    }
}

/// One codec (or DTMF) line in an SDP audio offer/answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferedCodec {
    Pcmu,
    Pcma,
    Opus { channels: u32 },
    AmrWb {
        payload_type: u8,
        octet_align: OctetAlign,
        mode_set: Option<String>,
    },
    TelephoneEvent { payload_type: u8, clock_rate: u32 },
}

impl OfferedCodec {
    pub const AMR_WB_PT: u8 = 102;
    pub const TE_WB_PT: u8 = 104;
    pub const TE_NB_PT: u8 = 101;

    pub fn payload_type(&self) -> u8 {
        match self {
            OfferedCodec::Pcmu => 0,
            OfferedCodec::Pcma => 8,
            OfferedCodec::Opus { .. } => 111,
            OfferedCodec::AmrWb { payload_type, .. } => *payload_type,
            OfferedCodec::TelephoneEvent { payload_type, .. } => *payload_type,
        }
    }

    pub fn encoding_name(&self) -> &'static str {
        match self {
            OfferedCodec::Pcmu => "PCMU",
            OfferedCodec::Pcma => "PCMA",
            OfferedCodec::Opus { .. } => "opus",
            OfferedCodec::AmrWb { .. } => "AMR-WB",
            OfferedCodec::TelephoneEvent { .. } => "telephone-event",
        }
    }
}

/// Ordered SDP audio payload list. Default matches historic sipr offers (G.711 + opus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOffer {
    pub codecs: Vec<OfferedCodec>,
    pub direction: String,
}

impl Default for AudioOffer {
    fn default() -> Self {
        Self::g711_opus()
    }
}

impl AudioOffer {
    /// Historic sipr offer: PCMU, PCMA, telephone-event/8000, opus.
    pub fn g711_opus() -> Self {
        Self {
            codecs: vec![
                OfferedCodec::Pcmu,
                OfferedCodec::Pcma,
                OfferedCodec::TelephoneEvent {
                    payload_type: OfferedCodec::TE_NB_PT,
                    clock_rate: 8000,
                },
                OfferedCodec::Opus { channels: 2 },
            ],
            direction: "sendrecv".to_string(),
        }
    }

    pub fn pcmu_only() -> Self {
        Self {
            codecs: vec![
                OfferedCodec::Pcmu,
                OfferedCodec::TelephoneEvent {
                    payload_type: OfferedCodec::TE_NB_PT,
                    clock_rate: 8000,
                },
            ],
            direction: "sendrecv".to_string(),
        }
    }

    /// AMR-WB preferred, then G.711. OA is applied to the AMR-WB fmtp.
    pub fn amrwb_then_g711(octet_align: OctetAlign) -> Self {
        Self::from_names(&["amrwb", "pcmu", "pcma"], octet_align)
    }

    /// G.711 preferred, AMR-WB second (Patrick IN2 / OUT2).
    pub fn g711_then_amrwb(octet_align: OctetAlign) -> Self {
        Self::from_names(&["pcmu", "amrwb"], octet_align)
    }

    /// Build from CLI names: `amrwb`, `pcmu`, `pcma`, `opus`.
    pub fn from_names(names: &[&str], octet_align: OctetAlign) -> Self {
        let mut codecs = Vec::new();
        let mut saw_amrwb = false;
        let mut saw_te8 = false;
        for raw in names {
            match raw.trim().to_ascii_lowercase().as_str() {
                "amrwb" | "amr-wb" | "amr_wb" => {
                    saw_amrwb = true;
                    codecs.push(OfferedCodec::AmrWb {
                        payload_type: OfferedCodec::AMR_WB_PT,
                        octet_align,
                        mode_set: Some("8".to_string()),
                    });
                    codecs.push(OfferedCodec::TelephoneEvent {
                        payload_type: OfferedCodec::TE_WB_PT,
                        clock_rate: 16000,
                    });
                }
                "pcmu" | "ulaw" | "g711u" | "g711" => codecs.push(OfferedCodec::Pcmu),
                "pcma" | "alaw" | "g711a" => codecs.push(OfferedCodec::Pcma),
                "opus" => codecs.push(OfferedCodec::Opus { channels: 2 }),
                _ => {}
            }
        }
        if codecs
            .iter()
            .any(|c| matches!(c, OfferedCodec::TelephoneEvent { clock_rate: 8000, .. }))
        {
            saw_te8 = true;
        }
        if !saw_te8 {
            codecs.push(OfferedCodec::TelephoneEvent {
                payload_type: OfferedCodec::TE_NB_PT,
                clock_rate: 8000,
            });
        }
        let _ = saw_amrwb;
        Self {
            codecs,
            direction: "sendrecv".to_string(),
        }
    }

    pub fn with_direction(mut self, direction: &str) -> Self {
        self.direction = direction.to_string();
        self
    }

    /// AMR-WB only (plus telephone-event/16000). Used as an SDP answer.
    pub fn amrwb_only(octet_align: OctetAlign) -> Self {
        Self::from_names(&["amrwb"], octet_align)
    }

    /// First non-DTMF encoding name (SDP `rtpmap` name).
    pub fn primary_name(&self) -> Option<&'static str> {
        self.codecs.iter().find_map(|c| match c {
            OfferedCodec::TelephoneEvent { .. } => None,
            other => Some(other.encoding_name()),
        })
    }

    /// Override AMR-WB `mode-set=` on every AMR-WB payload in this offer.
    pub fn with_amrwb_mode_set(mut self, mode_set: &str) -> Self {
        for codec in &mut self.codecs {
            if let OfferedCodec::AmrWb { mode_set: ms, .. } = codec {
                *ms = Some(mode_set.to_string());
            }
        }
        self
    }

    /// Octet-align of the AMR-WB payload, or `Omitted` if none is offered.
    pub fn octet_align(&self) -> OctetAlign {
        self.codecs
            .iter()
            .find_map(|c| match c {
                OfferedCodec::AmrWb { octet_align, .. } => Some(*octet_align),
                _ => None,
            })
            .unwrap_or(OctetAlign::Omitted)
    }

    /// Answer with the first codec we support from a remote offer.
    pub fn answer_from_remote(sdp: &SdpSession) -> Self {
        let names = sdp.audio_codec_names();
        let oa = sdp.amr_wb_octet_align().unwrap_or(OctetAlign::Omitted);
        let has = |want: &str| {
            names
                .iter()
                .any(|n| n.eq_ignore_ascii_case(want))
        };
        if has("AMR-WB") {
            Self::amrwb_only(oa)
        } else if has("PCMU") {
            Self::pcmu_only()
        } else if has("PCMA") {
            Self::from_names(&["pcma"], oa)
        } else if has("opus") {
            Self::from_names(&["opus"], oa)
        } else {
            Self::g711_opus()
        }
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
        self.add_audio_media_offer(port, &AudioOffer::g711_opus())
    }

    /// Offer/answer with an explicit codec list (AMR-WB + octet-align, G.711-only, …).
    pub fn add_audio_media_offer(&mut self, port: u16, offer: &AudioOffer) -> &mut MediaDescription {
        let mut media = MediaDescription::new_audio(port);
        apply_offer_to_media(&mut media, offer);
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
        self.add_audio_media_offer(port, &AudioOffer::g711_opus().with_direction(direction))
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

    /// Ordered audio encoding names from `m=` / rtpmap, telephone-event omitted.
    pub fn audio_codec_names(&self) -> Vec<String> {
        let Some(audio) = self
            .media_descriptions
            .iter()
            .find(|m| m.media_type == MediaType::Audio)
        else {
            return Vec::new();
        };
        audio
            .formats
            .iter()
            .filter_map(|pt| {
                let name = audio
                    .rtpmaps
                    .iter()
                    .find(|r| r.payload_type == *pt)
                    .map(|r| r.encoding_name.as_str())
                    .or_else(|| match pt {
                        0 => Some("PCMU"),
                        8 => Some("PCMA"),
                        _ => None,
                    })?;
                if name.eq_ignore_ascii_case("telephone-event") {
                    None
                } else {
                    Some(name.to_string())
                }
            })
            .collect()
    }

    /// Octet-align on the first AMR-WB payload. `None` if AMR-WB is not offered.
    pub fn amr_wb_octet_align(&self) -> Option<OctetAlign> {
        let audio = self
            .media_descriptions
            .iter()
            .find(|m| m.media_type == MediaType::Audio)?;
        let rtpmap = audio
            .rtpmaps
            .iter()
            .find(|r| r.encoding_name.eq_ignore_ascii_case("AMR-WB"))?;
        match audio.fmtp_for(rtpmap.payload_type) {
            Some(fmtp) => Some(OctetAlign::from_fmtp(fmtp)),
            None => Some(OctetAlign::Omitted),
        }
    }
}

fn apply_offer_to_media(media: &mut MediaDescription, offer: &AudioOffer) {
    for codec in &offer.codecs {
        match codec {
            OfferedCodec::Pcmu => media.add_codec(0, "PCMU", 8000, None),
            OfferedCodec::Pcma => media.add_codec(8, "PCMA", 8000, None),
            OfferedCodec::Opus { channels } => {
                media.add_codec(111, "opus", 48000, Some(*channels));
            }
            OfferedCodec::AmrWb {
                payload_type,
                octet_align,
                mode_set,
            } => {
                media.add_codec(*payload_type, "AMR-WB", 16000, None);
                let mut parts = Vec::new();
                if let Some(tok) = octet_align.fmtp_token() {
                    parts.push(tok.to_string());
                }
                if let Some(ms) = mode_set {
                    parts.push(format!("mode-set={}", ms));
                }
                if !parts.is_empty() {
                    media.add_attribute(
                        "fmtp",
                        Some(&format!("{} {}", payload_type, parts.join("; "))),
                    );
                }
            }
            OfferedCodec::TelephoneEvent {
                payload_type,
                clock_rate,
            } => {
                media.add_codec(*payload_type, "telephone-event", *clock_rate, None);
                media.add_attribute("fmtp", Some(&format!("{} 0-15", payload_type)));
            }
        }
    }
    if !offer.direction.is_empty() {
        media.add_attribute(&offer.direction, None);
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
    fn test_amrwb_mode_set_override() {
        let offer = AudioOffer::amrwb_then_g711(OctetAlign::One).with_amrwb_mode_set("0,1,2");
        let mut sdp = SdpSession::new("127.0.0.1");
        sdp.add_audio_media_offer(4000, &offer);
        let wire = sdp.to_string();
        assert!(wire.contains("mode-set=0,1,2"), "{wire}");
        assert!(!wire.contains("mode-set=8"), "{wire}");
        assert!(wire.contains("octet-align=1"), "{wire}");
    }

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

    #[test]
    fn test_amrwb_offer_oa1() {
        let mut sdp = SdpSession::new("10.0.0.1");
        sdp.add_audio_media_offer(5072, &AudioOffer::amrwb_then_g711(OctetAlign::One));
        let wire = sdp.to_string();
        assert!(wire.contains("m=audio 5072 RTP/AVP 102 104 0 8 101"));
        assert!(wire.contains("a=rtpmap:102 AMR-WB/16000"));
        assert!(wire.contains("a=fmtp:102 octet-align=1; mode-set=8"));
        assert!(wire.contains("a=rtpmap:104 telephone-event/16000"));
        assert_eq!(sdp.amr_wb_octet_align(), Some(OctetAlign::One));
        assert_eq!(sdp.audio_codec_names()[0], "AMR-WB");
    }

    #[test]
    fn test_amrwb_offer_oa0_and_omitted() {
        let mut sdp0 = SdpSession::new("10.0.0.1");
        sdp0.add_audio_media_offer(5004, &AudioOffer::amrwb_then_g711(OctetAlign::Zero));
        let w0 = sdp0.to_string();
        assert!(w0.contains("octet-align=0"));
        assert!(!w0.contains("octet-align=1"));
        assert_eq!(sdp0.amr_wb_octet_align(), Some(OctetAlign::Zero));

        let mut sdp_omit = SdpSession::new("10.0.0.1");
        sdp_omit.add_audio_media_offer(5004, &AudioOffer::amrwb_then_g711(OctetAlign::Omitted));
        let wo = sdp_omit.to_string();
        assert!(!wo.contains("octet-align="));
        assert!(wo.contains("a=fmtp:102 mode-set=8"));
        assert_eq!(sdp_omit.amr_wb_octet_align(), Some(OctetAlign::Omitted));
    }

    #[test]
    fn test_parse_amrwb_oa_from_wire() {
        let sdp = SdpSession::parse(
            "v=0\r\n\
o=- 1 1 IN IP4 10.0.0.1\r\n\
s=-\r\n\
c=IN IP4 10.0.0.1\r\n\
t=0 0\r\n\
m=audio 4000 RTP/AVP 102 0\r\n\
a=rtpmap:102 AMR-WB/16000\r\n\
a=fmtp:102 octet-align=1; mode-set=8\r\n\
a=rtpmap:0 PCMU/8000\r\n",
        )
        .unwrap();
        assert_eq!(sdp.amr_wb_octet_align(), Some(OctetAlign::One));
        assert_eq!(sdp.audio_codec_names(), vec!["AMR-WB", "PCMU"]);
    }

    #[test]
    fn test_g711_then_amrwb_first_codec() {
        let mut sdp = SdpSession::new("10.0.0.1");
        sdp.add_audio_media_offer(5004, &AudioOffer::g711_then_amrwb(OctetAlign::One));
        assert_eq!(sdp.audio_codec_names()[0], "PCMU");
        assert!(sdp.audio_codec_names().iter().any(|n| n == "AMR-WB"));
    }

    #[test]
    fn test_octet_align_parse_token() {
        assert_eq!(OctetAlign::parse_token("1").unwrap(), OctetAlign::One);
        assert_eq!(OctetAlign::parse_token("0").unwrap(), OctetAlign::Zero);
        assert_eq!(OctetAlign::parse_token("omit").unwrap(), OctetAlign::Omitted);
        assert!(OctetAlign::parse_token("xyz").is_err());
    }

    #[test]
    fn test_answer_from_remote_honors_amrwb_oa() {
        let mut offer = SdpSession::new("10.0.0.1");
        offer.add_audio_media_offer(5072, &AudioOffer::amrwb_then_g711(OctetAlign::Zero));
        let answer = AudioOffer::answer_from_remote(&offer);
        assert_eq!(answer.primary_name(), Some("AMR-WB"));
        assert_eq!(answer.octet_align(), OctetAlign::Zero);
        assert!(!answer
            .codecs
            .iter()
            .any(|c| matches!(c, OfferedCodec::Pcmu)));
    }
}
