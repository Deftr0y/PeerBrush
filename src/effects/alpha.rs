//! A complete, ordered native source reduction. The result is geometry only;
//! callers bind it to their captured scene/stage/source identities before use.
use super::{
    regional::{Frame, RegionalImage},
    styles::AlphaBounds,
};
use crate::{
    large_document::ResidentBudget,
    raster::backed::{Region, MAX_REGION_PIXELS},
};

pub(crate) struct AlphaReduction {
    frame: [u32; 2],
    depth: u16,
    next_y: u32,
    bounds: Option<[u32; 4]>,
    poisoned: bool,
    budget: ResidentBudget,
}
impl AlphaReduction {
    pub(crate) fn new(frame: Frame, depth: u16, budget: &ResidentBudget) -> Result<Self, String> {
        if ![8, 16].contains(&depth) {
            return Err("Alpha reduction requires native depth8or16".into());
        }
        Ok(Self {
            frame: frame.dimensions(),
            depth,
            next_y: 0,
            bounds: None,
            poisoned: false,
            budget: budget.clone(),
        })
    }
    pub(crate) fn next_region(&self) -> Result<Option<Region>, String> {
        if self.poisoned {
            return Err("Alpha reduction failed; discard its provisional result".into());
        }
        if self.next_y == self.frame[1] {
            return Ok(None);
        }
        let rows = (MAX_REGION_PIXELS / self.frame[0] as usize).max(1) as u32;
        Region::new([
            0,
            self.next_y as i32,
            self.frame[0] as i32,
            (self.next_y + rows).min(self.frame[1]) as i32,
        ])
        .map(Some)
        .map_err(|e| e.to_string())
    }
    pub(crate) fn visit(
        &mut self,
        input: &RegionalImage,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let expected = self.next_region()?;
        self.poisoned = true;
        let expected = expected.ok_or("Alpha reduction already has complete input")?;
        if input.bounds() != expected.bounds()
            || input.depth() != self.depth
            || !input.uses_budget(&self.budget)
        {
            return Err(
                "Alpha reduction received a foreign, reordered or incomplete native strip".into(),
            );
        }
        check()?;
        let mut bounds = self.bounds;
        let [left, top, right, bottom] = expected.bounds();
        for y in top..bottom {
            check()?;
            for x in left..right {
                if input.pixel16(x, y)?[3] != 0 {
                    let b = bounds.get_or_insert([x as u32, y as u32, x as u32 + 1, y as u32 + 1]);
                    b[0] = b[0].min(x as u32);
                    b[1] = b[1].min(y as u32);
                    b[2] = b[2].max(x as u32 + 1);
                    b[3] = b[3].max(y as u32 + 1);
                }
            }
        }
        check()?;
        self.bounds = bounds;
        self.next_y = bottom as u32;
        self.poisoned = false;
        Ok(())
    }
    pub(crate) fn finish(
        self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<AlphaBounds, String> {
        if self.poisoned || self.next_y != self.frame[1] {
            return Err("Alpha reduction did not validate the complete native source".into());
        }
        check()?;
        Ok(self.bounds.map_or(AlphaBounds::Empty, AlphaBounds::Nonzero))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_document::ScratchBytes;
    fn block(region: Region, depth: u16, budget: &ResidentBudget) -> RegionalImage {
        let length = region.pixels() * if depth == 16 { 8 } else { 4 };
        let mut bytes = ScratchBytes::new(budget, length, length).unwrap();
        let [l, t, r, b] = region.bounds();
        let mut at = 0;
        for y in t..b {
            for x in l..r {
                let a = if (x == 11 && y == 9) || (x == 138 && y == 2000) {
                    1
                } else {
                    0
                };
                if depth == 16 {
                    for v in [12347u16, 33559, 51237, a] {
                        bytes[at..at + 2].copy_from_slice(&v.to_be_bytes());
                        at += 2;
                    }
                } else {
                    bytes[at..at + 4].copy_from_slice(&[37, 91, 173, a as u8]);
                    at += 4;
                }
            }
        }
        RegionalImage::from_native(region, depth, bytes.freeze()).unwrap()
    }
    #[test]
    fn complete_native_low_alpha_reduction_and_no_early_finish() {
        for depth in [8, 16] {
            let budget = ResidentBudget::new(2 * 1024 * 1024).unwrap();
            let mut reduction =
                AlphaReduction::new(Frame::new(140, 2001).unwrap(), depth, &budget).unwrap();
            while let Some(region) = reduction.next_region().unwrap() {
                let bytes = block(region, depth, &budget);
                reduction.visit(&bytes, &mut || Ok(())).unwrap();
            }
            match reduction.finish(&mut || Ok(())).unwrap() {
                AlphaBounds::Nonzero(b) => assert_eq!(b, [11, 9, 139, 2001]),
                AlphaBounds::Empty => panic!("Native alpha1 was projected away"),
            };
            assert_eq!(budget.used(), 0);
            assert!(
                AlphaReduction::new(Frame::new(140, 2001).unwrap(), depth, &budget)
                    .unwrap()
                    .finish(&mut || Ok(()))
                    .is_err()
            );
        }
    }
    #[test]
    fn failed_or_foreign_strips_poison_the_reduction() {
        let budget = ResidentBudget::new(2 * 1024 * 1024).unwrap();
        let foreign = ResidentBudget::new(2 * 1024 * 1024).unwrap();
        let mut reduction =
            AlphaReduction::new(Frame::new(140, 2001).unwrap(), 16, &budget).unwrap();
        let region = reduction.next_region().unwrap().unwrap();
        let bytes = block(region, 16, &foreign);
        assert!(reduction.visit(&bytes, &mut || Ok(())).is_err());
        assert!(reduction.next_region().is_err());
        assert!(reduction.finish(&mut || Ok(())).is_err());
        drop(bytes);
        let mut reduction =
            AlphaReduction::new(Frame::new(140, 2001).unwrap(), 16, &budget).unwrap();
        let bytes = block(reduction.next_region().unwrap().unwrap(), 16, &budget);
        assert!(reduction
            .visit(&bytes, &mut || Err("Synthetic cancellation".into()))
            .is_err());
        assert!(reduction.finish(&mut || Ok(())).is_err());
        drop(bytes);
        assert_eq!(budget.used(), 0);
        assert_eq!(foreign.used(), 0);
    }
}
