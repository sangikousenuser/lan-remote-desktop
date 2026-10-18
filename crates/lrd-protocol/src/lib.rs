//! Minimal unencrypted **framing codec** for internal tests only.
//! Do NOT send sensitive data over a network until authenticated transport exists.

pub mod media;

use std::io::{self, Read, Write};

pub const MAJOR_VERSION: u8 = 1;
pub const MINOR_VERSION: u8 = 0;
pub const HEADER_WITHOUT_LENGTH: usize = 20;
pub const PREFIX_LEN: usize = 4;
pub const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    Truncated,
    LengthMismatch,
    FrameTooLarge,
    UnsupportedVersion,
    PayloadTooLarge,
    UnknownMessageType,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ControlFrame<'a> {
    pub message_type: u16,
    pub request_id: u64,
    pub session_id: u64,
    pub payload: &'a [u8],
}

pub fn encode(frame: &ControlFrame<'_>) -> Result<Vec<u8>, ParseError> {
    if !(1..=0x15).contains(&frame.message_type) {
        return Err(ParseError::UnknownMessageType);
    }
    let length = HEADER_WITHOUT_LENGTH
        .checked_add(frame.payload.len())
        .ok_or(ParseError::PayloadTooLarge)?;
    if length > MAX_CONTROL_FRAME_BYTES {
        return Err(ParseError::FrameTooLarge);
    }
    let mut bytes = Vec::with_capacity(PREFIX_LEN + length);
    bytes.extend_from_slice(&(length as u32).to_be_bytes());
    bytes.push(MAJOR_VERSION);
    bytes.push(MINOR_VERSION);
    bytes.extend_from_slice(&frame.message_type.to_be_bytes());
    bytes.extend_from_slice(&frame.request_id.to_be_bytes());
    bytes.extend_from_slice(&frame.session_id.to_be_bytes());
    bytes.extend_from_slice(frame.payload);
    Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> Result<ControlFrame<'_>, ParseError> {
    if bytes.len() < PREFIX_LEN {
        return Err(ParseError::Truncated);
    }
    let encoded_len = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if encoded_len > MAX_CONTROL_FRAME_BYTES {
        return Err(ParseError::FrameTooLarge);
    }
    if encoded_len < HEADER_WITHOUT_LENGTH {
        return Err(ParseError::Truncated);
    }
    if bytes.len() != PREFIX_LEN + encoded_len {
        return Err(ParseError::LengthMismatch);
    }
    if bytes[4] != MAJOR_VERSION {
        return Err(ParseError::UnsupportedVersion);
    }
    let message_type = u16::from_be_bytes(bytes[6..8].try_into().unwrap());
    if !(1..=0x15).contains(&message_type) {
        return Err(ParseError::UnknownMessageType);
    }
    let request_id = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
    let session_id = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
    Ok(ControlFrame {
        message_type,
        request_id,
        session_id,
        payload: &bytes[24..],
    })
}

/// Read exactly one bounded frame. Stream must already be authenticated and encrypted.
pub fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut prefix = [0; PREFIX_LEN];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if !(HEADER_WITHOUT_LENGTH..=MAX_CONTROL_FRAME_BYTES).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid control length",
        ));
    }
    let mut bytes = vec![0; PREFIX_LEN + length];
    bytes[..PREFIX_LEN].copy_from_slice(&prefix);
    reader.read_exact(&mut bytes[PREFIX_LEN..])?;
    decode(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid control frame"))?;
    Ok(bytes)
}

pub fn write_frame(writer: &mut impl Write, frame: &ControlFrame<'_>) -> io::Result<()> {
    let bytes = encode(frame)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid control frame"))?;
    writer.write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let frame = ControlFrame {
            message_type: 10,
            request_id: 42,
            session_id: 99,
            payload: b"hello",
        };
        let encoded = encode(&frame).unwrap();
        assert_eq!(decode(&encoded).unwrap(), frame);
        assert_eq!(encoded.len(), 29);
    }

    #[test]
    fn reject_truncated_frame() {
        assert_eq!(decode(&[0, 0, 0]).unwrap_err(), ParseError::Truncated);
    }

    #[test]
    fn reject_length_mismatch() {
        let mut encoded = encode(&ControlFrame {
            message_type: 1,
            request_id: 0,
            session_id: 0,
            payload: b"abc",
        })
        .unwrap();
        encoded.pop();
        assert_eq!(decode(&encoded).unwrap_err(), ParseError::LengthMismatch);
    }

    #[test]
    fn reject_unknown_major_version() {
        let mut encoded = encode(&ControlFrame {
            message_type: 1,
            request_id: 0,
            session_id: 0,
            payload: b"",
        })
        .unwrap();
        encoded[4] = 55;
        assert_eq!(
            decode(&encoded).unwrap_err(),
            ParseError::UnsupportedVersion
        );
    }

    #[test]
    fn reject_oversized_frame() {
        let invalid = ControlFrame {
            message_type: 1,
            request_id: 0,
            session_id: 0,
            payload: &vec![0; MAX_CONTROL_FRAME_BYTES],
        };
        assert_eq!(encode(&invalid).unwrap_err(), ParseError::FrameTooLarge);
    }

    #[test]
    fn streaming_reader_preserves_frame_boundaries() {
        let frame = ControlFrame {
            message_type: 8,
            request_id: 1,
            session_id: 2,
            payload: b"heartbeat",
        };
        let mut stream = Vec::new();
        write_frame(&mut stream, &frame).unwrap();
        write_frame(&mut stream, &frame).unwrap();
        let mut stream = std::io::Cursor::new(stream);
        for _ in 0..2 {
            assert_eq!(decode(&read_frame(&mut stream).unwrap()).unwrap(), frame);
        }
        assert_eq!(
            read_frame(&mut stream).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn oversized_prefix_rejected_before_reading_payload() {
        let mut stream = std::io::Cursor::new(u32::MAX.to_be_bytes());
        assert_eq!(
            read_frame(&mut stream).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn unknown_required_message_type_rejected() {
        let frame = ControlFrame {
            message_type: 0,
            request_id: 0,
            session_id: 0,
            payload: &[],
        };
        assert_eq!(encode(&frame), Err(ParseError::UnknownMessageType));
        let mut bytes = encode(&ControlFrame {
            message_type: 1,
            ..frame
        })
        .unwrap();
        bytes[6..8].copy_from_slice(&0xffffu16.to_be_bytes());
        assert_eq!(decode(&bytes), Err(ParseError::UnknownMessageType));
    }

    #[test]
    fn deterministic_arbitrary_byte_corpus_never_panics() {
        let mut seed = 0x4c5244u64;
        for length in 0..2048 {
            let bytes: Vec<_> = (0..length)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            let _ = decode(&bytes);
            let _ = media::decode(&bytes);
        }
    }
}
