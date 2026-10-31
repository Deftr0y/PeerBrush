//! Per-seed flood visitation without a canvas-sized array of generation stamps.
pub(super) struct Visited {
    words: Vec<u64>,
    touched: Vec<usize>,
}

impl Visited {
    pub(super) fn new(pixels: usize) -> Self {
        Self {
            words: vec![0; pixels.div_ceil(64)],
            touched: Vec::new(),
        }
    }

    pub(super) fn reset(&mut self) {
        // Quick Selection can have many small seeds on a large canvas. Clear
        // only words visited by the previous seed, rather than the whole map.
        for word in self.touched.drain(..) {
            self.words[word] = 0;
        }
    }

    #[inline]
    pub(super) fn mark(&mut self, index: usize) -> bool {
        let word = index / 64;
        let bit = 1u64 << (index % 64);
        let value = &mut self.words[word];
        if *value & bit != 0 {
            return false;
        }
        if *value == 0 {
            self.touched.push(word);
        }
        *value |= bit;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Visited;

    #[test]
    fn each_seed_can_revisit_sparse_and_adjacent_word_boundaries() {
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
    fn full_supported_canvas_visit_storage_stays_below_one_byte_per_four_pixels() {
        let pixels = 32 * 1024 * 1024;
        let mut visited = Visited::new(pixels);
        for index in (0..pixels).step_by(64) {
            assert!(visited.mark(index));
        }
        let bytes = visited.words.capacity() * std::mem::size_of::<u64>()
            + visited.touched.capacity() * std::mem::size_of::<usize>();
        assert!(bytes <= pixels / 4);
        visited.reset();
        assert!(visited.mark(pixels - 1));
    }
}
