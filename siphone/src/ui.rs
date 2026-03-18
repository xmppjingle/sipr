//! Terminal UI helpers — colors, status bar, and formatted output.

use crossterm::style::Color;
use std::fmt::Write as FmtWrite;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_color_enabled(enabled: bool) {
    COLOR_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Clear the current terminal input line and move cursor to column 0.
/// Useful when async status/log output interleaves with the interactive prompt.
pub fn clear_current_line() {
    print!("\r\x1b[2K");
    let _ = io::stdout().flush();
}

fn newline() -> &'static str {
    if crossterm::terminal::is_raw_mode_enabled().unwrap_or(false) {
        "\r\n"
    } else {
        "\n"
    }
}

pub fn print_line(msg: &str) {
    print!("{msg}{}", newline());
    let _ = io::stdout().flush();
}

pub fn print_block(msg: &str) {
    let nl = newline();
    let normalized = msg.replace('\n', nl);
    print!("{normalized}");
    if !normalized.ends_with(nl) {
        print!("{nl}");
    }
    let _ = io::stdout().flush();
}

fn color_on() -> bool {
    COLOR_ENABLED.load(Ordering::Relaxed)
}

// ── Color Palette ──────────────────────────────────────────────

const C_CYAN: Color = Color::Cyan;
const C_GREEN: Color = Color::Green;
const C_YELLOW: Color = Color::Yellow;
const C_RED: Color = Color::Red;
const C_MAGENTA: Color = Color::Magenta;
const C_BLUE: Color = Color::Blue;
const C_GREY: Color = Color::DarkGrey;
const C_WHITE: Color = Color::White;

// ── Semantic Print Helpers ─────────────────────────────────────

pub fn status(msg: &str) {
    clear_current_line();
    let fg = ansi_fg(C_GREEN);
    let r = ansi_reset();
    print_line(&format!("{fg}{msg}{r}"));
}

pub fn success(msg: &str) {
    clear_current_line();
    let fg = ansi_fg(C_GREEN);
    let b = ansi_bold();
    let r = ansi_reset();
    print_line(&format!("{fg}{b}✓ {msg}{r}"));
}

pub fn warning(msg: &str) {
    clear_current_line();
    let fg = ansi_fg(C_YELLOW);
    let r = ansi_reset();
    print_line(&format!("{fg}⚠ {msg}{r}"));
}

pub fn error(msg: &str) {
    clear_current_line();
    let fg = ansi_fg(C_RED);
    let r = ansi_reset();
    print_line(&format!("{fg}✗ {msg}{r}"));
}

pub fn info(msg: &str) {
    clear_current_line();
    let fg = ansi_fg(C_GREY);
    let r = ansi_reset();
    print_line(&format!("{fg}{msg}{r}"));
}

pub fn event(msg: &str) {
    clear_current_line();
    let fg = ansi_fg(C_WHITE);
    let b = ansi_bold();
    let r = ansi_reset();
    print_line(&format!("{fg}{b}{msg}{r}"));
}

pub fn prompt() {
    let fg = ansi_fg(C_BLUE);
    let b = ansi_bold();
    let r = ansi_reset();
    print!("{fg}{b}sipr› {r}");
    let _ = io::stdout().flush();
}

// ── SIP Method / Status Coloring ──────────────────────────────

fn method_color(method: &str) -> Color {
    match method {
        "INVITE" => C_CYAN,
        "BYE" | "CANCEL" => C_RED,
        "ACK" => C_GREEN,
        "REGISTER" => C_BLUE,
        "REFER" | "NOTIFY" => C_MAGENTA,
        "PRACK" | "UPDATE" => C_YELLOW,
        _ => C_WHITE,
    }
}

fn status_color(code: u16) -> Color {
    match code {
        100..=199 => C_YELLOW,
        200..=299 => C_GREEN,
        300..=399 => C_BLUE,
        400..=499 => C_RED,
        500..=699 => Color::DarkRed,
        _ => C_WHITE,
    }
}

fn status_indicator(code: u16) -> &'static str {
    match code {
        200..=299 => "✓",
        100..=199 => "…",
        _ => "✗",
    }
}

// ── Colored SIP Message Formatting ───────────────────────────

