//! Incremental Minecraft byte-stream codecs for asynchronous transports.
//!
//! WebSocket messages are arbitrary chunks of the TCP stream, not packet
//! boundaries. These codecs use exactly the native framing, compression and
//! encryption primitives, without sockets, threads or blocking reads.
use crate::{Error, Result, buf::Reader, crypto::Cfb8, frame};

/// Bound unconsumed wire data even when a peer sends faster than it is read.
pub const MAX_BUFFERED_BYTES: usize = 8 << 20;

#[derive(Default)]
pub struct PacketDecoder {
    buffer: Vec<u8>,
    decrypt: Option<Cfb8>,
    pub threshold: Option<usize>,
}

impl PacketDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_BUFFERED_BYTES.saturating_sub(self.buffer.len()) {
            return Err(Error::new("Network receive buffer exceeded its limit"));
        }
        let offset = self.buffer.len();
        self.buffer.extend_from_slice(bytes);
        if let Some(cipher) = &mut self.decrypt {
            cipher.decrypt(&mut self.buffer[offset..]);
        }
        Ok(())
    }

    /// Drain available packets before adding another transport chunk.
    pub fn next_packet(&mut self) -> Result<Option<(i32, Vec<u8>)>> {
        let Some(frame) = frame::split(&mut self.buffer)? else {
            return Ok(None);
        };
        let body = frame::decompress(frame, self.threshold)?;
        let mut reader = Reader::new(&body);
        let id = reader.varint()?;
        Ok(Some((id, body[reader.position()..].to_vec())))
    }

    /// Enable encryption at the login boundary, including already buffered bytes.
    pub fn enable_encryption(&mut self, secret: &[u8; 16]) {
        let mut cipher = Cfb8::new(secret, secret);
        cipher.decrypt(&mut self.buffer);
        self.decrypt = Some(cipher);
    }
}

#[derive(Default)]
pub struct PacketEncoder {
    encrypt: Option<Cfb8>,
    pub threshold: Option<usize>,
}

impl PacketEncoder {
    pub fn encode(&mut self, body: &[u8]) -> Result<Vec<u8>> {
        if body.len() > frame::MAX_UNCOMPRESSED {
            return Err(Error::new("Packet exceeds the uncompressed size limit"));
        }
        let mut bytes = frame::encode(body, self.threshold)?;
        if let Some(cipher) = &mut self.encrypt {
            cipher.encrypt(&mut bytes);
        }
        Ok(bytes)
    }

    pub fn enable_encryption(&mut self, secret: &[u8; 16]) {
        self.encrypt = Some(Cfb8::new(secret, secret));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_boundaries_can_split_and_combine_encrypted_compressed_packets() {
        let mut encoder = PacketEncoder::default();
        let mut decoder = PacketDecoder::default();
        encoder.threshold = Some(32);
        decoder.threshold = Some(32);
        encoder.enable_encryption(&[19; 16]);
        decoder.enable_encryption(&[19; 16]);
        let bodies = [vec![7, 1, 2], [vec![9], vec![42; 4096]].concat()];
        let bytes: Vec<_> = bodies
            .iter()
            .flat_map(|b| encoder.encode(b).unwrap())
            .collect();
        let mut packets = Vec::new();
        for chunk in bytes.chunks(3) {
            decoder.push(chunk).unwrap();
            while let Some(packet) = decoder.next_packet().unwrap() {
                packets.push(packet);
            }
        }
        assert_eq!(packets, vec![(7, vec![1, 2]), (9, vec![42; 4096])]);
        assert!(decoder.next_packet().unwrap().is_none());
    }

    #[test]
    fn compression_can_change_between_coalesced_packets() {
        let mut encoder = PacketEncoder::default();
        let mut bytes = encoder.encode(&[3, 32]).unwrap();
        encoder.threshold = Some(32);
        bytes.extend(encoder.encode(&[4, 5, 6]).unwrap());
        let mut decoder = PacketDecoder::default();
        decoder.push(&bytes).unwrap();
        assert_eq!(decoder.next_packet().unwrap(), Some((3, vec![32])));
        decoder.threshold = Some(32);
        assert_eq!(decoder.next_packet().unwrap(), Some((4, vec![5, 6])));
    }

    #[test]
    fn rejects_oversized_queues_and_malformed_frame_prefixes() {
        let mut decoder = PacketDecoder::default();
        assert!(decoder.push(&vec![0; MAX_BUFFERED_BYTES + 1]).is_err());
        decoder.push(&[0x80, 0x80, 0x80]).unwrap();
        assert!(decoder.next_packet().is_err());
    }

    #[test]
    fn encryption_boundary_decrypts_already_buffered_bytes() {
        let mut encoder = PacketEncoder::default();
        encoder.enable_encryption(&[27; 16]);
        let bytes = encoder.encode(&[5, 8, 13]).unwrap();
        let mut decoder = PacketDecoder::default();
        decoder.push(&bytes).unwrap();
        decoder.enable_encryption(&[27; 16]);
        assert_eq!(decoder.next_packet().unwrap(), Some((5, vec![8, 13])));
    }
}
