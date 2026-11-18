use anyhow::{Context, Result};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SizedSample};
use rusty_opus::{Application, OpusEncoder};
use sonic_bridge_shared::{CHANNELS, FRAME_MS, FRAME_SAMPLES, PacketHeader, SAMPLE_RATE};
use std::{net::UdpSocket, sync::mpsc, time::Duration};

#[derive(Parser, Debug)]
#[command(
    name = "sonic-bridge-client",
    about = "Send Windows system audio to a SonicBridge receiver"
)]
struct Args {
    /// Address of the Raspberry Pi receiver.
    #[arg(long, default_value = "127.0.0.1:8765")]
    target: String,

    /// Opus bitrate in bits per second.
    #[arg(long, default_value_t = 96_000)]
    bitrate: u32,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let host = cpal::default_host();

    // On Windows/WASAPI, opening the output endpoint as an input stream
    // gives us the endpoint's loopback audio rather than microphone audio.
    let device = host
        .default_output_device()
        .context("Windows has no default audio output device")?;

    let config = device
        .default_input_config()
        .context("could not open the Windows output endpoint for loopback")?;

    if config.channels() != CHANNELS as u16 || config.sample_rate().0 != SAMPLE_RATE {
        anyhow::bail!(
            "SonicBridge currently needs a 48 kHz stereo endpoint (found {} Hz / {} channels)",
            config.sample_rate().0,
            config.channels()
        );
    }

    eprintln!("SonicBridge client");
    eprintln!(
        "Audio endpoint: {}",
        device.name().unwrap_or_else(|_| "unknown".into())
    );
    eprintln!("Target: {}", args.target);
    eprintln!("Bitrate: {} bps", args.bitrate);

    let frame_samples = FRAME_SAMPLES * CHANNELS as usize;

    // Four frames is deliberately small. If the network dies, we do not
    // want an audio-history museum quietly growing inside RAM.
    let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(4);

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            build_capture::<f32>(&device, &config.into(), tx, frame_samples)?
        }
        cpal::SampleFormat::I16 => {
            build_capture::<i16>(&device, &config.into(), tx, frame_samples)?
        }
        cpal::SampleFormat::U16 => {
            build_capture::<u16>(&device, &config.into(), tx, frame_samples)?
        }
        other => anyhow::bail!("unsupported Windows sample format: {other:?}"),
    };

    stream.play().context("could not start Windows audio capture")?;

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

    loop {
        let samples = rx.recv().context("Windows audio capture stopped")?;

        let encoded_len = encoder
            .encode(&samples, FRAME_SAMPLES, &mut encoded)
            .context("Opus encoding failed")?;

        PacketHeader {
            sequence,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            frame_ms: FRAME_MS,
        }
        .encode(&encoded[..encoded_len], &mut packet);

        socket
            .send(&packet)
            .context("could not send audio packet")?;

        sequence = sequence.wrapping_add(1);
    }
}

fn build_capture<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tx: mpsc::SyncSender<Vec<f32>>,
    frame_samples: usize,
) -> Result<cpal::Stream>
where
    T: SizedSample + Sample,
{
    let mut frame = Vec::with_capacity(frame_samples);

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            for &sample in data {
                frame.push(sample.to_float_sample());

                if frame.len() == frame_samples {
                    let ready =
                        std::mem::replace(&mut frame, Vec::with_capacity(frame_samples));

                    // Dropping a frame is preferable to blocking the audio
                    // callback. The next frame is already on its way.
                    let _ = tx.try_send(ready);
                }
            }
        },
        move |error| eprintln!("audio capture error: {error}"),
        Some(Duration::from_millis(100)),
    )?;

    Ok(stream)
}
