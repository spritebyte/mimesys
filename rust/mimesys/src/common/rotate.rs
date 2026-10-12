use crate::common::system::Rotation;

pub fn rotate_rgba(src: &[u8], w: usize, h: usize, rot: Rotation) -> (Vec<u8>, usize, usize) {
    match rot {
        Rotation::None => (src.to_vec(), w, h),
        Rotation::CW90 => {
            let mut dst = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = (x * h + (h - 1 - y)) * 4;
                    dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
            (dst, h, w)
        }
        Rotation::CCW90 => {
            let mut dst = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = ((w - 1 - x) * h + y) * 4;
                    dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
            (dst, h, w)
        }
    }
}