use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HeaderName {
    Via,
    From,
    To,
    CallId,
    CSeq,
    Contact,
    MaxForwards,
    ContentType,
    ContentLength,
    Authorization,
    WwwAuthenticate,
    ProxyAuthenticate,
    ProxyAuthorization,
    Expires,
    UserAgent,
    Allow,
    Supported,
    Require,
    RAck,
    RSeq,
    ReferTo,
    ReferredBy,
    Event,
    SubscriptionState,
    Other(String),
}

impl HeaderName {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "via" | "v" => HeaderName::Via,
            "from" | "f" => HeaderName::From,
            "to" | "t" => HeaderName::To,
            "call-id" | "i" => HeaderName::CallId,
            "cseq" => HeaderName::CSeq,
            "contact" | "m" => HeaderName::Contact,
            "max-forwards" => HeaderName::MaxForwards,
            "content-type" | "c" => HeaderName::ContentType,
            "content-length" | "l" => HeaderName::ContentLength,
            "authorization" => HeaderName::Authorization,
            "www-authenticate" => HeaderName::WwwAuthenticate,
            "proxy-authenticate" => HeaderName::ProxyAuthenticate,
            "proxy-authorization" => HeaderName::ProxyAuthorization,
            "expires" => HeaderName::Expires,
            "user-agent" => HeaderName::UserAgent,
            "allow" => HeaderName::Allow,
            "supported" | "k" => HeaderName::Supported,
            "require" => HeaderName::Require,
            "rack" => HeaderName::RAck,
            "rseq" => HeaderName::RSeq,
            "refer-to" | "r" => HeaderName::ReferTo,
            "referred-by" | "b" => HeaderName::ReferredBy,
            "event" | "o" => HeaderName::Event,
            "subscription-state" => HeaderName::SubscriptionState,
            other => HeaderName::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            HeaderName::Via => "Via",
            HeaderName::From => "From",
            HeaderName::To => "To",
            HeaderName::CallId => "Call-ID",
            HeaderName::CSeq => "CSeq",
            HeaderName::Contact => "Contact",
            HeaderName::MaxForwards => "Max-Forwards",
            HeaderName::ContentType => "Content-Type",
            HeaderName::ContentLength => "Content-Length",
            HeaderName::Authorization => "Authorization",
            HeaderName::WwwAuthenticate => "WWW-Authenticate",
            HeaderName::ProxyAuthenticate => "Proxy-Authenticate",
            HeaderName::ProxyAuthorization => "Proxy-Authorization",
            HeaderName::Expires => "Expires",
            HeaderName::UserAgent => "User-Agent",
            HeaderName::Allow => "Allow",
            HeaderName::Supported => "Supported",
            HeaderName::Require => "Require",
            HeaderName::RAck => "RAck",
            HeaderName::RSeq => "RSeq",
            HeaderName::ReferTo => "Refer-To",
            HeaderName::ReferredBy => "Referred-By",
            HeaderName::Event => "Event",
            HeaderName::SubscriptionState => "Subscription-State",
            HeaderName::Other(s) => s.as_str(),
        }
    }
}

impl fmt::Display for HeaderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderValue(pub String);

impl HeaderValue {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HeaderValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: HeaderName,
    pub value: HeaderValue,
}

impl Header {
    pub fn new(name: HeaderName, value: impl Into<String>) -> Self {
        Self {
            name,
            value: HeaderValue::new(value),
        }
    }
}

impl fmt::Display for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.value)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Headers {
    headers: Vec<Header>,
}

impl Headers {
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
        }
    }

    pub fn add(&mut self, name: HeaderName, value: impl Into<String>) {
        self.headers.push(Header::new(name, value));
    }

    pub fn get(&self, name: &HeaderName) -> Option<&HeaderValue> {
        self.headers
            .iter()
            .find(|h| &h.name == name)
            .map(|h| &h.value)
    }

    pub fn get_all(&self, name: &HeaderName) -> Vec<&HeaderValue> {
        self.headers
            .iter()
            .filter(|h| &h.name == name)
            .map(|h| &h.value)
            .collect()
    }

    pub fn set(&mut self, name: HeaderName, value: impl Into<String>) {
        if let Some(header) = self.headers.iter_mut().find(|h| h.name == name) {
            header.value = HeaderValue::new(value);
        } else {
            self.add(name, value);
        }
    }

    pub fn remove(&mut self, name: &HeaderName) {
        self.headers.retain(|h| &h.name != name);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.headers.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.headers.len()
    }
}

impl fmt::Display for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for header in &self.headers {
            writeln!(f, "{}", header)?;
        }
        Ok(())
    }
}

/// Parse a SIP URI tag parameter, e.g. extract "tag=xyz" from a From/To header value.
pub fn extract_tag(header_value: &str) -> Option<String> {
    header_value
        .split(';')
        .find_map(|param| {
            let param = param.trim();
            if let Some(tag) = param.strip_prefix("tag=") {
                Some(tag.to_string())
            } else {
                None
            }
        })
}

