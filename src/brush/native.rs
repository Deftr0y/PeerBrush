//! Provisional native paint tiles. Input/output bytes have an explicit shared
//! resident owner; this kernel performs no I/O and grants no document authority.
use super::{
    accumulated16, accumulated8, apply_coverage, coverage_pixel16, dab_bounds, dabs,
    visit_tile_dab, work_budget, Dab, Settings, Tip,
};
use crate::{
    large_document::{PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
    raster::{project16, Pixel, Pixel16, TILE},
};
use std::mem::size_of;

const PLAN_BYTES: usize = 2 * 1024 * 1024;
const DABS_PER_GROUP: usize = 32;
const NO_TILE: u16 = u16::MAX;

/// Bounded row-major set of native tile coordinates.
pub(crate) struct TileChanges {
    bits: [u64; 64],
    columns: u32,
    rows: u32,
}
impl TileChanges {
    pub(crate) fn new([width, height]: [u32; 2]) -> Result<Self, String> {
        let columns = width.div_ceil(TILE);
        let rows = height.div_ceil(TILE);
        if columns == 0 || rows == 0 || u64::from(columns) * u64::from(rows) > 4096 {
            return Err("Native changed tile grid exceeds its footprint".into());
        }
        Ok(Self {
            bits: [0; 64],
            columns,
            rows,
        })
    }
    fn index(&self, key: (u32, u32)) -> Option<usize> {
        if key.0 >= self.columns || key.1 >= self.rows {
            return None;
        }
        Some((key.1 * self.columns + key.0) as usize)
    }
    pub(crate) fn insert(&mut self, key: (u32, u32)) -> Result<(), String> {
        let at = self
            .index(key)
            .ok_or("Changed tile is outside the native frame")?;
        self.bits[at / 64] |= 1 << (at % 64);
        Ok(())
    }
    pub(crate) fn contains(&self, key: (u32, u32)) -> bool {
        self.index(key)
            .is_some_and(|at| self.bits[at / 64] & (1 << (at % 64)) != 0)
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        (0..self.rows).flat_map(move |y| {
            (0..self.columns).filter_map(move |x| self.contains((x, y)).then_some((x, y)))
        })
    }
}

pub(crate) struct PreparedBrush<'a> {
    width: u32,
    height: u32,
    depth: u16,
    settings: Settings,
    color: Pixel16,
    erase: bool,
    clip: [i32; 4],
    polygon: Option<&'a [[f32; 2]]>,
    dabs: Vec<Dab>,
    tiles: Vec<(u32, u32)>,
    tile_slots: Vec<u16>,
    tile_groups: Vec<u64>,
    words_per_tile: usize,
    budget: ResidentBudget,
    // The bounded Vec owners above are destroyed before their admission charge.
    _metadata: Reservation,
}

