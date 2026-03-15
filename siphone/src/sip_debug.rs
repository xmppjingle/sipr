//! SIP packet debugger — sngrep-style traffic inspector for the terminal.
//!
//! Captures all SIP messages on a UDP port and displays them with color-coded
//! headers, call flow arrows, and timing information.

use chrono::Local;
use sip_core::message::{SipMessage, SipMethod};
use std::net::SocketAddr;
use tokio::net::UdpSocket;

use crate::ui;

/// A captured SIP message with metadata
#[derive(Debug, Clone)]
pub struct CapturedMessage {
    pub timestamp: String,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub message: SipMessage,
    pub raw_size: usize,
}

impl CapturedMessage {
    /// One-line summary for the message list view (plain text for tests)
    pub fn summary(&self) -> String {
        let direction = format!("{} → {}", self.source, self.destination);
        let desc = match &self.message {
            SipMessage::Request(req) => {
                format!("{} {}", req.method, req.uri)
            }
            SipMessage::Response(res) => {
                format!("{} {}", res.status, res.reason)
            }
        };
        format!("{} | {:42} | {} ({}B)", self.timestamp, direction, desc, self.raw_size)
    }

    /// Colored one-line summary
    pub fn summary_colored(&self) -> String {
        ui::sip_summary(&self.timestamp, &self.source, &self.destination, &self.message, self.raw_size)
    }

    /// Colored full detail display
    pub fn detail_colored(&self) -> String {
        ui::sip_detail(&self.timestamp, &self.source, &self.destination, &self.message, self.raw_size)
    }

    /// Full formatted display of the SIP message with all headers (plain text for tests)
    pub fn detail(&self) -> String {
        let mut out = String::new();

        out.push_str(&format!("╔══════════════════════════════════════════════════════════════╗\n"));
        out.push_str(&format!("║ {} {:>6}B\n", self.timestamp, self.raw_size));
        out.push_str(&format!("║ {} → {}\n", self.source, self.destination));
        out.push_str(&format!("╠══════════════════════════════════════════════════════════════╣\n"));

        match &self.message {
            SipMessage::Request(req) => {
                out.push_str(&format!("║ ▶ {} {} {}\n", req.method, req.uri, req.version));
            }
            SipMessage::Response(res) => {
                let indicator = if res.status.is_success() {
                    "✓"
                } else if res.status.is_provisional() {
                    "…"
                } else {
                    "✗"
                };
                out.push_str(&format!("║ {} {} {} {}\n", indicator, res.version, res.status, res.reason));
            }
        }
        out.push_str(&format!("╟──────────────────────────────────────────────────────────────╢\n"));

        for header in self.message.headers().iter() {
            out.push_str(&format!("║ {}: {}\n", header.name, header.value));
        }

        if let Some(body) = self.message.body() {
            out.push_str(&format!("╟──────────────────────────────────────────────────────────────╢\n"));
            for line in body.lines() {
                out.push_str(&format!("║ {}\n", line));
            }
        }

        out.push_str(&format!("╚══════════════════════════════════════════════════════════════╝\n"));
        out
    }
}

/// A captured RTP/media flow event tied to a call
#[derive(Debug, Clone)]
pub struct CapturedMediaEvent {
    pub timestamp: String,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub label: String,
}

/// A call flow identified by Call-ID
#[derive(Debug, Clone)]
pub struct CallFlow {
    pub call_id: String,
    pub messages: Vec<CapturedMessage>,
    pub media_events: Vec<CapturedMediaEvent>,
}

impl CallFlow {
    pub fn new(call_id: String) -> Self {
        Self {
            call_id,
            messages: Vec::new(),
            media_events: Vec::new(),
        }
    }

