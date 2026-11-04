//! Fallible scanline flooding over caller-owned, pre-admitted workspace bytes.
//! No source reads, result-mask allocation or document publication happen here.
const TILE: usize = 256;
const TILE_BYTES: usize = TILE * TILE / 4;
pub(super) const MAX_FRONTIER: usize = 65_536;

#[derive(Clone, Copy, Debug)]
pub(super) struct Layout {
    width: usize,
    height: usize,
    columns: usize,
    pub states: usize,
    pub generations: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::VecDeque};

    fn reference(
        width: u32,
        height: u32,
        seeds: &[[u32; 2]],
        radius: Option<u32>,
        matching: impl Fn(usize, u32, u32) -> bool,
    ) -> Vec<bool> {
        let mut output = vec![false; (width * height) as usize];
        for (index, seed) in seeds.iter().enumerate() {
            if seed[0] >= width || seed[1] >= height {
                continue;
            }
            let mut seen = vec![false; output.len()];
            let mut queue = VecDeque::from([*seed]);
            while let Some([x, y]) = queue.pop_front() {
                let at = (y * width + x) as usize;
                if seen[at] {
                    continue;
                }
                seen[at] = true;
                let distance =
                    (x as i64 - seed[0] as i64).pow(2) + (y as i64 - seed[1] as i64).pow(2);
                if radius.is_some_and(|r| distance > i64::from(r).pow(2)) || !matching(index, x, y)
                {
                    continue;
                }
                output[at] = true;
                for (nx, ny) in [
                    (x as i32 - 1, y as i32),
                    (x as i32 + 1, y as i32),
                    (x as i32, y as i32 - 1),
                    (x as i32, y as i32 + 1),
                ] {
                    if nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32 {
                        queue.push_back([nx as u32, ny as u32]);
                    }
                }
            }
        }
        output
    }

    #[test]
    fn scanline_matches_independent_pixel_bfs_for_holes_runs_seed_colors_and_quick_radius() {
        let (w, h) = (73, 69);
        let seeds = [[0, 0], [35, 34], [72, 68], [12, 15], [74, 2]];
        for pattern in 0..8 {
            for radius in [None, Some(1), Some(9), Some(256)] {
                let matching = |seed: usize, x: u32, y: u32| match pattern {
                    0 => true,
                    1 => (x + y) % 2 == 0,
                    2 => x % 7 < 4 || y % 9 < 3,
                    3 => {
                        x > 4
                            && x < 65
                            && y > 3
                            && y < 60
                            && !(x > 20 && x < 50 && y > 15 && y < 45)
                    }
                    4 => (x * 73 + y * 37 + seed as u32 * 11) % 17 < 11,
                    5 => x % 11 == 0 || y % 13 == 0,
                    6 => x % 13 != seed as u32 % 13,
                    _ => (x / 8 + y / 7 + seed as u32) % 3 != 0,
                };
                let expected = reference(w, h, &seeds, radius, matching);
                let shape = Layout::new(w, h).unwrap();
                let mut states = vec![0xff; shape.states];
                let mut stamps = vec![0xff; shape.generations];
                let mut frontier = vec![0; MAX_FRONTIER * 4];
                let mut actual = vec![false; expected.len()];
                let mut workspace =
                    Flood::new(shape, &mut states, &mut stamps, &mut frontier, 1_000_000).unwrap();
                let receipt = workspace
                    .run(
                        &seeds,
                        radius,
                        || Ok::<_, ()>(()),
                        |s, x, y| Ok(matching(s, x, y)),
                        |y, l, r| {
                            for x in l..r {
                                actual[(y * w + x) as usize] = true;
                            }
                            Ok(())
                        },
                    )
                    .unwrap();
                assert_eq!(actual, expected, "pattern={pattern} radius={radius:?}");
                assert!(
                    receipt.predicates <= u64::from(w) * u64::from(h) * (seeds.len() as u64 - 1)
                );
                assert!(receipt.operations <= receipt.predicates * 8 + 8);
            }
        }
    }

    #[test]
    fn each_seed_tests_each_candidate_at_most_once_and_keeps_native_comparison_inputs() {
        let shape = Layout::new(257, 259).unwrap();
        let mut states = vec![0; shape.states];
        let mut stamps = vec![0; shape.generations];
        let mut frontier = vec![0; MAX_FRONTIER * 4];
        let mut counts = vec![0u8; 257 * 259 * 2];
        let mut workspace = Flood::new(
            shape,
            &mut states,
            &mut stamps,
            &mut frontier,
            257 * 259 * 2,
        )
        .unwrap();
        let complete = workspace
            .run(
                &[[0, 0], [256, 258]],
                None,
                || Ok::<_, ()>(()),
                |s, x, y| {
                    let value = [
                        12347u16.wrapping_add(x as u16),
                        33559u16.wrapping_add(y as u16),
                        51237,
                        65535,
                    ];
                    assert_eq!(value[2], 51237);
                    let at = s * 257 * 259 + (y * 257 + x) as usize;
                    counts[at] += 1;
                    assert_eq!(counts[at], 1);
                    Ok(true)
                },
                |_, _, _| Ok(()),
            )
            .unwrap();
        assert_eq!(complete.predicates, 257 * 259 * 2);
        assert!(counts.iter().all(|v| *v == 1));
        assert_eq!(complete.spans, 259 * 2);
    }

    #[test]
    fn frontier_exhaustion_source_failure_and_final_cancellation_poison_completion() {
        for mode in 0..3 {
            let shape = Layout::new(7, 5).unwrap();
            let mut states = vec![0; shape.states];
            let mut stamps = vec![0; shape.generations];
            let mut frontier = vec![0; if mode == 0 { 4 } else { MAX_FRONTIER * 4 }];
            let mut workspace =
                Flood::new(shape, &mut states, &mut stamps, &mut frontier, 1000).unwrap();
            let cancelled = Cell::new(false);
            let mut emitted = 0;
            let result = workspace.run(
                &[[3, 2]],
                None,
                || {
                    if cancelled.get() {
                        Err("cancel")
                    } else {
                        Ok(())
                    }
                },
                |_, x, y| {
                    if mode == 1 && x == 3 && y == 3 {
                        Err("source")
                    } else {
                        Ok(true)
                    }
                },
                |_, l, r| {
                    emitted += r - l;
                    if mode == 2 && emitted == 35 {
                        cancelled.set(true)
                    }
                    Ok(())
                },
            );
            assert!(matches!(
                (mode, result),
                (0, Err(Error::Frontier))
                    | (1, Err(Error::Callback("source")))
                    | (2, Err(Error::Callback("cancel")))
            ));
            assert!(emitted > 0, "Failure is after provisional output exists");
            assert!(matches!(
                workspace.run(
                    &[[0, 0]],
                    None,
                    || Ok::<_, ()>(()),
                    |_, _, _| Ok(true),
                    |_, _, _| Ok(())
                ),
                Err(Error::Poisoned)
            ));
        }
    }

    #[test]
    fn actual_predicate_budget_and_invalid_workspace_are_enforced() {
        let shape = Layout::new(5, 5).unwrap();
        let mut states = vec![0; shape.states];
        let mut stamps = vec![0; shape.generations];
        let mut frontier = vec![0; MAX_FRONTIER * 4];
        assert!(Flood::new(
            shape,
            &mut states[..shape.states - 1],
            &mut stamps,
            &mut frontier,
            4
        )
        .is_err());
        let mut workspace = Flood::new(shape, &mut states, &mut stamps, &mut frontier, 4).unwrap();
        assert!(matches!(
            workspace.run(
                &[[2, 2]],
                None,
                || Ok::<_, ()>(()),
                |_, _, _| Ok(true),
                |_, _, _| Ok(())
            ),
            Err(Error::Work)
        ));
        assert_eq!(workspace.predicates, 5);
        assert!(Layout::new(16001, 1).is_err());
    }

    #[test]
    fn generation_rollover_never_reuses_stale_tile_classification() {
        let shape = Layout::new(513, 257).unwrap();
        let mut states = vec![0xff; shape.states];
        let mut stamps = vec![0xff; shape.generations];
        let mut frontier = vec![0; 4];
        let mut workspace =
            Flood::new(shape, &mut states, &mut stamps, &mut frontier, 1000).unwrap();
        workspace.next_seed();
        workspace.set(0, 0, 1);
        workspace.set(512, 256, 3);
        assert_eq!(workspace.state(512, 256), 3);
        workspace.epoch = u32::MAX;
        workspace.next_seed();
        assert_eq!(workspace.epoch, 1);
        assert_eq!(workspace.state(0, 0), 0);
        assert_eq!(workspace.state(512, 256), 0);
        workspace.set(1, 0, 2);
        assert_eq!(workspace.state(0, 0), 0);
        assert_eq!(workspace.state(1, 0), 2);
    }

    #[test]
    fn source_rows_are_explicitly_prepared_before_memory_only_sampling_and_failure_poisoned() {
        for fail in [None, Some(3)] {
            let shape = Layout::new(17, 7).unwrap();
            let mut states = vec![0; shape.states];
            let mut stamps = vec![0; shape.generations];
            let mut queue = vec![0; MAX_FRONTIER * 4];
            let mut flood = Flood::new(shape, &mut states, &mut stamps, &mut queue, 1000).unwrap();
            let active = Cell::new(u32::MAX);
            let mut pixels = 0;
            let result = flood.run_rows(
                &[[0, 0]],
                None,
                || Ok::<_, &str>(()),
                |y| {
                    if fail == Some(y) {
                        Err("row source changed")
                    } else {
                        active.set(y);
                        Ok(())
                    }
                },
                |_, _, y| {
                    assert_eq!(active.get(), y);
                    Ok(true)
                },
                |_, left, right| {
                    pixels += right - left;
                    Ok(())
                },
            );
            if fail.is_some() {
                assert!(matches!(result, Err(Error::Callback("row source changed"))));
                assert!(pixels > 0);
            } else {
                assert!(result.is_ok());
                assert_eq!(pixels, 17 * 7);
            }
        }
    }

    #[test]
    #[ignore = "Opt-in full256MP computed-source traversal; not native document acceptance"]
    fn dense_16000_square_visits_every_computed_native_pixel_with_bounded_workspace() {
        let shape = Layout::new(16000, 16000).unwrap();
        assert_eq!(shape.states, 65_028_096);
        assert_eq!(shape.generations, 15_876);
        let mut states = vec![0; shape.states];
        let mut stamps = vec![0; shape.generations];
        let mut frontier = vec![0; MAX_FRONTIER * 4];
        let mut workspace =
            Flood::new(shape, &mut states, &mut stamps, &mut frontier, 256_000_000).unwrap();
        let mut pixels = 0u64;
        let mut rows = 0u32;
        let mut channel_checksum = 0u64;
        let result = workspace
            .run(
                &[[8000, 8000]],
                None,
                || Ok::<_, ()>(()),
                |_, x, y| {
                    let native = [
                        12347u16.wrapping_add((x * 173 + y * 211) as u16),
                        33559u16.wrapping_add((x * 13 + y * 7) as u16),
                        51237,
                        65535,
                    ];
                    channel_checksum = channel_checksum
                        .wrapping_add(native.into_iter().map(u64::from).sum::<u64>());
                    Ok(true)
                },
                |_, l, r| {
                    assert_eq!((l, r), (0, 16000));
                    pixels += u64::from(r - l);
                    rows += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(pixels, 256_000_000);
        assert_eq!(rows, 16000);
        assert_eq!(result.predicates, pixels);
        assert!(channel_checksum > 0);
        assert!(result.operations < 8 * 256_000_000);
        println!("DENSE_FLOOD predicates={} operations={} spans={} workspace_bytes={} computed_native_channel_sum={}",result.predicates,result.operations,result.spans,shape.states+shape.generations+MAX_FRONTIER*4,channel_checksum);
    }
}

impl Layout {
    /// A workspace shape is not permission to admit a live document.
    pub fn new(width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 {
            return Err("Invalid bounded selection workspace dimensions");
        }
        let columns = (width as usize).div_ceil(TILE);
        let tiles = columns * (height as usize).div_ceil(TILE);
        Ok(Self {
            width: width as usize,
            height: height as usize,
            columns,
            states: tiles * TILE_BYTES,
            generations: tiles * 4,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Error<E> {
    Invalid,
    Poisoned,
    Work,
    Frontier,
    Callback(E),
}

impl<E: std::fmt::Display> std::fmt::Display for Error<E> {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => output.write_str("Invalid selection flood request"),
            Self::Poisoned => {
                output.write_str("Selection flood cannot resume after completion or failure")
            }
            Self::Work => {
                output.write_str("Selection brush work limit exceeded; use a shorter gesture")
            }
            Self::Frontier => output.write_str("Selection frontier exceeds its admitted workspace"),
            Self::Callback(error) => error.fmt(output),
        }
    }
}

/// This receipt only describes successful traversal. The source lease, result
/// coverage, revision and cancellation still require engine admission.
pub(super) struct Complete {
    predicates: u64,
    operations: u64,
    spans: u64,
    width: u32,
    height: u32,
}

impl Complete {
    pub fn predicates(&self) -> u64 {
        self.predicates
    }
    pub fn spans(&self) -> u64 {
        self.spans
    }
    pub fn shape(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

pub(super) struct Flood<'a> {
    shape: Layout,
    states: &'a mut [u8],
    generations: &'a mut [u8],
    frontier: &'a mut [u8],
    epoch: u32,
    head: usize,
    queued: usize,
    predicates: u64,
    operations: u64,
    spans: u64,
    limit: u64,
    finished: bool,
}

impl<'a> Flood<'a> {
    pub fn new(
        shape: Layout,
        states: &'a mut [u8],
        generations: &'a mut [u8],
        frontier: &'a mut [u8],
        limit: u64,
    ) -> Result<Self, &'static str> {
        if states.len() != shape.states
            || generations.len() != shape.generations
            || frontier.is_empty()
            || frontier.len() % 4 != 0
            || frontier.len() / 4 > MAX_FRONTIER
            || limit == 0
            || limit.checked_mul(8).is_none()
        {
            return Err("Invalid admitted selection workspace buffers");
        }
        generations.fill(0);
        Ok(Self {
            shape,
            states,
            generations,
            frontier,
            epoch: 0,
            head: 0,
            queued: 0,
            predicates: 0,
            operations: 0,
            spans: 0,
            limit,
            finished: false,
        })
    }

    fn location(&self, x: usize, y: usize) -> (usize, usize, u32) {
        let tile = y / TILE * self.shape.columns + x / TILE;
        let local = y % TILE * TILE + x % TILE;
        (
            tile,
            tile * TILE_BYTES + local / 4,
            ((local % 4) * 2) as u32,
        )
    }

    fn state(&self, x: usize, y: usize) -> u8 {
        let (tile, index, shift) = self.location(x, y);
        let stamp =
            u32::from_be_bytes(self.generations[tile * 4..tile * 4 + 4].try_into().unwrap());
        if stamp == self.epoch {
            (self.states[index] >> shift) & 3
        } else {
            0
        }
    }

    fn set(&mut self, x: usize, y: usize, value: u8) {
        let (tile, index, shift) = self.location(x, y);
        let bytes = &mut self.generations[tile * 4..tile * 4 + 4];
        if u32::from_be_bytes(bytes.try_into().unwrap()) != self.epoch {
            self.states[tile * TILE_BYTES..(tile + 1) * TILE_BYTES].fill(0);
            bytes.copy_from_slice(&self.epoch.to_be_bytes());
        }
        self.states[index] = (self.states[index] & !(3 << shift)) | (value << shift);
    }

    fn next_seed(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.generations.fill(0);
            self.epoch = 1;
        }
        self.head = 0;
        self.queued = 0;
    }

    fn enqueue<E>(&mut self, x: usize, y: usize) -> Result<(), Error<E>> {
        if self.queued == self.frontier.len() / 4 {
            return Err(Error::Frontier);
        }
        let at = (self.head + self.queued) % (self.frontier.len() / 4) * 4;
        self.frontier[at..at + 2].copy_from_slice(&(x as u16).to_be_bytes());
        self.frontier[at + 2..at + 4].copy_from_slice(&(y as u16).to_be_bytes());
        self.queued += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<(usize, usize)> {
        if self.queued == 0 {
            return None;
        }
        let at = self.head * 4;
        let point = (
            u16::from_be_bytes(self.frontier[at..at + 2].try_into().unwrap()) as usize,
            u16::from_be_bytes(self.frontier[at + 2..at + 4].try_into().unwrap()) as usize,
        );
        self.head = (self.head + 1) % (self.frontier.len() / 4);
        self.queued -= 1;
        Some(point)
    }

    fn operation<E>(&mut self) -> Result<(), Error<E>> {
        self.operations += 1;
        if self.operations > self.limit * 8 {
            Err(Error::Work)
        } else {
            Ok(())
        }
    }

    fn classify<E>(
        &mut self,
        seed_index: usize,
        seed: [u32; 2],
        radius: Option<u32>,
        x: usize,
        y: usize,
        check: &mut impl FnMut() -> Result<(), E>,
        matches: &mut impl FnMut(usize, u32, u32) -> Result<bool, E>,
    ) -> Result<bool, Error<E>> {
        self.operation()?;
        let state = self.state(x, y);
        if state != 0 {
            return Ok(state == 2);
        }
        self.predicates += 1;
        if self.predicates > self.limit {
            return Err(Error::Work);
        }
        if self.predicates % 4096 == 0 {
            check().map_err(Error::Callback)?;
        }
        let in_radius = radius.is_none_or(|radius| {
            let dx = x as i64 - i64::from(seed[0]);
            let dy = y as i64 - i64::from(seed[1]);
            dx * dx + dy * dy <= i64::from(radius).pow(2)
        });
        let accepted =
            in_radius && matches(seed_index, x as u32, y as u32).map_err(Error::Callback)?;
        self.set(x, y, if accepted { 2 } else { 1 });
        Ok(accepted)
    }

    pub fn run<E>(
        &mut self,
        seeds: &[[u32; 2]],
        radius: Option<u32>,
        check: impl FnMut() -> Result<(), E>,
        matches: impl FnMut(usize, u32, u32) -> Result<bool, E>,
        emit: impl FnMut(u32, u32, u32) -> Result<(), E>,
    ) -> Result<Complete, Error<E>> {
        self.run_rows(seeds, radius, check, |_| Ok(()), matches, emit)
    }

    /// The worker prepares an explicit source row/stripe before any matching
    /// samples from it. `matches` may only read that admitted immutable view.
    pub fn run_rows<E>(
        &mut self,
        seeds: &[[u32; 2]],
        radius: Option<u32>,
        mut check: impl FnMut() -> Result<(), E>,
        mut prepare_row: impl FnMut(u32) -> Result<(), E>,
        mut matches: impl FnMut(usize, u32, u32) -> Result<bool, E>,
        mut emit: impl FnMut(u32, u32, u32) -> Result<(), E>,
    ) -> Result<Complete, Error<E>> {
        if self.finished {
            return Err(Error::Poisoned);
        }
        self.finished = true;
        if seeds.len() > 8192 || radius.is_some_and(|r| r == 0 || r > 256) {
            return Err(Error::Invalid);
        }
        check().map_err(Error::Callback)?;
        for (seed_index, &seed) in seeds.iter().enumerate() {
            self.next_seed();
            if seed[0] as usize >= self.shape.width || seed[1] as usize >= self.shape.height {
                continue;
            }
            self.enqueue(seed[0] as usize, seed[1] as usize)?;
            while let Some((x, y)) = self.pop() {
                self.operation()?;
                check().map_err(Error::Callback)?;
                if matches!(self.state(x, y), 1 | 3) {
                    continue;
                }
                prepare_row(y as u32).map_err(Error::Callback)?;
                if !self.classify(seed_index, seed, radius, x, y, &mut check, &mut matches)? {
                    continue;
                }
                self.set(x, y, 3);
                let mut left = x;
                while left > 0
                    && self.classify(
                        seed_index,
                        seed,
                        radius,
                        left - 1,
                        y,
                        &mut check,
                        &mut matches,
                    )?
                {
                    left -= 1;
                    self.set(left, y, 3);
                }
                let mut right = x + 1;
                while right < self.shape.width
                    && self.classify(
                        seed_index,
                        seed,
                        radius,
                        right,
                        y,
                        &mut check,
                        &mut matches,
                    )?
                {
                    self.set(right, y, 3);
                    right += 1;
                }
                emit(y as u32, left as u32, right as u32).map_err(Error::Callback)?;
                self.spans += 1;
                for neighbor in [
                    y.checked_sub(1),
                    (y + 1 < self.shape.height).then_some(y + 1),
                ]
                .into_iter()
                .flatten()
                {
                    prepare_row(neighbor as u32).map_err(Error::Callback)?;
                    let mut in_span = false;
                    for next in left..right {
                        let accepted = self.classify(
                            seed_index,
                            seed,
                            radius,
                            next,
                            neighbor,
                            &mut check,
                            &mut matches,
                        )?;
                        if accepted && !in_span {
                            self.enqueue(next, neighbor)?;
                        }
                        in_span = accepted;
                    }
                }
            }
        }
        check().map_err(Error::Callback)?;
        Ok(Complete {
            predicates: self.predicates,
            operations: self.operations,
            spans: self.spans,
            width: self.shape.width as u32,
            height: self.shape.height as u32,
        })
    }
}