impl<'a> PreparedBrush<'a> {
    pub(crate) fn dabs(&self) -> &[Dab] {
        &self.dabs
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        width: u32,
        height: u32,
        depth: u16,
        points: &[[f32; 2]],
        pressures: Option<&[f32]>,
        settings: Settings,
        color: Pixel,
        erase: bool,
        clip: Option<[i32; 4]>,
        polygon: Option<&'a [[f32; 2]]>,
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::with_native_color(
            width, height, depth, points, pressures, settings,
            color.map(|value| u16::from(value) * 257), erase, clip, polygon, budget, check,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_native_color(
        width: u32,
        height: u32,
        depth: u16,
        points: &[[f32; 2]],
        pressures: Option<&[f32]>,
        settings: Settings,
        color: Pixel16,
        erase: bool,
        clip: Option<[i32; 4]>,
        polygon: Option<&'a [[f32; 2]]>,
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if width == 0 || height == 0 || width > 16000 || height > 16000 || ![8, 16].contains(&depth)
        {
            return Err("Invalid native brush frame".into());
        }
        if polygon.is_some_and(|points| points.iter().flatten().any(|v| !v.is_finite())) {
            return Err("Invalid native brush polygon".into());
        }
        let mut metadata = budget
            .reserve_bytes(PLAN_BYTES, PLAN_BYTES)
            .map_err(|e| e.to_string())?;
        let dabs = dabs(points, pressures, settings)?;
        let clip = clip.unwrap_or([0, 0, width as i32, height as i32]);
        let clip = [
            clip[0].max(0),
            clip[1].max(0),
            clip[2].min(width as i32),
            clip[3].min(height as i32),
        ];
        work_budget(&dabs, clip)?;
        let columns = width.div_ceil(TILE);
        let rows = height.div_ceil(TILE);
        // The native edge limit and 256-pixel tiles leave at most 3969 slots.
        let tile_count = (columns * rows) as usize;
        let words_per_tile = dabs.len().div_ceil(DABS_PER_GROUP).div_ceil(64);
        let mut tile_slots = Vec::new();
        let mut tile_groups = Vec::new();
        let mut occupied = 0u16;
        if settings.opacity > 0.0 && settings.flow > 0.0 && color[3] != 0 {
            tile_slots
                .try_reserve_exact(tile_count)
                .map_err(|_| "Native brush tile index allocation failed")?;
            tile_slots.resize(tile_count, NO_TILE);
            for (index, &dab) in dabs.iter().enumerate() {
                check()?;
                if dab.radius <= 0.001 || dab.opacity == 0.0 {
                    continue;
                }
                let b = dab_bounds(dab);
                let [left, top, right, bottom] = [
                    b[0].max(clip[0]),
                    b[1].max(clip[1]),
                    b[2].min(clip[2]),
                    b[3].min(clip[3]),
                ];
                if left >= right || top >= bottom {
                    continue;
                }
                let group = index / DABS_PER_GROUP;
                for y in top as u32 / TILE..=(bottom - 1) as u32 / TILE {
                    for x in left as u32 / TILE..=(right - 1) as u32 / TILE {
                        let slot = &mut tile_slots[(y * columns + x) as usize];
                        if *slot == NO_TILE {
                            tile_groups
                                .try_reserve(words_per_tile)
                                .map_err(|_| "Native brush dab index allocation failed")?;
                            tile_groups.resize(tile_groups.len() + words_per_tile, 0u64);
                            *slot = occupied;
                            occupied += 1;
                        }
                        let start = usize::from(*slot) * words_per_tile;
                        tile_groups[start + group / 64] |= 1 << (group % 64);
                    }
                }
            }
        }
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(usize::from(occupied))
            .map_err(|_| "Native brush tile metadata allocation failed")?;
        tiles.extend(
            tile_slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| **slot != NO_TILE)
                .map(|(index, _)| (index as u32 % columns, index as u32 / columns)),
        );
        let retained = dabs.capacity() * size_of::<Dab>()
            + tile_slots.capacity() * size_of::<u16>()
            + tile_groups.capacity() * size_of::<u64>()
            + tiles.capacity() * size_of::<(u32, u32)>();
        metadata
            .shrink_to(retained)
            .map_err(|_| "Native brush metadata exceeds its resident reservation")?;
        check()?;
        Ok(Self {
            width,
            height,
            depth,
            settings,
            color,
            erase,
            clip,
            polygon,
            dabs,
            tiles,
            tile_slots,
            tile_groups,
            words_per_tile,
            budget: budget.clone(),
            _metadata: metadata,
        })
    }

    pub(crate) fn tiles(&self) -> &[(u32, u32)] {
        &self.tiles
    }

    fn tile_dabs(&self, (x, y): (u32, u32)) -> Option<impl Iterator<Item = &Dab>> {
        let columns = self.width.div_ceil(TILE);
        if x >= columns || y >= self.height.div_ceil(TILE) {
            return None;
        }
        let &slot = self.tile_slots.get((y * columns + x) as usize)?;
        if slot == NO_TILE {
            return None;
        }
        let start = usize::from(slot) * self.words_per_tile;
        let words = &self.tile_groups[start..start + self.words_per_tile];
        // Visit marked groups in stroke order so native coverage rounds the same way.
        Some(
            words
                .iter()
                .enumerate()
                .flat_map(|(word, &bits)| {
                    let mut remaining = bits;
                    std::iter::from_fn(move || {
                        if remaining == 0 {
                            return None;
                        }
                        let bit = remaining.trailing_zeros() as usize;
                        remaining &= remaining - 1;
                        Some((word * 64 + bit) * DABS_PER_GROUP)
                    })
                })
                .flat_map(|start| &self.dabs[start..(start + DABS_PER_GROUP).min(self.dabs.len())]),
        )
    }

    /// Clone/heal use the existing 16-bit dab coverage at both source depths.
    /// Opacity is applied by their native sampling kernel after this quantization.
    pub(crate) fn coverage16_tile(
        &self,
        coordinate: (u32, u32),
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        check()?;
        let dabs = self
            .tile_dabs(coordinate)
            .ok_or("Native coverage tile is outside its prepared footprint")?;
        let length = (TILE * TILE * 2) as usize;
        let mut coverage =
            ScratchBytes::new(&self.budget, length, length).map_err(|e| e.to_string())?;
        let origin = [(coordinate.0 * TILE) as i32, (coordinate.1 * TILE) as i32];
        let tip = Tip::new(self.settings);
        for &dab in dabs {
            check()?;
            if dab.radius <= 0.001 || dab.opacity == 0.0 {
                continue;
            }
            visit_tile_dab(
                dab,
                tip.with_radius(dab.radius),
                self.clip,
                origin,
                self.polygon,
                |at, amount| {
                    let at = at * 2;
                    let old = u16::from_be_bytes([coverage[at], coverage[at + 1]]);
                    let value = accumulated16(old, amount, self.settings.flow, dab.opacity);
                    coverage[at..at + 2].copy_from_slice(&value.to_be_bytes());
                },
            );
        }
        check()?;
        Ok(coverage.freeze())
    }

    /// A removed dab is restored from the captured baseline, never from a
    /// previous preview. Include its old footprint even if no new dab visits it.
    pub(crate) fn changed_tiles(&self, previous: &Self) -> Result<TileChanges, String> {
        if self.frame() != previous.frame()
            || self.depth != previous.depth
            || self.settings != previous.settings
            || self.color != previous.color
            || self.erase != previous.erase
            || self.clip != previous.clip
            || self.polygon != previous.polygon
            || !self.budget.same_pool(&previous.budget)
        {
            return Err("Native brush preview changed its captured gesture parameters".into());
        }
        let mut changed = TileChanges::new(self.frame())?;
        let prefix = self
            .dabs
            .iter()
            .zip(&previous.dabs)
            .take_while(|(a, b)| a == b)
            .count();
        for &dab in previous.dabs[prefix..].iter().chain(&self.dabs[prefix..]) {
            if dab.radius <= 0.001 || dab.opacity == 0.0 {
                continue;
            }
            let b = dab_bounds(dab);
            let [left, top, right, bottom] = [
                b[0].max(self.clip[0]),
                b[1].max(self.clip[1]),
                b[2].min(self.clip[2]),
                b[3].min(self.clip[3]),
            ];
            if left >= right || top >= bottom {
                continue;
            }
            for y in top as u32 / TILE..=(bottom - 1) as u32 / TILE {
                for x in left as u32 / TILE..=(right - 1) as u32 / TILE {
                    changed.insert((x, y))?;
                }
            }
        }
        Ok(changed)
    }

    /// The input is the complete physical tile, including native edge padding.
    /// The worker must bind it to the captured Surface before and after calling.
    pub(crate) fn render_tile(
        &self,
        coordinate: (u32, u32),
        source: &PinnedBytes,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        check()?;
        if !source.uses_budget(&self.budget) {
            return Err("Native brush source uses a different resident pool".into());
        }
        let dabs = self
            .tile_dabs(coordinate)
            .ok_or("Native brush tile is outside its prepared footprint")?;
        let stride = if self.depth == 16 { 8 } else { 4 };
        let bytes = (TILE * TILE) as usize * stride;
        if source.len() != bytes {
            return Err("Native brush input tile length differs".into());
        }
        let mut output =
            ScratchBytes::new(&self.budget, bytes, bytes).map_err(|e| e.to_string())?;
        output.copy_from_slice(source);
        let coverage_bytes = (TILE * TILE) as usize * if self.depth == 16 { 2 } else { 1 };
        let mut coverage = ScratchBytes::new(&self.budget, coverage_bytes, coverage_bytes)
            .map_err(|e| e.to_string())?;
        let origin = [(coordinate.0 * TILE) as i32, (coordinate.1 * TILE) as i32];
        let tip = Tip::new(self.settings);
        for &dab in dabs {
            check()?;
            if dab.radius <= 0.001 || dab.opacity == 0.0 {
                continue;
            }
            visit_tile_dab(
                dab,
                tip.with_radius(dab.radius),
                self.clip,
                origin,
                self.polygon,
                |at, amount| {
                    if self.depth == 16 {
                        let at = at * 2;
                        let old = u16::from_be_bytes([coverage[at], coverage[at + 1]]);
                        let result = accumulated16(old, amount, self.settings.flow, dab.opacity);
                        coverage[at..at + 2].copy_from_slice(&result.to_be_bytes());
                    } else {
                        coverage[at] =
                            accumulated8(coverage[at], amount, self.settings.flow, dab.opacity);
                    }
                },
            );
        }
        if self.depth == 16 {
            for (index, pixel) in output.chunks_exact_mut(8).enumerate() {
                if index % 4096 == 0 {
                    check()?;
                }
                let c = index * 2;
                let alpha = u16::from_be_bytes([coverage[c], coverage[c + 1]]);
                if alpha == 0 {
                    continue;
                }
                let old =
                    std::array::from_fn(|i| u16::from_be_bytes([pixel[i * 2], pixel[i * 2 + 1]]));
                let result = coverage_pixel16(old, alpha, self.settings.opacity, self.color, self.erase);
                for (channel, value) in pixel.chunks_exact_mut(2).zip(result) {
                    channel.copy_from_slice(&value.to_be_bytes());
                }
            }
        } else {
            apply_coverage(
                &mut output,
                &coverage,
                self.settings.opacity,
                self.color.map(project16),
                self.erase,
            );
        }
        check()?;
        Ok(output.freeze())
    }

    pub(crate) fn frame(&self) -> [u32; 2] {
        [self.width, self.height]
    }
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{brush::TipKind, raster::Raster};

    fn native_tile(raster: &Raster, key: (u32, u32), budget: &ResidentBudget) -> PinnedBytes {
        let length = (TILE * TILE * if raster.depth == 16 { 8 } else { 4 }) as usize;
        let mut bytes = ScratchBytes::new(budget, length, length).unwrap();
        if raster.depth == 16 {
            if let Some(tile) = raster.samples16.get(&key) {
                for (dst, value) in bytes.chunks_exact_mut(2).zip(tile.iter()) {
                    dst.copy_from_slice(&value.to_be_bytes());
                }
            }
        } else if let Some(tile) = raster.tiles.get(&key) {
            bytes.copy_from_slice(tile);
        }
        bytes.freeze()
    }

    #[test]
    fn tile_dab_groups_skip_distant_work_and_keep_revisits_in_order() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let plan = PreparedBrush::new(
            16000,
            16000,
            16,
            &[[4090., 15997.], [8., 15997.], [4090., 15997.]],
            Some(&[0.4, 0.9, 0.6]),
            Settings {
                radius: 5.0,
                spacing: 0.2,
                smoothing: 0.0,
                ..Settings::default()
            },
            [37, 91, 173, 255],
            false,
            None,
            None,
            &budget,
            &mut || Ok(()),
        )
        .unwrap();

        assert!(plan.words_per_tile > 1);
        assert_eq!(plan.tiles(), &(0..16).map(|x| (x, 62)).collect::<Vec<_>>());
        assert_eq!(
            plan.tile_groups.len(),
            plan.tiles().len() * plan.words_per_tile
        );
        assert!(budget.used() < 256 * 1024);
        let touches_first_tile = |dab: &&Dab| dab_bounds(**dab)[0] < TILE as i32;
        let dabs = plan.tile_dabs((0, 62)).unwrap();
        assert!(dabs
            .filter(touches_first_tile)
            .eq(plan.dabs().iter().filter(touches_first_tile)));
        let mut checks = 0;
        plan.coverage16_tile((0, 62), &mut || {
            checks += 1;
            Ok(())
        })
        .unwrap();
        assert!(checks * 10 < plan.dabs().len());
        assert!(plan.tile_dabs((16, 62)).is_none());
        assert!(plan.tile_dabs((63, 61)).is_none());
    }

