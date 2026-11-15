//! Mask commands consume a prepared, scene-bound evaluator. They never evaluate
//! a mask stack through a source getter or acquire engine publication authority.
use super::{regional_refine, worker::Combine, SelectionSource};
use crate::{
    engine::scene::SceneIdentity,
    large_document::{DiskBudget, Error, ResidentBudget, Result, ScratchBytes, Store},
    raster::{backed::Region, project16, surface::Surface, TILE},
};
use serde_json::Value;
#[path = "regional_masks/native.rs"]
pub(crate) mod native;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub origin: [i32; 2],
}

/// The concrete scene adapter owns all raw paint sources and the captured
/// evaluator plan. Preparing may perform I/O outside engine/UI locks.
pub trait Source {
    type Key: Eq;
    type Prepared: Prepared<Key = Self::Key>;
    fn identity(&self) -> &SceneIdentity;
    fn layer_id(&self) -> &str;
    fn source_key(&self) -> &Self::Key;
    fn frame(&self) -> Frame;
    fn has_mask(&self) -> bool;
    fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool;
    fn prepare_region(
        &mut self,
        region: Region,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<Self::Prepared, String>;
}

/// Raw fractional factors in exactly the declared local footprint. Sampling
/// must be memory-only; errors cannot be replaced with zero or full coverage.
pub trait Prepared {
    type Key: Eq;
    fn identity(&self) -> &SceneIdentity;
    fn layer_id(&self) -> &str;
    fn source_key(&self) -> &Self::Key;
    fn frame(&self) -> Frame;
    fn region(&self) -> Region;
    fn uses_budget(&self, resident: &ResidentBudget) -> bool;
    fn value8(&self, x: i32, y: i32) -> std::result::Result<f32, String>;
    fn value16(&self, x: i32, y: i32) -> std::result::Result<f64, String>;
}

/// Return one opaque native mask candidate. The caller retains current source,
/// lease, cancellation and history checks before adopting its paint step.
pub fn prepare<S: Source>(
    source: &mut S,
    expected: &SceneIdentity,
    layer_id: &str,
    source_key: &S::Key,
    selection: Option<&SelectionSource>,
    mode: Combine,
    settings: &Value,
    guide: Option<&Surface>,
    store: Store,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<Surface, String> {
    let frame = source.frame();
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Mask command needs shared scene disk admission")?;
    if source.identity() != expected
        || source.layer_id() != layer_id
        || source.source_key() != source_key
        || !source.uses_budgets(&pool, &disk)
        || frame.width == 0
        || frame.height == 0
        || frame.width > 16000
        || frame.height > 16000
        || ![8, 16].contains(&frame.depth)
        || frame.origin.iter().any(|v| v.unsigned_abs() > 100000)
    {
        return Err("Mask source differs from captured scene admission".into());
    }
    if selection.is_some_and(|s| matches!(s, SelectionSource::All)) {
        return Err("Select an area before creating its mask".into());
    }
    if matches!(selection, Some(SelectionSource::Coverage(coverage)) if !coverage.uses_budget(&pool)) {
        return Err("Selection source uses a different payload ledger".into());
    }
    if selection.is_none() && !source.has_mask() {
        return Err("Add a mask before refining it".into());
    }
    let mut writer =
        super::native_tiles::Writer::new(frame.width, frame.height, frame.depth, store.clone())
            .map_err(|e| e.to_string())?;
    let bytes = (TILE * TILE * 4) as usize * usize::from(frame.depth / 8);
    let mut tile = ScratchBytes::new(&pool, bytes, bytes).map_err(|e| e.to_string())?;
    for top in (0..frame.height).step_by(TILE as usize) {
        for left in (0..frame.width).step_by(TILE as usize) {
            check().map_err(|e| e.to_string())?;
            let right = (left + TILE).min(frame.width);
            let bottom = (top + TILE).min(frame.height);
            let region = Region::new([left as i32, top as i32, right as i32, bottom as i32])
                .map_err(|e| e.to_string())?;
            let mask = source.prepare_region(region, check)?;
            if mask.identity() != expected
                || mask.layer_id() != layer_id
                || mask.source_key() != source_key
                || mask.frame() != frame
                || mask.region() != region
                || !mask.uses_budget(&pool)
            {
                return Err("Prepared mask differs from captured source footprint".into());
            }
            let selection_region = selection.map(|_| Region::new([
                left as i32 + frame.origin[0], top as i32 + frame.origin[1],
                right as i32 + frame.origin[0], bottom as i32 + frame.origin[1],
            ])).transpose().map_err(|e| e.to_string())?;
            if matches!(mode, Combine::Replace | Combine::Intersect)
                && selection.zip(selection_region).is_some_and(|(selected, region)| !selected.may_cover(region.bounds()))
            {
                writer.opaque_black_tile(left / TILE, top / TILE, &mut tile, &mut || check())
                    .map_err(|e| e.to_string())?;
                #[cfg(test)]
                tests::observe_completed_tile();
                continue;
            }
            tile.fill(0);
            let selected = selection.zip(selection_region)
                .map(|(s, region)| s.prepare_region(region, &pool, &mut || check()))
                .transpose()
                .map_err(|e| e.to_string())?;
            let width = (right - left) as usize;
            let mut coverage = [0; TILE as usize];
            for y in top..bottom {
                check().map_err(|e| e.to_string())?;
                if let Some(selected) = &selected {
                    selected.copy_row(y as i32 + frame.origin[1], &mut coverage[..width])
                        .map_err(|e| e.to_string())?;
                }
                let y_in_tile = (y - top) as usize;
                if selected.is_some() && mode == Combine::Replace {
                    for (x, &new) in coverage[..width].iter().enumerate() {
                        put(&mut tile, frame.depth, x, y_in_tile, u16::from(new) * 257, true);
                    }
                    continue;
                }
                for x in left..right {
                    let x_in_tile = (x - left) as usize;
                    // Round the evaluated mask to native words before combining coverage.
                    let old = if frame.depth == 16 {
                        let value = mask.value16(x as i32, y as i32)?;
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("Invalid native16 mask factor".into());
                        }
                        (value * 65535.).round() as u16
                    } else {
                        let value = mask.value8(x as i32, y as i32)?;
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return Err("Invalid native8 mask factor".into());
                        }
                        (value * 65535.).round() as u16
                    };
                    let value = if selected.is_some() {
                        let new = u16::from(coverage[x_in_tile]) * 257;
                        match mode {
                            Combine::Replace => new,
                            Combine::Add => old.max(new),
                            Combine::Subtract => {
                                ((u64::from(old) * u64::from(65535 - new) + 32767) / 65535) as u16
                            }
                            Combine::Intersect => old.min(new),
                        }
                    } else {
                        old
                    };
                    put(
                        &mut tile,
                        frame.depth,
                        x_in_tile,
                        y_in_tile,
                        value,
                        selection.is_some(),
                    );
                }
            }
            writer
                .tile(left / TILE, top / TILE, &tile, &mut || check())
                .map_err(|e| e.to_string())?;
            #[cfg(test)]
            tests::observe_completed_tile();
        }
    }
    drop(tile);
    let first = writer.finish(&mut || check()).map_err(|e| e.to_string())?;
    let output = if selection.is_some() {
        first
    } else {
        let refined =
            regional_refine::prepare(&first, guide, settings, store.clone(), &mut || check())?;
        opaque(&refined, store, check)?
    };
    check().map_err(|e| e.to_string())?;
    if source.identity() != expected
        || source.layer_id() != layer_id
        || source.source_key() != source_key
        || source.frame() != frame
        || !source.uses_budgets(&pool, &disk)
    {
        return Err("Mask source changed before candidate completion".into());
    }
    Ok(output)
}

