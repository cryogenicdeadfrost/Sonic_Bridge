# Security review notes

**Review date:** 21 November 2026  
**Scope:** Rust client, receiver/relay, shared packet code, desktop UI, install scripts, dependency configuration, and CI workflows.

This is a source-level review with automated checks. It is not a penetration test, a formal cryptographic review, or proof that every possible defect has been found. The fixes below are split across the companion pull requests so each area can be reviewed on its own.

## Findings and fixes

| Finding | Risk | Change |
|---|---|---|
| The receiver listened on every interface by default and allowed plaintext packets when no key was configured. | An unintended network exposure could accept arbitrary audio packets. | PR #15 defaults to loopback. A non-loopback listener now needs a pre-shared key, a source-IP allow-list, or an explicit insecure opt-in. |
| A relay or forwarding node cannot validate ciphertext authentication while forwarding blindly. A PSK on that node does not authenticate the sender if the node never decrypts the packet. | Any reachable host could inject traffic into the relay path, including when the relay has a configured but unused key. | PR #15 requires an allowed-peer list for exposed relay/forwarding nodes unless the operator explicitly opts into insecure exposure. This is an IP filter, not cryptographic identity. |
| The Pi system service ran with root privileges. | A receiver compromise would have unnecessarily broad local privileges. | PR #15 installs a dedicated `sonicbridge` account, grants only audio-group access, stores configuration in a root-only environment file, and applies systemd service hardening. |
| Packet encoding could cast an oversized payload length to a 16-bit field. The decoder also lacked explicit protocol payload and TTL ceilings. | Malformed or over-sized packets could be constructed or consume unnecessary receive-side work. | PR #14 validates flags, payload size, TTL, and encoded length. The receiver buffer is sized to the largest valid v0.3 datagram, with regression tests. |
| Audio capture chunks were treated as if each callback always contained exactly one codec frame. | Non-aligned callback sizes could be discarded, causing dropouts. | PR #13 carries samples across callback boundaries and tests split-frame and multi-frame input. |
| The desktop UI could inherit a transport key from its parent process even when the key field was blank. | An unexpected shell environment value could be passed to the worker. | PR #12 removes the inherited key before setting an explicitly supplied value. The key field is masked. |

## Residual risks

- UDP does not provide congestion control, delivery guarantees, or protection from traffic floods. Keep the receiver behind a private LAN or VPN. A valid source address or a valid key does not prevent denial-of-service traffic.
- The pre-shared key is manually distributed. There is no user identity, key exchange, or automated rotation protocol. Protect it as a secret and prefer `SONIC_BRIDGE_KEY` over `--key-hex`, which can be visible in process arguments.
- An IP allow-list is a network filter, not peer authentication. It should be paired with a trusted network boundary. For remote links, use a VPN and authenticated encryption.
- Relays intentionally forward ciphertext without decrypting it. A keyless relay therefore cannot verify the AEAD tag; its source-IP policy is part of the trust boundary.
- A rootless system service does not by itself prove that a particular PipeWire user session or Bluetooth A2DP sink is available after boot. Validate the audio-session setup on the target Raspberry Pi.
- Hosted CI validates Windows and Ubuntu builds. Raspberry Pi ARM64 performance, Bluetooth routing, and real-world end-to-end latency still require device testing.

## Automated checks

The security PR adds dependency auditing with `cargo audit`, Clippy with warnings denied, CodeQL analysis for Rust, and syntax checks for both installer scripts. The regular CI workflow also runs formatting, workspace tests, and release builds on Windows and Ubuntu.

A green CI run is evidence for those checks on the tested runners. It is not a substitute for reviewing deployment configuration or testing the Pi hardware.
