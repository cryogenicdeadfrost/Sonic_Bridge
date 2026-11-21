# SonicBridge

> Your PC makes the noise. The Raspberry Pi gets the Bluetooth job.

SonicBridge is a small Rust audio relay for the slightly ridiculous problem of wanting a Windows PC to play through Bluetooth headphones or speakers attached to a Raspberry Pi.

Version 0.3 keeps the original idea, but turns it into an actual platform:

- CLI-first daemons for unattended machines
- a native Rust GUI for humans who would rather click a button
- native system-audio loopback capture
- Opus over UDP
- configurable jitter buffering
- multi-Pi forwarding
- TTL protection against relay loops
- one receiver binary that can be a player, relay, or both
- optional authenticated audio transport with XChaCha20-Poly1305
- receiver authentication-failure and jitter statistics

The core remains deliberately boring. That is a compliment.

## The architecture

    Windows PC
    ┌──────────────────────┐
    │ SonicBridge Client   │
    │ native system audio  │
    │ loopback             │
    └──────────┬───────────┘
               │ Opus / UDP
               ▼
    ┌──────────────────────┐
    │ Raspberry Pi #1      │
    │ receiver + relay     │
    └──────────┬───────────┘
               │ Opus / UDP
               ▼
    ┌──────────────────────┐
    │ Raspberry Pi #2      │
    │ receiver + relay     │
    └──────────┬───────────┘
               │
               ▼
    ┌──────────────────────┐
    │ Raspberry Pi #3      │
    │ receiver             │
    │ PipeWire             │
    │       │              │
    │       ▼              │
    │ Bluetooth A2DP       │
    └───────┬──────────────┘
            ▼
       OnePlus Buds
       / speaker

The Pi does not magically increase the electrical volume of the original signal. It acts as a networked audio middleman and can retransmit the stream to another node.

That distinction matters. Otherwise we would be selling networking as a power amplifier.

## Why Rust?

The entire application remains Rust.

The GUI uses egui/eframe, giving us a native desktop interface without dragging a browser runtime behind it.

The audio path uses flexaudio for system-output capture. It provides a Rust API for system loopback on Windows through WASAPI and Linux through PipeWire, while normalizing captured audio into a predictable stream.

Bluetooth itself is intentionally left to the operating system:

    SonicBridge
         │
         ▼
      PipeWire
         │
         ▼
        BlueZ
         │
         ▼
    Bluetooth A2DP
         │
         ▼
    Earbuds / speaker

That is the sane division of labour. SonicBridge transports audio. PipeWire manages the audio graph. BlueZ manages Linux Bluetooth.

For Linux Bluetooth control and discovery, the project can later expose BlueZ through D-Bus without making the actual audio transport depend on raw Bluetooth packet handling.

## CLI and GUI are both first-class

The CLI is for machines.

    sonic-bridge-client --target 100.64.0.10:8765

    sonic-bridge-server --listen 0.0.0.0:8765

The GUI is for humans.

    cargo run --release -p sonic-bridge-ui

The GUI exposes the settings that actually matter:

- client target
- Opus bitrate
- packet TTL
- receiver listen address
- next relay
- jitter-buffer size
- relay-only mode
- optional 256-bit transport key

The GUI does not replace the daemon. It controls it.

That keeps the Pi installation tiny while still giving a desktop user an easy control surface.

## Running the client from a terminal

The command-line client is a first-class way to run SonicBridge. It does not need the desktop app.

PowerShell:

```powershell
$env:SONIC_BRIDGE_TARGET = "100.64.0.10:8765"
$env:SONIC_BRIDGE_KEY = "<64 hex characters>"
.\target\release\sonic-bridge-client.exe --bitrate 96000 --ttl 8
```

Windows Command Prompt (cmd.exe):

```bat
set "SONIC_BRIDGE_TARGET=100.64.0.10:8765"
set "SONIC_BRIDGE_KEY=<64 hex characters>"
target\release\sonic-bridge-client.exe --bitrate 96000 --ttl 8
```

Linux / macOS shell:

```sh
SONIC_BRIDGE_TARGET=100.64.0.10:8765 \
SONIC_BRIDGE_KEY="<64 hex characters>" \
./target/release/sonic-bridge-client --bitrate 96000 --ttl 8
```

You can also pass `--target` and `--key-hex` directly. Prefer environment variables for secrets so the key does not appear in the process command line. Run `sonic-bridge-client --help` for the full option list. Press Ctrl+C to stop cleanly; the client prints a packet count before exiting.

