<p align="center">
  <h1 align="center">📞 sipr</h1>
  <p align="center">
    <strong>A SIP softphone that lives in your terminal.</strong>
    <br />
    Make calls, record audio, and talk to the world — all from the command line.
  </p>
</p>

<p align="center">
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.70%2B-orange.svg" alt="Rust"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-PolyForm%20Noncommercial%201.0.0-blue.svg" alt="License: PolyForm Noncommercial 1.0.0"></a>
  <a href="https://github.com/xmppjingle/sipr/actions"><img src="https://github.com/xmppjingle/sipr/actions/workflows/ci.yml/badge.svg" alt="Build Status"></a>
  <a href="#quick-start"><img src="https://img.shields.io/badge/crates.io-unpublished-lightgrey.svg" alt="Crates.io unpublished"></a>
  <a href="#highlights"><img src="https://img.shields.io/badge/release-none%20yet-lightgrey.svg" alt="Release none yet"></a>
  <a href="#highlights"><img src="https://img.shields.io/badge/tests-130%2B-brightgreen.svg" alt="130+ tests"></a>
  <a href="#highlights"><img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-blue.svg" alt="Cross-platform"></a>
</p>

---

> **Why open a GUI when your terminal is already open?**
>
> `sipr` is a pure-Rust SIP softphone built for developers, sysadmins, and anyone who prefers `Ctrl+C` over clicking "End Call." No Overkill. No Electron. Just SIP over UDP, RTP audio, and your command line.

## Highlights

- **Zero-config calls** — dial a SIP URI directly, no registration needed
- **Real-time audio** — G.711 mu-law & A-law codecs at 8 kHz, played through your system speakers
- **Call recording** — save incoming audio to WAV with `--record`
- **Cross-platform audio** — CoreAudio (macOS), ALSA (Linux), WASAPI (Windows) via [cpal](https://github.com/RustAudioGroup/cpal)
- **Jitter buffer** — handles out-of-order and delayed packets gracefully
- **130+ tests** — from codec round-trips to full end-to-end audio fidelity checks
- **~8K lines of Rust** — small, auditable, hackable

## Quick Start

```sh
# Install from source
cargo install --path siphone

# Make a call — it's that simple (no --server or --user required)
siphone call sip:echo@sip.provider.com
```

Press **`Ctrl+C`** to hang up.

## Installation

### From Source (recommended)

```sh
git clone https://github.com/xmppjingle/sipr.git
cd sipr
cargo install --path siphone
```

### Requirements

- Rust 1.70+
- A working audio device (speakers + optional microphone)

## Usage

### Make a Call

The server is automatically extracted from the SIP URI — no need to specify it separately:

```sh
siphone call sip:bob@sip.example.com
```

Use a custom caller identity if you want:

```sh
siphone call sip:bob@sip.example.com --user alice
```

Or route through a specific SIP proxy:

```sh
siphone call sip:bob@example.com --server sip.proxy.com --user alice
```

### Record a Call

Capture incoming audio to a WAV file:

```sh
siphone call sip:echo@provider.com --user alice --record conversation.wav
```

The recording is saved even if you hang up with `Ctrl+C`.

### Send DTMF During a Call

After the call connects, use interactive commands:

```text
dtmf 123#      # queue/send RTP RFC2833 DTMF
dtmf-info 55   # queue/send SIP INFO DTMF
dtmf-send      # flush queued DTMF immediately
dtmf-queue     # show queued DTMF count
```

Incoming DTMF is announced in the CLI for both RTP RFC2833 and SIP INFO.

### Register with a SIP Server

```sh
siphone register --server sip.example.com --user alice --password secret
```

### Audio Device Management

```sh
# List all audio devices
siphone devices

# Test your speakers with a tone
siphone test-audio --duration 3

# Test mic capture + playback loop (3s default)
siphone test-mic
```

## Architecture

`sipr` is organized as a three-crate Rust workspace, keeping concerns cleanly separated:

```
sipr/
├── sip-core/     # SIP protocol engine
│   ├── Message parsing & serialization (INVITE, ACK, BYE, REGISTER, OPTIONS)
│   ├── UDP transport layer
│   ├── Dialog & transaction state machines
│   └── SDP offer/answer negotiation
│
├── rtp-core/     # Real-time audio engine
│   ├── RTP packet parsing & construction
│   ├── G.711 mu-law (PCMU) & A-law (PCMA) codecs
│   ├── Adaptive jitter buffer
│   ├── Audio device abstraction (cpal backend)
│   ├── Sample rate conversion & channel mapping
│   └── WAV file recording
│
└── siphone/      # CLI application
    ├── Call management (INVITE → media → BYE)
    ├── Registration flow
    └── Audio device enumeration & testing
```

## How It Works

```
┌─────────────┐     SIP/UDP      ┌──────────────┐
│   siphone   │ ◄──────────────► │  SIP Server   │
│   (CLI)     │                  │  / Endpoint   │
│             │     RTP/UDP      │              │
│  ┌────────┐ │ ◄──────────────► │              │
│  │ Jitter │ │                  └──────────────┘
│  │ Buffer │ │
│  └───┬────┘ │
│      │      │
│  ┌───▼────┐ │
│  │ G.711  │ │
│  │ Codec  │ │
│  └───┬────┘ │
│      │      │
│  ┌───▼────┐ │
│  │Speaker │ │  ──► WAV file (optional)
│  └────────┘ │
└─────────────┘
```

1. **SIP signaling** sets up the call (INVITE → 200 OK → ACK)
2. **SDP negotiation** agrees on codec and RTP port
3. **RTP packets** carry G.711-encoded audio at 8 kHz
4. **Jitter buffer** reorders packets and smooths playback
5. **cpal** plays decoded audio through your speakers (with automatic sample rate conversion)
6. Optionally, decoded audio is written to a **WAV file**

## Contributing

Contributions are welcome! Whether it's a bug fix, new codec, or improved documentation — open a PR.

```sh
# Run the test suite
cargo test --workspace

# Run with logging
RUST_LOG=debug siphone call sip:test@example.com --user test
```

## License

[PolyForm Noncommercial 1.0.0](LICENSE) — free to use, modify, and share for noncommercial purposes.
