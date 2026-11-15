//! Disjoint preview rows use bounded CPU parallelism while effect workers retain headroom.
pub(crate) mod mask_thumbnail;
pub(crate) mod native_preview;
pub(crate) mod region;
pub(crate) mod sample;
pub(crate) mod magnetic;
use crate::raster::{Pixel, Pixel16};
pub(crate) type Check<'a> = dyn Fn() -> Result<(), String> + Sync + 'a;

pub(crate) fn rgba8(width: u32, height: u32, sample: impl Fn(u32, u32) -> Pixel + Sync) -> Vec<u8> {
    rgba(width, height, &|| Ok(()), sample).expect("Valid full preview dimensions")
}

pub(crate) fn rgba16(
    width: u32,
    height: u32,
    sample: impl Fn(u32, u32) -> Pixel16 + Sync,
) -> Vec<u16> {
    rgba(width, height, &|| Ok(()), sample).expect("Valid full preview dimensions")
}

pub(crate) fn rgba<T: Copy + Default + Send>(
    width: u32,
    height: u32,
    check: &Check<'_>,
    sample: impl Fn(u32, u32) -> [T; 4] + Sync,
) -> Result<Vec<T>, String> {
    check()?;
    let mut data = vec![T::default(); width as usize * height as usize * 4];
    if width != 0 && height != 0 {
        area(&mut data, width, [0, 0, width, height], check, sample)?;
    }
    Ok(data)
}

/// Update a rectangle on the original sample grid, leaving all other bytes intact.
pub(crate) fn area8(
    bytes: &mut [u8],
    width: u32,
    bounds: [u32; 4],
    check: &Check<'_>,
    sample: impl Fn(u32, u32) -> Pixel + Sync,
) -> Result<(), String> {
    area(bytes, width, bounds, check, sample)
}

fn area<T: Copy + Send>(
    data: &mut [T],
    width: u32,
    bounds: [u32; 4],
    check: &Check<'_>,
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
        let mut jobs = Vec::new();
        for (index, chunk) in region.chunks_mut(rows * stride).enumerate() {
            let sample = &sample;
            let mut fill = move || {
                for (row, data) in chunk.chunks_mut(stride).enumerate() {
                    check()?;
                    let y = bounds[1] + (index * rows + row) as u32;
                    for (x, pixel) in data[bounds[0] as usize * 4..bounds[2] as usize * 4]
                        .chunks_exact_mut(4)
                        .enumerate()
                    {
                        pixel.copy_from_slice(&sample(bounds[0] + x as u32, y));
                    }
                }
                Ok::<_, String>(())
            };
            if workers == 1 {
                fill()?;
            } else {
                jobs.push(scope.spawn(fill));
            }
        }
        for job in jobs {
            job.join().expect("Preview row worker panicked")?;
        }
        check()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn superseded_rgba_rows_stop_without_returning_a_partial_frame() {
        let edge = 512;
        for depth in [8, 16] {
            let samples = AtomicUsize::new(0);
            let check = || {
                if samples.load(Ordering::SeqCst) == 0 {
                    Ok(())
                } else {
                    Err("Superseded frame".into())
                }
            };
            let note_sample = || { samples.fetch_add(1, Ordering::SeqCst); };
            let result = if depth == 8 {
                rgba(edge, edge, &check, |_, _| { note_sample(); [19u8; 4] }).map(drop)
            } else {
                rgba(edge, edge, &check, |_, _| { note_sample(); [23459u16; 4] }).map(drop)
            };
            assert_eq!(result.unwrap_err(), "Superseded frame");
            assert!((1..=edge as usize * 4).contains(&samples.load(Ordering::SeqCst)));
        }
    }

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
        area8(&mut got, w, bounds, &|| Ok(()), sample).unwrap();
        assert_eq!(got, expected);
        let before = got.clone();
        assert!(area8(&mut got, w, [0, 0, w, h + 1], &|| Ok(()), sample).is_err());
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
