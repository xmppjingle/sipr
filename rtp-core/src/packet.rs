use thiserror::Error;

/// RTP packet (RFC 3550)
///
/// ```text
///  0                   1                   2                   3
///  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |V=2|P|X|  CC   |M|     PT      |       sequence number         |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                           timestamp                           |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |           synchronization source (SSRC) identifier            |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub csrc_count: u8,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub csrc: Vec<u32>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum RtpError {
    #[error("packet too short: {0} bytes")]
    TooShort(usize),
    #[error("invalid RTP version: {0}")]
    InvalidVersion(u8),
    #[error("buffer too small")]
    BufferTooSmall,
}

impl RtpPacket {
    /// Minimum RTP header size (no CSRC, no extension)
    pub const MIN_HEADER_SIZE: usize = 12;

    /// Create a new RTP packet
    pub fn new(payload_type: u8, sequence_number: u16, timestamp: u32, ssrc: u32) -> Self {
        Self {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            csrc: Vec::new(),
            payload: Vec::new(),
        }
    }

    /// Set the payload data
    pub fn with_payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    /// Set the marker bit
    pub fn with_marker(mut self, marker: bool) -> Self {
        self.marker = marker;
        self
    }

    /// Parse an RTP packet from bytes
    pub fn parse(data: &[u8]) -> Result<Self, RtpError> {
        if data.len() < Self::MIN_HEADER_SIZE {
            return Err(RtpError::TooShort(data.len()));
        }

        let version = (data[0] >> 6) & 0x03;
        if version != 2 {
            return Err(RtpError::InvalidVersion(version));
        }

        let padding = (data[0] >> 5) & 0x01 != 0;
        let extension = (data[0] >> 4) & 0x01 != 0;
        let csrc_count = data[0] & 0x0F;
        let marker = (data[1] >> 7) & 0x01 != 0;
        let payload_type = data[1] & 0x7F;
        let sequence_number = u16::from_be_bytes([data[2], data[3]]);
        let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        let header_size = Self::MIN_HEADER_SIZE + (csrc_count as usize * 4);
        if data.len() < header_size {
            return Err(RtpError::TooShort(data.len()));
        }

        let mut csrc = Vec::with_capacity(csrc_count as usize);
        for i in 0..csrc_count as usize {
            let offset = Self::MIN_HEADER_SIZE + (i * 4);
            let csrc_id = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            csrc.push(csrc_id);
        }

        let mut payload_offset = header_size;

        // Skip extension header if present
        if extension && data.len() >= payload_offset + 4 {
            let ext_length =
                u16::from_be_bytes([data[payload_offset + 2], data[payload_offset + 3]]) as usize;
            payload_offset += 4 + (ext_length * 4);
        }

        let payload_end = if padding && !data.is_empty() {
            let padding_len = data[data.len() - 1] as usize;
            data.len().saturating_sub(padding_len)
        } else {
            data.len()
        };

        let payload = if payload_offset <= payload_end {
            data[payload_offset..payload_end].to_vec()
        } else {
            Vec::new()
        };

        Ok(RtpPacket {
            version,
            padding,
            extension,
            csrc_count,
            marker,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            csrc,
            payload,
        })
    }

    /// Serialize the RTP packet to bytes
    pub fn serialize(&self) -> Vec<u8> {
        let header_size = Self::MIN_HEADER_SIZE + (self.csrc.len() * 4);
        let mut buf = Vec::with_capacity(header_size + self.payload.len());

        // Byte 0: V=2, P, X, CC
        let byte0 = (self.version << 6)
            | ((self.padding as u8) << 5)
            | ((self.extension as u8) << 4)
            | (self.csrc.len() as u8 & 0x0F);
        buf.push(byte0);

        // Byte 1: M, PT
        let byte1 = ((self.marker as u8) << 7) | (self.payload_type & 0x7F);
        buf.push(byte1);

        // Bytes 2-3: sequence number
        buf.extend_from_slice(&self.sequence_number.to_be_bytes());

        // Bytes 4-7: timestamp
        buf.extend_from_slice(&self.timestamp.to_be_bytes());

        // Bytes 8-11: SSRC
        buf.extend_from_slice(&self.ssrc.to_be_bytes());

        // CSRC list
        for csrc_id in &self.csrc {
            buf.extend_from_slice(&csrc_id.to_be_bytes());
        }

        // Payload
        buf.extend_from_slice(&self.payload);

        buf
    }

