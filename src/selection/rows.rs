//! Provisional flood spans become ordered scalar coverage rows without a full
//! image allocation. Every byte buffer is supplied by the admitting worker.
use super::flood::Complete;

pub(super) const MAX_SPANS: usize = 500_000;

#[derive(Clone, Copy)]
pub(super) struct Layout {
    width: usize,
    height: usize,
    maximum: usize,
    pub records: usize,
    pub indices: usize,
    pub ends: usize,
}

#[cfg(test)]
mod tests {
    use super::super::flood::{Flood, Layout as FloodLayout, MAX_FRONTIER};
    use super::*;

    #[test]
    fn real_flood_spans_are_ordered_unioned_and_sealed_before_rows_can_be_read() {
        let (w, h) = (257u32, 259u32);
        let layout = Layout::new(w, h, 4096).unwrap();
        let mut records = vec![0; layout.records];
        let mut indices = vec![0; layout.indices];
        let mut ends = vec![0; layout.ends];
        let mut spans =
            Spans::new(layout, &mut records, &mut indices, &mut ends, 1_000_000).unwrap();
        let shape = FloodLayout::new(w, h).unwrap();
        let mut states = vec![0; shape.states];
        let mut stamps = vec![0; shape.generations];
        let mut queue = vec![0; MAX_FRONTIER * 4];
        let mut flood = Flood::new(shape, &mut states, &mut stamps, &mut queue, 1_000_000).unwrap();
        let mut expected = vec![0; (w * h) as usize];
        let completed = flood
            .run(
                &[[200, 200], [2, 2], [128, 128]],
                Some(31),
                || Ok::<_, String>(()),
                |s, x, y| Ok((x / 9 + y / 7 + s as u32) % 4 != 0),
                |y, l, r| {
                    spans.push(y, l, r).map_err(str::to_owned)?;
                    expected[(y * w + l) as usize..(y * w + r) as usize].fill(255);
                    Ok(())
                },
            )
            .unwrap();
        let rows = spans.finish(&completed, || Ok(())).unwrap();
        let mut output = vec![17; w as usize];
        for y in 0..h {
            rows.row(y, &mut output, || Ok(())).unwrap();
            assert_eq!(&output, &expected[(y * w) as usize..((y + 1) * w) as usize]);
        }
        output.fill(19);
        assert!(rows.row(h, &mut output, || Ok(())).is_err());
        assert!(output.iter().all(|v| *v == 19));
    }

    fn complete(width: u32, height: u32) -> Complete {
        let shape = FloodLayout::new(width, height).unwrap();
        let mut states = vec![0; shape.states];
        let mut stamps = vec![0; shape.generations];
        let mut queue = vec![0; MAX_FRONTIER * 4];
        Flood::new(shape, &mut states, &mut stamps, &mut queue, 1_000_000)
            .unwrap()
            .run(
                &[[0, 0]],
                None,
                || Ok::<_, ()>(()),
                |_, _, _| Ok(true),
                |_, _, _| Ok(()),
            )
            .unwrap()
    }