pub fn sip_summary(
    timestamp: &str,
    source: &std::net::SocketAddr,
    dest: &std::net::SocketAddr,
    message: &sip_core::message::SipMessage,
    raw_size: usize,
) -> String {
    use sip_core::message::SipMessage;

    let mut out = String::new();
    let time_short = if timestamp.len() > 11 {
        &timestamp[11..]
    } else {
        timestamp
    };

    let _ = write!(
        out,
        "{fg_grey}{time_short}{reset} {fg_grey}{source} → {dest}{reset} ",
        fg_grey = ansi_fg(C_GREY),
        reset = ansi_reset(),
    );

    match message {
        SipMessage::Request(req) => {
            let m = req.method.to_string();
            let _ = write!(
                out,
                "{bold}{fg}▶ {m} {uri}{reset}",
                bold = ansi_bold(),
                fg = ansi_fg(method_color(&m)),
                uri = req.uri,
                reset = ansi_reset(),
            );
        }
        SipMessage::Response(res) => {
            let code = res.status.0;
            let ind = status_indicator(code);
            let _ = write!(
                out,
                "{fg}{ind} {code} {reason}{reset}",
                fg = ansi_fg(status_color(code)),
                reason = res.reason,
                reset = ansi_reset(),
            );
        }
    }

    let _ = write!(
        out,
        " {fg_grey}({raw_size}B){reset}",
        fg_grey = ansi_fg(C_GREY),
        reset = ansi_reset(),
    );

    out
}

pub fn media_flow_summary(
    timestamp: &str,
    source: &std::net::SocketAddr,
    destination: &std::net::SocketAddr,
    label: &str,
) -> String {
    let mut out = String::new();
    let time_short = if timestamp.len() > 11 {
        &timestamp[11..]
    } else {
        timestamp
    };

    let phase_upper = label.to_ascii_uppercase();
    let (badge, phase_color) = if phase_upper.contains("EARLY") {
        ("[EARLY]", C_YELLOW)
    } else if phase_upper.contains("CONNECTED") {
        ("[CONNECTED]", C_GREEN)
    } else if phase_upper.contains("RX") {
        ("[RX]", C_CYAN)
    } else if phase_upper.contains("TX") {
        ("[TX]", C_MAGENTA)
    } else {
        ("[RTP]", C_WHITE)
    };

    let _ = write!(
        out,
        "{fg_grey}{time_short}{reset} {fg_grey}{source} → {destination}{reset} {bold}{fg_rtp}[RTP]{reset} {bold}{fg_phase}{badge}{reset} {fg_phase}{label}{reset}",
        fg_grey = ansi_fg(C_GREY),
        fg_rtp = ansi_fg(C_BLUE),
        fg_phase = ansi_fg(phase_color),
        bold = ansi_bold(),
        reset = ansi_reset(),
    );
    out
}

pub fn sip_detail(
    timestamp: &str,
    source: &std::net::SocketAddr,
    dest: &std::net::SocketAddr,
    message: &sip_core::message::SipMessage,
    raw_size: usize,
) -> String {
    use sip_core::message::SipMessage;

    let b = ansi_fg(C_GREY);
    let r = ansi_reset();
    let mut out = String::new();

    let _ = writeln!(out, "{b}╔══════════════════════════════════════════════════════════════╗{r}");
    let _ = writeln!(out, "{b}║{r} {timestamp} {b}{raw_size:>6}B{r}");
    let _ = writeln!(out, "{b}║{r} {fg}{source} → {dest}{r}", fg = ansi_fg(C_GREY));
    let _ = writeln!(out, "{b}╠══════════════════════════════════════════════════════════════╣{r}");

    match message {
        SipMessage::Request(req) => {
            let m = req.method.to_string();
            let _ = writeln!(
                out,
                "{b}║{r} {bold}{fg}▶ {m} {uri} {ver}{reset}",
                bold = ansi_bold(),
                fg = ansi_fg(method_color(&m)),
                uri = req.uri,
                ver = req.version,
                reset = ansi_reset(),
            );
        }
        SipMessage::Response(res) => {
            let code = res.status.0;
            let ind = status_indicator(code);
            let _ = writeln!(
                out,
                "{b}║{r} {fg}{ind} {ver} {code} {reason}{reset}",
                fg = ansi_fg(status_color(code)),
                ver = res.version,
                reason = res.reason,
                reset = ansi_reset(),
            );
        }
    }

    let _ = writeln!(out, "{b}╟──────────────────────────────────────────────────────────────╢{r}");

    for header in message.headers().iter() {
        let _ = writeln!(
            out,
            "{b}║{r} {bold}{name}{reset}: {val}",
            bold = ansi_bold(),
            name = header.name,
            reset = ansi_reset(),
            val = header.value,
        );
    }

    if let Some(body) = message.body() {
        let _ = writeln!(out, "{b}╟──────────────────────────────────────────────────────────────╢{r}");
        for line in body.lines() {
            let _ = writeln!(out, "{b}║{r} {fg}{line}{r}", fg = ansi_fg(C_GREEN));
        }
    }

    let _ = writeln!(out, "{b}╚══════════════════════════════════════════════════════════════╝{r}");
    out
}

