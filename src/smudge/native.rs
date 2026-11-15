//! Ordered provisional smudge over explicitly prepared, charged native memory.
use super::{premultiplied, premultiplied16, unpremultiplied, unpremultiplied16};
use crate::{
    brush::{self, native::PreparedBrush, Settings, Tip},
    large_document::{PinnedBytes, ResidentBudget, ScratchBytes},
    raster::{project16, Pixel16, TILE},
};

const STRIP_PIXELS: usize = 128 * 1024;

/// A bounded native strip from one exact provisional working step. The factory
/// performs any I/O before returning; pixel access is memory-only.
pub(crate) struct PreparedStrip {
    step: usize,
    area: [i32; 4],
    depth: u16,
    bytes: PinnedBytes,
}
impl PreparedStrip {
    pub(crate) fn new(
        step: usize,
        area: [i32; 4],
        depth: u16,
        bytes: PinnedBytes,
        pool: &ResidentBudget,
    ) -> Result<Self, String> {
        let width = i64::from(area[2]) - i64::from(area[0]);
        let height = i64::from(area[3]) - i64::from(area[1]);
        if width <= 0
            || height <= 0
            || width > 16000
            || height > 16000
            || area[0] < 0
            || area[1] < 0
            || area[2] > 16000
            || area[3] > 16000
            || width * height > STRIP_PIXELS as i64
            || ![8, 16].contains(&depth)
            || !bytes.uses_budget(pool)
            || bytes.len() != width as usize * height as usize * if depth == 16 { 8 } else { 4 }
        {
            return Err("Invalid native smudge strip ownership, shape or length".into());
        }
        Ok(Self {
            step,
            area,
            depth,
            bytes,
        })
    }
    fn pixel(&self, x: i32, y: i32) -> Pixel16 {
        // Coordinates come only from the validated exact-area iteration below.
        let index =
            ((y - self.area[1]) * (self.area[2] - self.area[0]) + x - self.area[0]) as usize;
        read_pixel(&self.bytes, self.depth, index)
    }
}

pub(crate) struct PreparedTile {
    step: usize,
    key: (u32, u32),
    depth: u16,
    bytes: PinnedBytes,
}
impl PreparedTile {
    pub(crate) fn new(
        step: usize,
        key: (u32, u32),
        depth: u16,
        bytes: PinnedBytes,
        pool: &ResidentBudget,
    ) -> Result<Self, String> {
        if ![8, 16].contains(&depth)
            || key.0 >= 63
            || key.1 >= 63
            || !bytes.uses_budget(pool)
            || bytes.len() != (TILE * TILE) as usize * if depth == 16 { 8 } else { 4 }
        {
            return Err("Invalid prepared native smudge tile".into());
        }
        Ok(Self {
            step,
            key,
            depth,
            bytes,
        })
    }
}

/// Explicit worker preparation and provisional storage. Implementations bind all
/// reads to the captured scene and all completed prior dabs. They must not commit
/// engine history or publish a document. An error drops the entire working owner.
pub(crate) trait WorkingSource {
    /// Captured immutable metadata; these getters perform no I/O.
    fn frame(&self) -> [u32; 2];
    fn depth(&self) -> u16;
    fn uses_budget(&self, pool: &ResidentBudget) -> bool;
    fn prepare_strip(
        &mut self,
        step: usize,
        area: [i32; 4],
        depth: u16,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedStrip, String>;
    fn prepare_tile(
        &mut self,
        step: usize,
        key: (u32, u32),
        depth: u16,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedTile, String>;
    fn stage_tile(
        &mut self,
        step: usize,
        key: (u32, u32),
        bytes: PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String>;
}

pub(crate) struct PreparedSmudge<'a> {
    geometry: PreparedBrush<'a>,
    depth: u16,
    clip: [i32; 4],
    polygon: Option<&'a [[f32; 2]]>,
    settings: Settings,
    color: Pixel16,
    pool: ResidentBudget,
}
impl<'a> PreparedSmudge<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        width: u32,
        height: u32,
        depth: u16,
        points: &[[f32; 2]],
        pressures: Option<&[f32]>,
        settings: Settings,
        color: Pixel16,
        clip: Option<[i32; 4]>,
        polygon: Option<&'a [[f32; 2]]>,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        // Geometry must remain present even for a transparent loading color:
        // smudge can pick up existing pigment with no newly loaded paint.
        let geometry = PreparedBrush::new(
            width, height, depth, points, pressures, settings, [255; 4], false, clip, polygon,
            pool, check,
        )?;
        let clip = clip.unwrap_or([0, 0, width as i32, height as i32]);
        let clip = [
            clip[0].max(0),
            clip[1].max(0),
            clip[2].min(width as i32),
            clip[3].min(height as i32),
        ];
        Ok(Self {
            geometry,
            depth,
            clip,
            polygon,
            settings,
            color,
            pool: pool.clone(),
        })
    }