    /// Get the total size of the serialized packet
    pub fn size(&self) -> usize {
        Self::MIN_HEADER_SIZE + (self.csrc.len() * 4) + self.payload.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_packet() {
        let pkt = RtpPacket::new(0, 1, 160, 0x12345678);
        assert_eq!(pkt.version, 2);
        assert_eq!(pkt.payload_type, 0);
        assert_eq!(pkt.sequence_number, 1);
        assert_eq!(pkt.timestamp, 160);
        assert_eq!(pkt.ssrc, 0x12345678);
        assert!(!pkt.marker);
        assert!(pkt.payload.is_empty());
    }

    #[test]
    fn test_with_payload() {
        let pkt = RtpPacket::new(0, 1, 160, 0x12345678).with_payload(vec![1, 2, 3, 4]);
        assert_eq!(pkt.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_with_marker() {
        let pkt = RtpPacket::new(0, 1, 160, 0x12345678).with_marker(true);
        assert!(pkt.marker);
    }

    #[test]
    fn test_serialize_parse_roundtrip() {
        let original = RtpPacket::new(0, 42, 3360, 0xDEADBEEF)
            .with_payload(vec![0x80, 0xFF, 0x00, 0x7F, 0x01, 0x02])
            .with_marker(true);

        let bytes = original.serialize();
        let parsed = RtpPacket::parse(&bytes).unwrap();

        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.payload_type, 0);
        assert_eq!(parsed.sequence_number, 42);
        assert_eq!(parsed.timestamp, 3360);
        assert_eq!(parsed.ssrc, 0xDEADBEEF);
        assert!(parsed.marker);
        assert_eq!(parsed.payload, vec![0x80, 0xFF, 0x00, 0x7F, 0x01, 0x02]);
    }

    #[test]
    fn test_parse_pcmu_packet() {
        // Construct a minimal PCMU RTP packet manually
        let mut data = vec![
            0x80, // V=2, P=0, X=0, CC=0
            0x00, // M=0, PT=0 (PCMU)
            0x00, 0x01, // seq=1
            0x00, 0x00, 0x00, 0xA0, // timestamp=160
            0x00, 0x00, 0x00, 0x01, // ssrc=1
        ];
        // Add 160 bytes of payload (one PCMU frame at 8kHz, 20ms)
        data.extend_from_slice(&vec![0x7F; 160]);

        let pkt = RtpPacket::parse(&data).unwrap();
        assert_eq!(pkt.version, 2);
        assert_eq!(pkt.payload_type, 0);
        assert_eq!(pkt.sequence_number, 1);
        assert_eq!(pkt.timestamp, 160);
        assert_eq!(pkt.ssrc, 1);
        assert_eq!(pkt.payload.len(), 160);
    }

    #[test]
    fn test_parse_too_short() {
        let data = vec![0x80, 0x00, 0x00];
        let result = RtpPacket::parse(&data);
        assert!(matches!(result, Err(RtpError::TooShort(3))));
    }

    #[test]
    fn test_parse_invalid_version() {
        let data = vec![
            0x00, // V=0
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xA0, 0x00, 0x00, 0x00, 0x01,
        ];
        let result = RtpPacket::parse(&data);
        assert!(matches!(result, Err(RtpError::InvalidVersion(0))));
    }

    #[test]
    fn test_serialize_header_only() {
        let pkt = RtpPacket::new(8, 100, 16000, 0xAAAABBBB);
        let bytes = pkt.serialize();
        assert_eq!(bytes.len(), RtpPacket::MIN_HEADER_SIZE);

        // Check version bits
        assert_eq!(bytes[0] & 0xC0, 0x80); // V=2

        // Check PT
        assert_eq!(bytes[1] & 0x7F, 8); // PT=8 (PCMA)

        // Check seq
        assert_eq!(u16::from_be_bytes([bytes[2], bytes[3]]), 100);
    }

    #[test]
    fn test_multiple_roundtrips() {
        for pt in [0u8, 8, 111] {
            for seq in [0u16, 1, 65535] {
                let pkt = RtpPacket::new(pt, seq, seq as u32 * 160, 0x11223344)
                    .with_payload(vec![0xAA; 20]);
                let bytes = pkt.serialize();
                let parsed = RtpPacket::parse(&bytes).unwrap();
                assert_eq!(parsed.payload_type, pt);
                assert_eq!(parsed.sequence_number, seq);
                assert_eq!(parsed.payload.len(), 20);
            }
        }
    }

    #[test]
    fn test_packet_size() {
        let pkt = RtpPacket::new(0, 1, 160, 1).with_payload(vec![0; 160]);
        assert_eq!(pkt.size(), 12 + 160);

        let pkt_empty = RtpPacket::new(0, 1, 160, 1);
        assert_eq!(pkt_empty.size(), 12);
    }

    #[test]
    fn test_csrc_roundtrip() {
        let mut pkt = RtpPacket::new(0, 1, 160, 0x12345678);
        pkt.csrc = vec![0xAAAA0001, 0xBBBB0002];
        pkt.csrc_count = 2;
        pkt.payload = vec![0xFF; 10];

        let bytes = pkt.serialize();
        let parsed = RtpPacket::parse(&bytes).unwrap();
        assert_eq!(parsed.csrc_count, 2);
        assert_eq!(parsed.csrc, vec![0xAAAA0001, 0xBBBB0002]);
        assert_eq!(parsed.payload.len(), 10);
    }

    #[test]
    fn test_marker_bit_serialization() {
        let pkt = RtpPacket::new(111, 1, 960, 1).with_marker(true);
        let bytes = pkt.serialize();
        assert_eq!(bytes[1] & 0x80, 0x80); // Marker bit set

        let pkt2 = RtpPacket::new(111, 1, 960, 1).with_marker(false);
        let bytes2 = pkt2.serialize();
        assert_eq!(bytes2[1] & 0x80, 0x00); // Marker bit clear
    }
}