The client assembles capture chunks into exact 20 ms Opus frames. Audio APIs do not promise that every callback lands on a codec-frame boundary, so assuming they do is a good way to make tiny, mysterious dropouts.

## Multi-Pi chaining

Every Pi runs the same receiver binary.

A simple chain looks like:

    PC
     │
     ▼
    Pi #1
     │
     ▼
    Pi #2
     │
     ▼
    Pi #3
     │
     ▼
    Bluetooth sink

Pi #1:

    sonic-bridge-server \
      --listen 0.0.0.0:8765 \
      --forward-to 100.64.0.11:8765 \
      --relay-only

Pi #2:

    sonic-bridge-server \
      --listen 0.0.0.0:8765 \
      --forward-to 100.64.0.12:8765 \
      --relay-only

Pi #3:

    sonic-bridge-server --listen 0.0.0.0:8765

The client sends to Pi #1.

Each relay decrements the packet TTL. If the TTL reaches zero, the packet is not forwarded.

This is deliberately primitive. Primitive is good when the alternative is debugging a five-node audio feedback loop at 2 AM.

## Secure transport

Version 0.3 adds optional authenticated encryption for the audio payload.

The primitive is XChaCha20-Poly1305 from RustCrypto. It is a pure-Rust AEAD implementation with a 192-bit nonce, which gives us a large nonce space without making the Pi run a heavyweight crypto stack.

Enable it on the client:

    sonic-bridge-client \
      --target 100.64.0.10:8765 \
      --key-hex 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

and configure the same 64-hex-character key on the receiver:

    sonic-bridge-server \
      --listen 0.0.0.0:8765 \
      --key-hex 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

The GUI exposes the same key as a masked field and passes it to the daemon through its environment rather than placing it in the child process command line.

A relay does not need the key. It forwards the authenticated ciphertext and only changes the packet TTL. The audio-format fields, sequence number, flags, protocol version, and nonce are authenticated as associated data. TTL is deliberately excluded because each relay must decrement it. This means the chain can remain:

    PC -> encrypted Pi #1 -> encrypted Pi #2 -> encrypted Pi #3 -> Bluetooth

Without a configured key, v0.3 retains plaintext operation for local development and compatibility testing.

The key can be supplied through `--key-hex` or `SONIC_BRIDGE_KEY`. Prefer the environment variable for interactive use so the secret is not visible in the process command line. Do not put a real key in a committed README, shell history, or public configuration file.

The v0.3 packet format is not wire-compatible with v0.2. Upgrade the client, every relay, and the final receiver together.

## Runtime statistics

The receiver now reports:

    rx
    forwarded
    concealed
    dropped
    auth_failed
    jitter

Example:

    stats: rx=1200 forwarded=1180 concealed=4 dropped=2 auth_failed=0 jitter=3

This gives the CLI a useful low-overhead health signal without introducing a database, telemetry service, or giant monitoring stack.

## Packet format

Version 0.3 uses a 43-byte header and carries an explicit encryption flag plus a 24-byte XChaCha20 nonce.

    magic(4)
    version(1)
    channels(1)
    frame_ms(1)
    ttl(1)
    flags(1)
    sequence(4)
    sample_rate(4)
    nonce(24)
    payload_len(2)
    payload(...)

Audio remains:

    48 kHz
    stereo
    20 ms
    960 frames/channel
    Opus

The packet sequence number lets the receiver detect gaps.

The TTL lets multiple SonicBridge nodes forward packets without creating immortal traffic.

## Jitter buffering

The first version delivered packets almost immediately.

That is nice until Wi-Fi decides that packet 381 should take the scenic route.

Version 0.3 adds a configurable packet-depth jitter buffer. The default target is 60 ms. Its memory is bounded: the target depth is capped at 64 frames and the queue has at most eight additional frames of headroom. A large sequence jump resets the buffer near the new stream position instead of allowing packets to accumulate indefinitely.

The receiver can trade latency for resilience:

    sonic-bridge-server --jitter-ms 20

or:

    sonic-bridge-server --jitter-ms 120

For a stable LAN, keep it small.

For a noisy wireless/Tailscale path, give the receiver some breathing room.

## Windows capture

The client now uses the native system-output capture path exposed through flexaudio.

