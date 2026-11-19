use std::io;

pub const MAGIC: [u8; 4] = *b"SBRG";
pub const VERSION: u8 = 2;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u8 = 2;
pub const FRAME_MS: u8 = 20;
pub const FRAME_SAMPLES: usize = 960;

// Header:
// magic(4) + version(1) + channels(1) + frame_ms(1) + ttl(1)
// + sequence(4) + sample_rate(4) + payload_len(2)
pub const HEADER_LEN: usize = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub sequence: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_ms: u8,
    pub ttl: u8,
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
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.sample_rate.to_be_bytes());
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

        let payload_len = u16::from_be_bytes([buf[16], buf[17]]) as usize;

        if buf.len() < HEADER_LEN + payload_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated SonicBridge packet",
            ));
        }

        let header = Self {
            sequence: u32::from_be_bytes(buf[8..12].try_into().unwrap()),
            sample_rate: u32::from_be_bytes(buf[12..16].try_into().unwrap()),
            channels: buf[5],
            frame_ms: buf[6],
            ttl: buf[7],
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
        };

        let payload = [1, 2, 3, 4];
        let mut packet = Vec::new();

        header.encode(&payload, &mut packet);

        let (decoded, body) = PacketHeader::decode(&packet).unwrap();

        assert_eq!(decoded, header);
        assert_eq!(body, payload);
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
        };

        let next = header.decremented_ttl().unwrap();
        assert_eq!(next.ttl, 0);
        assert!(next.decremented_ttl().is_none());
    }
}
