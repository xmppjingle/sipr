//! SIP Digest Authentication (RFC 2617 / RFC 7616).
//!
//! Handles 401 Unauthorized and 407 Proxy Authentication Required challenges.

use std::fmt;

/// Parsed challenge from WWW-Authenticate or Proxy-Authenticate header.
#[derive(Debug, Clone)]
pub struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    pub opaque: Option<String>,
    pub algorithm: DigestAlgorithm,
    pub qop: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlgorithm {
    Md5,
    Md5Sess,
}

impl fmt::Display for DigestAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DigestAlgorithm::Md5 => write!(f, "MD5"),
            DigestAlgorithm::Md5Sess => write!(f, "MD5-sess"),
        }
    }
}

/// Credentials for authentication.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Computed digest authentication response.
#[derive(Debug, Clone)]
pub struct DigestResponse {
    pub username: String,
    pub realm: String,
    pub nonce: String,
    pub uri: String,
    pub response: String,
    pub algorithm: DigestAlgorithm,
    pub opaque: Option<String>,
    pub qop: Option<String>,
    pub nc: Option<String>,
    pub cnonce: Option<String>,
}

impl fmt::Display for DigestResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", algorithm={}",
            self.username, self.realm, self.nonce, self.uri, self.response, self.algorithm)?;
        if let Some(ref opaque) = self.opaque {
            write!(f, ", opaque=\"{}\"", opaque)?;
        }
        if let Some(ref qop) = self.qop {
            write!(f, ", qop={}", qop)?;
            if let Some(ref nc) = self.nc {
                write!(f, ", nc={}", nc)?;
            }
            if let Some(ref cnonce) = self.cnonce {
                write!(f, ", cnonce=\"{}\"", cnonce)?;
            }
        }
        Ok(())
    }
}

/// Parse a Digest challenge from a WWW-Authenticate or Proxy-Authenticate header value.
///
/// Example input: `Digest realm="asterisk", nonce="abc123", algorithm=MD5, qop="auth"`
pub fn parse_challenge(header_value: &str) -> Option<DigestChallenge> {
    let value = header_value.strip_prefix("Digest ")
        .or_else(|| header_value.strip_prefix("digest "))?;

    let mut realm = None;
    let mut nonce = None;
    let mut opaque = None;
    let mut algorithm = DigestAlgorithm::Md5;
    let mut qop = None;
    let mut stale = false;

    // Parse comma-separated key=value pairs (values may be quoted)
    for param in split_params(value) {
        let param = param.trim();
        if let Some((key, val)) = param.split_once('=') {
            let key = key.trim().to_lowercase();
            let val = val.trim().trim_matches('"');
            match key.as_str() {
                "realm" => realm = Some(val.to_string()),
                "nonce" => nonce = Some(val.to_string()),
                "opaque" => opaque = Some(val.to_string()),
                "algorithm" => {
                    algorithm = match val.to_lowercase().as_str() {
                        "md5-sess" => DigestAlgorithm::Md5Sess,
                        _ => DigestAlgorithm::Md5,
                    };
                }
                "qop" => qop = Some(val.to_string()),
                "stale" => stale = val.eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
    }

    Some(DigestChallenge {
        realm: realm?,
        nonce: nonce?,
        opaque,
        algorithm,
        qop,
        stale,
    })
}

/// Compute the digest authentication response.
///
/// Per RFC 2617:
/// - HA1 = MD5(username:realm:password)
/// - HA2 = MD5(method:uri)
/// - response = MD5(HA1:nonce:HA2) -- without qop
/// - response = MD5(HA1:nonce:nc:cnonce:qop:HA2) -- with qop=auth
pub fn compute_digest(
    challenge: &DigestChallenge,
    creds: &Credentials,
    method: &str,
    uri: &str,
) -> DigestResponse {
    let ha1 = md5_hex(&format!("{}:{}:{}", creds.username, challenge.realm, creds.password));
    let ha2 = md5_hex(&format!("{}:{}", method, uri));

    let (response, qop, nc, cnonce) = if let Some(ref qop_val) = challenge.qop {
        if qop_val.contains("auth") {
            let cnonce = generate_cnonce();
            let nc = "00000001".to_string();
            let response = md5_hex(&format!("{}:{}:{}:{}:auth:{}", ha1, challenge.nonce, nc, cnonce, ha2));
            (response, Some("auth".to_string()), Some(nc), Some(cnonce))
        } else {
            let response = md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, ha2));
            (response, None, None, None)
        }
    } else {
        let response = md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, ha2));
        (response, None, None, None)
    };

    DigestResponse {
        username: creds.username.clone(),
        realm: challenge.realm.clone(),
        nonce: challenge.nonce.clone(),
        uri: uri.to_string(),
        response,
        algorithm: challenge.algorithm,
        opaque: challenge.opaque.clone(),
        qop,
        nc,
        cnonce,
    }
}

