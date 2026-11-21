use anyhow::{Context, Result};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use rusty_opus::OpusDecoder;
use sonic_bridge_shared::{
    CHANNELS, FRAME_MS, FRAME_SAMPLES, PacketHeader, SAMPLE_RATE, decrypt_payload, parse_key_hex,
};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Parser, Debug)]
#[command(
    name = "sonic-bridge-server",
    about = "Receive, play, and optionally relay SonicBridge audio"
)]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "SONIC_BRIDGE_LISTEN", default_value = "127.0.0.1:8765")]
    listen: String,

    /// Forward received packets to another SonicBridge node.
    #[arg(long, env = "SONIC_BRIDGE_FORWARD_TO")]
    forward_to: Option<String>,

    /// Do not play locally. Useful for a pure relay Pi.
    #[arg(long, env = "SONIC_BRIDGE_RELAY_ONLY", default_value_t = false)]
    relay_only: bool,

    /// Packet buffering before playback, in milliseconds.
    #[arg(long, default_value_t = 60)]
    jitter_ms: u32,

    /// Optional 256-bit key as 64 hexadecimal characters.
    #[arg(long, env = "SONIC_BRIDGE_KEY")]
    key_hex: Option<String>,

    /// Print receiver statistics at this interval.
    #[arg(long, default_value_t = 5)]
    stats_interval: u64,

    /// Only accept datagrams from these source IP addresses.
    #[arg(
        long = "allowed-peer",
        env = "SONIC_BRIDGE_ALLOWED_PEERS",
        value_delimiter = ','
    )]
    allowed_peers: Vec<IpAddr>,

    /// Explicitly allow unencrypted traffic on a non-loopback interface.
    #[arg(
        long,
        env = "SONIC_BRIDGE_ALLOW_INSECURE_LISTEN",
        default_value_t = false
    )]
    allow_insecure_listen: bool,
}

fn validate_listener_policy(
    listen_addr: SocketAddr,
    has_key: bool,
    relay_only: bool,
    forwards_packets: bool,
    has_allowed_peers: bool,
    allow_insecure: bool,
) -> Result<()> {
    if listen_addr.ip().is_loopback() {
        return Ok(());
    }

    if (relay_only || forwards_packets) && !has_allowed_peers && !allow_insecure {
        anyhow::bail!(
            "non-loopback relays and forwarding receivers need --allowed-peer; a blind forwarding path cannot validate a PSK"
        );
    }

    if !has_key && !has_allowed_peers && !allow_insecure {
        anyhow::bail!(
            "refusing an unencrypted non-loopback listener; configure SONIC_BRIDGE_KEY, add --allowed-peer for trusted sender IPs, or explicitly pass --allow-insecure-listen"
        );
    }

    Ok(())
}

struct JitterBuffer {
    packets: BTreeMap<u32, Vec<u8>>,
    next_sequence: Option<u32>,
    target_frames: usize,
    max_frames: usize,
}

impl JitterBuffer {
    fn new(jitter_ms: u32) -> Self {
        let target_frames = (jitter_ms as usize)
            .div_ceil(FRAME_MS as usize)
            .clamp(1, 64);

        Self {
            packets: BTreeMap::new(),
            next_sequence: None,
            target_frames,
            max_frames: target_frames + 8,
        }
    }

    fn insert(&mut self, sequence: u32, payload: &[u8]) -> bool {
        let next = *self.next_sequence.get_or_insert(sequence);
        let distance = sequence.wrapping_sub(next);

        if distance >= 0x8000_0000 {
            return false;
        }

        if distance as usize >= self.max_frames {
            self.packets.clear();
            self.next_sequence =
                Some(sequence.wrapping_sub(self.target_frames.saturating_sub(1) as u32));
        }

        if self.packets.len() >= self.max_frames && !self.packets.contains_key(&sequence) {
            return false;
        }

        self.packets
            .entry(sequence)
            .or_insert_with(|| payload.to_vec());
        true
    }

    fn ready(&self) -> bool {
        self.packets.len() >= self.target_frames
    }