    /// Render an ASCII call flow diagram (ladder diagram) — plain text for tests
    pub fn ladder_diagram(&self) -> String {
        if self.messages.is_empty() {
            return String::from("(no messages)");
        }

        // Collect unique endpoints
        let mut endpoints: Vec<SocketAddr> = Vec::new();
        for msg in &self.messages {
            if !endpoints.contains(&msg.source) {
                endpoints.push(msg.source);
            }
            if !endpoints.contains(&msg.destination) {
                endpoints.push(msg.destination);
            }
        }

        let col_width = 24;
        let mut out = String::new();

        // Header
        out.push_str(&format!("Call-ID: {}\n\n", self.call_id));
        for ep in &endpoints {
            out.push_str(&format!("{:^width$}", ep, width = col_width));
        }
        out.push('\n');
        for _ in &endpoints {
            out.push_str(&format!("{:^width$}", "│", width = col_width));
        }
        out.push('\n');

        // Messages
        for msg in &self.messages {
            let src_idx = endpoints.iter().position(|e| e == &msg.source).unwrap();
            let dst_idx = endpoints.iter().position(|e| e == &msg.destination).unwrap();

            let label = match &msg.message {
                SipMessage::Request(req) => format!("{}", req.method),
                SipMessage::Response(res) => format!("{} {}", res.status, res.reason),
            };

            // Build the arrow line
            let mut line = vec![' '; endpoints.len() * col_width];

            // Place vertical bars for non-participating endpoints
            for (i, _) in endpoints.iter().enumerate() {
                let center = i * col_width + col_width / 2;
                if i != src_idx && i != dst_idx {
                    line[center] = '│';
                }
            }

            let left = src_idx.min(dst_idx);
            let right = src_idx.max(dst_idx);
            let left_col = left * col_width + col_width / 2;
            let right_col = right * col_width + col_width / 2;

            // Draw arrow
            let going_right = src_idx < dst_idx;
            for pos in left_col..=right_col {
                line[pos] = '─';
            }

            if going_right {
                line[right_col] = '▶';
                line[left_col] = '├';
            } else {
                line[left_col] = '◀';
                line[right_col] = '┤';
            }

            // Place label in the middle
            let mid = (left_col + right_col) / 2;
            let label_start = mid.saturating_sub(label.len() / 2);
            // Write label above the arrow first
            let mut label_line = vec![' '; endpoints.len() * col_width];
            for (i, _) in endpoints.iter().enumerate() {
                let center = i * col_width + col_width / 2;
                label_line[center] = '│';
            }
            for (i, ch) in label.chars().enumerate() {
                if label_start + i < label_line.len() {
                    label_line[label_start + i] = ch;
                }
            }

            let time_prefix = format!("{} ", &msg.timestamp[11..]); // HH:MM:SS.mmm
            out.push_str(&time_prefix);
            out.push_str(&label_line.iter().collect::<String>().trim_end().to_string());
            out.push('\n');
            out.push_str(&" ".repeat(time_prefix.len()));
            out.push_str(&line.iter().collect::<String>().trim_end().to_string());
            out.push('\n');
        }

        // Footer
        for _ in &endpoints {
            out.push_str(&format!("{:^width$}", "│", width = col_width));
        }
        out.push('\n');

        out
    }

    /// Render a colored call flow diagram
    pub fn ladder_diagram_colored(&self) -> String {
        ui::ladder_diagram(&self.call_id, &self.messages)
    }
}

/// SIP traffic sniffer/debugger
pub struct SipDebugger {
    calls: Vec<CallFlow>,
    messages: Vec<CapturedMessage>,
    verbose: bool,
    active: bool,
}

impl SipDebugger {
    pub fn new(verbose: bool) -> Self {
        Self {
            calls: Vec::new(),
            messages: Vec::new(),
            verbose,
            active: false,
        }
    }

    /// Check if sniffing is active
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Start sniffing
    pub fn start(&mut self, verbose: bool) {
        self.verbose = verbose;
        self.active = true;
    }

    /// Stop sniffing
    pub fn stop(&mut self) {
        self.active = false;
    }

    /// Capture a SIP message from the call's transport layer.
    pub fn capture_incoming(&mut self, message: &SipMessage, source: SocketAddr, local_addr: SocketAddr) {
        if !self.active {
            return;
        }

        let raw_size = message.to_bytes().len();
        let captured = CapturedMessage {
            timestamp: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            source,
            destination: local_addr,
            message: message.clone(),
            raw_size,
        };
        self.process(captured);
    }

    /// Capture an outgoing SIP message.
    pub fn capture_outgoing(&mut self, message: &SipMessage, local_addr: SocketAddr, destination: SocketAddr) {
        if !self.active {
            return;
        }

        let raw_size = message.to_bytes().len();
        let captured = CapturedMessage {
            timestamp: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            source: local_addr,
            destination,
            message: message.clone(),
            raw_size,
        };
        self.process(captured);
    }

    /// Process a captured SIP message
    pub fn process(&mut self, msg: CapturedMessage) {
        ui::clear_current_line();
        // Print colored summary
        ui::print_line(&msg.summary_colored());

        // Print full detail in verbose mode
        if self.verbose {
            ui::clear_current_line();
            ui::print_block(&msg.detail_colored());
        }

        // Track by call-id
        if let Some(call_id) = msg.message.call_id() {
            if let Some(flow) = self.calls.iter_mut().find(|f| f.call_id == call_id) {
                flow.messages.push(msg.clone());
            } else {
                let mut flow = CallFlow::new(call_id);
                flow.messages.push(msg.clone());
                self.calls.push(flow);
            }
        }

        self.messages.push(msg);
    }

