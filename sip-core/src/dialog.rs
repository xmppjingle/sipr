use crate::header::{extract_tag, extract_uri, HeaderName};
use crate::message::{SipMessage, SipMethod};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogState {
    /// Dialog created, INVITE sent, waiting for response
    Early,
    /// 2xx received, dialog confirmed
    Confirmed,
    /// BYE sent or received, dialog is ending
    Terminated,
}

#[derive(Debug, Clone)]
pub struct SipDialog {
    pub call_id: String,
    pub local_tag: String,
    pub remote_tag: Option<String>,
    pub local_uri: String,
    pub remote_uri: String,
    pub remote_target: Option<String>,
    pub local_cseq: u32,
    pub remote_cseq: Option<u32>,
    pub state: DialogState,
}

impl SipDialog {
    /// Create a dialog from an outgoing INVITE request (UAC side)
    pub fn new_uac(call_id: String, local_tag: String, local_uri: String, remote_uri: String) -> Self {
        Self {
            call_id,
            local_tag,
            remote_tag: None,
            local_uri,
            remote_uri,
            remote_target: None,
            local_cseq: 1,
            remote_cseq: None,
            state: DialogState::Early,
        }
    }

    /// Create a dialog from an incoming INVITE request (UAS side)
    pub fn new_uas(
        call_id: String,
        local_tag: String,
        remote_tag: String,
        local_uri: String,
        remote_uri: String,
    ) -> Self {
        Self {
            call_id,
            local_tag,
            remote_tag: Some(remote_tag),
            local_uri,
            remote_uri,
            remote_target: None,
            local_cseq: 1,
            remote_cseq: None,
            state: DialogState::Early,
        }
    }

    /// Try to create a dialog from an incoming INVITE request
    pub fn from_invite(msg: &SipMessage) -> Option<Self> {
        if let SipMessage::Request(req) = msg {
            if req.method != SipMethod::Invite {
                return None;
            }

            let call_id = req.headers.get(&HeaderName::CallId)?.0.clone();

            let from_val = req.headers.get(&HeaderName::From)?.as_str();
            let remote_tag = extract_tag(from_val)?;
            let remote_uri = extract_uri(from_val)?;

            let to_val = req.headers.get(&HeaderName::To)?.as_str();
            let local_uri = extract_uri(to_val)?;

            let local_tag = crate::header::generate_tag();

            let contact = req
                .headers
                .get(&HeaderName::Contact)
                .and_then(|v| extract_uri(v.as_str()));

            Some(Self {
                call_id,
                local_tag,
                remote_tag: Some(remote_tag),
                local_uri,
                remote_uri,
                remote_target: contact,
                local_cseq: 1,
                remote_cseq: None,
                state: DialogState::Early,
            })
        } else {
            None
        }
    }

    /// Process an incoming response (for UAC dialogs)
    pub fn process_response(&mut self, msg: &SipMessage) -> bool {
        if let SipMessage::Response(res) = msg {
            // Verify Call-ID matches
            if let Some(call_id) = res.headers.get(&HeaderName::CallId) {
                if call_id.0 != self.call_id {
                    return false;
                }
            } else {
                return false;
            }

            // Extract remote tag from To header
            if let Some(to_val) = res.headers.get(&HeaderName::To) {
                if let Some(tag) = extract_tag(to_val.as_str()) {
                    self.remote_tag = Some(tag);
                }
            }

            // Extract remote target from Contact
            if let Some(contact) = res.headers.get(&HeaderName::Contact) {
                self.remote_target = extract_uri(contact.as_str());
            }

            // Update state based on status code
            match res.status.0 {
                100..=199 => {
                    // Provisional: dialog stays Early
                    if self.state == DialogState::Early {
                        // Already early, no change
                    }
                }
                200..=299 => {
                    self.state = DialogState::Confirmed;
                }
                300..=699 => {
                    self.state = DialogState::Terminated;
                }
                _ => {}
            }

            true
        } else {
            false
        }
    }

