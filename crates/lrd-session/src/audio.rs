use crate::{fail, Result};
pub fn validate(bytes: &[u8]) -> Result<(u32, u8, &[u8])> {
    if bytes.len() < 13 || bytes.len() > 65536 {
        return fail("audio packet length");
    }
    let rate = u32::from_be_bytes(bytes[..4].try_into()?);
    let channels = bytes[4];
    let pcm = &bytes[13..];
    if !(8000..=192000).contains(&rate)
        || !(1..=2).contains(&channels)
        || !pcm.len().is_multiple_of(channels as usize * 4)
    {
        return fail("invalid PCM configuration");
    }
    Ok((rate, channels, pcm))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_aligned_pcm() {
        for n in 0..13 {
            assert!(validate(&vec![0; n]).is_err());
        }
        let mut p = 48000u32.to_be_bytes().to_vec();
        p.push(2);
        p.extend(0u64.to_be_bytes());
        p.extend([0u8; 8]);
        assert_eq!(validate(&p).unwrap().0, 48000);
        p.push(0);
        assert!(validate(&p).is_err());
        p.truncate(13);
        p[4] = 255;
        assert!(validate(&p).is_err());
    }
}