That is important because the original implementation treated CPAL's normal input-device API as if it automatically meant WASAPI loopback. It was a clever assumption. It was also the sort of assumption that eventually bites you.

Version 0.3 explicitly asks for system-output capture.

The resulting pipeline is:

    Windows system audio
            │
            ▼
    native loopback
            │
            ▼
    48 kHz stereo f32
            │
            ▼
    20 ms frame
            │
            ▼
    Opus
            │
            ▼
    UDP

## Raspberry Pi audio

The Pi does not need to understand Bluetooth packets.

Pair the earbuds normally using BlueZ and select the Bluetooth A2DP sink through PipeWire.

For example:

    wpctl status
    wpctl set-default <bluetooth-sink-id>

SonicBridge then writes decoded PCM into the normal Linux audio output path.

## Build

Install a current Rust toolchain.

Build everything:

    cargo build --release

Build only the Windows client:

    cargo build --release -p sonic-bridge-client

Build the Pi receiver:

    cargo build --release -p sonic-bridge-server

Build the GUI:

    cargo build --release -p sonic-bridge-ui

## Persistent Pi installation

The existing installer can install the receiver as a systemd service.

    chmod +x scripts/install-pi.sh
    ./scripts/install-pi.sh ./target/release/sonic-bridge-server

For a relay, configure the service with the forwarding options shown above.

The service is designed to restart after failure and start after networking and the audio/Bluetooth stack are available.

## Persistent Windows installation

    Set-ExecutionPolicy -Scope Process Bypass
    .\scripts\install-windows.ps1 -Target 100.64.0.10:8765

The current installer uses a Windows Scheduled Task at logon.

The next step is a proper tray/service split so the GUI can remain separate from the persistent audio worker.

## Tailscale

SonicBridge works fine over Tailscale.

    University PC
          │
          │ Tailscale
          ▼
    Home Pi #1
          │
          ▼
    Home Pi #2
          │
          ▼
    Bluetooth

Use the Pi's Tailscale address as the client target.

Do not expose UDP 8765 directly to the public Internet.

## Memory philosophy

The release profile favours runtime throughput over the smallest possible executable. Thin LTO keeps the optimizer useful without making every release build needlessly expensive.

The system is intentionally hostile to runaway buffering.

There is no:

- database
- media library
- browser runtime
- unbounded queue
- giant audio history
- "we'll clean it up later" allocator

Audio is processed in small fixed-duration chunks.

Queues are bounded.

Old audio is disposable.

If the network is behind, the system should lose audio rather than slowly transform a 20 ms delay into a 20 second delay.

## Current roadmap

### 0.2

- [x] Native Rust GUI
- [x] Windows system-output loopback through flexaudio
- [x] Opus transport
- [x] Configurable jitter buffer
- [x] Multi-Pi forwarding
- [x] TTL loop protection
- [x] CLI + GUI control paths
- [x] Relay-only mode

### 0.3

- [x] Optional XChaCha20-Poly1305 authenticated audio payloads
- [x] Masked encryption-key field in the GUI
- [x] Authentication-failure counters
- [x] Runtime receiver statistics
- [x] Relay forwarding of encrypted packets without decrypting them
- [x] Authenticated packet metadata (TTL excluded for relay mutation)
- [x] Bounded jitter-buffer capacity and sequence-wrap handling
- [x] Tests for packet authentication, relay TTL, and jitter ordering
- [ ] BlueZ device discovery in the GUI
- [ ] Automatic PipeWire Bluetooth sink selection
- [ ] Latency and packet-loss dashboard
- [ ] Automatic reconnect state machine
- [ ] Persistent TOML configuration
- [ ] GUI-managed chain topology
- [ ] Per-node health checks
- [ ] Better Windows startup/service integration

### 0.4

- [ ] Adaptive jitter buffering
- [ ] Forward-error correction experiments
- [ ] Multiple source clients
- [ ] Synchronised multi-speaker output
- [ ] Audio gain/DSP controls
- [ ] Remote node management
- [ ] Optional WebSocket control plane

## The important bit

SonicBridge is still not trying to become a media server, Bluetooth stack, or distributed Spotify clone.

It has a simpler job:

**take the audio your computer is already producing, move it across a network, let one or more Raspberry Pis relay it, and finally hand it to the Linux Bluetooth stack.**

The CLI keeps the system usable as a headless daemon.

The GUI makes the same system usable by an actual human.

Both talk to the same underlying audio pipeline.

That is the platform.

## License

MIT