    #[test]
    fn native_tiles_match_resident_pressure_texture_opacity_erasure_and_padding() {
        for depth in [8, 16] {
            for (case, tip) in [
                TipKind::Round,
                TipKind::Dry,
                TipKind::Chalk,
                TipKind::Grain,
                TipKind::Bristle,
            ]
            .into_iter()
            .enumerate()
            {
                let budget = ResidentBudget::new(8 * 1024 * 1024).unwrap();
                let mut raster = Raster::new_depth(273, 265, depth);
                for y in 0..265 {
                    for x in 0..273 {
                        raster.set16(
                            x,
                            y,
                            [12347u16.wrapping_add(x as u16 * 31), 33559, 51237, 45679],
                        );
                    }
                }
                // Native edge padding is source data even though pixel getters clip it.
                if depth == 16 {
                    std::sync::Arc::make_mut(raster.samples16.get_mut(&(1, 1)).unwrap())[65535] =
                        43219;
                } else {
                    std::sync::Arc::make_mut(raster.tiles.get_mut(&(1, 1)).unwrap())[65535] = 173;
                }
                let points = [[12., 18.], [129., 139.], [269., 261.]];
                let pressures = [0.33, 0.91, 0.57];
                let polygon = [[0., 0.], [273., 0.], [273., 265.], [0., 265.]];
                let settings = Settings {
                    radius: 9.3,
                    hardness: 0.37,
                    opacity: 0.71,
                    flow: 0.38,
                    spacing: 0.21,
                    tip,
                    angle: 17.,
                    roundness: 0.69,
                    taper_end: 8.,
                    texture_origin: [-17, 23],
                    ..Settings::default()
                };
                let mut expected = raster.clone();
                let erase = case % 2 == 1;
                super::super::paint_with_pressure(
                    &mut expected,
                    &points,
                    Some(&pressures),
                    settings,
                    [71, 137, 211, 193],
                    erase,
                    None,
                    Some(&polygon),
                )
                .unwrap();
                let plan = PreparedBrush::new(
                    273,
                    265,
                    depth,
                    &points,
                    Some(&pressures),
                    settings,
                    [71, 137, 211, 193],
                    erase,
                    None,
                    Some(&polygon),
                    &budget,
                    &mut || Ok(()),
                )
                .unwrap();
                assert_eq!(plan.frame(), [273, 265]);
                assert_eq!(plan.depth(), depth);
                for &key in plan.tiles() {
                    let source = native_tile(&raster, key, &budget);
                    let result = plan.render_tile(key, &source, &mut || Ok(())).unwrap();
                    let wanted = native_tile(&expected, key, &budget);
                    assert_eq!(&*result, &*wanted, "depth{depth} tip{case} tile{key:?}");
                }
                drop(plan);
                assert_eq!(budget.used(), 0);
            }
        }
    }

