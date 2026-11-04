//! Provisional native paint tiles. Input/output bytes have an explicit shared
//! resident owner; this kernel performs no I/O and grants no document authority.
use super::{
    accumulated16, accumulated8, apply_coverage, coverage_pixel16, dab_bounds, dabs,
    visit_tile_dab, work_budget, Dab, Settings, Tip,
};
use crate::{
    large_document::{PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
    raster::{Pixel, TILE},
};

const PLAN_BYTES: usize = 2 * 1024 * 1024;

/// Conservative changed preview footprint, including dabs removed from a tail.
/// A removed tile is restored by dropping its preview override, not repainting
/// on the previous result. The captured baseline remains immutable.
pub(crate) struct TileChanges {
    bits: [u64; 64],
    columns: u32,
    rows: u32,
}
impl TileChanges {
    pub(crate) fn contains(&self, key: (u32, u32)) -> bool {
        if key.0 >= self.columns || key.1 >= self.rows {
            return false;
        }
        let at = (key.1 * self.columns + key.0) as usize;
        self.bits[at / 64] & (1 << (at % 64)) != 0
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
    color: Pixel,
    erase: bool,
    clip: [i32; 4],
    polygon: Option<&'a [[f32; 2]]>,
    dabs: Vec<Dab>,
    tiles: Vec<(u32, u32)>,
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
        check()?;
        if width == 0 || height == 0 || width > 16000 || height > 16000 || ![8, 16].contains(&depth)
        {
            return Err("Invalid native brush frame".into());
        }
        if polygon.is_some_and(|points| points.iter().flatten().any(|v| !v.is_finite())) {
            return Err("Invalid native brush polygon".into());
        }
        let metadata = budget
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
        let mut bits = [0u64; 64];
        if settings.opacity > 0.0 && settings.flow > 0.0 && color[3] != 0 {
            for &dab in &dabs {
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
                for y in top as u32 / TILE..=(bottom - 1) as u32 / TILE {
                    for x in left as u32 / TILE..=(right - 1) as u32 / TILE {
                        let index = (y * columns + x) as usize;
                        bits[index / 64] |= 1 << (index % 64);
                    }
                }
            }
        }
        let count = bits.iter().map(|v| v.count_ones() as usize).sum();
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(count)
            .map_err(|_| "Native brush tile metadata allocation failed")?;
        for y in 0..rows {
            for x in 0..columns {
                let index = (y * columns + x) as usize;
                if bits[index / 64] & (1 << (index % 64)) != 0 {
                    tiles.push((x, y));
                }
            }
        }
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
            budget: budget.clone(),
            _metadata: metadata,
        })
    }

    pub(crate) fn tiles(&self) -> &[(u32, u32)] {
        &self.tiles
    }

    /// Clone/heal use the existing 16-bit dab coverage at both source depths.
    /// Opacity is applied by their native sampling kernel after this quantization.
    pub(crate) fn coverage16_tile(
        &self,
        coordinate: (u32, u32),
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PinnedBytes, String> {
        check()?;
        if self
            .tiles
            .binary_search_by_key(&(coordinate.1, coordinate.0), |&(x, y)| (y, x))
            .is_err()
        {
            return Err("Native coverage tile is outside its prepared footprint".into());
        }
        let length = (TILE * TILE * 2) as usize;
        let mut coverage =
            ScratchBytes::new(&self.budget, length, length).map_err(|e| e.to_string())?;
        let origin = [(coordinate.0 * TILE) as i32, (coordinate.1 * TILE) as i32];
        let tip = Tip::new(self.settings);
        for &dab in &self.dabs {
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
        let mut changed = TileChanges {
            bits: [0; 64],
            columns: self.width.div_ceil(TILE),
            rows: self.height.div_ceil(TILE),
        };
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
                    let at = (y * changed.columns + x) as usize;
                    changed.bits[at / 64] |= 1 << (at % 64);
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
        if self
            .tiles
            .binary_search_by_key(&(coordinate.1, coordinate.0), |&(x, y)| (y, x))
            .is_err()
        {
            return Err("Native brush tile is outside its prepared footprint".into());
        }
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
        for &dab in &self.dabs {
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
            let color = self.color.map(|v| u16::from(v) * 257);
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
                let result = coverage_pixel16(old, alpha, self.settings.opacity, color, self.erase);
                for (channel, value) in pixel.chunks_exact_mut(2).zip(result) {
                    channel.copy_from_slice(&value.to_be_bytes());
                }
            }
        } else {
            apply_coverage(
                &mut output,
                &coverage,
                self.settings.opacity,
                self.color,
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
        let held = budget
            .reserve_bytes(1024 * 1024 + 1, 1024 * 1024 + 1)
            .unwrap();
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