fn put(bytes: &mut [u8], depth: u16, x: usize, y: usize, value: u16, opaque: bool) {
    let at = (y * TILE as usize + x) * 4 * usize::from(depth / 8);
    if depth == 8 {
        bytes[at..at + 3].fill(project16(value));
        bytes[at + 3] = if opaque { 255 } else { project16(value) };
    } else {
        for part in bytes[at..at + 6].chunks_exact_mut(2) {
            part.copy_from_slice(&value.to_be_bytes());
        }
        bytes[at + 6..at + 8].copy_from_slice(&if opaque { 65535u16 } else { value }.to_be_bytes());
    }
}

fn opaque(
    input: &Surface,
    store: Store,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<Surface, String> {
    let mut writer = super::native_tiles::Writer::new(
        input.width(),
        input.height(),
        input.depth(),
        store.clone(),
    )
    .map_err(|e| e.to_string())?;
    let n = (TILE * TILE * 4) as usize * usize::from(input.depth() / 8);
    let mut tile = ScratchBytes::new(&store.resident_budget(), n, n).map_err(|e| e.to_string())?;
    for top in (0..input.height()).step_by(TILE as usize) {
        for left in (0..input.width()).step_by(TILE as usize) {
            let right = (left + TILE).min(input.width());
            let bottom = (top + TILE).min(input.height());
            let rect = [left as i32, top as i32, right as i32, bottom as i32];
            if !input.has_physical_tiles(rect) {
                writer.opaque_black_tile(left / TILE, top / TILE, &mut tile, &mut || check())
                    .map_err(|e| e.to_string())?;
                continue;
            }
            tile.fill(0);
            let view = input
                .prepare_region(
                    Region::new(rect).map_err(|e| e.to_string())?,
                    &mut || check(),
                )
                .map_err(|e| e.to_string())?;
            for y in top..bottom {
                check().map_err(|e| e.to_string())?;
                for x in left..right {
                    put(
                        &mut tile,
                        input.depth(),
                        (x - left) as usize,
                        (y - top) as usize,
                        view.pixel16(x as i32, y as i32)
                            .map_err(|e| e.to_string())?[0],
                        true,
                    );
                }
            }
            writer
                .tile(left / TILE, top / TILE, &tile, &mut || check())
                .map_err(|e| e.to_string())?;
        }
    }
    writer.finish(&mut || check()).map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    thread_local! { static COMPLETED_TILES:RefCell<Option<Rc<Cell<usize>>>>=const {RefCell::new(None)}; }
    pub(crate) struct CompletedTiles {
        count: Rc<Cell<usize>>,
        previous: Option<Rc<Cell<usize>>>,
    }
    impl CompletedTiles {
        pub(crate) fn observe() -> Self {
            let count = Rc::new(Cell::new(0));
            let previous = COMPLETED_TILES.with(|slot| slot.replace(Some(count.clone())));
            Self { count, previous }
        }
        pub(crate) fn count(&self) -> usize {
            self.count.get()
        }
    }
    impl Drop for CompletedTiles {
        fn drop(&mut self) {
            COMPLETED_TILES.with(|slot| *slot.borrow_mut() = self.previous.take());
        }
    }
    pub(super) fn observe_completed_tile() {
        COMPLETED_TILES.with(|slot| {
            if let Some(count) = slot.borrow().as_ref() {
                count.set(count.get() + 1);
            }
        });
    }
    use crate::{
        engine::{id, Document, Mask, MaskStep},
        large_document::{Limits, Reservation},
        raster::Raster,
    };
    use serde_json::json;

    struct Legacy<'a> {
        doc: &'a Document,
        identity: SceneIdentity,
        frame: Frame,
        pool: ResidentBudget,
        disk: DiskBudget,
        wrong: u8,
    }
    struct Values {
        identity: SceneIdentity,
        layer_id: String,
        key: u64,
        frame: Frame,
        region: Region,
        eight: Vec<f32>,
        sixteen: Vec<f64>,
        pool: ResidentBudget,
        _charge: Reservation,
    }
    impl Prepared for Values {
        type Key = u64;
        fn layer_id(&self) -> &str {
            &self.layer_id
        }
        fn source_key(&self) -> &u64 {
            &self.key
        }
        fn identity(&self) -> &SceneIdentity {
            &self.identity
        }
        fn frame(&self) -> Frame {
            self.frame
        }
        fn region(&self) -> Region {
            self.region
        }
        fn uses_budget(&self, p: &ResidentBudget) -> bool {
            self.pool.same_pool(p)
        }
        fn value8(&self, x: i32, y: i32) -> std::result::Result<f32, String> {
            Ok(self.eight[self.at(x, y)?])
        }
        fn value16(&self, x: i32, y: i32) -> std::result::Result<f64, String> {
            Ok(self.sixteen[self.at(x, y)?])
        }
    }
    impl Values {
        fn at(&self, x: i32, y: i32) -> std::result::Result<usize, String> {
            let [l, t, r, b] = self.region.bounds();
            if x < l || x >= r || y < t || y >= b {
                return Err("Outside prepared mask".into());
            }
            Ok((y - t) as usize * (r - l) as usize + (x - l) as usize)
        }
    }
    impl Source for Legacy<'_> {
        type Key = u64;
        type Prepared = Values;
        fn layer_id(&self) -> &str {
            &self.doc.layers[0].id
        }
        fn source_key(&self) -> &u64 {
            &991
        }
        fn identity(&self) -> &SceneIdentity {
            &self.identity
        }
        fn frame(&self) -> Frame {
            self.frame
        }
        fn has_mask(&self) -> bool {
            self.doc.layers[0].mask.is_some()
        }
        fn uses_budgets(&self, p: &ResidentBudget, d: &DiskBudget) -> bool {
            self.pool.same_pool(p) && self.disk.same_pool(d)
        }
        fn prepare_region(
            &mut self,
            region: Region,
            check: &dyn Fn() -> Result<()>,
        ) -> std::result::Result<Values, String> {
            check().map_err(|e| e.to_string())?;
            let charge = self
                .pool
                .reserve_bytes(region.pixels() * 12 + 1024, 1024 * 1024)
                .map_err(|e| e.to_string())?;
            let layer = &self.doc.layers[0];
            let old8 = layer
                .mask
                .as_ref()
                .and_then(|m| m.prepare(self.frame.width, self.frame.height));
            let old16 = if self.frame.depth == 16 {
                crate::depth16::mask_image(self.doc, 0)?
            } else {
                None
            };
            let mut eight = Vec::new();
            let mut sixteen = Vec::new();
            eight
                .try_reserve_exact(region.pixels())
                .map_err(|_| "Allocation refused")?;
            sixteen
                .try_reserve_exact(region.pixels())
                .map_err(|_| "Allocation refused")?;
            let [l, t, r, b] = region.bounds();
            for y in t..b {
                for x in l..r {
                    eight.push(layer.mask_value_prepared(x, y, old8.as_deref(), true));
                    sixteen.push(
                        old16
                            .as_ref()
                            .map_or(1., |m| f64::from(m.get(x, y)[0]) / 65535.),
                    );
                }
            }
            let mut identity = self.identity.clone();
            if self.wrong == 1 {
                identity.revision += 1;
            }
            Ok(Values {
                identity,
                layer_id: if self.wrong == 2 {
                    "foreign-layer".into()
                } else {
                    self.doc.layers[0].id.clone()
                },
                key: if self.wrong == 3 { 992 } else { 991 },
                frame: self.frame,
                region,
                eight,
                sixteen,
                pool: self.pool.clone(),
                _charge: charge,
            })
        }
    }

    #[test]
    fn actual_native_mask_commands_preserve_round_combine_projection_and_opaque_alpha() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(96 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(128 * 1024 * 1024, 8).unwrap();
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("peerbrush-mask-command-{}", id()));
            let store = Store::create_with_disk_budget(
                &path,
                pool.clone(),
                disk.clone(),
                Limits {
                    disk_bytes: 128 * 1024 * 1024,
                    max_blocks: 1024,
                    max_resident_blocks: 16,
                    manifest_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            let mut doc = Document::new_depth(33, 29, depth).unwrap();
            doc.layers[0].pixels = Raster::new_depth(17, 19, depth);
            doc.layers[0].x = 5;
            doc.layers[0].y = -3;
            let mut mask = Raster::new_depth(17, 19, depth);
            for y in 0..19 {
                for x in 0..17 {
                    let v = (x * 3727 + y * 1759 + 177) as u16;
                    mask.set16(x, y, [v, v, v, 65535]);
                }
            }
            doc.layers[0].mask = Some(Mask {
                enabled: true,
                linked: true,
                cache_key: id(),
                steps: vec![MaskStep {
                    id: id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 0.731,
                    value: 0.,
                    pixels: mask,
                    settings: Value::Null,
                }],
            });
            let mut selected = Raster::new(33, 29);
            for y in 0..29 {
                for x in 0..33 {
                    selected.set(x, y, [(x * 13 + y * 19) as u8; 4]);
                }
            }
            let coverage = crate::selection::from_mask(selected).unwrap();
            doc.selection = Some(coverage.bounds);
            doc.selection_coverage = Some(coverage.clone());
            let selected = SelectionSource::Coverage(
                super::super::backed::CoverageSource::stage_legacy(
                    &coverage,
                    store.clone(),
                    &mut || Ok(()),
                )
                .unwrap(),
            );
            let identity = SceneIdentity {
                project_id: id(),
                document_id: doc.id.clone(),
                revision: doc.revision,
            };
            for (mode, name) in [
                (Combine::Replace, "replace"),
                (Combine::Add, "add"),
                (Combine::Subtract, "subtract"),
                (Combine::Intersect, "intersect"),
            ] {
                let mut expected = doc.clone();
                crate::selection::masks::apply(
                    &mut expected,
                    0,
                    &json!({"op":"mask.from_selection","mode":name}),
                )
                .unwrap();
                let frame = Frame {
                    width: 17,
                    height: 19,
                    depth,
                    origin: [5, -3],
                };
                let mut source = Legacy {
                    doc: &doc,
                    identity: identity.clone(),
                    frame,
                    pool: pool.clone(),
                    disk: disk.clone(),
                    wrong: 0,
                };
                let actual = prepare(
                    &mut source,
                    &identity,
                    &doc.layers[0].id,
                    &991,
                    Some(&selected),
                    mode,
                    &Value::Null,
                    None,
                    store.clone(),
                    &|| Ok(()),
                )
                .unwrap();
                let view = actual
                    .prepare_region(Region::new([0, 0, 17, 19]).unwrap(), &mut || Ok(()))
                    .unwrap();
                let expected = &expected.layers[0]
                    .mask
                    .as_ref()
                    .unwrap()
                    .steps
                    .last()
                    .unwrap()
                    .pixels;
                for y in 0..19 {
                    for x in 0..17 {
                        assert_eq!(
                            view.pixel16(x, y).unwrap(),
                            expected.get16(x, y),
                            "{depth} {name} at{x},{y}"
                        );
                    }
                }
                drop(view);
                drop(actual);
                for wrong in 1..=3 {
                    source.wrong = wrong;
                    assert!(prepare(
                        &mut source,
                        &identity,
                        &doc.layers[0].id,
                        &991,
                        Some(&selected),
                        mode,
                        &Value::Null,
                        None,
                        store.clone(),
                        &|| Ok(())
                    )
                    .is_err());
                }
            }
            let settings =
                json!({"op":"mask.refine","shift":-1,"feather":3,"smooth":2,"contrast":21});
            let mut expected = doc.clone();
            crate::selection::masks::apply(&mut expected, 0, &settings).unwrap();
            let mut source = Legacy {
                doc: &doc,
                identity: identity.clone(),
                frame: Frame {
                    width: 17,
                    height: 19,
                    depth,
                    origin: [5, -3],
                },
                pool: pool.clone(),
                disk: disk.clone(),
                wrong: 0,
            };
            let actual = prepare(
                &mut source,
                &identity,
                &doc.layers[0].id,
                &991,
                None,
                Combine::Replace,
                &settings,
                None,
                store.clone(),
                &|| Ok(()),
            )
            .unwrap();
            let view = actual
                .prepare_region(Region::new([0, 0, 17, 19]).unwrap(), &mut || Ok(()))
                .unwrap();
            let expected = &expected.layers[0]
                .mask
                .as_ref()
                .unwrap()
                .steps
                .last()
                .unwrap()
                .pixels;
            for y in 0..19 {
                for x in 0..17 {
                    assert_eq!(view.pixel16(x, y).unwrap(), expected.get16(x, y));
                }
            }
            drop(view);
            drop(actual);
            drop(source);
            drop(selected);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