pub fn ladder_diagram(
    call_id: &str,
    messages: &[crate::sip_debug::CapturedMessage],
) -> String {
    use sip_core::message::SipMessage;

    if messages.is_empty() {
        return String::from("(no messages)");
    }

    let mut endpoints: Vec<std::net::SocketAddr> = Vec::new();
    for msg in messages {
        if !endpoints.contains(&msg.source) {
            endpoints.push(msg.source);
        }
        if !endpoints.contains(&msg.destination) {
            endpoints.push(msg.destination);
        }
    }

    let col_width = 24;
    let b = ansi_fg(C_GREY);
    let r = ansi_reset();
    let bold = ansi_bold();
    let mut out = String::new();

    // Header
    let _ = writeln!(out, "{b}Call-ID:{r} {bold}{call_id}{r}\n", bold = ansi_bold());
    for ep in &endpoints {
        let _ = write!(out, "{bold}{ep:^width$}{r}", width = col_width);
    }
    out.push('\n');
    for _ in &endpoints {
        let _ = write!(out, "{b}{:^width$}{r}", "│", width = col_width);
    }
    out.push('\n');

    for msg in messages {
        let src_idx = endpoints.iter().position(|e| e == &msg.source).unwrap();
        let dst_idx = endpoints.iter().position(|e| e == &msg.destination).unwrap();

        let (label, color) = match &msg.message {
            SipMessage::Request(req) => {
                let m = req.method.to_string();
                let c = method_color(&m);
                (m, c)
            }
            SipMessage::Response(res) => {
                let code = res.status.0;
                (format!("{} {}", code, res.reason), status_color(code))
            }
        };

        // Build plain arrow line (for positioning), then colorize
        let left = src_idx.min(dst_idx);
        let right = src_idx.max(dst_idx);
        let left_col = left * col_width + col_width / 2;
        let right_col = right * col_width + col_width / 2;
        let going_right = src_idx < dst_idx;

        // Label line
        let mut label_line = vec![' '; endpoints.len() * col_width];
        for (i, _) in endpoints.iter().enumerate() {
            label_line[i * col_width + col_width / 2] = '│';
        }
        let mid = (left_col + right_col) / 2;
        let label_start = mid.saturating_sub(label.len() / 2);
        for (i, ch) in label.chars().enumerate() {
            if label_start + i < label_line.len() {
                label_line[label_start + i] = ch;
            }
        }

        // Arrow line
        let mut arrow_line = vec![' '; endpoints.len() * col_width];
        for (i, _) in endpoints.iter().enumerate() {
            let center = i * col_width + col_width / 2;
            if i != src_idx && i != dst_idx {
                arrow_line[center] = '│';
            }
        }
        for pos in left_col..=right_col {
            arrow_line[pos] = '─';
        }
        if going_right {
            arrow_line[right_col] = '▶';
            arrow_line[left_col] = '├';
        } else {
            arrow_line[left_col] = '◀';
            arrow_line[right_col] = '┤';
        }

        let time_str = if msg.timestamp.len() > 11 {
            &msg.timestamp[11..]
        } else {
            &msg.timestamp
        };

        // Colorized output
        let _ = write!(
            out,
            "{fg_grey}{time_str} {r}{fg}{}{r}\n",
            label_line.iter().collect::<String>().trim_end(),
            fg_grey = ansi_fg(C_GREY),
            fg = ansi_fg(color),
        );
        let _ = write!(
            out,
            "{}  {fg}{}{r}\n",
            " ".repeat(time_str.len()),
            arrow_line.iter().collect::<String>().trim_end(),
            fg = ansi_fg(color),
        );
    }

    // Footer
    for _ in &endpoints {
        let _ = write!(out, "{b}{:^width$}{r}", "│", width = col_width);
    }
    out.push('\n');
    out
}

// ── Status Bar ────────────────────────────────────────────────

