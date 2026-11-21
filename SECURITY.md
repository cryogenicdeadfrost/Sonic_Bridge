# Security policy

SonicBridge moves real-time audio over UDP. Treat every network interface as an exposure boundary.

## Supported deployment baseline

- Keep the receiver bound to loopback unless it needs to accept remote nodes.
- For remote nodes, use XChaCha20-Poly1305 with a private 256-bit key.
- For keyless relays, configure an allow-list of expected sender IP addresses.
- Use a private LAN or a VPN such as Tailscale. Do not publish UDP 8765 to the open Internet.
- Keep `SONIC_BRIDGE_KEY` out of shell history, command-line arguments, source control, and shared service files.
- The Raspberry Pi installer stores the key in a root-only environment file and runs the receiver as an unprivileged account.

The shared key is a pre-shared secret. It is not a user identity system or a key-exchange protocol. Relays forward encrypted packets without decrypting them, so they cannot validate the audio authentication tag themselves. Their source allow-list is therefore part of the trust boundary.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting for this repository rather than posting exploit details in a public issue. Include the affected version or commit, the deployment assumptions, a minimal reproduction, and the impact you observed.

## Audit notes

The current transport is UDP and does not provide congestion control or guaranteed delivery. Authentication protects encrypted packet contents and metadata, but it should not be mistaken for protection against traffic flooding or network-level denial of service. Keep the listener behind a trusted network boundary.