/// Split parameters respecting quoted strings.
fn split_params(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                result.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < s.len() {
        result.push(&s[start..]);
    }
    result
}

/// Compute MD5 hex digest of a string.
fn md5_hex(input: &str) -> String {
    // Implement MD5 directly since we don't want to add a dependency.
    // Use a simple pure-Rust MD5 implementation.
    let digest = md5_compute(input.as_bytes());
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn generate_cnonce() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 8] = rng.gen();
    hex_encode(&bytes)
}

// ── Pure-Rust MD5 implementation (RFC 1321) ──────────────────────────

const S: [u32; 64] = [
    7,12,17,22, 7,12,17,22, 7,12,17,22, 7,12,17,22,
    5, 9,14,20, 5, 9,14,20, 5, 9,14,20, 5, 9,14,20,
    4,11,16,23, 4,11,16,23, 4,11,16,23, 4,11,16,23,
    6,10,15,21, 6,10,15,21, 6,10,15,21, 6,10,15,21,
];

const K: [u32; 64] = [
    0xd76aa478,0xe8c7b756,0x242070db,0xc1bdceee,
    0xf57c0faf,0x4787c62a,0xa8304613,0xfd469501,
    0x698098d8,0x8b44f7af,0xffff5bb1,0x895cd7be,
    0x6b901122,0xfd987193,0xa679438e,0x49b40821,
    0xf61e2562,0xc040b340,0x265e5a51,0xe9b6c7aa,
    0xd62f105d,0x02441453,0xd8a1e681,0xe7d3fbc8,
    0x21e1cde6,0xc33707d6,0xf4d50d87,0x455a14ed,
    0xa9e3e905,0xfcefa3f8,0x676f02d9,0x8d2a4c8a,
    0xfffa3942,0x8771f681,0x6d9d6122,0xfde5380c,
    0xa4beea44,0x4bdecfa9,0xf6bb4b60,0xbebfbc70,
    0x289b7ec6,0xeaa127fa,0xd4ef3085,0x04881d05,
    0xd9d4d039,0xe6db99e5,0x1fa27cf8,0xc4ac5665,
    0xf4292244,0x432aff97,0xab9423a7,0xfc93a039,
    0x655b59c3,0x8f0ccc92,0xffeff47d,0x85845dd1,
    0x6fa87e4f,0xfe2ce6e0,0xa3014314,0x4e0811a1,
    0xf7537e82,0xbd3af235,0x2ad7d2bb,0xeb86d391,
];