    /// Capture RTP/media flow information (early media and connected call media)
    pub fn capture_rtp_event(
        &mut self,
        call_id: &str,
        source: SocketAddr,
        destination: SocketAddr,
        label: &str,
    ) {
        if !self.active {
            return;
        }

        let event = CapturedMediaEvent {
            timestamp: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            source,
            destination,
            label: label.to_string(),
        };

        ui::print_line(&ui::media_flow_summary(
            &event.timestamp,
            &event.source,
            &event.destination,
            &event.label,
        ));

        if let Some(flow) = self.calls.iter_mut().find(|f| f.call_id == call_id) {
            flow.media_events.push(event);
        } else {
            let mut flow = CallFlow::new(call_id.to_string());
            flow.media_events.push(event);
            self.calls.push(flow);
        }
    }

    /// Print colored call flow diagrams for all tracked calls
    pub fn print_flows(&self) {
        if self.calls.is_empty() {
            ui::clear_current_line();
            ui::print_line("No calls captured.");
            return;
        }
        for flow in &self.calls {
            ui::clear_current_line();
            if !flow.messages.is_empty() {
                ui::print_block(&format!("\n{}", flow.ladder_diagram_colored()));
            } else {
                ui::print_block(&format!("\nCall-ID: {}", flow.call_id));
            }
            if !flow.media_events.is_empty() {
                ui::print_line("RTP/media flow:");
                for ev in &flow.media_events {
                    ui::print_line(&format!(
                        "  {}",
                        ui::media_flow_summary(
                            &ev.timestamp,
                            &ev.source,
                            &ev.destination,
                            &ev.label,
                        )
                    ));
                }
            }
        }
    }

    /// Print a colored summary table
    pub fn print_summary(&self) {
        let requests = self.messages.iter().filter(|m| m.message.is_request()).count();
        let responses = self.messages.iter().filter(|m| m.message.is_response()).count();

        let mut methods: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for msg in &self.messages {
            let key = match &msg.message {
                SipMessage::Request(req) => req.method.to_string(),
                SipMessage::Response(res) => format!("{} {}", res.status, res.reason),
            };
            *methods.entry(key).or_insert(0) += 1;
        }

        let mut sorted: Vec<_> = methods.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));

        ui::print_capture_summary(
            self.messages.len(),
            self.calls.len(),
            requests,
            responses,
            &sorted,
        );
    }

    /// Total captured messages count
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Total captured RTP/media events count
    pub fn media_event_count(&self) -> usize {
        self.calls.iter().map(|c| c.media_events.len()).sum()
    }

    /// True if any SIP or RTP/media capture exists
    pub fn has_captures(&self) -> bool {
        self.message_count() > 0 || self.media_event_count() > 0
    }
}

/// Run the SIP sniffer on a given port
pub async fn run_sniffer(
    port: u16,
    verbose: bool,
    filter_method: Option<SipMethod>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = format!("0.0.0.0:{}", port);
    let socket = UdpSocket::bind(&bind_addr).await?;
    let local_addr = socket.local_addr()?;

    ui::print_sniffer_header(
        &local_addr,
        verbose,
        filter_method.as_ref().map(|m| m.as_str()),
    );

    let mut debugger = SipDebugger::new(verbose);
    let mut buf = vec![0u8; 65535];

    // Handle Ctrl+C gracefully
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::channel::<()>(1);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop_tx.send(()).await;
    });

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, source)) => {
                        let data = String::from_utf8_lossy(&buf[..len]);
                        match SipMessage::parse(&data) {
                            Ok(message) => {
                                // Apply method filter
                                if let Some(ref filter) = filter_method {
                                    if let SipMessage::Request(ref req) = message {
                                        if &req.method != filter {
                                            continue;
                                        }
                                    }
                                    // Show responses for filtered methods too (by CSeq)
                                    if let SipMessage::Response(_) = message {
                                        if let Some((_, cseq_method)) = message.cseq() {
                                            if &cseq_method != filter {
                                                continue;
                                            }
                                        }
                                    }
                                }

                                let captured = CapturedMessage {
                                    timestamp: Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                                    source,
                                    destination: local_addr,
                                    raw_size: len,
                                    message,
                                };
                                debugger.process(captured);
                            }
                            Err(_) => {
                                // Not a SIP message, show as unknown if verbose
                                if verbose {
                                    ui::info(&format!("[{}] Non-SIP: {} bytes from {}",
                                        Local::now().format("%H:%M:%S%.3f"),
                                        len, source));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        ui::error(&format!("Receive error: {}", e));
                        break;
                    }
                }
            }
            _ = stop_rx.recv() => {
                break;
            }
        }
    }

    println!();
    debugger.print_summary();
    debugger.print_flows();

    Ok(())
}
