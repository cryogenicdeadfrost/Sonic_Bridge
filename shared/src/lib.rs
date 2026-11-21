use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use std::io;

pub const MAGIC: [u8; 4] = *b"SBRG";
pub const VERSION: u8 = 3;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u8 = 2;
pub const FRAME_MS: u8 = 20;
pub const FRAME_SAMPLES: usize = 960;

pub const FLAG_ENCRYPTED: u8 = 0x01;
pub const NONCE_LEN: usize = 24;
pub const AAD_LEN: usize = 40;

// Header:
// magic(4) + version(1) + channels(1) + frame_ms(1) + ttl(1)
// + flags(1) + sequence(4) + sample_rate(4) + nonce(24) + payload_len(2)
pub const HEADER_LEN: usize = 43;
pub const MAX_OPUS_PAYLOAD_LEN: usize = 1_275;
pub const AEAD_TAG_LEN: usize = 16;
pub const MAX_PACKET_PAYLOAD_LEN: usize = MAX_OPUS_PAYLOAD_LEN + AEAD_TAG_LEN;
pub const MAX_PACKET_SIZE: usize = HEADER_LEN + MAX_PACKET_PAYLOAD_LEN;
pub const MAX_TTL: u8 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub sequence: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_ms: u8,
    pub ttl: u8,
    pub flags: u8,
    pub nonce: [u8; NONCE_LEN],
}

impl PacketHeader {
    pub fn encode(self, payload: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        let max_payload_len = if self.encrypted() {
            MAX_PACKET_PAYLOAD_LEN
        } else {
            MAX_OPUS_PAYLOAD_LEN
        };

        if self.flags & !FLAG_ENCRYPTED != 0
            || self.ttl > MAX_TTL
            || self.sample_rate != SAMPLE_RATE
            || self.channels != CHANNELS
            || self.frame_ms != FRAME_MS
            || payload.len() > max_payload_len
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SonicBridge packet exceeds its protocol limits",
            ));
        }

        let payload_len = u16::try_from(payload.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "SonicBridge payload length does not fit in the packet header",
            )
        })?;

        out.clear();
        out.reserve(HEADER_LEN + payload.len());

        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(self.channels);
        out.push(self.frame_ms);
        out.push(self.ttl);
        out.push(self.flags);
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.sample_rate.to_be_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&payload_len.to_be_bytes());
        out.extend_from_slice(payload);
        Ok(())
    }

    pub fn decode(buf: &[u8]) -> io::Result<(Self, &[u8])> {
        if buf.len() < HEADER_LEN || buf[0..4] != MAGIC || buf[4] != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid SonicBridge packet",
            ));
        }

        if buf[8] & !FLAG_ENCRYPTED != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported SonicBridge packet flags",
            ));
        }

        let sample_rate = u32::from_be_bytes(buf[13..17].try_into().unwrap());
        if sample_rate != SAMPLE_RATE || buf[5] != CHANNELS || buf[6] != FRAME_MS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported SonicBridge audio format",
            ));
        }

        let payload_len = u16::from_be_bytes([buf[41], buf[42]]) as usize;
        let max_payload_len = if buf[8] & FLAG_ENCRYPTED != 0 {
            MAX_PACKET_PAYLOAD_LEN
        } else {
            MAX_OPUS_PAYLOAD_LEN
        };

        if buf[7] > MAX_TTL || payload_len > max_payload_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SonicBridge packet exceeds its protocol limits",
            ));
        }

        if buf.len() != HEADER_LEN + payload_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SonicBridge packet length does not match its header",
            ));
        }

        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&buf[17..41]);

        let header = Self {
            sequence: u32::from_be_bytes(buf[9..13].try_into().unwrap()),
            sample_rate: u32::from_be_bytes(buf[13..17].try_into().unwrap()),
            channels: buf[5],
            frame_ms: buf[6],
            ttl: buf[7],
            flags: buf[8],
            nonce,
        };

        Ok((header, &buf[HEADER_LEN..]))
    }

    // TTL is intentionally excluded because intermediate relays decrement it.
    // The stream identity and audio-format fields must remain authenticated.
    pub fn authenticated_data(&self) -> [u8; AAD_LEN] {
        let mut aad = [0u8; AAD_LEN];

        aad[0..4].copy_from_slice(&MAGIC);
        aad[4] = VERSION;
        aad[5] = self.channels;
        aad[6] = self.frame_ms;
        aad[7] = self.flags;
        aad[8..12].copy_from_slice(&self.sequence.to_be_bytes());
        aad[12..16].copy_from_slice(&self.sample_rate.to_be_bytes());
        aad[16..40].copy_from_slice(&self.nonce);

        aad
    }

    pub fn decremented_ttl(mut self) -> Option<Self> {
        if self.ttl == 0 {
            None
        } else {
            self.ttl -= 1;
            Some(self)
        }
    }

    pub fn encrypted(self) -> bool {
        self.flags & FLAG_ENCRYPTED != 0
    }
}

pub fn parse_key_hex(value: &str) -> io::Result<[u8; 32]> {
    let bytes = value.trim().as_bytes();

    if bytes.len() != 64 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SonicBridge keys must contain exactly 64 hexadecimal characters",
        ));
    }

    let mut key = [0u8; 32];

    for (index, byte) in key.iter_mut().enumerate() {
        let offset = index * 2;
        let high = hex_nibble(bytes[offset]).expect("validated hexadecimal key");
        let low = hex_nibble(bytes[offset + 1]).expect("validated hexadecimal key");
        *byte = (high << 4) | low;
    }

    Ok(key)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn fresh_nonce() -> [u8; NONCE_LEN] {
    rand::random::<[u8; NONCE_LEN]>()
}

