//! Per-seed flood visitation with one byte per pixel instead of a full-width stamp.
pub(super) struct Visited {
    stamps: Vec<u8>,
    generation: u8,
}

impl Visited {
    pub(super) fn new(pixels: usize) -> Self {
        Self {
            stamps: vec![0; pixels],
            generation: 1,
        }
    }

    pub(super) fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Zero is never a live generation. Clear before reusing stamps;
            // otherwise a pixel from an earlier seed could be skipped.
            self.stamps.fill(0);
            self.generation = 1;
        }
    }

    #[inline]
    pub(super) fn mark(&mut self, index: usize) -> bool {
        let stamp = &mut self.stamps[index];
        if *stamp == self.generation {
            return false;
        }
        *stamp = self.generation;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Visited;

    #[test]
    fn each_seed_can_revisit_sparse_and_adjacent_pixels() {
        let mut visited = Visited::new(1023);
        for _ in 0..3 {
            for index in [0, 63, 64, 65, 255, 256, 257, 1022] {
                assert!(visited.mark(index));
                assert!(!visited.mark(index));
            }
            visited.reset();
            visited.reset();
        }
    }

    #[test]
    fn full_supported_canvas_visit_payload_is_one_byte_per_pixel() {
        let pixels = 32 * 1024 * 1024;
        let mut visited = Visited::new(pixels);
        for index in 0..pixels {
            assert!(visited.mark(index));
        }
        assert_eq!(visited.stamps.capacity(), pixels);
        visited.reset();
        assert!(visited.mark(pixels - 1));
    }

    #[test]
    fn generation_rollover_clears_pixels_unvisited_by_intermediate_seeds() {
        let mut visited = Visited::new(4099);
        assert!(visited.mark(0));
        assert!(visited.mark(4098));
        for _ in 0..254 {
            visited.reset();
            assert!(visited.mark(1));
            assert!(!visited.mark(1));
        }
        visited.reset();
        for index in [0, 1, 4098] {
            assert!(visited.mark(index));
            assert!(!visited.mark(index));
        }
    }

    #[test]
    fn repeated_short_seeds_match_full_width_stamps_across_many_rollovers() {
        let mut visited = Visited::new(4099);
        let mut reference = vec![0u32; 4099];
        let mut random = 1u64;
        for seed in 1..=8192 {
            visited.reset();
            for _ in 0..128 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let index = random as usize % reference.len();
                let expected = reference[index] != seed;
                reference[index] = seed;
                assert_eq!(visited.mark(index), expected);
                assert!(!visited.mark(index));
            }
        }
    }
}
