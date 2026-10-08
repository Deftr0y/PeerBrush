//! Disjoint preview rows use bounded CPU parallelism while effect workers retain headroom.
use crate::raster::{Pixel, Pixel16};
pub(crate) fn rgba8(width: u32, height: u32, sample: impl Fn(u32, u32) -> Pixel + Sync) -> Vec<u8> {
    let mut bytes = vec![0; width as usize * height as usize * 4];
    if width != 0 && height != 0 {
        area8(&mut bytes, width, [0, 0, width, height], sample)
            .expect("Valid full preview dimensions");
    }
    bytes
}

pub(crate) fn rgba16(
    width: u32,
    height: u32,
    sample: impl Fn(u32, u32) -> Pixel16 + Sync,
) -> Vec<u16> {
    let mut words = vec![0; width as usize * height as usize * 4];
    if width != 0 && height != 0 {
        area(&mut words, width, [0, 0, width, height], sample)
            .expect("Valid full preview dimensions");
    }
    words
}

/// Update a rectangle on the original sample grid, leaving all other bytes intact.
pub(crate) fn area8(
    bytes: &mut [u8],
    width: u32,
    bounds: [u32; 4],
    sample: impl Fn(u32, u32) -> Pixel + Sync,
) -> Result<(), String> {
    area(bytes, width, bounds, sample)
}

fn area<T: Copy + Send>(
    data: &mut [T],
    width: u32,
    bounds: [u32; 4],
    sample: impl Fn(u32, u32) -> [T; 4] + Sync,
) -> Result<(), String> {
    let stride = (width as usize)
        .checked_mul(4)
        .filter(|&n| n != 0)
        .ok_or("Invalid preview width")?;
    if data.len() % stride != 0
        || bounds[0] > bounds[2]
        || bounds[1] > bounds[3]
        || bounds[2] > width
        || bounds[3] as usize > data.len() / stride
    {
        return Err("Invalid preview update rectangle".into());
    }
    if bounds[0] == bounds[2] || bounds[1] == bounds[3] {
        return Ok(());
    }
    let pixels = u64::from(bounds[2] - bounds[0]) * u64::from(bounds[3] - bounds[1]);
    let workers = if pixels >= 128 * 1024 {
        std::thread::available_parallelism().map_or(1, |n| n.get().min(4))
    } else {
        1
    };
    let rows = ((bounds[3] - bounds[1]) as usize).div_ceil(workers).max(1);
    let region = &mut data[bounds[1] as usize * stride..bounds[3] as usize * stride];
    std::thread::scope(|scope| {
        for (index, chunk) in region.chunks_mut(rows * stride).enumerate() {
            let sample = &sample;
            let mut fill = move || {
                for (row, data) in chunk.chunks_mut(stride).enumerate() {
                    let y = bounds[1] + (index * rows + row) as u32;
                    for (x, pixel) in data[bounds[0] as usize * 4..bounds[2] as usize * 4]
                        .chunks_exact_mut(4)
                        .enumerate()
                    {
                        pixel.copy_from_slice(&sample(bounds[0] + x as u32, y));
                    }
                }
            };
            if workers == 1 {
                fill();
            } else {
                scope.spawn(fill);
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_regions_match_serial_coordinates_and_preserve_outside_pixels() {
        let (w, h) = (517, 523);
        let bounds = [17, 3, 501, 510];
        let sample =
            |x: u32, y: u32| [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 173];
        let mut got = vec![29; w as usize * h as usize * 4];
        let mut expected = got.clone();
        for y in bounds[1]..bounds[3] {
            for x in bounds[0]..bounds[2] {
                let at = ((y * w + x) * 4) as usize;
                expected[at..at + 4].copy_from_slice(&sample(x, y));
            }
        }
        area8(&mut got, w, bounds, sample).unwrap();
        assert_eq!(got, expected);
        let before = got.clone();
        assert!(area8(&mut got, w, [0, 0, w, h + 1], sample).is_err());
        assert_eq!(got, before);
    }
    #[test]
    fn native_full_rows_preserve_all_words_and_tiny_previews_use_the_same_grid() {
        let (w, h) = (519, 263);
        let sample = |x: u32, y: u32| [10001 + x as u16, 30003 + y as u16, 50007, 65535];
        let words = rgba16(w, h, sample);
        for y in 0..h {
            for x in 0..w {
                let at = ((y * w + x) * 4) as usize;
                assert_eq!(&words[at..at + 4], &sample(x, y));
            }
        }
        assert_eq!(
            rgba8(1, 1, |x, y| [x as u8, y as u8, 17, 255]),
            [0, 0, 17, 255]
        );
    }
}
