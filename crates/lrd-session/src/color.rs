//! Full-range BGRA to limited-range BT.709 NV12, matching the MFT attributes.
use crate::{fail, Result};
pub fn bgra_to_nv12(data: &[u8], width: usize, height: usize) -> Result<Vec<u8>> {
    if width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || width > 8192
        || height > 8192
        || data.len() != width * height * 4
    {
        return fail("invalid BGRA image");
    }
    let mut out = vec![0; width * height * 3 / 2];
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 4;
            let b = data[i] as i32;
            let g = data[i + 1] as i32;
            let r = data[i + 2] as i32;
            out[y * width + x] =
                (((11966 * r + 40254 * g + 4064 * b + 32768) >> 16) + 16).clamp(16, 235) as u8;
        }
    }
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let (mut r, mut g, mut b) = (0, 0, 0);
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((y + dy) * width + x + dx) * 4;
                    b += data[i] as i32;
                    g += data[i + 1] as i32;
                    r += data[i + 2] as i32;
                }
            }
            r /= 4;
            g /= 4;
            b /= 4;
            let i = width * height + (y / 2) * width + x;
            out[i] =
                (((-6596 * r - 22189 * g + 28785 * b + 32768) >> 16) + 128).clamp(16, 240) as u8;
            out[i + 1] =
                (((28784 * r - 26145 * g - 2639 * b + 32768) >> 16) + 128).clamp(16, 240) as u8;
        }
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bt709_black_white_and_primaries_have_expected_range() {
        for (bgra, expected) in [
            ([0, 0, 0, 255], [16, 128, 128]),
            ([255, 255, 255, 255], [235, 128, 128]),
            ([0, 0, 255, 255], [63, 102, 240]),
            ([0, 255, 0, 255], [173, 42, 26]),
            ([255, 0, 0, 255], [32, 240, 118]),
        ] {
            let input = bgra.repeat(4);
            let out = bgra_to_nv12(&input, 2, 2).unwrap();
            assert!((out[0] as i16 - expected[0] as i16).abs() <= 1);
            assert!((out[4] as i16 - expected[1] as i16).abs() <= 1);
            assert!((out[5] as i16 - expected[2] as i16).abs() <= 1);
            assert_eq!(&out[..4], &[out[0]; 4]);
        }
    }
    #[test]
    fn malformed_frame_is_rejected() {
        assert!(bgra_to_nv12(&[], 0, 0).is_err());
        assert!(bgra_to_nv12(&[0; 16], 1, 4).is_err());
        assert!(bgra_to_nv12(&[0; 16], 2, 4).is_err());
    }
}
