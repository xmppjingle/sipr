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
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="License: Apache-2.0"></a>
  <a href="https://github.com/xmppjingle/sipr/actions"><img src="https://github.com/xmppjingle/sipr/actions/workflows/ci.yml/badge.svg" alt="Build Status"></a>
  <a href="#quick-start"><img src="https://img.shields.io/badge/crates.io-unpublished-lightgrey.svg" alt="Crates.io unpublished"></a>
  <a href="#highlights"><img src="https://img.shields.io/badge/release-none%20yet-lightgrey.svg" alt="Release none yet"></a>
  <a href="#highlights"><img src="https://img.shields.io/badge/tests-220%2B-brightgreen.svg" alt="220+ tests"></a>
  <a href="#highlights"><img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-blue.svg" alt="Cross-platform"></a>
</p>

---

> **Why open a GUI when your terminal is already open?**
>
> `sipr` is a pure-Rust SIP softphone built for developers, sysadmins, and anyone who prefers `Ctrl+C` over clicking "End Call." No Overkill. No Electron. Just SIP over UDP, RTP audio, and your command line.

## Highlights

- **Zero-config calls** — dial a SIP URI directly, no registration needed
- **Digest authentication** — RFC 2617 MD5 challenge-response for 401/407, works with Ooma, Asterisk, FreeSWITCH, etc.
- **Incoming call support** — accept inbound INVITEs with the `listen` command
- **Real-time audio** — G.711 mu-law & A-law codecs at 8 kHz, plus real Opus codec at 48 kHz
- **Call hold/resume** — re-INVITE with `a=sendonly` / `a=sendrecv` SDP direction
- **Call transfer (REFER)** — blind transfers with Refer-To and NOTIFY status updates
- **PRACK** — reliable provisional responses (100rel / RFC 3262)
- **DNS SRV resolution** — automatic `_sip._udp.<domain>` server discovery and failover
- **Call recording** — save incoming audio to WAV with `--record`
- **Interactive command history** — Up/Down navigation with persistent history in `~/.sipr.history`
- **Reverse history search** — `Ctrl+R` incremental search inside in-call CLI
- **Speed dial slots** — store SIP URIs in config (`0-9`) and dial quickly with `siphone call <slot>`
- **SIP + RTP flow tracing** — `sniff`/`flows` now shows early media and connected RTP flow events
- **Cross-platform audio** — CoreAudio (macOS), ALSA (Linux), WASAPI (Windows) via [cpal](https://github.com/RustAudioGroup/cpal)
- **Jitter buffer** — handles out-of-order and delayed packets gracefully
- **220+ tests** — from codec round-trips to full end-to-end SIP scenario tests
- **~10K lines of Rust** — small, auditable, hackable

## Quick Start

```sh
# Install from source
cargo install --path siphone

# Make a call — it's that simple (no --server or --user required)
siphone call sip:echo@sip.provider.com
```

Press **`Ctrl+C`** to hang up.

## Changelog (short)

### v0.3.0

- **Fixed audio degradation over call time** — replaced hard buffer trim with adaptive clock
  recovery. When the sender RTP clock drifts faster than the audio hardware clock, 1 sample per
  frame is gently dropped (linear blend, ~0.6% rate adjust) instead of a hard discontinuous trim
  that repeated every few seconds. Symmetric low-water duplicate keeps latency bounded in both
  directions. A hard cap (80 ms) still acts as a burst safety net but trims to the 40 ms midpoint
  to leave headroom.
- **Fixed metallic artifacts from RTCP packets decoded as audio** — RTCP packets (PT 200–204) were
  parsed as RTP with payload type 72–76 and fed to the G.711 decoder, producing brief metallic
  noise. Added RFC 5761 payload type filter (PT 64–95) to discard them before decoding.
- **Fixed unbounded playback latency** — added a 4-frame (80 ms) cap to the output buffer to
  prevent the mpsc channel from filling, which previously caused silent-frame drops heard as clicks.
- **Fixed real-time audio callback blocking** — changed `Mutex::lock` (potentially blocking) to
  `try_lock` in the cpal output callback, with a local `VecDeque` that drains in one short critical
  section. Eliminated O(n) `Vec::drain` front-drain artefacts by using `VecDeque::pop_front`.
- Added persistent in-call history (`~/.sipr.history`) with Up/Down navigation.
- Added `Ctrl+R` reverse search for in-call command history.
- Added `max_history` config option (default `1000`) to cap stored history entries.
- Added speed dial slots in config (`speed_dials`) with CLI management commands.
- Added `siphone call <slot>` support to dial speed-dial entries directly.
- Added in-call shortcut `Ctrl+0..9` for quick speed-dial transfer (`speed <slot>`).
- Added RTP flow visibility in `flows`, including:
  - early media RTP announced on `183`
  - connected RTP announced on `200 OK`
  - first active RTP `RX` and `TX` events
- Improved terminal rendering in interactive mode (fixed drift/skew under raw mode).
- Updated `hangup` command to wait for BYE `200 OK` (or timeout after 3s).

## Installation

### From Source (recommended)

```sh
git clone https://github.com/xmppjingle/sipr.git
cd sipr
cargo install --path siphone
```

### Homebrew (custom tap)

Install from a Homebrew tap (after a formula is published):

```sh
brew tap <owner>/sipr
brew install siphone
```

### Ubuntu (.deb)

Install from a GitHub Release `.deb`:

```sh
wget https://github.com/xmppjingle/sipr/releases/download/vX.Y.Z/siphone_X.Y.Z_amd64.deb
sudo apt install ./siphone_X.Y.Z_amd64.deb
```

Build your own `.deb` package from source:

```sh
sudo apt-get update
sudo apt-get install -y libasound2-dev pkg-config
cargo install cargo-deb
cargo deb -p siphone
sudo apt install ./target/debian/siphone_*_amd64.deb
```

### Requirements

- Rust 1.70+
- A working audio device (speakers + optional microphone)
- libopus (for Opus codec support — installed automatically on most systems)

## Usage

### Make a Call

The server is automatically extracted from the SIP URI — no need to specify it separately:

```sh
siphone call sip:bob@sip.example.com
```

Authenticate with a SIP server that requires credentials:

```sh
siphone call sip:bob@sip.example.com --user alice --password secret
```

Or route through a specific SIP proxy:

```sh
siphone call sip:bob@example.com --server sip.proxy.com --user alice
```

Call a configured speed-dial slot:

```sh
siphone call 1
# or:
siphone dial 1
```

### Accept Incoming Calls

Listen for inbound INVITEs on a specific port:

```sh
siphone listen --port 5060
siphone listen --port 5060 --record incoming.wav --timeout 120
```

### In-Call Commands

After the call connects, use interactive commands:

```text
hold           # put the remote party on hold
resume         # resume from hold
transfer <uri> # blind transfer (REFER) to another SIP URI
dtmf 123#      # queue/send RTP RFC2833 DTMF
dtmf-info 55   # queue/send SIP INFO DTMF
dtmf-send      # flush queued DTMF immediately
dtmf-queue     # show queued DTMF count
sniff          # start SIP tracing
sniff verbose  # SIP tracing with full message details
flows          # show SIP ladder + RTP flow events (EARLY/CONNECTED/RX/TX)
speed 1        # transfer call to speed-dial slot 1 (REFER)
hangup         # send BYE and wait for 200 OK (up to 3 seconds)
```

Incoming DTMF is announced in the CLI for both RTP RFC2833 and SIP INFO.
Use **Up/Down** for history and **Ctrl+R** to search history.
Use **Ctrl+0..9** as a shortcut for `speed <slot>`.

### Config File

Generate a template and save it:

```sh
siphone config --init > ~/.config/sipr/config.json
```

Example history limit:

```json
{
  "max_history": 1000
}
```

Configure speed dials from CLI:

```sh
# Set slot 1
siphone speed-dial set 1 sip:bob@example.com

# Update slot 1 (alias of set)
siphone speed-dial update 1 sip:bob@new.example.com

# Show one slot
siphone speed-dial show 1

# List all configured slots
siphone speed-dial list

# Remove slot
siphone speed-dial remove 1
```

Or define directly in config JSON:

```json
{
  "speed_dials": {
    "1": "sip:alice@example.com",
    "2": "sip:bob@example.com"
  }
}
```

### Record a Call

Capture incoming audio to a WAV file:

```sh
siphone call sip:echo@provider.com --user alice --record conversation.wav
```

The recording is saved even if you hang up with `Ctrl+C`.

### Register with a SIP Server

Registration supports digest authentication automatically:

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
│   ├── Message parsing & serialization (INVITE, ACK, BYE, REGISTER, REFER, PRACK, …)
│   ├── Digest authentication (RFC 2617) with pure-Rust MD5
│   ├── UDP transport layer
│   ├── Dialog & transaction state machines
│   └── SDP offer/answer negotiation (including hold/resume direction)
│
├── rtp-core/     # Real-time audio engine
│   ├── RTP packet parsing & construction
│   ├── G.711 mu-law (PCMU) & A-law (PCMA) codecs
│   ├── Real Opus codec (48 kHz via audiopus)
│   ├── Adaptive jitter buffer
│   ├── Audio device abstraction (cpal backend)
│   ├── Sample rate conversion & channel mapping
│   └── WAV file recording
│
└── siphone/      # CLI application
    ├── Call management (INVITE → media → BYE)
    ├── Incoming call acceptance (UAS / listen mode)
    ├── Hold, resume, blind transfer (REFER), PRACK
    ├── DNS SRV resolution (_sip._udp.domain)
    ├── Registration with digest auth
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
3. **RTP packets** carry G.711 or Opus-encoded audio
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

## Publish to Homebrew

You can publish via your own tap, and Apache-2.0 is compatible with Homebrew/core licensing requirements.

1. Create a tap repository, for example `github.com/<owner>/homebrew-sipr`.
2. Add a formula using the helper script:

```sh
# From the sipr repo:
./scripts/update-homebrew-formula.sh v0.1.0 ../homebrew-sipr/Formula/siphone.rb
```

3. Commit and push in the tap repository:

```sh
cd ../homebrew-sipr
git add Formula/siphone.rb
git commit -m "siphone v0.1.0"
git push
```

4. Users can then install:

```sh
brew tap <owner>/sipr
brew install siphone
```

### Optional: GitHub Actions publisher

This repo includes `.github/workflows/publish-homebrew.yml` to update a tap formula automatically.

- Add a repository secret named `TAP_GITHUB_TOKEN` with push access to your tap repo.
- Run the workflow manually with:
  - `tag`: release tag (for example `v0.1.0`)
  - `tap_repo`: full tap repo name (for example `xmppjingle/homebrew-sipr`)

## Publish Ubuntu Packages

This repo includes `.github/workflows/release-deb.yml` to build and publish `.deb` packages.

- On every `v*` tag push, it:
  - builds `siphone` Debian package via `cargo deb`
  - uploads it as a workflow artifact
  - attaches it to the GitHub Release for that tag

Release example:

```sh
git tag v0.1.0
git push origin v0.1.0
```

## License

[Apache License 2.0](LICENSE).
