use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
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

// Header:
// magic(4) + version(1) + channels(1) + frame_ms(1) + ttl(1)
// + flags(1) + sequence(4) + sample_rate(4) + nonce(24) + payload_len(2)
pub const HEADER_LEN: usize = 43;

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
    pub fn encode(self, payload: &[u8], out: &mut Vec<u8>) {
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
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(payload);
    }

    pub fn decode(buf: &[u8]) -> io::Result<(Self, &[u8])> {
        if buf.len() < HEADER_LEN || buf[0..4] != MAGIC || buf[4] != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid SonicBridge packet",
            ));
        }

        let payload_len = u16::from_be_bytes([buf[41], buf[42]]) as usize;

        if buf.len() < HEADER_LEN + payload_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated SonicBridge packet",
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

        Ok((header, &buf[HEADER_LEN..HEADER_LEN + payload_len]))
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
    let value = value.trim();

    if value.len() != 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SonicBridge keys must contain exactly 64 hexadecimal characters",
        ));
    }

    let mut key = [0u8; 32];

    for (index, byte) in key.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&value[offset..offset + 2], 16).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "SonicBridge key contains non-hexadecimal characters",
            )
        })?;
    }

    Ok(key)
}

pub fn encrypt_payload(key: &[u8; 32], plaintext: &[u8]) -> io::Result<([u8; NONCE_LEN], Vec<u8>)> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::from(rand::random::<[u8; NONCE_LEN]>());

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "audio encryption failed"))?;

    Ok((nonce.into(), ciphertext))
}

pub fn decrypt_payload(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
) -> io::Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::from(*nonce);

    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "audio authentication failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip() {
        let header = PacketHeader {
            sequence: 42,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            frame_ms: FRAME_MS,
            ttl: 8,
            flags: 0,
            nonce: [0; NONCE_LEN],
        };

        let payload = [1, 2, 3, 4];
        let mut packet = Vec::new();

        header.encode(&payload, &mut packet);

        let (decoded, body) = PacketHeader::decode(&packet).unwrap();

        assert_eq!(decoded, header);
        assert_eq!(body, payload);
    }

    #[test]
    fn encrypted_payload_round_trip() {
        let key = [7u8; 32];
        let plaintext = b"audio frame";

        let (nonce, ciphertext) = encrypt_payload(&key, plaintext).unwrap();
        let decrypted = decrypt_payload(&key, &nonce, &ciphertext).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_is_rejected() {
        let key = [7u8; 32];
        let wrong_key = [8u8; 32];
        let (nonce, ciphertext) = encrypt_payload(&key, b"audio").unwrap();

        assert!(decrypt_payload(&wrong_key, &nonce, &ciphertext).is_err());
    }

    #[test]
    fn truncated_packet_is_rejected() {
        let error = PacketHeader::decode(b"SBRG").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn ttl_reaches_zero_then_dies() {
        let header = PacketHeader {
            sequence: 1,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            frame_ms: FRAME_MS,
            ttl: 1,
            flags: 0,
            nonce: [0; NONCE_LEN],
        };

        let next = header.decremented_ttl().unwrap();
        assert_eq!(next.ttl, 0);
        assert!(next.decremented_ttl().is_none());
    }
}
