# SonicBridge

> Your PC makes the noise. The Raspberry Pi gets the Bluetooth job.

SonicBridge is a tiny Rust audio relay for the slightly ridiculous problem of wanting your Windows PC to play audio through Bluetooth headphones that are paired to a Raspberry Pi.

It has two jobs:

1. **Windows client** captures the audio being played by Windows.
2. **Linux/Pi server** receives that audio and hands it to the Pi's normal audio stack.

So the Pi becomes a Bluetooth middleman instead of a very expensive paperweight.

## The idea

~~~text
             LAN / Tailscale
Windows PC                         Raspberry Pi
┌─────────────────┐              ┌──────────────────┐
│ Windows audio   │              │ SonicBridge      │
│       │         │              │      │           │
│ WASAPI loopback │── Opus/UDP ─▶│ Opus decoder     │
│       │         │              │      │           │
│ Opus encoder    │              │ PipeWire / ALSA  │
└─────────────────┘              │      │           │
                                 │ Bluetooth A2DP   │
                                 └──────┼───────────┘
                                        ▼
                                  OnePlus Buds
                                  or any BT sink
~~~

We do **not** intercept Bluetooth packets on Windows. That would be an unnecessarily painful way to reinvent half of the Bluetooth stack.

Windows already has the decoded audio. We grab that, compress it, send it over the network, and let Linux handle Bluetooth.

## Why Rust?

Because this thing is supposed to sit in the background and behave itself.

Rust gives SonicBridge:

- a small native executable
- predictable memory usage
- no garbage collector
- bounded queues
- easy Linux + Windows deployment
- very little runtime baggage

In other words: it should consume less RAM than the browser tab you forgot was open.

## Current design

The first release intentionally keeps the protocol boring.

- 48 kHz
- stereo
- 20 ms audio frames
- Opus compression
- UDP transport
- sequence numbers
- Opus packet-loss concealment
- bounded queues
- Windows logon startup
- Raspberry Pi systemd startup

The audio path is:

~~~text
Windows audio
     │
     ▼
WASAPI loopback
     │
     ▼
20 ms PCM frame
     │
     ▼
Opus
     │
     ▼
UDP packet
     │
     ▼
Raspberry Pi
     │
     ▼
Opus
     │
     ▼
PipeWire / ALSA
     │
     ▼
Bluetooth
~~~

UDP is intentional. If packet 417 disappears, we would rather lose roughly 20 ms of audio than wait around for packet 417 like it owes us money.

## Repository

~~~text
Sonic_Bridge/
├── client/                  # Windows sender
│   └── src/main.rs
├── server/                  # Linux / Raspberry Pi receiver
│   └── src/main.rs
├── shared/                  # Packet format + audio constants
│   └── src/lib.rs
├── scripts/
│   ├── install-pi.sh
│   └── install-windows.ps1
├── Cargo.toml
├── README.md
├── LICENSE
└── .gitignore
~~~

## Build

You need a reasonably recent Rust toolchain.

### Windows

Build the client:

~~~powershell
cargo build --release -p sonic-bridge-client
~~~

Run it:

~~~powershell
.\target\release\sonic-bridge-client.exe --target 192.168.1.50:8765
~~~

Replace the address with the Raspberry Pi address.

If the Pi is reachable through Tailscale, use its Tailscale address instead. SonicBridge doesn't particularly care how the packets get there. It just wants them to arrive.

### Raspberry Pi / Linux

Build the server:

~~~bash
cargo build --release -p sonic-bridge-server
~~~

Run it:

~~~bash
./target/release/sonic-bridge-server --listen 0.0.0.0:8765
~~~

The Pi should have its Bluetooth headphones paired and selected as the normal Linux audio output.

On PipeWire systems:

~~~bash
wpctl status
wpctl set-default <bluetooth-sink-id>
~~~

Then SonicBridge writes into that normal output path.

## Make it persistent

### Raspberry Pi

Install the receiver as a systemd service:

~~~bash
chmod +x scripts/install-pi.sh
./scripts/install-pi.sh ./target/release/sonic-bridge-server
~~~

It will restart if the process crashes.

### Windows

Install the client as a Windows Scheduled Task:

~~~powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\install-windows.ps1 -Target 192.168.1.50:8765
~~~

Now the client starts at logon instead of requiring you to remember that this tiny audio goblin exists.

## Remote mode with Tailscale

If your PC and Pi are on different networks but both are connected through Tailscale:

~~~text
University / wherever

Windows PC
    │
    │ Tailscale
    ▼
Raspberry Pi at home
    │
    │ Bluetooth
    ▼
Earbuds
~~~

Run the Windows client against the Pi's Tailscale address:

~~~powershell
sonic-bridge-client.exe --target <pi-tailscale-ip>:8765
~~~

Do **not** expose port 8765 directly to the public Internet.

## Memory and latency

The design uses fixed-size audio frames and bounded channels.

There is no:

- database
- media library
- GUI framework
- browser runtime
- unbounded audio queue
- "let's just store everything in a Vec and see what happens" philosophy

The intended steady-state memory footprint is small, although the exact number depends on the Rust allocator, CPAL backend, PipeWire/ALSA stack, and OS.

The current version favours simplicity over ultra-low latency. The next serious step is a proper jitter buffer with measured latency and packet-loss statistics.

## Security

The first protocol is designed for a trusted private network.

SonicBridge currently provides transport integrity through the surrounding private network rather than pretending that a bare UDP listener is an authentication system.

For remote use, Tailscale is strongly preferred.

Future versions can add authenticated encryption at the packet layer.

## Troubleshooting

### No sound on the Pi

Check the normal Linux output first:

~~~bash
wpctl status
~~~

Then make sure the Bluetooth device is the default sink.

### Client says the format is unsupported

The first release expects:

~~~text
48,000 Hz
2 channels
~~~

If your Windows endpoint is running at another rate, change the Windows output format or use a compatible endpoint.

### Audio cuts out

Check the network first.

For Tailscale:

~~~bash
tailscale ping <pi>
~~~

Also remember that UDP intentionally does not retransmit old audio.

### High latency

Congratulations, you've discovered the part where networking meets audio engineering.

The current version uses 20 ms frames and bounded buffering. A future jitter-buffer implementation will make this much easier to tune.

## Roadmap

- [x] Windows system-audio capture
- [x] Opus audio encoding
- [x] UDP transport
- [x] Raspberry Pi/Linux receiver
- [x] Sequence numbers
- [x] Packet-loss concealment
- [x] Windows startup task
- [x] systemd receiver
- [ ] Automatic device selection
- [ ] Config file
- [ ] Authentication / encryption
- [ ] Jitter buffer
- [ ] Latency statistics
- [ ] Reconnect state machine
- [ ] Windows tray/status app
- [ ] Better device discovery
- [ ] Cross-platform client support

## The important bit

SonicBridge is deliberately not trying to become a media server, a Bluetooth stack, or a distributed Spotify clone.

It does one thing:

**take the audio your computer is already playing and get it to a Bluetooth speaker/headset attached to another machine.**

If it can do that quietly for months without demanding attention, we've won.

## License

MIT
