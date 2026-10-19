//! Bounded BGRA row copy and DXGI clockwise rotation into desktop coordinates.
use crate::{fail, Result};
#[derive(Clone, Copy, Debug)]
pub enum Rotation {
    Identity,
    Clockwise90,
    Clockwise180,
    Clockwise270,
}
impl Rotation {
    pub fn dimensions(self, width: u32, height: u32) -> (u32, u32) {
        match self {
            Self::Identity | Self::Clockwise180 => (width, height),
            Self::Clockwise90 | Self::Clockwise270 => (height, width),
        }
    }
}
pub fn copy_bgra(
    source: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    rotation: Rotation,
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return fail("invalid capture dimensions");
    }
    let row = width as usize * 4;
    let height = height as usize;
    let width = width as usize;
    let end = stride
        .checked_mul(height - 1)
        .and_then(|n| n.checked_add(row))
        .ok_or("capture stride overflow")?;
    if stride < row || source.len() < end {
        return fail("invalid capture row storage");
    }
    let mut output = vec![0u8; row * height];
    if matches!(rotation, Rotation::Identity) {
        for (y, out) in output.chunks_exact_mut(row).enumerate() {
            out.copy_from_slice(&source[y * stride..y * stride + row]);
        }
        return Ok(output);
    }
    let (out_width, _) = rotation.dimensions(width as u32, height as u32);
    // 32x32 tiles keep both the source rows and transposed destination columns
    // in cache rather than walking a whole portrait column at a time.
    for tile_y in (0..height).step_by(32) {
        for tile_x in (0..width).step_by(32) {
            for y in tile_y..height.min(tile_y + 32) {
                for x in tile_x..width.min(tile_x + 32) {
                    let (dx, dy) = match rotation {
                        Rotation::Clockwise90 => (height - 1 - y, x),
                        Rotation::Clockwise180 => (width - 1 - x, height - 1 - y),
                        Rotation::Clockwise270 => (y, width - 1 - x),
                        Rotation::Identity => unreachable!(),
                    };
                    let from = y * stride + x * 4;
                    let to = (dy * out_width as usize + dx) * 4;
                    output[to..to + 4].copy_from_slice(&source[from..from + 4]);
                }
            }
        }
    }
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn labels(bytes: &[u8]) -> Vec<u8> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                assert_eq!(&p[1..], &[99, 100, 255]);
                p[0]
            })
            .collect()
    }
    #[test]
    fn portrait_and_flipped_outputs_match_desktop_corner_coordinates() {
        // Raw 3x2, padded rows. Every BGRA component must survive unchanged.
        let mut raw = Vec::new();
        for row in [[1, 2, 3], [4, 5, 6]] {
            for label in row {
                raw.extend([label, 99, 100, 255]);
            }
            raw.extend([0xfe; 4]);
        }
        for (rotation, expected) in [
            (Rotation::Identity, vec![1, 2, 3, 4, 5, 6]),
            (Rotation::Clockwise90, vec![4, 1, 5, 2, 6, 3]),
            (Rotation::Clockwise180, vec![6, 5, 4, 3, 2, 1]),
            (Rotation::Clockwise270, vec![3, 6, 2, 5, 1, 4]),
        ] {
            assert_eq!(
                labels(&copy_bgra(&raw, 3, 2, 16, rotation).unwrap()),
                expected
            );
        }
        assert_eq!(Rotation::Clockwise90.dimensions(1920, 1080), (1080, 1920));
    }
    #[test]
    fn tiled_transposes_preserve_all_pixels_across_tile_boundaries() {
        let (w, h) = (34, 66);
        let raw: Vec<_> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let portrait = copy_bgra(&raw, w, h, w as usize * 4, Rotation::Clockwise90).unwrap();
        let back = copy_bgra(&portrait, h, w, h as usize * 4, Rotation::Clockwise270).unwrap();
        assert_eq!(back, raw);
        let flipped = copy_bgra(&raw, w, h, w as usize * 4, Rotation::Clockwise180).unwrap();
        assert_eq!(
            copy_bgra(&flipped, w, h, w as usize * 4, Rotation::Clockwise180).unwrap(),
            raw
        );
    }
    #[test]
    fn malformed_dimensions_stride_and_truncated_final_row_are_rejected() {
        for (w, h, stride) in [
            (0, 1, 4),
            (1, 0, 4),
            (8193, 1, 32772),
            (2, 2, 4),
            (2, 2, usize::MAX),
        ] {
            assert!(copy_bgra(&[0; 16], w, h, stride, Rotation::Identity).is_err());
        }
        assert!(copy_bgra(&[0; 15], 2, 2, 8, Rotation::Identity).is_err());
        assert!(copy_bgra(&[0; 20], 2, 2, 12, Rotation::Identity).is_ok());
    }
}
