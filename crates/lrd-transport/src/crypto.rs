//! AES-256-GCM datagrams. Keys come from a mutually authenticated TLS exporter.
//! Each direction has its own key. Sequence numbers never wrap or reset under
//! the same key; the replay bitmap changes only after a tag verifies.
use lrd_session::{fail, Result};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
pub const MAX_PAYLOAD: usize = 1100;
const HEADER: usize = 24;
const TAG: usize = 16;
pub const MAX_DATAGRAM: usize = HEADER + MAX_PAYLOAD + TAG;
pub struct Sealer {
    key: LessSafeKey,
    session: u64,
    next: u64,
}
pub struct Opener {
    key: LessSafeKey,
    session: u64,
    replay: Replay,
}
#[derive(Default)]
struct Replay {
    highest: u64,
    bits: [u64; 16],
}
impl Replay {
    fn accepts(&self, sequence: u64) -> bool {
        sequence != 0
            && (sequence > self.highest || {
                let distance = self.highest - sequence;
                distance < 1024
                    && self.bits[distance as usize / 64] & (1u64 << (distance % 64)) == 0
            })
    }
    fn record(&mut self, sequence: u64) {
        if sequence > self.highest {
            let delta = sequence - self.highest;
            if delta >= 1024 {
                self.bits = [0; 16];
            } else {
                let words = delta as usize / 64;
                let shift = delta as usize % 64;
                for i in (0..16).rev() {
                    self.bits[i] = if i < words {
                        0
                    } else {
                        let mut value = self.bits[i - words] << shift;
                        if shift > 0 && i > words {
                            value |= self.bits[i - words - 1] >> (64 - shift);
                        }
                        value
                    };
                }
            }
            self.highest = sequence;
        }
        let distance = (self.highest - sequence) as usize;
        self.bits[distance / 64] |= 1u64 << (distance % 64);
    }
}
fn key(bytes: &[u8]) -> Result<LessSafeKey> {
    Ok(LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, bytes)
            .map_err(|_| std::io::Error::other("invalid media key"))?,
    ))
}
fn nonce(sequence: u64) -> Nonce {
    let mut bytes = [0; 12];
    bytes[4..].copy_from_slice(&sequence.to_be_bytes());
    Nonce::assume_unique_for_key(bytes)
}
pub fn directional(exporter: &[u8; 72], client: bool) -> Result<(Sealer, Opener)> {
    let session = u64::from_be_bytes(exporter[64..].try_into()?).max(1);
    let (send, receive) = if client {
        (&exporter[..32], &exporter[32..64])
    } else {
        (&exporter[32..64], &exporter[..32])
    };
    Ok((
        Sealer {
            key: key(send)?,
            session,
            next: 1,
        },
        Opener {
            key: key(receive)?,
            session,
            replay: Replay::default(),
        },
    ))
}
impl Sealer {
    pub fn seal(&mut self, kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
        if payload.is_empty()
            || payload.len() > MAX_PAYLOAD
            || !matches!(kind, 1..=3)
            || self.next == u64::MAX
        {
            return fail("media datagram kind/length/key lifetime limit");
        }
        let sequence = self.next;
        self.next += 1;
        let mut header = b"LRDU".to_vec();
        header.extend([1, kind]);
        header.extend(self.session.to_be_bytes());
        header.extend(sequence.to_be_bytes());
        header.extend((payload.len() as u16).to_be_bytes());
        let mut bytes = payload.to_vec();
        self.key
            .seal_in_place_append_tag(nonce(sequence), Aad::from(&header), &mut bytes)
            .map_err(|_| std::io::Error::other("media encryption failed"))?;
        header.extend(bytes);
        Ok(header)
    }
}
impl Opener {
    pub fn open(&mut self, bytes: &[u8]) -> Result<(u8, Vec<u8>)> {
        if bytes.len() < HEADER + TAG
            || bytes.len() > MAX_DATAGRAM
            || &bytes[..4] != b"LRDU"
            || bytes[4] != 1
            || !matches!(bytes[5], 1..=3)
            || u64::from_be_bytes(bytes[6..14].try_into()?) != self.session
        {
            return fail("invalid encrypted datagram header");
        }
        let sequence = u64::from_be_bytes(bytes[14..22].try_into()?);
        let size = u16::from_be_bytes(bytes[22..24].try_into()?) as usize;
        if size == 0
            || size > MAX_PAYLOAD
            || size + HEADER + TAG != bytes.len()
            || !self.replay.accepts(sequence)
        {
            return fail("stale or malformed media datagram");
        }
        let mut ciphertext = bytes[HEADER..].to_vec();
        let plaintext = self
            .key
            .open_in_place(
                nonce(sequence),
                Aad::from(&bytes[..HEADER]),
                &mut ciphertext,
            )
            .map_err(|_| std::io::Error::other("media authentication rejected"))?;
        let payload = plaintext.to_vec();
        self.replay.record(sequence);
        Ok((bytes[5], payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn exporter() -> [u8; 72] {
        let mut e = [0; 72];
        for (i, v) in e.iter_mut().enumerate() {
            *v = (i + 1) as u8;
        }
        e
    }
    #[test]
    fn reordered_packets_verify_once_and_forged_future_sequence_cannot_advance_window() {
        let (mut tx, _) = directional(&exporter(), true).unwrap();
        let (_, mut rx) = directional(&exporter(), false).unwrap();
        let packets: Vec<_> = (0..1100).map(|_| tx.seal(1, b"frame").unwrap()).collect();
        assert_eq!(rx.open(&packets[100]).unwrap().1, b"frame");
        assert!(rx.open(&packets[99]).is_ok());
        assert!(rx.open(&packets[100]).is_err());
        let mut forged = packets[101].clone();
        forged[14..22].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(rx.open(&forged).is_err());
        assert!(rx.open(&packets[101]).is_ok());
        assert!(rx.open(&packets[1099]).is_ok());
        assert!(rx.open(&packets[1]).is_err());
        assert!(rx.open(&packets[1098]).is_ok());
    }
    #[test]
    fn wrong_direction_session_tag_size_and_nonce_exhaustion_are_rejected() {
        let (mut tx, mut wrong_direction) = directional(&exporter(), true).unwrap();
        let packet = tx.seal(2, b"audio").unwrap();
        assert!(wrong_direction.open(&packet).is_err());
        for offset in 0..packet.len() {
            let mut bytes = packet.clone();
            bytes[offset] ^= 1;
            let (_, mut rx) = directional(&exporter(), false).unwrap();
            assert!(rx.open(&bytes).is_err(), "offset {offset}");
        }
        assert!(tx.seal(0, b"x").is_err());
        assert!(tx.seal(1, &[]).is_err());
        assert!(tx.seal(1, &vec![0; MAX_PAYLOAD + 1]).is_err());
        tx.next = u64::MAX;
        assert!(tx.seal(1, b"x").is_err());
    }
}
