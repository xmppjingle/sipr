use crate::header::HeaderName;
use crate::message::{SipMessage, SipMethod, StatusCode};
use std::time::{Duration, Instant};

/// SIP transaction timer values (RFC 3261 Section 17)
pub const T1: Duration = Duration::from_millis(500);
pub const T2: Duration = Duration::from_secs(4);
pub const T4: Duration = Duration::from_secs(5);
pub const TIMER_B: Duration = Duration::from_secs(32); // 64*T1
pub const TIMER_D: Duration = Duration::from_secs(32);
pub const TIMER_F: Duration = Duration::from_secs(32);
pub const TIMER_H: Duration = Duration::from_secs(32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionKind {
    ClientInvite,
    ClientNonInvite,
    ServerInvite,
    ServerNonInvite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionState {
    /// Initial state — request not yet sent/received
    Trying,
    /// INVITE client: 1xx received
    Proceeding,
    /// INVITE client: 2xx received (pass to TU), INVITE server: 2xx sent
    Completed,
    /// ACK sent (INVITE client) or ACK received (INVITE server)
    Confirmed,
    /// Transaction is done and can be cleaned up
    Terminated,
}

#[derive(Debug, Clone)]
pub struct SipTransaction {
    pub id: String,
    pub kind: TransactionKind,
    pub state: TransactionState,
    pub method: SipMethod,
    pub branch: String,
    pub call_id: String,
    pub original_request: Option<SipMessage>,
    pub last_response: Option<SipMessage>,
    pub retransmit_count: u32,
    pub created_at: Instant,
    pub last_retransmit: Option<Instant>,
}

impl SipTransaction {
    /// Create a new client transaction from an outgoing request
    pub fn new_client(request: &SipMessage) -> Option<Self> {
        if let SipMessage::Request(req) = request {
            let branch = Self::extract_branch(&request)?;
            let call_id = req.headers.get(&HeaderName::CallId)?.0.clone();

            let kind = if req.method == SipMethod::Invite {
                TransactionKind::ClientInvite
            } else {
                TransactionKind::ClientNonInvite
            };

            let id = format!("{}:{}", branch, req.method);

            Some(Self {
                id,
                kind,
                state: TransactionState::Trying,
                method: req.method.clone(),
                branch,
                call_id,
                original_request: Some(request.clone()),
                last_response: None,
                retransmit_count: 0,
                created_at: Instant::now(),
                last_retransmit: None,
            })
        } else {
            None
        }
    }

    /// Create a new server transaction from an incoming request
    pub fn new_server(request: &SipMessage) -> Option<Self> {
        if let SipMessage::Request(req) = request {
            let branch = Self::extract_branch(&request)?;
            let call_id = req.headers.get(&HeaderName::CallId)?.0.clone();

            let kind = if req.method == SipMethod::Invite {
                TransactionKind::ServerInvite
            } else {
                TransactionKind::ServerNonInvite
            };

            let id = format!("{}:{}", branch, req.method);

            Some(Self {
                id,
                kind,
                state: TransactionState::Trying,
                method: req.method.clone(),
                branch,
                call_id,
                original_request: Some(request.clone()),
                last_response: None,
                retransmit_count: 0,
                created_at: Instant::now(),
                last_retransmit: None,
            })
        } else {
            None
        }
    }

    /// Process an incoming response for a client transaction
    pub fn process_response(&mut self, response: &SipMessage) -> TransactionAction {
        if let SipMessage::Response(res) = response {
            match &self.kind {
                TransactionKind::ClientInvite => {
                    self.process_client_invite_response(res.status)
                }
                TransactionKind::ClientNonInvite => {
                    self.process_client_non_invite_response(res.status)
                }
                _ => TransactionAction::None,
            }
        } else {
            TransactionAction::None
        }
    }

    fn process_client_invite_response(&mut self, status: StatusCode) -> TransactionAction {
        match self.state {
            TransactionState::Trying | TransactionState::Proceeding => {
                if status.is_provisional() {
                    self.state = TransactionState::Proceeding;
                    TransactionAction::PassToTU
                } else if status.is_success() {
                    self.state = TransactionState::Terminated;
                    TransactionAction::PassToTU
                } else {
                    // 3xx-6xx
                    self.state = TransactionState::Completed;
                    TransactionAction::SendAck
                }
            }
            TransactionState::Completed => {
                // Retransmission of final response
                TransactionAction::SendAck
            }
            _ => TransactionAction::None,
        }
    }

    fn process_client_non_invite_response(&mut self, status: StatusCode) -> TransactionAction {
        match self.state {
            TransactionState::Trying | TransactionState::Proceeding => {
                if status.is_provisional() {
                    self.state = TransactionState::Proceeding;
                    TransactionAction::PassToTU
                } else {
                    self.state = TransactionState::Completed;
                    TransactionAction::PassToTU
                }
            }
            _ => TransactionAction::None,
        }
    }

    /// Process an outgoing response for a server transaction
    pub fn send_response(&mut self, response: &SipMessage) -> TransactionAction {
        if let SipMessage::Response(res) = response {
            self.last_response = Some(response.clone());

            match &self.kind {
                TransactionKind::ServerInvite => {
                    if res.status.is_provisional() {
                        self.state = TransactionState::Proceeding;
                        TransactionAction::SendResponse
                    } else if res.status.is_success() {
                        self.state = TransactionState::Terminated;
                        TransactionAction::SendResponse
                    } else {
                        self.state = TransactionState::Completed;
                        TransactionAction::SendResponse
                    }
                }
                TransactionKind::ServerNonInvite => {
                    if res.status.is_provisional() {
                        self.state = TransactionState::Proceeding;
                        TransactionAction::SendResponse
                    } else {
                        self.state = TransactionState::Completed;
                        TransactionAction::SendResponse
                    }
                }
                _ => TransactionAction::None,
            }
        } else {
            TransactionAction::None
        }
    }

    /// Check if the transaction should retransmit (for unreliable transport)
    pub fn should_retransmit(&self) -> bool {
        if self.kind != TransactionKind::ClientInvite && self.kind != TransactionKind::ClientNonInvite {
            return false;
        }

        match self.state {
            TransactionState::Trying => true,
            TransactionState::Proceeding if self.kind == TransactionKind::ClientInvite => true,
            _ => false,
        }
    }

    /// Get the next retransmit interval
    pub fn retransmit_interval(&self) -> Duration {
        let base = T1;
        let multiplier = 2u32.pow(self.retransmit_count.min(6));
        let interval = base * multiplier;

        match self.kind {
            TransactionKind::ClientInvite => interval.min(T2),
            TransactionKind::ClientNonInvite => interval.min(T2),
            _ => interval,
        }
    }

    /// Mark that a retransmission was done
    pub fn mark_retransmit(&mut self) {
        self.retransmit_count += 1;
        self.last_retransmit = Some(Instant::now());
    }

    /// Check if the transaction has timed out
    pub fn is_timed_out(&self) -> bool {
        let elapsed = self.created_at.elapsed();
        match self.kind {
            TransactionKind::ClientInvite => elapsed > TIMER_B,
            TransactionKind::ClientNonInvite => elapsed > TIMER_F,
            TransactionKind::ServerInvite => {
                self.state == TransactionState::Completed && elapsed > TIMER_H
            }
            TransactionKind::ServerNonInvite => {
                self.state == TransactionState::Completed && elapsed > Duration::from_secs(32)
            }
        }
    }

    /// Check if the transaction is in a terminal state
    pub fn is_terminated(&self) -> bool {
        self.state == TransactionState::Terminated
    }

    /// Check if this transaction matches a given message
    pub fn matches(&self, msg: &SipMessage) -> bool {
        if let Some(branch) = Self::extract_branch(msg) {
            if branch == self.branch {
                // Also check method for CANCEL matching
                if let Some((_seq, method)) = msg.cseq() {
                    return method == self.method;
                }
                return true;
            }
        }
        false
    }

    fn extract_branch(msg: &SipMessage) -> Option<String> {
        let via = msg.headers().get(&HeaderName::Via)?;
        let via_str = via.as_str();
        for param in via_str.split(';') {
            let param = param.trim();
            if let Some(branch) = param.strip_prefix("branch=") {
                return Some(branch.to_string());
            }
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionAction {
    /// No action needed
    None,
    /// Pass the message to the Transaction User (dialog layer)
    PassToTU,
    /// Send an ACK for a non-2xx final response
    SendAck,
    /// Send the response on the transport
    SendResponse,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::Headers;
    use crate::message::{SipRequest, SipResponse};

    fn make_request(method: SipMethod, branch: &str, call_id: &str) -> SipMessage {
        let mut headers = Headers::new();
        headers.add(
            HeaderName::Via,
            format!("SIP/2.0/UDP 10.0.0.1:5060;branch={}", branch),
        );
        headers.add(HeaderName::From, "<sip:alice@a.com>;tag=t1");
        headers.add(HeaderName::To, "<sip:bob@b.com>");
        headers.add(HeaderName::CallId, call_id);
        headers.add(
            HeaderName::CSeq,
            format!("1 {}", method.as_str()),
        );
        headers.add(HeaderName::ContentLength, "0");

        SipMessage::Request(SipRequest {
            method,
            uri: "sip:bob@b.com".to_string(),
            version: "SIP/2.0".to_string(),
            headers,
            body: None,
        })
    }

    fn make_response(status: StatusCode, branch: &str, call_id: &str, method: &str) -> SipMessage {
        let mut headers = Headers::new();
        headers.add(
            HeaderName::Via,
            format!("SIP/2.0/UDP 10.0.0.1:5060;branch={}", branch),
        );
        headers.add(HeaderName::From, "<sip:alice@a.com>;tag=t1");
        headers.add(HeaderName::To, "<sip:bob@b.com>;tag=t2");
        headers.add(HeaderName::CallId, call_id);
        headers.add(HeaderName::CSeq, format!("1 {}", method));
        headers.add(HeaderName::ContentLength, "0");

        SipMessage::Response(SipResponse {
            version: "SIP/2.0".to_string(),
            status,
            reason: status.reason_phrase().to_string(),
            headers,
            body: None,
        })
    }

    #[test]
    fn test_create_client_invite_transaction() {
        let req = make_request(SipMethod::Invite, "z9hG4bK776", "call-1");
        let txn = SipTransaction::new_client(&req).unwrap();

        assert_eq!(txn.kind, TransactionKind::ClientInvite);
        assert_eq!(txn.state, TransactionState::Trying);
        assert_eq!(txn.method, SipMethod::Invite);
        assert_eq!(txn.branch, "z9hG4bK776");
        assert_eq!(txn.call_id, "call-1");
    }

    #[test]
    fn test_create_client_non_invite_transaction() {
        let req = make_request(SipMethod::Register, "z9hG4bK777", "call-2");
        let txn = SipTransaction::new_client(&req).unwrap();

        assert_eq!(txn.kind, TransactionKind::ClientNonInvite);
        assert_eq!(txn.state, TransactionState::Trying);
        assert_eq!(txn.method, SipMethod::Register);
    }

    #[test]
    fn test_create_server_transaction() {
        let req = make_request(SipMethod::Invite, "z9hG4bK778", "call-3");
        let txn = SipTransaction::new_server(&req).unwrap();

        assert_eq!(txn.kind, TransactionKind::ServerInvite);
        assert_eq!(txn.state, TransactionState::Trying);
    }

    #[test]
    fn test_client_invite_provisional_response() {
        let req = make_request(SipMethod::Invite, "z9hG4bK779", "call-4");
        let mut txn = SipTransaction::new_client(&req).unwrap();

        let ringing = make_response(StatusCode::RINGING, "z9hG4bK779", "call-4", "INVITE");
        let action = txn.process_response(&ringing);

        assert_eq!(action, TransactionAction::PassToTU);
        assert_eq!(txn.state, TransactionState::Proceeding);
    }

    #[test]
    fn test_client_invite_success_response() {
        let req = make_request(SipMethod::Invite, "z9hG4bK780", "call-5");
        let mut txn = SipTransaction::new_client(&req).unwrap();

        let ok = make_response(StatusCode::OK, "z9hG4bK780", "call-5", "INVITE");
        let action = txn.process_response(&ok);

        assert_eq!(action, TransactionAction::PassToTU);
        assert_eq!(txn.state, TransactionState::Terminated);
    }

    #[test]
    fn test_client_invite_error_response() {
        let req = make_request(SipMethod::Invite, "z9hG4bK781", "call-6");
        let mut txn = SipTransaction::new_client(&req).unwrap();

        let not_found = make_response(StatusCode::NOT_FOUND, "z9hG4bK781", "call-6", "INVITE");
        let action = txn.process_response(&not_found);

        assert_eq!(action, TransactionAction::SendAck);
        assert_eq!(txn.state, TransactionState::Completed);
    }

    #[test]
    fn test_client_non_invite_success() {
        let req = make_request(SipMethod::Register, "z9hG4bK782", "call-7");
        let mut txn = SipTransaction::new_client(&req).unwrap();

        let ok = make_response(StatusCode::OK, "z9hG4bK782", "call-7", "REGISTER");
        let action = txn.process_response(&ok);

        assert_eq!(action, TransactionAction::PassToTU);
        assert_eq!(txn.state, TransactionState::Completed);
    }

    #[test]
    fn test_server_invite_provisional() {
        let req = make_request(SipMethod::Invite, "z9hG4bK783", "call-8");
        let mut txn = SipTransaction::new_server(&req).unwrap();

        let ringing = make_response(StatusCode::RINGING, "z9hG4bK783", "call-8", "INVITE");
        let action = txn.send_response(&ringing);

        assert_eq!(action, TransactionAction::SendResponse);
        assert_eq!(txn.state, TransactionState::Proceeding);
    }

    #[test]
    fn test_server_invite_success() {
        let req = make_request(SipMethod::Invite, "z9hG4bK784", "call-9");
        let mut txn = SipTransaction::new_server(&req).unwrap();

        let ok = make_response(StatusCode::OK, "z9hG4bK784", "call-9", "INVITE");
        let action = txn.send_response(&ok);

        assert_eq!(action, TransactionAction::SendResponse);
        assert_eq!(txn.state, TransactionState::Terminated);
    }

    #[test]
    fn test_transaction_matching() {
        let req = make_request(SipMethod::Invite, "z9hG4bK785", "call-10");
        let txn = SipTransaction::new_client(&req).unwrap();

        // Same branch should match
        let response = make_response(StatusCode::OK, "z9hG4bK785", "call-10", "INVITE");
        assert!(txn.matches(&response));

        // Different branch should not match
        let other = make_response(StatusCode::OK, "z9hG4bK999", "call-10", "INVITE");
        assert!(!txn.matches(&other));
    }

    #[test]
    fn test_retransmit_interval() {
        let req = make_request(SipMethod::Invite, "z9hG4bK786", "call-11");
        let mut txn = SipTransaction::new_client(&req).unwrap();

        assert_eq!(txn.retransmit_interval(), T1); // 500ms
        txn.mark_retransmit();
        assert_eq!(txn.retransmit_interval(), T1 * 2); // 1000ms
        txn.mark_retransmit();
        assert_eq!(txn.retransmit_interval(), T1 * 4); // 2000ms
        txn.mark_retransmit();
        assert_eq!(txn.retransmit_interval(), T2); // 4000ms (capped)
    }

    #[test]
    fn test_should_retransmit() {
        let req = make_request(SipMethod::Invite, "z9hG4bK787", "call-12");
        let mut txn = SipTransaction::new_client(&req).unwrap();

        assert!(txn.should_retransmit()); // Trying state

        let ringing = make_response(StatusCode::RINGING, "z9hG4bK787", "call-12", "INVITE");
        txn.process_response(&ringing);
        assert!(txn.should_retransmit()); // Proceeding state for INVITE

        let ok = make_response(StatusCode::OK, "z9hG4bK787", "call-12", "INVITE");
        txn.process_response(&ok);
        assert!(!txn.should_retransmit()); // Terminated
    }

    #[test]
    fn test_transaction_terminated() {
        let req = make_request(SipMethod::Register, "z9hG4bK788", "call-13");
        let mut txn = SipTransaction::new_client(&req).unwrap();

        assert!(!txn.is_terminated());

        let ok = make_response(StatusCode::OK, "z9hG4bK788", "call-13", "REGISTER");
        txn.process_response(&ok);

        // Non-invite goes to Completed, not Terminated
        assert!(!txn.is_terminated());
        assert_eq!(txn.state, TransactionState::Completed);
    }

    #[test]
    fn test_create_from_response_fails() {
        let response = make_response(StatusCode::OK, "z9hG4bK789", "call-14", "INVITE");
        assert!(SipTransaction::new_client(&response).is_none());
        assert!(SipTransaction::new_server(&response).is_none());
    }

    #[test]
    fn test_process_response_on_request() {
        let req = make_request(SipMethod::Invite, "z9hG4bK790", "call-15");
        let mut txn = SipTransaction::new_client(&req).unwrap();

        // Passing a request to process_response should return None action
        let action = txn.process_response(&req);
        assert_eq!(action, TransactionAction::None);
    }

    #[test]
    fn test_server_non_invite_response() {
        let req = make_request(SipMethod::Register, "z9hG4bK791", "call-16");
        let mut txn = SipTransaction::new_server(&req).unwrap();

        assert_eq!(txn.kind, TransactionKind::ServerNonInvite);

        let ok = make_response(StatusCode::OK, "z9hG4bK791", "call-16", "REGISTER");
        let action = txn.send_response(&ok);
        assert_eq!(action, TransactionAction::SendResponse);
        assert_eq!(txn.state, TransactionState::Completed);
    }
}
