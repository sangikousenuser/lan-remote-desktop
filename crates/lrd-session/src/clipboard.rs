use crate::{
    fail,
    wire::{Packet, CLIPBOARD},
    Result,
};
use sha2::{Digest, Sha256};
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
pub fn validate(format: u8, data: &[u8]) -> Result<()> {
    if data.len() > MAX_BYTES {
        return fail("clipboard size limit");
    }
    match format {
        0 => {
            let text = std::str::from_utf8(data)?;
            if text.contains('\0') {
                return fail("clipboard text contains NUL");
            }
        }
        1 => {
            if data.len() < 33 || &data[..8] != b"\x89PNG\r\n\x1a\n" || &data[12..16] != b"IHDR" {
                return fail("PNG clipboard format required");
            }
            let w = u32::from_be_bytes(data[16..20].try_into()?);
            let h = u32::from_be_bytes(data[20..24].try_into()?);
            if w == 0 || h == 0 || w > 8192 || h > 8192 || w as u64 * h as u64 > 32 * 1024 * 1024 {
                return fail("clipboard image dimensions");
            }
        }
        _ => return fail("unsupported clipboard format"),
    }
    Ok(())
}
pub fn packet(format: u8, data: &[u8]) -> Result<Packet> {
    validate(format, data)?;
    let mut payload = vec![format];
    payload.extend(Sha256::digest(data));
    payload.extend(data);
    Ok(Packet {
        kind: CLIPBOARD,
        payload,
    })
}
pub fn unpack(payload: &[u8]) -> Result<(u8, &[u8])> {
    if payload.len() < 33 {
        return fail("truncated clipboard update");
    }
    let data = &payload[33..];
    validate(payload[0], data)?;
    if Sha256::digest(data).as_slice() != &payload[1..33] {
        return fail("clipboard digest mismatch");
    }
    Ok((payload[0], data))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_forged_format_size_and_digest() {
        let p = packet(0, "日本語".as_bytes()).unwrap();
        assert_eq!(unpack(&p.payload).unwrap(), (0, "日本語".as_bytes()));
        let mut bad = p.payload.clone();
        bad[1] ^= 1;
        assert!(unpack(&bad).is_err());
        assert!(packet(0, b"abc\0secret").is_err());
        assert!(packet(1, b"not an image").is_err());
    }
}