    /// Runs outside engine/UI locks. Ownership is returned only after every dab
    /// and the final cancellation check succeeds. A failed run drops all staged
    /// work; successful work still requires the common guarded engine adoption.
    pub(crate) fn run<W: WorkingSource>(
        &self,
        mut working: W,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<W, String> {
        self.run_in_place(&mut working, check)?;
        Ok(working)
    }

    /// Session-owned callers retain the working owner on every error so they
    /// can explicitly retire unpublished fragments after all readers drop.
    pub(crate) fn run_in_place<W: WorkingSource>(
        &self,
        working: &mut W,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        if working.frame() != self.geometry.frame()
            || working.depth() != self.depth
            || !working.uses_budget(&self.pool)
        {
            return Err("Native smudge working frame, depth or pool differs".into());
        }
        if self.settings.opacity == 0. || self.settings.flow == 0. || self.settings.wetness == 0. {
            return Ok(());
        }
        let base_tip = Tip::new(self.settings);
        let mut carry = None;
        for (step, &dab) in self.geometry.dabs().iter().enumerate() {
            check()?;
            if dab.radius <= 0.001 || dab.opacity == 0. {
                continue;
            }
            let tip = base_tip.with_radius(dab.radius);
            let b = brush::dab_bounds(dab);
            let area = [
                b[0].max(self.clip[0]),
                b[1].max(self.clip[1]),
                b[2].min(self.clip[2]),
                b[3].min(self.clip[3]),
            ];
            if area[0] >= area[2] || area[1] >= area[3] {
                continue;
            }
            let width = (area[2] - area[0]) as usize;
            let rows = (STRIP_PIXELS / width).max(1) as i32;
            let mut average = Pigment::zero(self.depth);
            let mut weight = Weight::zero(self.depth);
            let mut top = area[1];
            while top < area[3] {
                check()?;
                let strip_area = [area[0], top, area[2], (top + rows).min(area[3])];
                let strip = working.prepare_strip(step, strip_area, self.depth, check)?;
                if strip.step != step
                    || strip.area != strip_area
                    || strip.depth != self.depth
                    || !strip.bytes.uses_budget(&self.pool)
                {
                    return Err(
                        "Native smudge strip differs from its declared working footprint".into(),
                    );
                }
                for y in strip_area[1]..strip_area[3] {
                    check()?;
                    for x in strip_area[0]..strip_area[2] {
                        if !self.selected(x, y) {
                            continue;
                        }
                        let amount = tip.at(dab.document_point, x, y);
                        if amount == 0. {
                            continue;
                        }
                        average.accumulate(strip.pixel(x, y), amount);
                        weight.add(amount);
                    }
                }
                top = strip_area[3];
            }
            if !weight.positive() {
                continue;
            }
            average.divide(weight);
            let Some(mut pigment) = carry else {
                average.move_toward(
                    Pigment::from_pixel(self.depth, self.color),
                    self.settings.load,
                );
                carry = Some(average);
                continue;
            };
            pigment.move_toward(average, self.settings.pickup);
            carry = Some(pigment);
            if !pigment.visible() {
                continue;
            }
            // The average is complete before any write for this dab. Tiles can
            // be staged in order; the next dab must see every staged prior tile.
            for ty in area[1] as u32 / TILE..=(area[3] - 1) as u32 / TILE {
                for tx in area[0] as u32 / TILE..=(area[2] - 1) as u32 / TILE {
                    check()?;
                    let key = (tx, ty);
                    let before = working.prepare_tile(step, key, self.depth, check)?;
                    let length = (TILE * TILE) as usize * if self.depth == 16 { 8 } else { 4 };
                    if before.step != step
                        || before.key != key
                        || before.depth != self.depth
                        || !before.bytes.uses_budget(&self.pool)
                        || before.bytes.len() != length
                    {
                        return Err("Native smudge tile ownership or length differs".into());
                    }
                    let mut output =
                        ScratchBytes::new(&self.pool, length, length).map_err(|e| e.to_string())?;
                    output.copy_from_slice(&before.bytes);
                    let left = (tx * TILE) as i32;
                    let top = (ty * TILE) as i32;
                    for y in area[1].max(top)..area[3].min(top + TILE as i32) {
                        check()?;
                        for x in area[0].max(left)..area[2].min(left + TILE as i32) {
                            if !self.selected(x, y) {
                                continue;
                            }
                            // Preserve the legacy f32 product even at depth16.
                            let amount = tip.at(dab.document_point, x, y)
                                * self.settings.flow
                                * self.settings.opacity
                                * self.settings.wetness
                                * dab.opacity;
                            if amount <= 0. {
                                continue;
                            }
                            let index = ((y - top) * TILE as i32 + x - left) as usize;
                            let mut mixed = Pigment::from_pixel(
                                self.depth,
                                read_pixel(&before.bytes, self.depth, index),
                            );
                            mixed.move_toward(pigment, amount);
                            write_pixel(&mut output, self.depth, index, mixed.pixel());
                        }
                    }
                    check()?;
                    if *output != *before.bytes {
                        working.stage_tile(step, key, output.freeze(), check)?;
                    }
                }
            }
        }
        check()?;
        Ok(())
    }
    fn selected(&self, x: i32, y: i32) -> bool {
        self.polygon
            .is_none_or(|p| crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5))
    }
}

