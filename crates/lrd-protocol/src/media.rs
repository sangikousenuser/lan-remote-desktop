//! Bounded, transport-independent media framing; encryption belongs to the transport.
use crate::MAJOR_VERSION;
use std::{collections::BTreeMap, time::Duration};

pub const HEADER_BYTES: usize = 36;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_FRAGMENTS: usize = 8192;
pub const MAX_IN_FLIGHT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Video = 1,
    Audio = 2,
    Input = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Version,
    Kind,
    Flags,
    Fragments,
    Size,
    Session,
    Inconsistent,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet<'a> {
    pub kind: Kind,
    pub flags: u16,
    pub session_id: u64,
    pub stream_id: u32,
    pub sequence: u64,
    pub fragment_index: u16,
    pub fragment_count: u16,
    pub capture_time_us: u64,
    pub payload: &'a [u8],
}

impl Packet<'_> {
    fn validate(&self) -> Result<(), Error> {
        if self.session_id == 0 {
            return Err(Error::Session);
        }
        if self.flags & !3 != 0 {
            return Err(Error::Flags);
        }
        if self.fragment_count == 0
            || usize::from(self.fragment_count) > MAX_FRAGMENTS
            || self.fragment_index >= self.fragment_count
        {
            return Err(Error::Fragments);
        }
        if self.payload.is_empty() || self.payload.len() > MAX_FRAME_BYTES {
            return Err(Error::Size);
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut out = Vec::with_capacity(HEADER_BYTES + self.payload.len());
        out.extend([MAJOR_VERSION, self.kind as u8]);
        out.extend(self.flags.to_be_bytes());
        out.extend(self.session_id.to_be_bytes());
        out.extend(self.stream_id.to_be_bytes());
        out.extend(self.sequence.to_be_bytes());
        out.extend(self.fragment_index.to_be_bytes());
        out.extend(self.fragment_count.to_be_bytes());
        out.extend(self.capture_time_us.to_be_bytes());
        out.extend(self.payload);
        Ok(out)
    }
}

pub fn decode(bytes: &[u8]) -> Result<Packet<'_>, Error> {
    if bytes.len() < HEADER_BYTES {
        return Err(Error::Truncated);
    }
    if bytes[0] != MAJOR_VERSION {
        return Err(Error::Version);
    }
    let kind = match bytes[1] {
        1 => Kind::Video,
        2 => Kind::Audio,
        3 => Kind::Input,
        _ => return Err(Error::Kind),
    };
    let packet = Packet {
        kind,
        flags: u16::from_be_bytes(bytes[2..4].try_into().unwrap()),
        session_id: u64::from_be_bytes(bytes[4..12].try_into().unwrap()),
        stream_id: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
        sequence: u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
        fragment_index: u16::from_be_bytes(bytes[24..26].try_into().unwrap()),
        fragment_count: u16::from_be_bytes(bytes[26..28].try_into().unwrap()),
        capture_time_us: u64::from_be_bytes(bytes[28..36].try_into().unwrap()),
        payload: &bytes[36..],
    };
    packet.validate()?;
    Ok(packet)
}

struct Pending {
    first_seen: Duration,
    flags: u16,
    capture_time_us: u64,
    parts: Vec<Option<Vec<u8>>>,
    received: usize,
    bytes: usize,
}

/// One instance per authenticated session, media kind and monitor/stream.
pub struct Reassembler {
    session_id: u64,
    kind: Kind,
    stream_id: u32,
    pending: BTreeMap<u64, Pending>,
    newest_complete: Option<u64>,
    timeout: Duration,
}