    fn pop_next(&mut self) -> Option<(u32, Vec<u8>)> {
        let sequence = self.next_sequence?;

        if let Some(payload) = self.packets.remove(&sequence) {
            self.next_sequence = Some(sequence.wrapping_add(1));
            Some((sequence, payload))
        } else if self.ready() {
            self.next_sequence = Some(sequence.wrapping_add(1));
            Some((sequence, Vec::new()))
        } else {
            None
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    let running = Arc::new(AtomicBool::new(true));
    let stop_flag = Arc::clone(&running);
    ctrlc::set_handler(move || stop_flag.store(false, Ordering::Relaxed))
        .context("could not install Ctrl+C handler")?;

    let key = args
        .key_hex
        .as_deref()
        .map(parse_key_hex)
        .transpose()
        .context("invalid SonicBridge encryption key")?;

    let listen_addr: SocketAddr = args
        .listen
        .parse()
        .context("--listen must be an IP address and port, for example 0.0.0.0:8765")?;
    let exposed = !listen_addr.ip().is_loopback();
    validate_listener_policy(
        listen_addr,
        key.is_some(),
        args.relay_only,
        args.forward_to.is_some(),
        !args.allowed_peers.is_empty(),
        args.allow_insecure_listen,
    )
    .context("unsafe receiver network configuration")?;

    let socket =
        UdpSocket::bind(&args.listen).context("could not bind the SonicBridge UDP socket")?;

    socket
        .set_read_timeout(Some(Duration::from_millis(250)))
        .context("could not configure the UDP socket")?;

    let host = cpal::default_host();

    let device = if args.relay_only {
        None
    } else {
        Some(
            host.default_output_device()
                .context("Linux has no default output device")?,
        )
    };

    let (tx, _stream) = if let Some(device) = device {
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

        eprintln!(
            "Output: {}",
            device.name().unwrap_or_else(|_| "unknown".into())
        );

        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(6);

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build_output::<f32>(&device, &config.into(), rx)?,
            cpal::SampleFormat::I16 => build_output::<i16>(&device, &config.into(), rx)?,
            cpal::SampleFormat::U16 => build_output::<u16>(&device, &config.into(), rx)?,
            other => anyhow::bail!("unsupported Linux sample format: {other:?}"),
        };

        stream
            .play()
            .context("could not start Linux audio playback")?;
        (Some(tx), Some(stream))
    } else {
        (None, None)
    };

    eprintln!("SonicBridge server v0.3");
    eprintln!("Listening on {}", args.listen);
    eprintln!(
        "Relay target: {}",
        args.forward_to.as_deref().unwrap_or("none")
    );
    eprintln!("Relay only: {}", args.relay_only);
    eprintln!("Jitter buffer: {} ms", args.jitter_ms);
    if !args.allowed_peers.is_empty() {
        eprintln!("Allowed peers: {:?}", args.allowed_peers);
    }
    if exposed
        && args.allowed_peers.is_empty()
        && args.allow_insecure_listen
        && (key.is_none() || args.relay_only || args.forward_to.is_some())
    {
        eprintln!("WARNING: accepting traffic on a non-loopback interface without a peer filter");
    }

    let forward_socket = if args.forward_to.is_some() {
        Some(UdpSocket::bind("0.0.0.0:0").context("could not create relay UDP socket")?)
    } else {
        None
    };

    let mut decoder = if args.relay_only {
        None
    } else {
        Some(
            OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS as usize)
                .context("could not initialise Opus")?,
        )
    };

    let mut jitter = JitterBuffer::new(args.jitter_ms);
    let mut buf = [0u8; 2048];
    let mut decoded = vec![0.0f32; FRAME_SAMPLES * CHANNELS as usize];

    let mut received = 0u64;
    let mut forwarded = 0u64;
    let mut concealed = 0u64;
    let mut dropped = 0u64;
    let mut auth_failed = 0u64;
    let mut last_report = Instant::now();

