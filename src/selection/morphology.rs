//! Shared extrema kernel and bounded native selection bands.
use crate::large_document::{Reservation, ResidentBudget, ScratchBytes};

pub(super) fn line<T: Copy + Ord + Default>(
    input: &[T],
    radius: usize,
    expand: bool,
    output: &mut [T],
    queue: &mut [usize],
) {
    let (mut head, mut tail, mut end) = (0, 0, 0);
    for (i, value) in output.iter_mut().enumerate() {
        while end < (i + radius + 1).min(input.len()) {
            while tail > head
                && if expand {
                    input[queue[tail - 1]] <= input[end]
                } else {
                    input[queue[tail - 1]] >= input[end]
                }
            {
                tail -= 1;
            }
            queue[tail] = end;
            tail += 1;
            end += 1;
        }
        while tail > head && queue[head] < i.saturating_sub(radius) {
            head += 1;
        }
        *value = if !expand && (i < radius || i + radius >= input.len()) {
            T::default()
        } else {
            input[queue[head]]
        };
    }
}

pub(crate) struct Rows {
    width: usize,
    height: usize,
    radius: usize,
    expand: bool,
    next: usize,
    failed: bool,
    stripe: ScratchBytes,
    input: ScratchBytes,
    horizontal: ScratchBytes,
    column: ScratchBytes,
    output: ScratchBytes,
    queue: Vec<usize>,
    _queue_charge: Reservation,
}
impl Rows {
    pub(crate) fn new(
        width: u32,
        height: u32,
        radius: usize,
        expand: bool,
        pool: &ResidentBudget,
    ) -> Result<Self, String> {
        if width == 0
            || height == 0
            || width > 16000
            || height > 16000
            || !(1..=64).contains(&radius)
        {
            return Err("Invalid selection morphology frame or radius".into());
        }
        let width = width as usize;
        let height = height as usize;
        let side = crate::raster::TILE as usize + 2 * radius;
        let alloc = |bytes| ScratchBytes::new(pool, bytes, bytes).map_err(|e| e.to_string());
        let queue_bytes = side * std::mem::size_of::<usize>();
        let queue_charge = pool
            .reserve_bytes(queue_bytes, queue_bytes)
            .map_err(|e| e.to_string())?;
        let mut queue = Vec::new();
        queue
            .try_reserve_exact(side)
            .map_err(|_| "Selection morphology queue allocation refused")?;
        if queue.capacity() * std::mem::size_of::<usize>() > queue_charge.bytes() {
            return Err("Selection morphology queue exceeds admission".into());
        }
        queue.resize(side, 0);
        Ok(Self {
            width,
            height,
            radius,
            expand,
            next: 0,
            failed: false,
            stripe: alloc(width * height.min(crate::raster::TILE as usize))?,
            input: alloc(side * side)?,
            horizontal: alloc(side * side)?,
            column: alloc(side)?,
            output: alloc(side)?,
            queue,
            _queue_charge: queue_charge,
        })
    }
    /// Halo readers supply scalar8 values, including zero outside the canvas.
    /// A failed or out-of-order stream cannot resume with partly prepared bands.
    pub(crate) fn row(
        &mut self,
        y: u32,
        row: &mut [u8],
        check: &dyn Fn() -> Result<(), String>,
        mut read: impl FnMut([i32; 4], &mut [u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let invalid = self.failed
            || y as usize != self.next
            || self.next >= self.height
            || row.len() != self.width;
        self.failed = true;
        if invalid {
            return Err("Selection morphology rows differ from their admitted frame".into());
        }
        check()?;
        let tile = crate::raster::TILE as usize;
        let y = y as usize;
        if y % tile == 0 {
            let h = tile.min(self.height - y);
            let sh = h + 2 * self.radius;
            for left in (0..self.width).step_by(tile) {
                check()?;
                let w = tile.min(self.width - left);
                let sw = w + 2 * self.radius;
                let area = [
                    left as i32 - self.radius as i32,
                    y as i32 - self.radius as i32,
                    (left + w + self.radius) as i32,
                    (y + h + self.radius) as i32,
                ];
                read(area, &mut self.input[..sw * sh])?;
                for yy in 0..sh {
                    check()?;
                    line(
                        &self.input[yy * sw..(yy + 1) * sw],
                        self.radius,
                        self.expand,
                        &mut self.horizontal[yy * sw..(yy + 1) * sw],
                        &mut self.queue,
                    );
                }
                for x in 0..w {
                    check()?;
                    for yy in 0..sh {
                        self.column[yy] = self.horizontal[yy * sw + x + self.radius];
                    }
                    line(
                        &self.column[..sh],
                        self.radius,
                        self.expand,
                        &mut self.output[..sh],
                        &mut self.queue,
                    );
                    for yy in 0..h {
                        self.stripe[yy * self.width + left + x] = self.output[yy + self.radius];
                    }
                }
            }
        }
        let start = y % tile * self.width;
        row.copy_from_slice(&self.stripe[start..start + self.width]);
        check()?;
        self.next += 1;
        self.failed = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bands_match_square_extrema_across_tiles_and_small_canvas_edges() {
        for (width, height, radius) in [(1, 1, 1), (17, 13, 64), (269, 263, 3)] {
            for expand in [false, true] {
                let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
                let mut rows = Rows::new(width, height, radius, expand, &pool).unwrap();
                let sample = |x: i32, y: i32| -> u8 {
                    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                        0
                    } else {
                        ((x * 17 + y * 13 + x * y * 3 + 137) % 256) as u8
                    }
                };
                let mut row = vec![0; width as usize];
                for y in 0..height {
                    rows.row(y, &mut row, &|| Ok(()), |area, values| {
                        let stride = (area[2] - area[0]) as usize;
                        for yy in area[1]..area[3] {
                            for x in area[0]..area[2] {
                                values[(yy - area[1]) as usize * stride + (x - area[0]) as usize] =
                                    sample(x, yy);
                            }
                        }
                        Ok(())
                    })
                    .unwrap();
                    for (x, &actual) in row.iter().enumerate() {
                        let mut expected = if expand { 0 } else { 255 };
                        for dy in -(radius as i32)..=radius as i32 {
                            for dx in -(radius as i32)..=radius as i32 {
                                let value = sample(x as i32 + dx, y as i32 + dy);
                                expected = if expand {
                                    expected.max(value)
                                } else {
                                    expected.min(value)
                                };
                            }
                        }
                        assert_eq!(
                            actual, expected,
                            "{width}x{height}, r{radius}, expand {expand}, {x},{y}"
                        );
                    }
                }
                assert!(rows
                    .row(height, &mut row, &|| Ok(()), |_, _| Ok(()))
                    .is_err());
                drop(rows);
                assert_eq!(pool.used(), 0);
            }
        }
    }
    #[test]
    fn morphology_admission_is_bounded_and_failed_streams_cannot_resume() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let rows = Rows::new(16000, 16000, 64, false, &pool).unwrap();
        assert!(pool.used() < 5 * 1024 * 1024);
        drop(rows);
        assert_eq!(pool.used(), 0);
        for fail_after_read in [false, true] {
            let mut rows = Rows::new(17, 13, 3, true, &pool).unwrap();
            let read = std::cell::Cell::new(false);
            let mut row = [0; 17];
            let checked = || {
                if read.get() {
                    Err("cancel morphology".into())
                } else {
                    Ok(())
                }
            };
            let result = rows.row(
                if fail_after_read { 0 } else { 1 },
                &mut row,
                &checked,
                |_, values| {
                    values.fill(137);
                    read.set(true);
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(read.get(), fail_after_read);
            assert!(rows.row(0, &mut row, &|| Ok(()), |_, _| Ok(())).is_err());
            drop(rows);
            assert_eq!(pool.used(), 0);
        }
    }
    #[test]
    fn shared_line_retains_native16_words_and_zero_edges() {
        let values = [12347u16, 23459, 59871, 15, 49153];
        for expand in [false, true] {
            let mut output = [0; 5];
            line(&values, 1, expand, &mut output, &mut [0; 5]);
            let expected = if expand {
                [23459, 59871, 59871, 59871, 49153]
            } else {
                [0, 12347, 15, 15, 0]
            };
            assert_eq!(output, expected);
        }
    }
}
