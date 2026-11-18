use anyhow::{Context, Result};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use rusty_opus::OpusDecoder;
use sonic_bridge_shared::{CHANNELS, FRAME_MS, FRAME_SAMPLES, PacketHeader, SAMPLE_RATE};
use std::{net::UdpSocket, sync::mpsc, time::Duration};

#[derive(Parser, Debug)]
#[command(
    name = "sonic-bridge-server",
    about = "Receive SonicBridge audio and play it through Linux"
)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:8765")]
    listen: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let socket =
        UdpSocket::bind(&args.listen).context("could not bind the SonicBridge UDP socket")?;

    socket
        .set_read_timeout(Some(Duration::from_millis(250)))
        .context("could not configure the UDP socket")?;

    let host = cpal::default_host();

    let device = host
        .default_output_device()
        .context("Linux has no default output device")?;

    let config = device
        .default_output_config()
        .context("could not open the default Linux audio output")?;

    if config.channels() != CHANNELS as u16 || config.sample_rate().0 != SAMPLE_RATE {
        anyhow::bail!(
            "SonicBridge needs a 48 kHz stereo output (found {} Hz / {} channels)",
            config.sample_rate().0,
            config.channels()
        );
    }

    eprintln!("SonicBridge server");
    eprintln!("Listening on {}", args.listen);
    eprintln!(
        "Output: {}",
        device.name().unwrap_or_else(|_| "unknown".into())
    );

    // Six frames gives us a small amount of breathing room without turning
    // the receiver into a delayed radio station.
    let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(6);

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build_output::<f32>(&device, &config.into(), rx)?,
        cpal::SampleFormat::I16 => build_output::<i16>(&device, &config.into(), rx)?,
        cpal::SampleFormat::U16 => build_output::<u16>(&device, &config.into(), rx)?,
        other => anyhow::bail!("unsupported Linux sample format: {other:?}"),
    };

    stream.play().context("could not start Linux audio playback")?;

    let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS as usize)
        .context("could not initialise Opus decoder")?;

    let mut buf = [0u8; 2048];
    let mut decoded = vec![0.0f32; FRAME_SAMPLES * CHANNELS as usize];
    let mut last_sequence: Option<u32> = None;

    loop {
        let len = match socket.recv(&mut buf) {
            Ok(len) => len,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue
            }
            Err(error) => return Err(error.into()),
        };

        let (header, payload) = match PacketHeader::decode(&buf[..len]) {
            Ok(packet) => packet,
            Err(_) => continue,
        };

        if header.sample_rate != SAMPLE_RATE
            || header.channels != CHANNELS
            || header.frame_ms != FRAME_MS
        {
            continue;
        }

        // If packets vanished in transit, Opus can generate a concealed frame
        // instead of making the listener hear a tiny but very noticeable hole.
        if let Some(previous) = last_sequence {
            let gap = header.sequence.wrapping_sub(previous);

            if gap > 1 && gap < 32 {
                for _ in 0..(gap - 1) {
                    if decoder.decode(&[], FRAME_SAMPLES, &mut decoded).is_ok() {
                        let _ = tx.try_send(decoded.clone());
                    }
                }
            }
        }

        last_sequence = Some(header.sequence);

        match decoder.decode(payload, FRAME_SAMPLES, &mut decoded) {
            Ok(samples) if samples == FRAME_SAMPLES => {
                // If playback is behind, sacrifice this frame rather than
                // letting latency grow forever.
                let _ = tx.try_send(decoded.clone());
            }
            Ok(_) | Err(_) => {}
        }
    }
}

fn build_output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    rx: mpsc::Receiver<Vec<f32>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let mut current = Vec::<f32>::new();
    let mut index = 0usize;

    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _| {
            for sample in out.iter_mut() {
                if index >= current.len() {
                    match rx.try_recv() {
                        Ok(next) => {
                            current = next;
                            index = 0;
                        }
                        Err(_) => {
                            // Silence beats a callback that blocks forever.
                            *sample = T::from_sample(0.0);
                            continue;
                        }
                    }
                }

                *sample = T::from_sample(current[index]);
                index += 1;
            }
        },
        move |error| eprintln!("audio playback error: {error}"),
        Some(Duration::from_millis(100)),
    )?;

    Ok(stream)
}