fn md5_compute(data: &[u8]) -> [u8; 16] {
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    // Pre-processing: add padding
    let orig_len_bits = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&orig_len_bits.to_le_bytes());

    // Process each 512-bit (64-byte) chunk
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };

            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut result = [0u8; 16];
    result[0..4].copy_from_slice(&a0.to_le_bytes());
    result[4..8].copy_from_slice(&b0.to_le_bytes());
    result[8..12].copy_from_slice(&c0.to_le_bytes());
    result[12..16].copy_from_slice(&d0.to_le_bytes());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_known_values() {
        // RFC 1321 test vectors
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5_hex("message digest"), "f96b697d7cb7938d525a2f31aaf161d0");
    }

    #[test]
    fn test_parse_challenge_basic() {
        let header = r#"Digest realm="asterisk", nonce="abc123def""#;
        let challenge = parse_challenge(header).unwrap();
        assert_eq!(challenge.realm, "asterisk");
        assert_eq!(challenge.nonce, "abc123def");
        assert_eq!(challenge.algorithm, DigestAlgorithm::Md5);
        assert!(challenge.opaque.is_none());
        assert!(challenge.qop.is_none());
    }

    #[test]
    fn test_parse_challenge_full() {
        let header = r#"Digest realm="biloxi.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41", qop="auth", algorithm=MD5"#;
        let challenge = parse_challenge(header).unwrap();
        assert_eq!(challenge.realm, "biloxi.com");
        assert_eq!(challenge.nonce, "dcd98b7102dd2f0e8b11d0f600bfb0c093");
        assert_eq!(challenge.opaque.as_deref(), Some("5ccc069c403ebaf9f0171e9517f40e41"));
        assert_eq!(challenge.qop.as_deref(), Some("auth"));
        assert_eq!(challenge.algorithm, DigestAlgorithm::Md5);
    }

    #[test]
    fn test_compute_digest_rfc2617_example() {
        // Based on RFC 2617 Section 3.5 example
        let challenge = DigestChallenge {
            realm: "testrealm@host.com".to_string(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".to_string(),
            opaque: Some("5ccc069c403ebaf9f0171e9517f40e41".to_string()),
            algorithm: DigestAlgorithm::Md5,
            qop: Some("auth".to_string()),
            stale: false,
        };
        let creds = Credentials {
            username: "Mufasa".to_string(),
            password: "Circle Of Life".to_string(),
        };
        let resp = compute_digest(&challenge, &creds, "GET", "/dir/index.html");
        assert_eq!(resp.realm, "testrealm@host.com");
        assert_eq!(resp.username, "Mufasa");
        // Can't check exact response since cnonce is random, but verify it's 32 hex chars
        assert_eq!(resp.response.len(), 32);
        assert!(resp.response.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(resp.qop.as_deref(), Some("auth"));
    }

    #[test]
    fn test_compute_digest_no_qop() {
        let challenge = DigestChallenge {
            realm: "asterisk".to_string(),
            nonce: "1234567890".to_string(),
            opaque: None,
            algorithm: DigestAlgorithm::Md5,
            qop: None,
            stale: false,
        };
        let creds = Credentials {
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let resp = compute_digest(&challenge, &creds, "REGISTER", "sip:asterisk");

        // Manually compute expected:
        // HA1 = MD5("alice:asterisk:secret")
        // HA2 = MD5("REGISTER:sip:asterisk")
        // response = MD5(HA1:1234567890:HA2)
        let ha1 = md5_hex("alice:asterisk:secret");
        let ha2 = md5_hex("REGISTER:sip:asterisk");
        let expected = md5_hex(&format!("{}:1234567890:{}", ha1, ha2));
        assert_eq!(resp.response, expected);
        assert!(resp.qop.is_none());
    }

    #[test]
    fn test_digest_response_display() {
        let resp = DigestResponse {
            username: "alice".to_string(),
            realm: "asterisk".to_string(),
            nonce: "abc123".to_string(),
            uri: "sip:asterisk".to_string(),
            response: "deadbeef01234567890abcdef0123456".to_string(),
            algorithm: DigestAlgorithm::Md5,
            opaque: Some("xyz".to_string()),
            qop: None,
            nc: None,
            cnonce: None,
        };
        let s = resp.to_string();
        assert!(s.starts_with("Digest "));
        assert!(s.contains("username=\"alice\""));
        assert!(s.contains("opaque=\"xyz\""));
        assert!(!s.contains("qop="));
    }

    #[test]
    fn test_parse_challenge_stale() {
        let header = r#"Digest realm="test", nonce="new_nonce", stale=true"#;
        let challenge = parse_challenge(header).unwrap();
        assert!(challenge.stale);
    }

    #[test]
    fn test_split_params_with_quotes() {
        let input = r#"realm="a,b", nonce="c""#;
        let params = split_params(input);
        assert_eq!(params.len(), 2);
        assert!(params[0].contains("a,b"));
    }
}