    /// Process an incoming BYE request
    pub fn process_bye(&mut self, msg: &SipMessage) -> bool {
        if let SipMessage::Request(req) = msg {
            if req.method == SipMethod::Bye {
                if let Some(call_id) = req.headers.get(&HeaderName::CallId) {
                    if call_id.0 == self.call_id {
                        self.state = DialogState::Terminated;
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Mark the dialog as terminated (when we send BYE)
    pub fn terminate(&mut self) {
        self.state = DialogState::Terminated;
    }

    /// Get the next CSeq number
    pub fn next_cseq(&mut self) -> u32 {
        self.local_cseq += 1;
        self.local_cseq
    }

    /// Check if a message belongs to this dialog
    pub fn matches(&self, msg: &SipMessage) -> bool {
        let headers = msg.headers();

        // Check Call-ID
        if let Some(call_id) = headers.get(&HeaderName::CallId) {
            if call_id.0 != self.call_id {
                return false;
            }
        } else {
            return false;
        }

        // Check tags
        if let Some(from_val) = headers.get(&HeaderName::From) {
            let from_tag = extract_tag(from_val.as_str());
            if let Some(to_val) = headers.get(&HeaderName::To) {
                let to_tag = extract_tag(to_val.as_str());

                // For requests coming from the remote side
                if msg.is_request() {
                    if let Some(ref rt) = self.remote_tag {
                        if from_tag.as_deref() != Some(rt.as_str()) {
                            return false;
                        }
                    }
                    if to_tag.as_deref() != Some(self.local_tag.as_str()) {
                        return false;
                    }
                } else {
                    // For responses
                    if let Some(ref rt) = self.remote_tag {
                        if to_tag.as_deref() != Some(rt.as_str())
                            && from_tag.as_deref() != Some(self.local_tag.as_str())
                        {
                            return false;
                        }
                    }
                }
            }
        }

        true
    }

    pub fn is_confirmed(&self) -> bool {
        self.state == DialogState::Confirmed
    }

    pub fn is_terminated(&self) -> bool {
        self.state == DialogState::Terminated
    }

    pub fn is_early(&self) -> bool {
        self.state == DialogState::Early
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::Headers;
    use crate::message::{SipRequest, SipResponse, StatusCode};

    fn make_invite() -> SipMessage {
        let mut headers = Headers::new();
        headers.add(
            HeaderName::Via,
            "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK776",
        );
        headers.add(
            HeaderName::From,
            "\"Alice\" <sip:alice@atlanta.com>;tag=abc123",
        );
        headers.add(HeaderName::To, "<sip:bob@biloxi.com>");
        headers.add(HeaderName::CallId, "test-call-id-12345");
        headers.add(HeaderName::CSeq, "1 INVITE");
        headers.add(HeaderName::Contact, "<sip:alice@10.0.0.1:5060>");
        headers.add(HeaderName::MaxForwards, "70");
        headers.add(HeaderName::ContentLength, "0");

        SipMessage::Request(SipRequest {
            method: SipMethod::Invite,
            uri: "sip:bob@biloxi.com".to_string(),
            version: "SIP/2.0".to_string(),
            headers,
            body: None,
        })
    }

    fn make_200_ok(call_id: &str, from_tag: &str, to_tag: &str) -> SipMessage {
        let mut headers = Headers::new();
        headers.add(
            HeaderName::Via,
            "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK776",
        );
        headers.add(
            HeaderName::From,
            format!("<sip:alice@atlanta.com>;tag={}", from_tag),
        );
        headers.add(
            HeaderName::To,
            format!("<sip:bob@biloxi.com>;tag={}", to_tag),
        );
        headers.add(HeaderName::CallId, call_id);
        headers.add(HeaderName::CSeq, "1 INVITE");
        headers.add(HeaderName::Contact, "<sip:bob@10.0.0.2:5060>");
        headers.add(HeaderName::ContentLength, "0");

        SipMessage::Response(SipResponse {
            version: "SIP/2.0".to_string(),
            status: StatusCode::OK,
            reason: "OK".to_string(),
            headers,
            body: None,
        })
    }

    fn make_bye(call_id: &str, from_tag: &str, to_tag: &str) -> SipMessage {
        let mut headers = Headers::new();
        headers.add(
            HeaderName::Via,
            "SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bKbye",
        );
        headers.add(
            HeaderName::From,
            format!("<sip:bob@biloxi.com>;tag={}", from_tag),
        );
        headers.add(
            HeaderName::To,
            format!("<sip:alice@atlanta.com>;tag={}", to_tag),
        );
        headers.add(HeaderName::CallId, call_id);
        headers.add(HeaderName::CSeq, "1 BYE");
        headers.add(HeaderName::ContentLength, "0");

        SipMessage::Request(SipRequest {
            method: SipMethod::Bye,
            uri: "sip:alice@10.0.0.1:5060".to_string(),
            version: "SIP/2.0".to_string(),
            headers,
            body: None,
        })
    }

    #[test]
    fn test_dialog_new_uac() {
        let dialog = SipDialog::new_uac(
            "call-123".to_string(),
            "tag-local".to_string(),
            "sip:alice@atlanta.com".to_string(),
            "sip:bob@biloxi.com".to_string(),
        );

        assert_eq!(dialog.state, DialogState::Early);
        assert_eq!(dialog.call_id, "call-123");
        assert_eq!(dialog.local_tag, "tag-local");
        assert!(dialog.remote_tag.is_none());
        assert!(dialog.is_early());
    }

    #[test]
    fn test_dialog_from_invite() {
        let invite = make_invite();
        let dialog = SipDialog::from_invite(&invite).unwrap();

        assert_eq!(dialog.call_id, "test-call-id-12345");
        assert_eq!(dialog.remote_tag, Some("abc123".to_string()));
        assert_eq!(dialog.remote_uri, "sip:alice@atlanta.com");
        assert_eq!(dialog.local_uri, "sip:bob@biloxi.com");
        assert_eq!(dialog.state, DialogState::Early);
    }

    #[test]
    fn test_dialog_from_invite_requires_invite_method() {
        let mut headers = Headers::new();
        headers.add(HeaderName::From, "<sip:alice@atlanta.com>;tag=abc");
        headers.add(HeaderName::To, "<sip:bob@biloxi.com>");
        headers.add(HeaderName::CallId, "test");

        let bye = SipMessage::Request(SipRequest {
            method: SipMethod::Bye,
            uri: "sip:bob@biloxi.com".to_string(),
            version: "SIP/2.0".to_string(),
            headers,
            body: None,
        });

        assert!(SipDialog::from_invite(&bye).is_none());
    }

    #[test]
    fn test_dialog_process_response_ok() {
        let mut dialog = SipDialog::new_uac(
            "test-call-id-12345".to_string(),
            "local-tag".to_string(),
            "sip:alice@atlanta.com".to_string(),
            "sip:bob@biloxi.com".to_string(),
        );

        let ok = make_200_ok("test-call-id-12345", "local-tag", "remote-tag");
        assert!(dialog.process_response(&ok));
        assert_eq!(dialog.state, DialogState::Confirmed);
        assert_eq!(dialog.remote_tag, Some("remote-tag".to_string()));
        assert!(dialog.is_confirmed());
    }

    #[test]
    fn test_dialog_process_response_wrong_callid() {
        let mut dialog = SipDialog::new_uac(
            "call-1".to_string(),
            "tag-1".to_string(),
            "sip:alice@a.com".to_string(),
            "sip:bob@b.com".to_string(),
        );

        let ok = make_200_ok("call-2", "tag-1", "tag-remote");
        assert!(!dialog.process_response(&ok));
        assert_eq!(dialog.state, DialogState::Early);
    }

    #[test]
    fn test_dialog_process_bye() {
        let mut dialog = SipDialog::new_uac(
            "call-123".to_string(),
            "local-tag".to_string(),
            "sip:alice@atlanta.com".to_string(),
            "sip:bob@biloxi.com".to_string(),
        );
        dialog.state = DialogState::Confirmed;
        dialog.remote_tag = Some("remote-tag".to_string());

        let bye = make_bye("call-123", "remote-tag", "local-tag");
        assert!(dialog.process_bye(&bye));
        assert_eq!(dialog.state, DialogState::Terminated);
        assert!(dialog.is_terminated());
    }

    #[test]
    fn test_dialog_terminate() {
        let mut dialog = SipDialog::new_uac(
            "call-1".to_string(),
            "t1".to_string(),
            "sip:a@a.com".to_string(),
            "sip:b@b.com".to_string(),
        );
        dialog.state = DialogState::Confirmed;
        dialog.terminate();
        assert_eq!(dialog.state, DialogState::Terminated);
    }

    #[test]
    fn test_dialog_next_cseq() {
        let mut dialog = SipDialog::new_uac(
            "call-1".to_string(),
            "t1".to_string(),
            "sip:a@a.com".to_string(),
            "sip:b@b.com".to_string(),
        );
        assert_eq!(dialog.next_cseq(), 2);
        assert_eq!(dialog.next_cseq(), 3);
        assert_eq!(dialog.next_cseq(), 4);
    }

    #[test]
    fn test_dialog_provisional_keeps_early() {
        let mut dialog = SipDialog::new_uac(
            "call-prov".to_string(),
            "local-tag".to_string(),
            "sip:alice@a.com".to_string(),
            "sip:bob@b.com".to_string(),
        );

        let mut headers = Headers::new();
        headers.add(HeaderName::Via, "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK1");
        headers.add(HeaderName::From, "<sip:alice@a.com>;tag=local-tag");
        headers.add(HeaderName::To, "<sip:bob@b.com>;tag=remote-tag");
        headers.add(HeaderName::CallId, "call-prov");
        headers.add(HeaderName::CSeq, "1 INVITE");
        headers.add(HeaderName::ContentLength, "0");

        let ringing = SipMessage::Response(SipResponse {
            version: "SIP/2.0".to_string(),
            status: StatusCode::RINGING,
            reason: "Ringing".to_string(),
            headers,
            body: None,
        });

        assert!(dialog.process_response(&ringing));
        assert_eq!(dialog.state, DialogState::Early);
        assert_eq!(dialog.remote_tag, Some("remote-tag".to_string()));
    }

    #[test]
    fn test_dialog_error_response_terminates() {
        let mut dialog = SipDialog::new_uac(
            "call-err".to_string(),
            "local-tag".to_string(),
            "sip:alice@a.com".to_string(),
            "sip:bob@b.com".to_string(),
        );

        let mut headers = Headers::new();
        headers.add(HeaderName::Via, "SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bK1");
        headers.add(HeaderName::From, "<sip:alice@a.com>;tag=local-tag");
        headers.add(HeaderName::To, "<sip:bob@b.com>;tag=remote-tag");
        headers.add(HeaderName::CallId, "call-err");
        headers.add(HeaderName::CSeq, "1 INVITE");
        headers.add(HeaderName::ContentLength, "0");

        let not_found = SipMessage::Response(SipResponse {
            version: "SIP/2.0".to_string(),
            status: StatusCode::NOT_FOUND,
            reason: "Not Found".to_string(),
            headers,
            body: None,
        });

        assert!(dialog.process_response(&not_found));
        assert_eq!(dialog.state, DialogState::Terminated);
    }

    #[test]
    fn test_dialog_new_uas() {
        let dialog = SipDialog::new_uas(
            "call-uas".to_string(),
            "local-tag".to_string(),
            "remote-tag".to_string(),
            "sip:bob@b.com".to_string(),
            "sip:alice@a.com".to_string(),
        );

        assert_eq!(dialog.state, DialogState::Early);
        assert_eq!(dialog.remote_tag, Some("remote-tag".to_string()));
    }
}