pub fn encrypt_payload(
    key: &[u8; 32],
    header: &PacketHeader,
    plaintext: &[u8],
) -> io::Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::from(header.nonce);
    let aad = header.authenticated_data();

    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "audio encryption failed"))
}

pub fn decrypt_payload(
    key: &[u8; 32],
    header: &PacketHeader,
    ciphertext: &[u8],
) -> io::Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::from(header.nonce);
    let aad = header.authenticated_data();

    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "audio authentication failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_header() -> PacketHeader {
        PacketHeader {
            sequence: 42,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            frame_ms: FRAME_MS,
            ttl: 8,
            flags: FLAG_ENCRYPTED,
            nonce: fresh_nonce(),
        }
    }

    #[test]
    fn key_parser_accepts_uppercase_and_lowercase_hex() {
        assert_eq!(parse_key_hex(&"aB".repeat(32)).unwrap(), [0xAB; 32]);
    }

    #[test]
    fn key_parser_rejects_non_ascii_without_panicking() {
        let invalid = format!("€{}", "0".repeat(61));
        assert_eq!(invalid.len(), 64);
        assert_eq!(
            parse_key_hex(&invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn packet_round_trip() {
        let header = test_header();
        let payload = [1, 2, 3, 4];
        let mut packet = Vec::new();

        header.encode(&payload, &mut packet).unwrap();

        let (decoded, body) = PacketHeader::decode(&packet).unwrap();

        assert_eq!(decoded, header);
        assert_eq!(body, payload);
    }

    #[test]
    fn encrypted_payload_round_trip() {
        let key = [7u8; 32];
        let header = test_header();
        let plaintext = b"audio frame";

        let ciphertext = encrypt_payload(&key, &header, plaintext).unwrap();
        let decrypted = decrypt_payload(&key, &header, &ciphertext).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_is_rejected() {
        let key = [7u8; 32];
        let wrong_key = [8u8; 32];
        let header = test_header();
        let ciphertext = encrypt_payload(&key, &header, b"audio").unwrap();

        assert!(decrypt_payload(&wrong_key, &header, &ciphertext).is_err());
    }

    #[test]
    fn stream_metadata_is_authenticated() {
        let key = [7u8; 32];
        let header = test_header();
        let ciphertext = encrypt_payload(&key, &header, b"audio").unwrap();
        let mut changed = header;
        changed.sequence = changed.sequence.wrapping_add(1);

        assert!(decrypt_payload(&key, &changed, &ciphertext).is_err());
    }

    #[test]
    fn relay_can_decrement_ttl_without_breaking_authentication() {
        let key = [7u8; 32];
        let header = test_header();
        let ciphertext = encrypt_payload(&key, &header, b"audio").unwrap();
        let forwarded_header = header.decremented_ttl().unwrap();

        assert_eq!(
            decrypt_payload(&key, &forwarded_header, &ciphertext).unwrap(),
            b"audio"
        );
    }

    #[test]
    fn truncated_packet_is_rejected() {
        let error = PacketHeader::decode(b"SBRG").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn trailing_packet_bytes_are_rejected() {
        let header = test_header();
        let mut packet = Vec::new();
        header.encode(&[1, 2, 3], &mut packet).unwrap();
        packet.push(4);

        assert_eq!(
            PacketHeader::decode(&packet).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn unsupported_audio_formats_are_rejected() {
        for (offset, value) in [(5, 1), (6, 40), (16, 1)] {
            let header = test_header();
            let mut packet = Vec::new();
            header.encode(&[1, 2, 3], &mut packet).unwrap();
            packet[offset] = value;

            assert_eq!(
                PacketHeader::decode(&packet).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn unsupported_flags_are_rejected() {
        let header = test_header();
        let mut packet = Vec::new();
        header.encode(&[1, 2, 3], &mut packet).unwrap();
        packet[8] = 0x80;

        assert_eq!(
            PacketHeader::decode(&packet).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn oversized_opus_payload_is_rejected() {
        let header = PacketHeader {
            flags: 0,
            nonce: [0u8; NONCE_LEN],
            ..test_header()
        };
        let mut packet = Vec::new();
        assert_eq!(
            header
                .encode(&vec![0u8; MAX_OPUS_PAYLOAD_LEN + 1], &mut packet)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn decoder_rejects_oversized_opus_payload() {
        let header = PacketHeader {
            flags: 0,
            nonce: [0u8; NONCE_LEN],
            ..test_header()
        };
        let mut packet = Vec::new();
        header
            .encode(&vec![0u8; MAX_OPUS_PAYLOAD_LEN], &mut packet)
            .unwrap();
        packet[41..43].copy_from_slice(&((MAX_OPUS_PAYLOAD_LEN + 1) as u16).to_be_bytes());
        packet.push(0);

        assert_eq!(
            PacketHeader::decode(&packet).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn excessive_ttl_is_rejected() {
        let header = test_header();
        let mut packet = Vec::new();
        header.encode(&[1, 2, 3], &mut packet).unwrap();
        packet[7] = MAX_TTL + 1;

        assert_eq!(
            PacketHeader::decode(&packet).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn ttl_reaches_zero_then_dies() {
        let header = test_header();
        let next = header.decremented_ttl().unwrap();
        assert_eq!(next.ttl, 7);
        assert!(PacketHeader { ttl: 0, ..next }.decremented_ttl().is_none());
    }
}