    #[test]
    fn cancellation_wrong_pool_and_capacity_refuse_without_changing_source() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let raster = Raster::new_depth(256, 256, 16);
        let plan = PreparedBrush::new(
            256,
            256,
            16,
            &[[10., 10.], [150., 150.]],
            None,
            Settings::default(),
            [37, 91, 173, 255],
            false,
            None,
            None,
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
        let source = native_tile(&raster, (0, 0), &budget);
        let before = budget.used();
        let mut checks = 0;
        assert!(plan
            .render_tile((0, 0), &source, &mut || {
                checks += 1;
                if checks == 4 {
                    Err("Cancelled captured stroke".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert_eq!(budget.used(), before);
        assert!(source.iter().all(|v| *v == 0));
        let foreign = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let wrong = native_tile(&raster, (0, 0), &foreign);
        assert!(plan.render_tile((0, 0), &wrong, &mut || Ok(())).is_err());
        assert!(plan.render_tile((1, 0), &source, &mut || Ok(())).is_err());
        let held_bytes = budget.limit() - before - source.len();
        let held = budget.reserve_bytes(held_bytes, held_bytes).unwrap();
        assert!(plan.render_tile((0, 0), &source, &mut || Ok(())).is_err());
        assert_eq!(budget.used(), before + held.bytes());
        drop(held);
        drop(wrong);
        drop(source);
        drop(plan);
        assert_eq!(budget.used(), 0);
        assert_eq!(foreign.used(), 0);
    }

    #[test]
    fn immutable_preview_clone_keeps_output_charge_until_last_owner() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let source = native_tile(&Raster::new_depth(256, 256, 16), (0, 0), &budget);
        let plan = PreparedBrush::new(
            256,
            256,
            16,
            &[[10., 10.]],
            None,
            Settings::default(),
            [17, 91, 201, 255],
            false,
            None,
            None,
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
        let output = plan.render_tile((0, 0), &source, &mut || Ok(())).unwrap();
        let preview = output.clone();
        drop(output);
        drop(source);
        drop(plan);
        assert_eq!(budget.used(), 512 * 1024);
        drop(preview);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn preview_changes_follow_exact_dab_prefix_and_restore_removed_tail_tiles() {
        let budget = ResidentBudget::new(12 * 1024 * 1024).unwrap();
        let settings = Settings {
            radius: 8.,
            spacing: 1.,
            ..Settings::default()
        };
        let make = |points: &[[f32; 2]]| {
            PreparedBrush::new(
                1024,
                256,
                16,
                points,
                None,
                settings,
                [91, 137, 211, 255],
                false,
                None,
                None,
                &budget,
                &mut || Ok(()),
            )
            .unwrap()
        };
        let a = make(&[[10., 10.], [400., 10.]]);
        let b = make(&[[10., 10.], [450., 10.]]);
        let changed = b.changed_tiles(&a).unwrap();
        assert!(!changed.contains((0, 0)));
        assert!(changed.contains((1, 0)));
        assert_eq!(changed.iter().collect::<Vec<_>>(), vec![(1, 0)]);
        let long = make(&[[10., 10.], [530., 10.]]);
        let short = make(&[[10., 10.]]);
        assert!(short.changed_tiles(&long).unwrap().contains((2, 0)));
        assert!(!short.tiles().contains(&(2, 0)));
        assert!(short.changed_tiles(&short).unwrap().iter().next().is_none());
        drop(a);
        drop(b);
        drop(long);
        drop(short);
        assert_eq!(budget.used(), 0);
    }
}