pub fn status_bar(
    duration_secs: u64,
    on_hold: bool,
    muted: bool,
    recording: bool,
    sniffing: bool,
    sniff_count: usize,
) -> String {
    let mins = duration_secs / 60;
    let secs = duration_secs % 60;

    let mut out = String::new();
    let b = ansi_bold();
    let r = ansi_reset();
    let _ = write!(out, "{b}{}[{:02}:{:02}]{r}", ansi_fg(C_WHITE), mins, secs);

    if on_hold {
        let _ = write!(out, " {}HOLD{r}", ansi_fg(C_YELLOW));
    } else {
        let _ = write!(out, " {}ACTIVE{r}", ansi_fg(C_GREEN));
    }

    if muted {
        let _ = write!(out, " {}🔇{r}", ansi_fg(C_RED));
    }

    if recording {
        let _ = write!(out, " {}● REC{r}", ansi_fg(C_RED));
    }

    if sniffing {
        let _ = write!(out, " {}📡 {sniff_count}{r}", ansi_fg(C_CYAN));
    }

    out
}

// ── Colored Help Menu ─────────────────────────────────────────

pub fn print_help() {
    let b = ansi_bold();
    let r = ansi_reset();
    let c = ansi_fg(C_CYAN);
    let g = ansi_fg(C_GREY);
    let y = ansi_fg(C_YELLOW);

    println!();
    println!("{b}{c}  Call Control{r}");
    println!("  {c}hold{r}               {g}Put call on hold{r}");
    println!("  {c}resume{r}             {g}Resume held call{r}");
    println!("  {c}transfer{r} {y}<uri>{r}     {g}Blind transfer (REFER){r}");
    println!("  {c}speed{r} {y}<0-9>{r}         {g}Transfer using configured speed dial slot{r}");
    println!("  {c}hangup{r}             {g}End the call{r}");
    println!();
    println!("{b}{c}  DTMF{r}");
    println!("  {c}dtmf{r} {y}<digits>{r}      {g}Send DTMF via RTP (RFC2833){r}");
    println!("  {c}dtmf-info{r} {y}<digits>{r} {g}Send DTMF via SIP INFO{r}");
    println!("  {c}dtmf-send{r}          {g}Flush queued DTMF now{r}");
    println!();
    println!("{b}{c}  Recording{r}");
    println!("  {c}record{r} {y}<file.wav>{r}  {g}Start recording{r}");
    println!("  {c}stop{r}               {g}Pause recording{r}");
    println!();
    println!("{b}{c}  Audio{r}");
    println!("  {c}mute{r}               {g}Silence outgoing audio{r}");
    println!("  {c}unmute{r}             {g}Resume outgoing audio{r}");
    println!();
    println!("{b}{c}  Diagnostics{r}");
    println!("  {c}stats{r}              {g}Show call statistics{r}");
    println!("  {c}sniff{r}              {g}Start SIP tracing{r}");
    println!("  {c}sniff verbose{r}      {g}Trace with full headers{r}");
    println!("  {c}sniff stop{r}         {g}Stop SIP tracing{r}");
    println!("  {c}flows{r}              {g}Show call flow diagram{r}");
    println!("  {c}Ctrl+0..9{r}          {g}Shortcut for 'speed <slot>'{r}");
    println!();
}

// ── Colored Stats Box ─────────────────────────────────────────

pub fn print_stats(
    duration_secs: u64,
    muted: bool,
    rtp_stats: Option<&rtp_core::session::SessionStats>,
    recording_active: bool,
    rec_duration_ms: Option<u64>,
    rec_frames: Option<usize>,
    sniff_active: bool,
    sniff_count: usize,
) {
    let b = ansi_bold();
    let r = ansi_reset();
    let c = ansi_fg(C_CYAN);
    let g = ansi_fg(C_GREY);
    let mins = duration_secs / 60;
    let secs = duration_secs % 60;

    println!("{c}┌─────────────────────────────┐{r}");
    println!("{c}│{r} {b}Call Statistics{r}             {c}│{r}");
    println!("{c}├─────────────────────────────┤{r}");
    println!("{c}│{r}  Duration:  {b}{:02}:{:02}{r}           {c}│{r}", mins, secs);
    println!("{c}│{r}  Muted:     {}{r}         {c}│{r}",
        if muted { format!("{}yes{}", ansi_fg(C_RED), ansi_reset()) }
        else { format!("{}no{}", ansi_fg(C_GREEN), ansi_reset()) });

    if let Some(s) = rtp_stats {
        println!("{c}│{r}  Codec:     {b}{}{r}          {c}│{r}", s.codec);
        println!("{c}│{r}  RTP local: {g}{}{r}  {c}│{r}", s.local_addr);
        println!("{c}│{r}  RTP remote:{g}{}{r}  {c}│{r}", s.remote_addr);
        println!("{c}│{r}  Packets TX:{b}{}{r}           {c}│{r}", s.packets_sent);
        println!("{c}│{r}  SSRC:      {g}0x{:08X}{r}     {c}│{r}", s.ssrc);
    }

    if let (Some(dur), Some(frames)) = (rec_duration_ms, rec_frames) {
        println!("{c}│{r}  Recording: {} ({:.1}s, {} frames) {c}│{r}",
            if recording_active { format!("{}active{}", ansi_fg(C_RED), ansi_reset()) }
            else { format!("{}paused{}", ansi_fg(C_YELLOW), ansi_reset()) },
            dur as f64 / 1000.0, frames);
    }

    println!("{c}│{r}  Sniffing:  {} ({} msgs)    {c}│{r}",
        if sniff_active { format!("{}active{}", ansi_fg(C_GREEN), ansi_reset()) }
        else { format!("{}off{}", ansi_fg(C_GREY), ansi_reset()) },
        sniff_count);
    println!("{c}└─────────────────────────────┘{r}");
}

