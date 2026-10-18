use crate::{fail, Result};
/// Canonicalize Media Foundation's Annex B / length-prefixed samples and avcC
/// configuration records before handing any lengths to VideoToolbox.
pub fn annex_b(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.len() > 8 * 1024 * 1024 {
        return fail("H.264 frame limit");
    }
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        return Ok(bytes.to_vec());
    }
    let mut out = Vec::new();
    if bytes[0] == 1 && bytes.len() >= 7 {
        let mut i = 6;
        let sps = (bytes[5] & 31) as usize;
        for _ in 0..sps {
            read_nal(bytes, &mut i, 2, &mut out)?;
        }
        let pps = *bytes.get(i).ok_or("truncated avcC")? as usize;
        i += 1;
        for _ in 0..pps {
            read_nal(bytes, &mut i, 2, &mut out)?;
        }
        return Ok(out);
    }
    let mut i = 0;
    let mut n = 0;
    while i < bytes.len() {
        read_nal(bytes, &mut i, 4, &mut out)?;
        n += 1;
        if n > 128 {
            return fail("NAL count limit");
        }
    }
    Ok(out)
}
fn read_nal(bytes: &[u8], i: &mut usize, length_bytes: usize, out: &mut Vec<u8>) -> Result<()> {
    if bytes.len().saturating_sub(*i) < length_bytes {
        return fail("truncated NAL length");
    }
    let mut n = 0;
    for b in &bytes[*i..*i + length_bytes] {
        n = (n << 8) | usize::from(*b);
    }
    *i += length_bytes;
    if n == 0 || n > bytes.len().saturating_sub(*i) {
        return fail("invalid NAL length");
    }
    out.extend([0, 0, 0, 1]);
    out.extend(&bytes[*i..*i + n]);
    *i += n;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn avcc_and_configuration_are_bounded() {
        assert_eq!(
            annex_b(&[0, 0, 0, 2, 0x65, 1]).unwrap(),
            [0, 0, 0, 1, 0x65, 1]
        );
        assert!(annex_b(&[255, 255, 255, 255]).is_err());
        assert!(annex_b(&[1, 2, 3, 4, 5, 6, 7]).is_err());
    }
}