/// Extract the URI from a header value like `"Alice" <sip:alice@example.com>;tag=xyz`
pub fn extract_uri(header_value: &str) -> Option<String> {
    if let Some(start) = header_value.find('<') {
        if let Some(end) = header_value.find('>') {
            return Some(header_value[start + 1..end].to_string());
        }
    }
    // If no angle brackets, the value itself might be a URI
    let uri = header_value.split(';').next()?.trim();
    if uri.starts_with("sip:") || uri.starts_with("sips:") {
        Some(uri.to_string())
    } else {
        None
    }
}

/// Generate a random tag for From/To headers
pub fn generate_tag() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let tag: u64 = rng.gen();
    format!("{:x}", tag)
}

/// Generate a random branch parameter for Via headers (RFC 3261 magic cookie)
pub fn generate_branch() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let branch: u64 = rng.gen();
    format!("z9hG4bK{:x}", branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_name_parsing() {
        assert_eq!(HeaderName::from_str("Via"), HeaderName::Via);
        assert_eq!(HeaderName::from_str("v"), HeaderName::Via);
        assert_eq!(HeaderName::from_str("VIA"), HeaderName::Via);
        assert_eq!(HeaderName::from_str("from"), HeaderName::From);
        assert_eq!(HeaderName::from_str("f"), HeaderName::From);
        assert_eq!(HeaderName::from_str("Call-ID"), HeaderName::CallId);
        assert_eq!(HeaderName::from_str("i"), HeaderName::CallId);
        assert_eq!(HeaderName::from_str("CSeq"), HeaderName::CSeq);
        assert_eq!(
            HeaderName::from_str("X-Custom"),
            HeaderName::Other("x-custom".to_string())
        );
    }

    #[test]
    fn test_header_name_display() {
        assert_eq!(HeaderName::Via.as_str(), "Via");
        assert_eq!(HeaderName::CallId.as_str(), "Call-ID");
        assert_eq!(HeaderName::ContentType.as_str(), "Content-Type");
    }

    #[test]
    fn test_headers_collection() {
        let mut headers = Headers::new();
        headers.add(HeaderName::Via, "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK776asdhds");
        headers.add(HeaderName::From, "<sip:alice@atlanta.com>;tag=1928301774");
        headers.add(HeaderName::To, "<sip:bob@biloxi.com>");
        headers.add(HeaderName::CallId, "a84b4c76e66710@pc33.atlanta.com");

        assert_eq!(headers.len(), 4);
        assert!(!headers.is_empty());

        assert_eq!(
            headers.get(&HeaderName::From).unwrap().as_str(),
            "<sip:alice@atlanta.com>;tag=1928301774"
        );

        // Multiple Via headers
        headers.add(HeaderName::Via, "SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bKnashds8");
        assert_eq!(headers.get_all(&HeaderName::Via).len(), 2);
    }

    #[test]
    fn test_headers_set_replaces() {
        let mut headers = Headers::new();
        headers.add(HeaderName::ContentLength, "0");
        headers.set(HeaderName::ContentLength, "150");
        assert_eq!(headers.get(&HeaderName::ContentLength).unwrap().as_str(), "150");
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn test_headers_remove() {
        let mut headers = Headers::new();
        headers.add(HeaderName::Via, "SIP/2.0/UDP 10.0.0.1:5060");
        headers.add(HeaderName::From, "<sip:alice@atlanta.com>");
        headers.remove(&HeaderName::Via);
        assert!(headers.get(&HeaderName::Via).is_none());
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn test_extract_tag() {
        assert_eq!(
            extract_tag("<sip:alice@atlanta.com>;tag=1928301774"),
            Some("1928301774".to_string())
        );
        assert_eq!(extract_tag("<sip:bob@biloxi.com>"), None);
        assert_eq!(
            extract_tag("\"Alice\" <sip:alice@atlanta.com>;tag=abc123"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn test_extract_uri() {
        assert_eq!(
            extract_uri("<sip:alice@atlanta.com>;tag=1928301774"),
            Some("sip:alice@atlanta.com".to_string())
        );
        assert_eq!(
            extract_uri("\"Alice\" <sip:alice@atlanta.com>"),
            Some("sip:alice@atlanta.com".to_string())
        );
        assert_eq!(
            extract_uri("sip:bob@biloxi.com"),
            Some("sip:bob@biloxi.com".to_string())
        );
    }

    #[test]
    fn test_generate_tag() {
        let tag = generate_tag();
        assert!(!tag.is_empty());
        // Should be different each time
        let tag2 = generate_tag();
        assert_ne!(tag, tag2);
    }

    #[test]
    fn test_generate_branch() {
        let branch = generate_branch();
        assert!(branch.starts_with("z9hG4bK"));
    }

    #[test]
    fn test_header_display() {
        let header = Header::new(HeaderName::From, "<sip:alice@atlanta.com>;tag=123");
        assert_eq!(
            header.to_string(),
            "From: <sip:alice@atlanta.com>;tag=123"
        );
    }
}