// ── Banner ────────────────────────────────────────────────────

pub fn print_banner() {
    if !color_on() {
        return; // Skip banner in plain mode
    }
    let c = ansi_fg(C_CYAN);
    let r = ansi_reset();
    let b = ansi_bold();
    println!("{c}{b}  ┌──────────────────────────────────────┐{r}");
    println!("{c}{b}  │  📞 sipr — SIP softphone for the CLI │{r}");
    println!("{c}{b}  └──────────────────────────────────────┘{r}");
}

// ── Colored Capture Summary ───────────────────────────────────

pub fn print_capture_summary(
    total: usize,
    calls: usize,
    requests: usize,
    responses: usize,
    breakdown: &[(String, usize)],
) {
    let c = ansi_fg(C_CYAN);
    let b = ansi_bold();
    let g = ansi_fg(C_GREY);
    let r = ansi_reset();

    println!();
    println!("{c}{b}═══ Capture Summary ═══{r}");
    println!("{b}Total messages:{r} {total}");
    println!("{b}Active calls:{r}   {calls}");
    println!("{b}Requests:{r}       {requests}");
    println!("{b}Responses:{r}      {responses}");

    if !breakdown.is_empty() {
        println!();
        println!("{b}Breakdown:{r}");
        for (method, count) in breakdown {
            println!("  {g}{method:30}{r} {b}{count}{r}");
        }
    }
}

// ── Sniffer Header Box ────────────────────────────────────────

pub fn print_sniffer_header(local_addr: &std::net::SocketAddr, verbose: bool, filter: Option<&str>) {
    let c = ansi_fg(C_CYAN);
    let b = ansi_bold();
    let r = ansi_reset();
    let g = ansi_fg(C_GREY);

    println!("{c}╔════════════════════════════════════════════════════╗{r}");
    println!("{c}║{r}  {b}📡 siphone SIP Debugger / Sniffer{r}                {c}║{r}");
    println!("{c}╟────────────────────────────────────────────────────╢{r}");
    println!("{c}║{r}  Listening on: {b}{local_addr}{r}");
    println!("{c}║{r}  Mode:         {b}{}{r}",
        if verbose { "verbose (full headers)" } else { "summary" });
    if let Some(f) = filter {
        println!("{c}║{r}  Filter:       {b}{f}{r}");
    }
    println!("{c}║{r}  {g}Press Ctrl+C to stop and show call flows{r}");
    println!("{c}╚════════════════════════════════════════════════════╝{r}");
    println!();
}

// ── ANSI Helpers ──────────────────────────────────────────────

fn ansi_reset() -> &'static str {
    if color_on() { "\x1b[0m" } else { "" }
}

fn ansi_bold() -> &'static str {
    if color_on() { "\x1b[1m" } else { "" }
}

fn ansi_fg(color: Color) -> String {
    if !color_on() {
        return String::new();
    }
    match color {
        Color::Cyan => "\x1b[36m".to_string(),
        Color::Green => "\x1b[32m".to_string(),
        Color::Yellow => "\x1b[33m".to_string(),
        Color::Red => "\x1b[31m".to_string(),
        Color::Magenta => "\x1b[35m".to_string(),
        Color::Blue => "\x1b[34m".to_string(),
        Color::DarkGrey => "\x1b[90m".to_string(),
        Color::White => "\x1b[37m".to_string(),
        Color::DarkRed => "\x1b[91m".to_string(),
        _ => String::new(),
    }
}