#[derive(Clone, Copy)]
enum Weight {
    Eight(f32),
    Sixteen(f64),
}
impl Weight {
    fn zero(depth: u16) -> Self {
        if depth == 16 {
            Self::Sixteen(0.)
        } else {
            Self::Eight(0.)
        }
    }
    fn add(&mut self, amount: f32) {
        match self {
            Self::Eight(v) => *v += amount,
            Self::Sixteen(v) => *v += amount as f64,
        }
    }
    fn positive(self) -> bool {
        match self {
            Self::Eight(v) => v > 0.,
            Self::Sixteen(v) => v > 0.,
        }
    }
}
#[derive(Clone, Copy)]
enum Pigment {
    Eight([f32; 4]),
    Sixteen([f64; 4]),
}
impl Pigment {
    fn zero(depth: u16) -> Self {
        if depth == 16 {
            Self::Sixteen([0.; 4])
        } else {
            Self::Eight([0.; 4])
        }
    }
    fn from_pixel(depth: u16, pixel: Pixel16) -> Self {
        if depth == 16 {
            Self::Sixteen(premultiplied16(pixel))
        } else {
            Self::Eight(premultiplied(pixel.map(project16)))
        }
    }
    fn accumulate(&mut self, pixel: Pixel16, amount: f32) {
        match self {
            Self::Eight(values) => {
                let p = premultiplied(pixel.map(project16));
                for c in 0..4 {
                    values[c] += p[c] * amount;
                }
            }
            Self::Sixteen(values) => {
                let p = premultiplied16(pixel);
                for c in 0..4 {
                    values[c] += p[c] * amount as f64;
                }
            }
        }
    }
    fn divide(&mut self, weight: Weight) {
        match (self, weight) {
            (Self::Eight(v), Weight::Eight(w)) => {
                for c in v {
                    *c /= w;
                }
            }
            (Self::Sixteen(v), Weight::Sixteen(w)) => {
                for c in v {
                    *c /= w;
                }
            }
            _ => unreachable!("The plan fixes one native depth"),
        }
    }
    fn move_toward(&mut self, target: Self, amount: f32) {
        match (self, target) {
            (Self::Eight(v), Self::Eight(t)) => {
                for c in 0..4 {
                    v[c] += (t[c] - v[c]) * amount;
                }
            }
            (Self::Sixteen(v), Self::Sixteen(t)) => {
                for c in 0..4 {
                    v[c] += (t[c] - v[c]) * amount as f64;
                }
            }
            _ => unreachable!("The plan fixes one native depth"),
        }
    }
    fn visible(self) -> bool {
        match self {
            Self::Eight(v) => v[3] > 0.,
            Self::Sixteen(v) => v[3] > 0.,
        }
    }
    fn pixel(self) -> Pixel16 {
        match self {
            Self::Eight(v) => unpremultiplied(v).map(|v| v as u16 * 257),
            Self::Sixteen(v) => unpremultiplied16(v),
        }
    }
}
fn read_pixel(bytes: &[u8], depth: u16, index: usize) -> Pixel16 {
    if depth == 16 {
        let at = index * 8;
        std::array::from_fn(|c| u16::from_be_bytes([bytes[at + c * 2], bytes[at + c * 2 + 1]]))
    } else {
        let at = index * 4;
        std::array::from_fn(|c| bytes[at + c] as u16 * 257)
    }
}
fn write_pixel(bytes: &mut [u8], depth: u16, index: usize, pixel: Pixel16) {
    if depth == 16 {
        let at = index * 8;
        for c in 0..4 {
            bytes[at + c * 2..at + c * 2 + 2].copy_from_slice(&pixel[c].to_be_bytes());
        }
    } else {
        let at = index * 4;
        for c in 0..4 {
            bytes[at + c] = project16(pixel[c]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::Raster;
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
    };

    // A resident immutable fixture is only an oracle/input adapter, never a live
    // large-document authority. Every provisional byte is charged until drop.
    struct Candidate {
        frame: [u32; 2],
        original: Raster,
        pending: BTreeMap<(u32, u32), PinnedBytes>,
        pool: ResidentBudget,
        dropped: Arc<AtomicBool>,
        stages: Arc<AtomicUsize>,
        strips: usize,
        maximum_strip: usize,
        wrong_step: bool,
        fail_stage: bool,
    }
    impl Candidate {
        fn new(original: Raster, pool: &ResidentBudget) -> Self {
            Self {
                frame: [original.width, original.height],
                original,
                pending: BTreeMap::new(),
                pool: pool.clone(),
                dropped: Arc::new(AtomicBool::new(false)),
                stages: Arc::new(AtomicUsize::new(0)),
                strips: 0,
                maximum_strip: 0,
                wrong_step: false,
                fail_stage: false,
            }
        }
        fn pixel(&self, x: i32, y: i32) -> Pixel16 {
            let key = (x as u32 / TILE, y as u32 / TILE);
            if let Some(bytes) = self.pending.get(&key) {
                read_pixel(
                    bytes,
                    self.original.depth,
                    ((y as u32 % TILE) * TILE + x as u32 % TILE) as usize,
                )
            } else {
                self.original.get16(x, y)
            }
        }
    }
    impl Drop for Candidate {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }
    impl WorkingSource for Candidate {
        fn frame(&self) -> [u32; 2] {
            self.frame
        }
        fn depth(&self) -> u16 {
            self.original.depth
        }
        fn uses_budget(&self, pool: &ResidentBudget) -> bool {
            self.pool.same_pool(pool)
        }
        fn prepare_strip(
            &mut self,
            step: usize,
            area: [i32; 4],
            depth: u16,
            check: &mut dyn FnMut() -> Result<(), String>,
        ) -> Result<PreparedStrip, String> {
            check()?;
            let pixels = ((area[2] - area[0]) * (area[3] - area[1])) as usize;
            self.strips += 1;
            self.maximum_strip = self.maximum_strip.max(pixels);
            let length = pixels * if depth == 16 { 8 } else { 4 };
            let mut bytes =
                ScratchBytes::new(&self.pool, length, length).map_err(|e| e.to_string())?;
            let mut index = 0;
            for y in area[1]..area[3] {
                for x in area[0]..area[2] {
                    write_pixel(&mut bytes, depth, index, self.pixel(x, y));
                    index += 1;
                }
            }
            PreparedStrip::new(
                step + usize::from(self.wrong_step),
                area,
                depth,
                bytes.freeze(),
                &self.pool,
            )
        }
        fn prepare_tile(
            &mut self,
            step: usize,
            key: (u32, u32),
            depth: u16,
            check: &mut dyn FnMut() -> Result<(), String>,
        ) -> Result<PreparedTile, String> {
            check()?;
            let bytes = if let Some(bytes) = self.pending.get(&key) {
                bytes.clone()
            } else {
                let length = (TILE * TILE) as usize * if depth == 16 { 8 } else { 4 };
                let mut bytes =
                    ScratchBytes::new(&self.pool, length, length).map_err(|e| e.to_string())?;
                if depth == 16 {
                    if let Some(tile) = self.original.samples16.get(&key) {
                        for (dst, value) in bytes.chunks_exact_mut(2).zip(tile.iter()) {
                            dst.copy_from_slice(&value.to_be_bytes());
                        }
                    }
                } else if let Some(tile) = self.original.tiles.get(&key) {
                    bytes.copy_from_slice(tile);
                }
                bytes.freeze()
            };
            PreparedTile::new(step, key, depth, bytes, &self.pool)
        }
        fn stage_tile(
            &mut self,
            _step: usize,
            key: (u32, u32),
            bytes: PinnedBytes,
            check: &mut dyn FnMut() -> Result<(), String>,
        ) -> Result<(), String> {
            check()?;
            if self.fail_stage {
                return Err("Provisional store refused smudge output".into());
            }
            self.pending.insert(key, bytes);
            self.stages.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    fn input(depth: u16) -> Raster {
        let mut original = Raster::new_depth(273, 265, depth);
        for y in 0..265 {
            for x in 0..273 {
                original.set16(
                    x,
                    y,
                    [
                        12347u16.wrapping_add(x as u16 * 173),
                        33559u16.wrapping_add(y as u16 * 97),
                        51237,
                        if (x + y) % 11 == 0 { 0 } else { 45679 },
                    ],
                );
            }
        }
        original
    }
    #[test]
    fn ordered_provisional_smudge_matches_resident_native_pixels_and_padding() {
        for depth in [8, 16] {
            let load_colors = if depth == 16 {
                [[18248, 35211, 54229, 49599], [18248, 35211, 54229, 0]]
            } else {
                [[71, 137, 211, 193], [71, 137, 211, 0]].map(|rgba| rgba.map(|value| value * 257))
            };
            for color in load_colors {
                let original = input(depth);
                let mut expected = original.clone();
                let settings = Settings {
                    radius: 13.3,
                    hardness: 0.37,
                    opacity: 0.71,
                    flow: 0.38,
                    spacing: 0.21,
                    tip: brush::TipKind::Grain,
                    angle: 17.,
                    roundness: 0.69,
                    texture_origin: [-17, 23],
                    load: 0.33,
                    pickup: 0.47,
                    wetness: 0.69,
                    ..Settings::default()
                };
                let points = [[12., 18.], [129., 139.], [269., 261.]];
                let pressures = [0.33, 0.91, 0.57];
                let polygon = [[8., 4.], [273., 0.], [273., 265.], [0., 256.]];
                let clip = Some([5, 7, 273, 265]);
                super::super::paint_native_color(
                    &mut expected,
                    &points,
                    Some(&pressures),
                    settings,
                    color,
                    clip,
                    Some(&polygon),
                )
                .unwrap();
                let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
                let plan = PreparedSmudge::new(
                    273,
                    265,
                    depth,
                    &points,
                    Some(&pressures),
                    settings,
                    color,
                    clip,
                    Some(&polygon),
                    &pool,
                    &mut || Ok(()),
                )
                .unwrap();
                let candidate = Candidate::new(original, &pool);
                let finished = plan.run(candidate, &mut || Ok(())).unwrap();
                assert!(finished.stages.load(Ordering::SeqCst) > 0);
                for (&key, bytes) in &finished.pending {
                    for i in 0..(TILE * TILE) as usize {
                        let actual = read_pixel(bytes, depth, i);
                        let expected = if depth == 16 {
                            expected
                                .samples16
                                .get(&key)
                                .map_or([0; 4], |tile| std::array::from_fn(|c| tile[i * 4 + c]))
                        } else {
                            expected.tiles.get(&key).map_or([0; 4], |tile| {
                                std::array::from_fn(|c| tile[i * 4 + c] as u16 * 257)
                            })
                        };
                        assert_eq!(
                            actual, expected,
                            "smudge depth{depth} alpha{} key{key:?} pixel{i}",
                            color[3]
                        );
                    }
                }
                // Compare untouched/unallocated locations as well as staged tiles.
                for y in 0..265 {
                    for x in 0..273 {
                        assert_eq!(finished.pixel(x, y), expected.get16(x, y));
                    }
                }
                drop(finished);
                drop(plan);
                assert_eq!(pool.used(), 0);
            }
        }
    }
    #[test]
    fn failed_or_cancelled_working_owner_is_dropped_without_partial_completion() {
        let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let points = [[15., 15.], [260., 260.]];
        let plan = PreparedSmudge::new(
            273,
            265,
            16,
            &points,
            None,
            Settings::default(),
            [211 * 257, 17 * 257, 37 * 257, 65535],
            None,
            None,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        for mode in 0..4 {
            let mut candidate = Candidate::new(input(16), &pool);
            candidate.wrong_step = mode == 0;
            candidate.fail_stage = mode == 1;
            if mode == 3 {
                candidate.frame = [1, 1];
            }
            let dropped = candidate.dropped.clone();
            let stages = candidate.stages.clone();
            let before = pool.used();
            let outcome = plan.run(candidate, &mut || {
                if mode == 2 && stages.load(Ordering::SeqCst) > 0 {
                    Err("Cancelled after provisional output".into())
                } else {
                    Ok(())
                }
            });
            assert!(outcome.is_err());
            assert!(dropped.load(Ordering::SeqCst));
            assert_eq!(pool.used(), before);
        }
        drop(plan);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn large_dab_source_preparation_stays_within_strip_budget() {
        let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        // Only the first dab loads pigment. Its 1026-square footprint must still
        // be prepared in multiple bounded strips, never as one dense buffer.
        let points = [[136.5, 132.5]];
        let plan = PreparedSmudge::new(
            16000,
            16000,
            16,
            &points,
            None,
            Settings {
                radius: 512.,
                ..Settings::default()
            },
            [211 * 257, 17 * 257, 37 * 257, 65535],
            None,
            None,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let mut candidate = Candidate::new(input(16), &pool);
        candidate.frame = [16000, 16000];
        let finished = plan.run(candidate, &mut || Ok(())).unwrap();
        assert!(finished.strips > 1);
        assert!(finished.maximum_strip <= STRIP_PIXELS);
        assert_eq!(finished.stages.load(Ordering::SeqCst), 0);
        drop(finished);
        drop(plan);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn invalid_prepared_strip_is_refused_before_coordinate_arithmetic() {
        let pool = ResidentBudget::new(1024).unwrap();
        let bytes = ScratchBytes::new(&pool, 4, 4).unwrap().freeze();
        assert!(
            PreparedStrip::new(0, [i32::MIN, i32::MIN, i32::MAX, i32::MAX], 8, bytes, &pool)
                .is_err()
        );
        assert_eq!(pool.used(), 0);
    }
}