    #[test]
    fn wrong_geometry_budget_failure_and_late_source_check_never_make_readable_rows() {
        for mode in 0..3 {
            let layout = Layout::new(2, 2, 2).unwrap();
            let mut records = vec![0; layout.records];
            let mut indices = vec![0; layout.indices];
            let mut ends = vec![0; layout.ends];
            let mut spans = Spans::new(layout, &mut records, &mut indices, &mut ends, 16).unwrap();
            spans.push(0, 0, 2).unwrap();
            spans.push(1, 0, 2).unwrap();
            if mode == 1 {
                assert!(spans.push(0, 0, 1).is_err());
                assert!(spans.push(0, 0, 1).is_err());
            }
            let completed = complete(if mode == 0 { 3 } else { 2 }, 2);
            let mut checks = 0;
            let result = spans.finish(&completed, || {
                checks += 1;
                if mode == 2 && checks == 3 {
                    Err("source changed".to_owned())
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            if mode == 2 {
                assert_eq!(result.err().unwrap(), "source changed");
            }
        }
    }

    fn reference(input: &[u8], width: usize, height: usize, radius: usize) -> Vec<u8> {
        let count = (radius * 2 + 1) as u32;
        let mut horizontal = vec![0; input.len()];
        let mut output = horizontal.clone();
        for y in 0..height {
            for x in 0..width {
                let sum: u32 = (-(radius as isize)..=radius as isize)
                    .filter_map(|d| {
                        let xx = x as isize + d;
                        (xx >= 0 && xx < width as isize)
                            .then(|| u32::from(input[y * width + xx as usize]))
                    })
                    .sum();
                horizontal[y * width + x] = (sum / count) as u8;
            }
        }
        for y in 0..height {
            for x in 0..width {
                let sum: u32 = (-(radius as isize)..=radius as isize)
                    .filter_map(|d| {
                        let yy = y as isize + d;
                        (yy >= 0 && yy < height as isize)
                            .then(|| u32::from(horizontal[yy as usize * width + x]))
                    })
                    .sum();
                output[y * width + x] = (sum / count) as u8;
            }
        }
        output
    }

    #[test]
    fn streamed_soft_coverage_is_byte_exact_with_independent_two_pass_box_filter() {
        for (w, h) in [(1usize, 1usize), (3, 5), (31, 29), (257, 13)] {
            for radius in [0usize, 1, 4, 64] {
                let input: Vec<u8> = (0..w * h)
                    .map(|i| if i % 7 < 3 { 255 } else { (i * 173) as u8 })
                    .collect();
                let expected = reference(&input, w, h, radius);
                let mut ring = vec![0; (2 * radius + 1) * w];
                let mut sums = vec![0; w * 4];
                let mut row = vec![0; w];
                let mut output = vec![0; w];
                let mut actual = Vec::new();
                let mut worker = Feather::new(
                    w as u32,
                    h as u32,
                    radius as u32,
                    &mut ring,
                    &mut sums,
                    &mut row,
                    &mut output,
                )
                .unwrap();
                let receipt = worker
                    .run(
                        || Ok(()),
                        |y, row| {
                            row.copy_from_slice(&input[y as usize * w..(y as usize + 1) * w]);
                            Ok(())
                        },
                        |_, row| {
                            actual.extend_from_slice(row);
                            Ok(())
                        },
                    )
                    .unwrap();
                assert_eq!(receipt.rows(), h as u32);
                assert_eq!(actual, expected, "shape={w}x{h},radius={radius}");
            }
        }
    }

    #[test]
    fn streaming_read_emit_and_late_cancel_errors_refuse_completion_and_retry() {
        for mode in 0..3 {
            let (w, h) = (17usize, 9usize);
            let radius = 1;
            let mut ring = vec![0; (2 * radius + 1) * w];
            let mut sums = vec![0; w * 4];
            let mut row = vec![0; w];
            let mut output = vec![0; w];
            let emitted = std::cell::Cell::new(0);
            let mut worker = Feather::new(
                w as u32,
                h as u32,
                radius as u32,
                &mut ring,
                &mut sums,
                &mut row,
                &mut output,
            )
            .unwrap();
            let result = worker.run(
                || {
                    if mode == 2 && emitted.get() == h {
                        Err("late cancel".into())
                    } else {
                        Ok(())
                    }
                },
                |y, row| {
                    if mode == 0 && y == 4 {
                        Err("row IO".into())
                    } else {
                        row.fill(255);
                        Ok(())
                    }
                },
                |_, _| {
                    emitted.set(emitted.get() + 1);
                    if mode == 1 && emitted.get() == 4 {
                        Err("sink IO".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(
                result.err().unwrap(),
                ["row IO", "sink IO", "late cancel"][mode]
            );
            assert!(emitted.get() > 0);
            assert!(worker
                .run(
                    || Ok(()),
                    |_, row| {
                        row.fill(255);
                        Ok(())
                    },
                    |_, _| Ok(())
                )
                .is_err());
        }
    }
}

impl Layout {
    pub fn new(width: u32, height: u32, maximum: usize) -> Result<Self, &'static str> {
        if width == 0
            || height == 0
            || width > 16000
            || height > 16000
            || maximum == 0
            || maximum > MAX_SPANS
        {
            return Err("Invalid bounded selection span layout");
        }
        Ok(Self {
            width: width as usize,
            height: height as usize,
            maximum,
            records: maximum * 6,
            indices: maximum * 4,
            ends: height as usize * 4,
        })
    }
}

pub(super) struct Spans<'a> {
    layout: Layout,
    records: &'a mut [u8],
    indices: &'a mut [u8],
    ends: &'a mut [u8],
    count: usize,
    selected: u64,
    maximum_work: u64,
    failed: bool,
}

fn word(bytes: &[u8], index: usize) -> u32 {
    u32::from_be_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}
fn put_word(bytes: &mut [u8], index: usize, value: u32) {
    bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
}

impl<'a> Spans<'a> {
    pub fn new(
        layout: Layout,
        records: &'a mut [u8],
        indices: &'a mut [u8],
        ends: &'a mut [u8],
        maximum_work: u64,
    ) -> Result<Self, &'static str> {
        if records.len() != layout.records
            || indices.len() != layout.indices
            || ends.len() != layout.ends
            || maximum_work == 0
        {
            return Err("Selection span buffers differ from their admitted layout");
        }
        ends.fill(0);
        Ok(Self {
            layout,
            records,
            indices,
            ends,
            count: 0,
            selected: 0,
            maximum_work,
            failed: false,
        })
    }

    pub fn push(&mut self, y: u32, left: u32, right: u32) -> Result<(), &'static str> {
        if self.failed {
            return Err("Selection span collector cannot resume after failure");
        }
        if y as usize >= self.layout.height || left >= right || right as usize > self.layout.width {
            self.failed = true;
            return Err("Selection span is outside its exact canvas");
        }
        if self.count == self.layout.maximum
            || self
                .selected
                .checked_add(u64::from(right - left))
                .is_none_or(|sum| sum > self.maximum_work)
        {
            self.failed = true;
            return Err("Selection spans exceed their admitted work or record budget");
        }
        let at = self.count * 6;
        for (i, value) in [y, left, right].into_iter().enumerate() {
            self.records[at + i * 2..at + i * 2 + 2].copy_from_slice(&(value as u16).to_be_bytes());
        }
        self.count += 1;
        self.selected += u64::from(right - left);
        put_word(self.ends, y as usize, word(self.ends, y as usize) + 1);
        Ok(())
    }

    pub fn finish(
        self,
        complete: &Complete,
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<Rows<'a>, String> {
        if self.failed
            || complete.shape() != (self.layout.width as u32, self.layout.height as u32)
            || self.count as u64 != complete.spans()
            || self.selected > complete.predicates()
        {
            return Err("Selection spans differ from their completed flood traversal".into());
        }
        check()?;
        let mut sum = 0;
        for y in 0..self.layout.height {
            let count = word(self.ends, y);
            put_word(self.ends, y, sum);
            sum += count;
        }
        for i in 0..self.count {
            if i % 4096 == 0 {
                check()?;
            }
            let y = u16::from_be_bytes(self.records[i * 6..i * 6 + 2].try_into().unwrap()) as usize;
            let at = word(self.ends, y);
            put_word(self.indices, at as usize, i as u32);
            put_word(self.ends, y, at + 1);
        }
        check()?;
        Ok(Rows {
            layout: self.layout,
            records: self.records,
            indices: self.indices,
            ends: self.ends,
        })
    }
}

pub(super) struct Rows<'a> {
    layout: Layout,
    records: &'a [u8],
    indices: &'a [u8],
    ends: &'a [u8],
}

impl Rows<'_> {
    pub fn row(
        &self,
        y: u32,
        output: &mut [u8],
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        if y as usize >= self.layout.height || output.len() != self.layout.width {
            return Err("Selection output row differs from its exact canvas".into());
        }
        check()?;
        output.fill(0);
        let first = if y == 0 {
            0
        } else {
            word(self.ends, y as usize - 1)
        };
        let end = word(self.ends, y as usize);
        for index in first..end {
            if index % 256 == 0 {
                check()?;
            }
            let at = word(self.indices, index as usize) as usize * 6;
            let left =
                u16::from_be_bytes(self.records[at + 2..at + 4].try_into().unwrap()) as usize;
            let right =
                u16::from_be_bytes(self.records[at + 4..at + 6].try_into().unwrap()) as usize;
            output[left..right].fill(255);
        }
        check()
    }
}

/// The row ring is (2*radius+1)*width bytes, sums are width*4 bytes and
/// input/output are width bytes each. Caller admission covers all four buffers.
pub(super) struct Feather<'a> {
    width: usize,
    height: usize,
    radius: usize,
    ring: &'a mut [u8],
    sums: &'a mut [u8],
    input: &'a mut [u8],
    output: &'a mut [u8],
    finished: bool,
}