impl Reassembler {
    pub fn new(session_id: u64, kind: Kind, stream_id: u32, timeout: Duration) -> Self {
        Self {
            session_id,
            kind,
            stream_id,
            pending: BTreeMap::new(),
            newest_complete: None,
            timeout,
        }
    }
    pub fn expire(&mut self, now: Duration) -> usize {
        let before = self.pending.len();
        self.pending.retain(|_, p| {
            now.checked_sub(p.first_seen)
                .is_some_and(|elapsed| elapsed <= self.timeout)
        });
        before - self.pending.len()
    }
    pub fn push(&mut self, packet: Packet<'_>, now: Duration) -> Result<Option<Vec<u8>>, Error> {
        packet.validate()?;
        if packet.session_id != self.session_id
            || packet.kind != self.kind
            || packet.stream_id != self.stream_id
        {
            return Err(Error::Session);
        }
        if self
            .newest_complete
            .is_some_and(|last| packet.sequence <= last)
        {
            return Err(Error::Stale);
        }
        self.expire(now);
        if !self.pending.contains_key(&packet.sequence) && self.pending.len() == MAX_IN_FLIGHT {
            let oldest = *self.pending.first_key_value().unwrap().0;
            if packet.sequence < oldest {
                return Err(Error::Stale);
            }
            self.pending.remove(&oldest);
        }
        let p = self
            .pending
            .entry(packet.sequence)
            .or_insert_with(|| Pending {
                first_seen: now,
                flags: packet.flags,
                capture_time_us: packet.capture_time_us,
                parts: vec![None; usize::from(packet.fragment_count)],
                received: 0,
                bytes: 0,
            });
        if p.parts.len() != usize::from(packet.fragment_count)
            || p.flags != packet.flags
            || p.capture_time_us != packet.capture_time_us
        {
            return Err(Error::Inconsistent);
        }
        let slot = &mut p.parts[usize::from(packet.fragment_index)];
        if let Some(existing) = slot {
            return if existing == packet.payload {
                Ok(None)
            } else {
                Err(Error::Inconsistent)
            };
        }
        if p.bytes + packet.payload.len() > MAX_FRAME_BYTES {
            self.pending.remove(&packet.sequence);
            return Err(Error::Size);
        }
        *slot = Some(packet.payload.to_vec());
        p.bytes += packet.payload.len();
        p.received += 1;
        if p.received != p.parts.len() {
            return Ok(None);
        }
        let complete = self.pending.remove(&packet.sequence).unwrap();
        let mut frame = Vec::with_capacity(complete.bytes);
        for part in complete.parts {
            frame.extend(part.unwrap());
        }
        self.newest_complete = Some(packet.sequence);
        self.pending
            .retain(|sequence, _| *sequence > packet.sequence);
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(index: u16, sequence: u64, data: &[u8]) -> Packet<'_> {
        Packet {
            kind: Kind::Video,
            flags: 1,
            session_id: 42,
            stream_id: 1,
            sequence,
            fragment_index: index,
            fragment_count: 2,
            capture_time_us: 100,
            payload: data,
        }
    }
    fn reassembler() -> Reassembler {
        Reassembler::new(42, Kind::Video, 1, Duration::from_millis(100))
    }
    #[test]
    fn golden_header_and_every_truncated_length() {
        let p = packet(0, 1, b"hello");
        let bytes = p.encode().unwrap();
        assert_eq!(&bytes[..4], &[1, 1, 0, 1]);
        assert_eq!(bytes.len(), 41);
        assert_eq!(decode(&bytes), Ok(p));
        for length in 0..=HEADER_BYTES {
            assert!(decode(&bytes[..length]).is_err());
        }
    }
    #[test]
    fn out_of_order_and_duplicates() {
        let mut r = reassembler();
        assert_eq!(r.push(packet(1, 1, b"world"), Duration::ZERO), Ok(None));
        assert_eq!(r.push(packet(1, 1, b"world"), Duration::ZERO), Ok(None));
        assert_eq!(
            r.push(packet(0, 1, b"hello"), Duration::ZERO),
            Ok(Some(b"helloworld".to_vec()))
        );
        assert_eq!(
            r.push(packet(0, 1, b"hello"), Duration::ZERO),
            Err(Error::Stale)
        );
    }
    #[test]
    fn forged_and_inconsistent_fragments() {
        let mut r = reassembler();
        let mut p = packet(0, 1, b"a");
        p.session_id = 12;
        assert_eq!(r.push(p, Duration::ZERO), Err(Error::Session));
        r.push(packet(0, 1, b"a"), Duration::ZERO).unwrap();
        assert_eq!(
            r.push(packet(0, 1, b"b"), Duration::ZERO),
            Err(Error::Inconsistent)
        );
        let mut p = packet(1, 1, b"a");
        p.fragment_count = 3;
        assert_eq!(r.push(p, Duration::ZERO), Err(Error::Inconsistent));
    }
    #[test]
    fn bounded_pending_and_timeout() {
        let mut r = reassembler();
        for n in 1..=100 {
            r.push(packet(0, n, b"a"), Duration::ZERO).unwrap();
        }
        assert_eq!(r.pending.len(), MAX_IN_FLIGHT);
        assert_eq!(r.expire(Duration::from_millis(101)), MAX_IN_FLIGHT);
        assert_eq!(r.pending.len(), 0);
    }
    #[test]
    fn invalid_enums_counts_and_lengths() {
        let bytes = packet(0, 1, b"a").encode().unwrap();
        for (offset, value, error) in [
            (0, 2, Error::Version),
            (1, 9, Error::Kind),
            (3, 4, Error::Flags),
            (25, 2, Error::Fragments),
            (27, 0, Error::Fragments),
        ] {
            let mut bad = bytes.clone();
            bad[offset] = value;
            assert_eq!(decode(&bad), Err(error));
        }
    }
}
