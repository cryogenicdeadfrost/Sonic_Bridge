use anyhow::{Context, Result};
use clap::Parser;
use flexaudio::{OutputFormat, SourceKind, StreamConfig, open};
use rusty_opus::{Application, OpusEncoder};
use sonic_bridge_shared::{encrypt_payload, parse_key_hex, CHANNELS, FLAG_ENCRYPTED, FRAME_MS, FRAME_SAMPLES, PacketHeader, SAMPLE_RATE, NONCE_LEN};
use std::{net::UdpSocket, thread, time::Duration};

#[derive(Parser, Debug)]
#[command(
    name = "sonic-bridge-client",
    about = "Send Windows system audio to a SonicBridge receiver"
)]
struct Args {
    /// Address of the Raspberry Pi receiver or next relay.
    #[arg(long, default_value = "127.0.0.1:8765")]
    target: String,

    /// Opus bitrate in bits per second.
    #[arg(long, default_value_t = 96_000)]
    bitrate: u32,

    /// Maximum number of relay hops before the packet is discarded.
    #[arg(long, default_value_t = 8)]
    ttl: u8,

    /// Optional 256-bit key as 64 hexadecimal characters.
    #[arg(long, env = "SONIC_BRIDGE_KEY")]
    key_hex: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("SonicBridge client v0.2");
    eprintln!("Target: {}", args.target);
    eprintln!("Bitrate: {} bps", args.bitrate);
    eprintln!("Packet TTL: {}", args.ttl);

    let mut stream = open(StreamConfig {
        kind: SourceKind::SystemLoopback,
        output: OutputFormat {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS as u16,
        },
        ..Default::default()
    })
    .context("could not initialise system-audio loopback")?;

    stream
        .start()
        .context("could not start system-audio capture")?;

    let socket =
        UdpSocket::bind("0.0.0.0:0").context("could not create the SonicBridge UDP socket")?;

    socket
        .connect(&args.target)
        .with_context(|| format!("could not connect to {}", args.target))?;

    let mut encoder = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as usize, Application::Audio)
        .context("could not initialise Opus")?;

    encoder.bitrate_bps = args.bitrate as i32;
    encoder.complexity = 5;
    encoder.use_cbr = false;
    encoder.use_dtx = true;

    let key = args
        .key_hex
        .as_deref()
        .map(parse_key_hex)
        .transpose()
        .context("invalid SonicBridge encryption key")?;

    if key.is_some() {
        eprintln!("Audio encryption: XChaCha20-Poly1305");
    } else {
        eprintln!("Audio encryption: disabled");
    }

    let mut sequence = 0u32;
    let mut encoded = [0u8; 1500];
    let mut encrypted = Vec::with_capacity(1600);
    let mut packet = Vec::with_capacity(1700);
    let expected_samples = FRAME_SAMPLES * CHANNELS as usize;

    loop {
        let chunk = match stream.poll_chunk() {
            Some(chunk) => chunk,
            None => {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
        };

        if chunk.data.len() != expected_samples {
            continue;
        }

        let encoded_len = encoder
            .encode(&chunk.data, FRAME_SAMPLES, &mut encoded)
            .context("Opus encoding failed")?;

        packet.clear();

        if let Some(key) = key.as_ref() {
            let (nonce, ciphertext) = encrypt_payload(key, &encoded[..encoded_len])
                .context("could not encrypt audio frame")?;
            encrypted.clear();
            encrypted.extend_from_slice(&ciphertext);

            PacketHeader {
                sequence,
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS,
                frame_ms: FRAME_MS,
                ttl: args.ttl,
                flags: FLAG_ENCRYPTED,
                nonce,
            }
            .encode(&encrypted, &mut packet);
        } else {
            PacketHeader {
                sequence,
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS,
                frame_ms: FRAME_MS,
                ttl: args.ttl,
                flags: 0,
                nonce: [0u8; NONCE_LEN],
            }
            .encode(&encoded[..encoded_len], &mut packet);
        }

        socket
            .send(&packet)
            .context("could not send audio packet")?;

        sequence = sequence.wrapping_add(1);
    }
}