    while running.load(Ordering::Relaxed) {
        let (len, peer) = match socket.recv_from(&mut buf) {
            Ok(packet) => packet,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };

        if !args.allowed_peers.is_empty() && !args.allowed_peers.contains(&peer.ip()) {
            dropped += 1;
            continue;
        }

        let (header, payload) = match PacketHeader::decode(&buf[..len]) {
            Ok(packet) => packet,
            Err(_) => {
                dropped += 1;
                continue;
            }
        };

        if header.sample_rate != SAMPLE_RATE
            || header.channels != CHANNELS
            || header.frame_ms != FRAME_MS
        {
            dropped += 1;
            continue;
        }

        received += 1;

        if let (Some(target), Some(relay_socket), Some(next_header)) = (
            args.forward_to.as_ref(),
            forward_socket.as_ref(),
            header.decremented_ttl(),
        ) {
            let mut relay_packet = Vec::with_capacity(len);
            next_header.encode(payload, &mut relay_packet);

            if relay_socket.send_to(&relay_packet, target).is_ok() {
                forwarded += 1;
            }
        }

        if args.relay_only {
            if last_report.elapsed() >= Duration::from_secs(args.stats_interval.max(1)) {
                eprintln!(
                    "stats: rx={} forwarded={} concealed={} dropped={} auth_failed={} jitter={}",
                    received,
                    forwarded,
                    concealed,
                    dropped,
                    auth_failed,
                    jitter.packets.len()
                );
                last_report = Instant::now();
            }
            continue;
        }

        let decrypted_payload = if header.encrypted() {
            if let Some(key) = key.as_ref() {
                match decrypt_payload(key, &header, payload) {
                    Ok(value) => value,
                    Err(_) => {
                        auth_failed += 1;
                        continue;
                    }
                }
            } else {
                dropped += 1;
                continue;
            }
        } else {
            if key.is_some() {
                auth_failed += 1;
                continue;
            }
            payload.to_vec()
        };

        if !jitter.insert(header.sequence, &decrypted_payload) {
            dropped += 1;
            continue;
        }

        while let Some((_sequence, packet)) = jitter.pop_next() {
            if packet.is_empty() {
                match (decoder.as_mut(), tx.as_ref()) {
                    (Some(decoder), Some(tx))
                        if decoder.decode(&[], FRAME_SAMPLES, &mut decoded).is_ok()
                            && tx.try_send(decoded.clone()).is_ok() =>
                    {
                        concealed += 1;
                    }
                    _ => {}
                }
                continue;
            }

            if let Some(decoder) = decoder.as_mut() {
                match decoder.decode(&packet, FRAME_SAMPLES, &mut decoded) {
                    Ok(samples) if samples == FRAME_SAMPLES => {
                        if tx.as_ref().unwrap().try_send(decoded.clone()).is_err() {
                            dropped += 1;
                        }
                    }
                    Ok(_) | Err(_) => dropped += 1,
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(args.stats_interval.max(1)) {
            eprintln!(
                "stats: rx={} forwarded={} concealed={} dropped={} auth_failed={} jitter={}",
                received,
                forwarded,
                concealed,
                dropped,
                auth_failed,
                jitter.packets.len()
            );
            last_report = Instant::now();
        }
    }

    eprintln!(
        "Stopped. rx={received} forwarded={forwarded} concealed={concealed} dropped={dropped} auth_failed={auth_failed}"
    );
    drop(_stream);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_listener_needs_no_network_authentication() {
        let addr = "127.0.0.1:8765".parse().unwrap();
        assert!(validate_listener_policy(addr, false, false, false, false, false).is_ok());
    }

    #[test]
    fn exposed_plaintext_listener_needs_an_explicit_boundary() {
        let addr = "0.0.0.0:8765".parse().unwrap();
        assert!(validate_listener_policy(addr, false, false, false, false, false).is_err());
        assert!(validate_listener_policy(addr, false, false, false, true, false).is_ok());
        assert!(validate_listener_policy(addr, false, false, false, false, true).is_ok());
    }

    #[test]
    fn encrypted_final_receiver_can_listen_without_an_ip_filter() {
        let addr = "0.0.0.0:8765".parse().unwrap();
        assert!(validate_listener_policy(addr, true, false, false, false, false).is_ok());
    }

    #[test]
    fn blind_forwarders_need_an_ip_filter_even_when_given_a_key() {
        let addr = "0.0.0.0:8765".parse().unwrap();
        assert!(validate_listener_policy(addr, true, true, false, false, false).is_err());
        assert!(validate_listener_policy(addr, true, false, true, false, false).is_err());
        assert!(validate_listener_policy(addr, true, true, false, true, false).is_ok());
    }

    #[test]
    fn jitter_buffer_reorders_packets() {
        let mut buffer = JitterBuffer::new(20);

        assert!(buffer.insert(10, &[10]));
        assert!(buffer.insert(12, &[12]));
        assert!(buffer.insert(11, &[11]));

        assert_eq!(buffer.pop_next().unwrap(), (10, vec![10]));
        assert_eq!(buffer.pop_next().unwrap(), (11, vec![11]));
        assert_eq!(buffer.pop_next().unwrap(), (12, vec![12]));
    }

    #[test]
    fn jitter_buffer_handles_sequence_wraparound() {
        let mut buffer = JitterBuffer::new(20);

        assert!(buffer.insert(u32::MAX - 1, &[1]));
        assert!(buffer.insert(0, &[3]));
        assert!(buffer.insert(u32::MAX, &[2]));

        assert_eq!(buffer.pop_next().unwrap(), (u32::MAX - 1, vec![1]));
        assert_eq!(buffer.pop_next().unwrap(), (u32::MAX, vec![2]));
        assert_eq!(buffer.pop_next().unwrap(), (0, vec![3]));
    }

    #[test]
    fn jitter_buffer_rejects_old_packets() {
        let mut buffer = JitterBuffer::new(20);
        assert!(buffer.insert(100, &[1]));
        assert!(!buffer.insert(99, &[2]));
    }

    #[test]
    fn jitter_buffer_memory_is_bounded() {
        let mut buffer = JitterBuffer::new(u32::MAX);

        for sequence in 0..100_000u32 {
            let _ = buffer.insert(sequence, &[0u8; 128]);
            assert!(buffer.packets.len() <= buffer.max_frames);
        }

        assert!(buffer.max_frames <= 72);
    }
}