pub(super) struct CompleteRows {
    rows: u32,
    width: u32,
}
impl CompleteRows {
    pub fn rows(&self) -> u32 {
        self.rows
    }
    pub fn shape(&self) -> (u32, u32) {
        (self.width, self.rows)
    }
}

impl<'a> Feather<'a> {
    pub fn new(
        width: u32,
        height: u32,
        radius: u32,
        ring: &'a mut [u8],
        sums: &'a mut [u8],
        input: &'a mut [u8],
        output: &'a mut [u8],
    ) -> Result<Self, &'static str> {
        if width == 0
            || height == 0
            || width > 16000
            || height > 16000
            || radius > 64
            || ring.len() != (2 * radius as usize + 1) * width as usize
            || sums.len() != width as usize * 4
            || input.len() != width as usize
            || output.len() != width as usize
        {
            return Err("Selection feather buffers differ from the admitted row layout");
        }
        ring.fill(0);
        sums.fill(0);
        Ok(Self {
            width: width as usize,
            height: height as usize,
            radius: radius as usize,
            ring,
            sums,
            input,
            output,
            finished: false,
        })
    }

    fn load(
        &mut self,
        y: usize,
        read: &mut impl FnMut(u32, &mut [u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        read(y as u32, self.input)?;
        let count = 2 * self.radius + 1;
        let target = &mut self.ring[(y % count) * self.width..(y % count + 1) * self.width];
        let mut sum: u32 = self.input[..(self.radius + 1).min(self.width)]
            .iter()
            .map(|v| u32::from(*v))
            .sum();
        for (x, value) in target.iter_mut().enumerate() {
            *value = (sum / count as u32) as u8;
            if x + self.radius + 1 < self.width {
                sum += u32::from(self.input[x + self.radius + 1]);
            }
            if x >= self.radius {
                sum -= u32::from(self.input[x - self.radius]);
            }
            put_word(self.sums, x, word(self.sums, x) + u32::from(*value));
        }
        Ok(())
    }

    pub fn run(
        &mut self,
        mut check: impl FnMut() -> Result<(), String>,
        mut read: impl FnMut(u32, &mut [u8]) -> Result<(), String>,
        mut emit: impl FnMut(u32, &[u8]) -> Result<(), String>,
    ) -> Result<CompleteRows, String> {
        if self.finished {
            return Err("Selection feather cannot resume after completion or failure".into());
        }
        self.finished = true;
        let count = 2 * self.radius + 1;
        for y in 0..(self.radius + 1).min(self.height) {
            check()?;
            self.load(y, &mut read)?;
        }
        for y in 0..self.height {
            check()?;
            for x in 0..self.width {
                self.output[x] = (word(self.sums, x) / count as u32) as u8;
            }
            emit(y as u32, self.output)?;
            if y >= self.radius {
                let row = (y - self.radius) % count;
                for x in 0..self.width {
                    put_word(
                        self.sums,
                        x,
                        word(self.sums, x) - u32::from(self.ring[row * self.width + x]),
                    );
                }
            }
            let next = y + self.radius + 1;
            if next < self.height {
                check()?;
                self.load(next, &mut read)?;
            }
        }
        check()?;
        Ok(CompleteRows {
            rows: self.height as u32,
            width: self.width as u32,
        })
    }
}
