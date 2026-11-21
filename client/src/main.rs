use anyhow::{Context, Result, bail};
use clap::Parser;
use flexaudio::{OutputFormat, SourceKind, StreamConfig, open};
use rusty_opus::{Application, OpusEncoder};
use sonic_bridge_shared::{
    CHANNELS, FLAG_ENCRYPTED, FRAME_MS, FRAME_SAMPLES, NONCE_LEN, PacketHeader, SAMPLE_RATE,
    encrypt_payload, fresh_nonce, parse_key_hex,
};
use std::{
    net::UdpSocket,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_OPUS_FRAME_BYTES: usize = 1_275;
const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS as usize;

#[derive(Parser, Debug)]
#[command(
    name = "sonic-bridge-client",
    version,
    about = "Send system audio to a SonicBridge receiver"
)]
struct Args {
    /// Address of the Raspberry Pi receiver or next relay.
    #[arg(long, env = "SONIC_BRIDGE_TARGET", default_value = "127.0.0.1:8765")]
    target: String,

    /// Opus bitrate in bits per second (6,000..=512,000).
    #[arg(long, default_value_t = 96_000)]
    bitrate: u32,

    /// Maximum number of relay hops before the packet is discarded (1..=32).
    #[arg(long, default_value_t = 8)]
    ttl: u8,

    /// Print a transmission summary every N seconds. Set to 0 to disable.
    #[arg(long, default_value_t = 5)]
    stats_interval: u64,

    /// Optional 256-bit key as 64 hexadecimal characters.
    #[arg(long, env = "SONIC_BRIDGE_KEY")]
    key_hex: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if !(6_000..=512_000).contains(&args.bitrate) {
        bail!("--bitrate must be between 6000 and 512000 bits per second");
    }
    if !(1..=32).contains(&args.ttl) {
        bail!("--ttl must be between 1 and 32");
    }
    if args.target.trim().is_empty() {
        bail!("--target cannot be empty");
    }

    let key = args
        .key_hex
        .as_deref()
        .map(parse_key_hex)
        .transpose()
        .context("invalid SonicBridge encryption key")?;

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

    let running = Arc::new(AtomicBool::new(true));
    let stop_flag = Arc::clone(&running);
    ctrlc::set_handler(move || stop_flag.store(false, Ordering::Relaxed))
        .context("could not install Ctrl+C handler")?;

    eprintln!("SonicBridge client v{}", env!("CARGO_PKG_VERSION"));
    eprintln!("Target: {}", args.target);
    eprintln!("Bitrate: {} bps", args.bitrate);
    eprintln!("Packet TTL: {}", args.ttl);
    eprintln!(
        "Audio encryption: {}",
        if key.is_some() {
            "XChaCha20-Poly1305"
        } else {
            "disabled"
        }
    );
    eprintln!("Press Ctrl+C to stop.");

    let mut sequence = 0u32;
    let mut sent = 0u64;
    let mut assembler = FrameAssembler::default();
    let mut encoded = [0u8; MAX_OPUS_FRAME_BYTES];
    let mut packet = Vec::with_capacity(1_350);
    let mut last_report = Instant::now();

    while running.load(Ordering::Relaxed) {
        let Some(chunk) = stream.poll_chunk() else {
            thread::sleep(Duration::from_millis(1));
            continue;
        };

        // Capture callback boundaries rarely line up with codec frame boundaries.
        assembler.push(&chunk.data, |frame| {
            ship_frame(
                &mut encoder,
                frame,
                key.as_ref(),
                args.ttl,
                sequence,
                &mut encoded,
                &mut packet,
                &socket,
            )?;
            sequence = sequence.wrapping_add(1);
            sent += 1;
            Ok(())
        })?;

        if args.stats_interval > 0
            && last_report.elapsed() >= Duration::from_secs(args.stats_interval)
        {
            eprintln!("stats: sent={sent} packets");
            last_report = Instant::now();
        }
    }

    eprintln!("Stopped. Sent {sent} audio packets.");
    Ok(())
}

struct FrameAssembler {
    samples: [f32; FRAME_LEN],
    filled: usize,
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self {
            samples: [0.0; FRAME_LEN],
            filled: 0,
        }
    }
}

impl FrameAssembler {
    fn push(
        &mut self,
        input: &[f32],
        mut send_frame: impl FnMut(&[f32; FRAME_LEN]) -> Result<()>,
    ) -> Result<()> {
        let mut offset = 0;

        while offset < input.len() {
            let take = (FRAME_LEN - self.filled).min(input.len() - offset);
            self.samples[self.filled..self.filled + take]
                .copy_from_slice(&input[offset..offset + take]);
            self.filled += take;
            offset += take;

            if self.filled == FRAME_LEN {
                send_frame(&self.samples)?;
                self.filled = 0;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_chunks_are_joined_into_complete_frames() {
        let mut assembler = FrameAssembler::default();
        let samples: Vec<f32> = (0..FRAME_LEN * 2).map(|n| n as f32).collect();
        let mut frames = Vec::new();

        for chunk in [
            &samples[..17],
            &samples[17..987],
            &samples[987..1_400],
            &samples[1_400..],
        ] {
            assembler
                .push(chunk, |frame| {
                    frames.push((frame[0], frame[FRAME_LEN - 1]));
                    Ok(())
                })
                .unwrap();
        }

        assert_eq!(frames, vec![(0.0, (FRAME_LEN - 1) as f32), (FRAME_LEN as f32, (FRAME_LEN * 2 - 1) as f32)]);
        assert_eq!(assembler.filled, 0);
    }

    #[test]
    fn incomplete_capture_is_kept_until_the_next_chunk() {
        let mut assembler = FrameAssembler::default();
        let mut frames_sent = 0;

        assembler
            .push(&[1.0; FRAME_LEN - 1], |_| {
                frames_sent += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(frames_sent, 0);

        assembler
            .push(&[2.0], |frame| {
                frames_sent += 1;
                assert_eq!(frame[0], 1.0);
                assert_eq!(frame[FRAME_LEN - 1], 2.0);
                Ok(())
            })
            .unwrap();
        assert_eq!(frames_sent, 1);
    }
}

fn ship_frame(
    encoder: &mut OpusEncoder,
    frame: &[f32; FRAME_SAMPLES * CHANNELS as usize],
    key: Option<&[u8; 32]>,
    ttl: u8,
    sequence: u32,
    encoded: &mut [u8; MAX_OPUS_FRAME_BYTES],
    packet: &mut Vec<u8>,
    socket: &UdpSocket,
) -> Result<()> {
    let encoded_len = encoder
        .encode(frame, FRAME_SAMPLES, encoded)
        .context("Opus encoding failed")?;

    if let Some(key) = key {
        let header = PacketHeader {
            sequence,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            frame_ms: FRAME_MS,
            ttl,
            flags: FLAG_ENCRYPTED,
            nonce: fresh_nonce(),
        };
        let ciphertext = encrypt_payload(key, &header, &encoded[..encoded_len])
            .context("could not encrypt audio frame")?;
        header.encode(&ciphertext, packet);
    } else {
        let header = PacketHeader {
            sequence,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            frame_ms: FRAME_MS,
            ttl,
            flags: 0,
            nonce: [0u8; NONCE_LEN],
        };
        header.encode(&encoded[..encoded_len], packet);
    }

    socket.send(packet).context("could not send audio packet")?;
    Ok(())
}
