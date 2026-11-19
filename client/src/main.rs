use anyhow::{Context, Result};
use clap::Parser;
use flexaudio::{open, OutputFormat, SourceKind, StreamConfig};
use rusty_opus::{Application, OpusEncoder};
use sonic_bridge_shared::{CHANNELS, FRAME_MS, FRAME_SAMPLES, PacketHeader, SAMPLE_RATE};
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
}

fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("SonicBridge client v0.2");
    eprintln!("Target: {}", args.target);
    eprintln!("Bitrate: {} bps", args.bitrate);
    eprintln!("Packet TTL: {}", args.ttl);

    let mut stream = open(StreamConfig {
        kind: SourceKind::System,
        output: OutputFormat {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS as u16,
        },
        ..Default::default()
    })
    .context("could not initialise system-audio loopback")?;

    stream.start().context("could not start system-audio capture")?;

    let socket = UdpSocket::bind("0.0.0.0:0")
        .context("could not create the SonicBridge UDP socket")?;

    socket
        .connect(&args.target)
        .with_context(|| format!("could not connect to {}", args.target))?;

    let mut encoder =
        OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as usize, Application::Audio)
            .context("could not initialise Opus")?;

    encoder.bitrate_bps = args.bitrate as i32;
    encoder.complexity = 5;
    encoder.use_cbr = false;
    encoder.use_dtx = true;

    let mut sequence = 0u32;
    let mut encoded = [0u8; 1500];
    let mut packet = Vec::with_capacity(1500);
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

        PacketHeader {
            sequence,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            frame_ms: FRAME_MS,
            ttl: args.ttl,
        }
        .encode(&encoded[..encoded_len], &mut packet);

        socket
            .send(&packet)
            .context("could not send audio packet")?;

        sequence = sequence.wrapping_add(1);
    }
}
