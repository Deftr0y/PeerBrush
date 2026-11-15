//! Retained mask samples outside the current paint frame.
use super::input::RasterInput;
use crate::{
    engine::Document,
    large_document::{Error, PinnedBytes, ResidentBudget},
    raster::{backed::Region, check_size, Raster, Surface, SurfaceRegion, TILE},
    retained::{pose::Pose, Original},
    transform::native::{self, Mapping, Work},
};

pub(crate) struct MaskOriginal<'a> {
    pub(crate) source: &'a Surface,
    pub(crate) origin: [i32; 2],
    source_bounds: [i32; 4],
    pose: Option<Pose>,
    pool: &'a ResidentBudget,
    disk: &'a crate::large_document::DiskBudget,
    work: std::cell::RefCell<Work>,
    current: &'a Surface,
    current_origin: [i32; 2],
}

pub(crate) fn bounds(origin: [i32; 2], shape: [u32; 2]) -> [i32; 4] {
    let [x, y] = origin;
    [x, y, x + shape[0] as i32, y + shape[1] as i32]
}

pub(crate) fn contains(outer: [i32; 4], inner: [i32; 4]) -> bool {
    outer[0] <= inner[0] && outer[1] <= inner[1] && inner[2] <= outer[2] && inner[3] <= outer[3]
}

fn intersect(a: [i32; 4], b: [i32; 4]) -> Option<[i32; 4]> {
    let area = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (area[0] < area[2] && area[1] < area[3]).then_some(area)
}

fn local([l, t, r, b]: [i32; 4], [x, y]: [i32; 2]) -> [i32; 4] {
    [l - x, t - y, r - x, b - y]
}

fn integer_offset([a, b, c, d, x, y]: [f64; 6]) -> Option<[i32; 2]> {
    if [a, b, c, d] != [1., 0., 0., 1.]
        || [x, y]
            .into_iter()
            .any(|v| v.abs() > 200_000. || v.fract() != 0.)
    {
        return None;
    }
    Some([x as i32, y as i32])
}

fn pixel_bounds([l, t, r, b]: [f64; 4]) -> Result<[i32; 4], String> {
    let rect = [l.floor(), t.floor(), r.ceil(), b.ceil()];
    if rect.iter().any(|v| !v.is_finite() || v.abs() > 200_000.) || l >= r || t >= b {
        return Err("Retained mask projection exceeds the coordinate limits".into());
    }
    Ok(rect.map(|v| v as i32))
}

fn resident_original(current: &Raster) -> Result<Option<(&Original, [i32; 4], bool)>, String> {
    let Some(original) = current.retained.as_deref() else {
        return Ok(None);
    };
    let offset = if original.projective.is_none() && original.warps.is_empty() {
        integer_offset(original.matrix)
    } else {
        None
    };
    let rect = match offset {
        Some(offset) => bounds(offset, [original.pixels.width, original.pixels.height]),
        None => pixel_bounds(original.support_bounds_in(crate::projective::Map::IDENTITY)?)?,
    };
    Ok(
        (!contains(bounds([0, 0], [current.width, current.height]), rect)).then_some((
            original,
            rect,
            offset.is_some(),
        )),
    )
}

fn project_resident(original: &Original, [l, t, r, b]: [i32; 4]) -> Result<Raster, String> {
    let mut shifted = original.clone();
    shifted.matrix = crate::retained::compose(
        [1., 0., 0., 1., -f64::from(l), -f64::from(t)],
        shifted.matrix,
    );
    let mut projected = shifted.render((r - l) as u32, (b - t) as u32)?;
    projected.retained = None;
    Ok(projected)
}

pub(super) fn reveal_resident(
    current: &Raster,
    grown: &mut Raster,
    [dx, dy]: [i32; 2],
) -> Result<(), String> {
    let Some((original, rect, integral)) = resident_original(current)? else {
        return Ok(());
    };
    let old = bounds([dx, dy], [current.width, current.height]);
    let Some(visible) = intersect(
        bounds([0, 0], [grown.width, grown.height]),
        local(rect, [-dx, -dy]),
    )
    .filter(|&visible| !contains(old, visible)) else {
        return Ok(());
    };
    let projected;
    let (source, origin) = if integral {
        (original.pixels.as_ref(), [rect[0] + dx, rect[1] + dy])
    } else {
        projected = project_resident(original, local(visible, [dx, dy]))?;
        (&projected, [visible[0], visible[1]])
    };
    for &(tx, ty) in source.tiles.keys().chain(source.samples16.keys()) {
        let tile = bounds(
            [
                origin[0] + (tx * TILE) as i32,
                origin[1] + (ty * TILE) as i32,
            ],
            [TILE, TILE],
        );
        let Some([l, t, r, b]) = intersect(visible, tile).filter(|&rect| !contains(old, rect))
        else {
            continue;
        };
        for yy in t..b {
            for xx in l..r {
                if xx < old[0] || xx >= old[2] || yy < old[1] || yy >= old[3] {
                    grown.set16(xx, yy, source.get16(xx - origin[0], yy - origin[1]));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn reconcile_resident(
    before: &Document,
    after: &mut Document,
    command: &serde_json::Value,
) -> Result<(), String> {
    if command["mask"] != true
        || !matches!(
            command["op"].as_str(),
            Some(
                "paint"
                    | "smudge"
                    | "clone"
                    | "heal"
                    | "shape"
                    | "gradient"
                    | "fill"
                    | "paint.fill"
                    | "paint.clear_selection"
            )
        )
    {
        return Ok(());
    }
    let target = command["layer"].as_str();
    let Some(old) = before
        .layers
        .iter()
        .find(|layer| Some(layer.id.as_str()) == target)
    else {
        return Ok(());
    };
    let Some(layer) = after.layers.iter_mut().find(|layer| layer.id == old.id) else {
        return Ok(());
    };
    let (Some(before), Some(after)) = (&old.mask, &mut layer.mask) else {
        return Ok(());
    };
    let Ok(step) = crate::mask::paint_step_index(before, command["step"].as_str()) else {
        return Ok(());
    };
    let step = &before.steps[step];
    let Some(paint) = after
        .steps
        .iter_mut()
        .find(|paint| paint.id == step.id && paint.kind == "paint")
    else {
        return Ok(());
    };
    // The regular reconciler preserves an unchanged or explicitly replaced original.
    if paint.pixels.retained.is_none() {
        paint.pixels.retained = update_resident(&step.pixels, &paint.pixels)?;
    }
    Ok(())
}

fn update_resident(before: &Raster, after: &Raster) -> Result<Option<Box<Original>>, String> {
    let Some((original, source_bounds, integral)) = resident_original(before)? else {
        return Ok(None);
    };
    if (before.width, before.height, before.depth) != (after.width, after.height, after.depth) {
        return Ok(None);
    }
    let tiles = after.changed_tile_keys(before);
    let changed = || {
        tiles.iter().flat_map(|&(tx, ty)| {
            let left = (tx * TILE) as i32;
            let top = (ty * TILE) as i32;
            (top..(top + TILE as i32).min(after.height as i32)).flat_map(move |y| {
                (left..(left + TILE as i32).min(after.width as i32)).filter_map(move |x| {
                    let pixel = after.get16(x, y);
                    (pixel != before.get16(x, y)).then_some((x, y, pixel))
                })
            })
        })
    };
    if changed().next().is_none() {
        return Ok(Some(Box::new(original.clone())));
    }
    let area = if integral {
        changed().fold(source_bounds, |area, (x, y, _)| {
            crate::engine::reframe_scope::union(area, [x, y, x + 1, y + 1])
        })
    } else {
        crate::engine::reframe_scope::union(
            source_bounds,
            bounds([0, 0], [after.width, after.height]),
        )
    };
    let [x, y, r, b] = area;
    let (width, height) = ((r - x) as u32, (b - y) as u32);
    check_size(width, height).map_err(|_| {
        "Mask edit would exceed the retained resident frame; current work preserved"
    })?;
    let pixels = if integral {
        let mut pixels = crate::transform::copy_shift(
            &original.pixels,
            width,
            height,
            source_bounds[0] - x,
            source_bounds[1] - y,
        );
        for (px, py, pixel) in changed() {
            pixels.set16(px - x, py - y, pixel);
        }
        pixels
    } else {
        let mut pixels = project_resident(original, area)?;
        // A pixel edit establishes a new integer-grid original. Keep the full
        // current mask exactly, including zeroes, inside that new original.
        for py in 0..after.height as i32 {
            for px in 0..after.width as i32 {
                pixels.set16(px - x, py - y, after.get16(px, py));
            }
        }
        pixels
    };
    Ok(Some(Box::new(Original {
        pixels: Box::new(pixels),
        matrix: [1., 0., 0., 1., f64::from(x), f64::from(y)],
        projective: None,
        warps: Vec::new(),
    })))
}

impl<'a> MaskOriginal<'a> {
    pub(crate) fn capture(input: &RasterInput<'a>) -> Result<Option<Self>, String> {
        let Some(original) = input.metadata().retained().filter(|_| input.is_mask()) else {
            return Ok(None);
        };
        if !input.layer_metadata().mask().is_some_and(|mask| {
            mask.steps().any(|step| {
                step.kind() == "paint" && step.pixels().slot() == input.metadata().slot()
            })
        }) {
            return Ok(None);
        }
        let source = input.document().source(original.pixels().slot())?;
        if source.depth() != input.depth() {
            return Err("Retained mask differs from its captured channel depth".into());
        }
        let [left, top] = input.origin();
        let shape = [source.width(), source.height()];
        let offset = if original.projective().is_none() && original.warps().is_empty() {
            integer_offset(original.matrix())
        } else {
            None
        };
        let (origin, source_bounds, pose) = if let Some([x, y]) = offset {
            let origin = [left + x, top + y];
            (origin, bounds(origin, shape), None)
        } else {
            let pose = Pose::new(shape, source.depth(), Some(original), input.pool())?;
            let rect = pixel_bounds(pose.support_bounds_in(crate::projective::Map::IDENTITY)?)?;
            (input.origin(), local(rect, [-left, -top]), Some(pose))
        };
        if contains(bounds(input.origin(), input.shape()), source_bounds) {
            return Ok(None);
        }
        Ok(Some(Self {
            source,
            origin,
            source_bounds,
            pose,
            pool: input.document().budget(),
            disk: input.document().disk_budget(),
            work: std::cell::RefCell::new(Work::new(128_000_000)?),
            current: input.surface(),
            current_origin: input.origin(),
        }))
    }

    pub(crate) fn bounds(&self) -> [i32; 4] {
        self.source_bounds
    }

    pub(crate) fn transformed(&self) -> bool {
        self.pose.is_some()
    }

    pub(crate) fn admit_projection(&self, [width, height]: [u32; 2]) -> Result<(), String> {
        if let Some(pose) = self.pose.as_ref().filter(|pose| !pose.warps().is_empty()) {
            crate::warp::work_budget(width, height, 1, pose.warps().len(), 0)?;
        }
        Ok(())
    }

    pub(crate) fn admit_frame(&self, origin: [i32; 2], shape: [u32; 2]) -> Result<(), String> {
        if self.transformed() {
            if let Some([l, t, r, b]) = self.revealed_bounds(bounds(origin, shape)) {
                self.admit_projection([(r - l) as u32, (b - t) as u32])?;
            }
        }
        Ok(())
    }

    pub(crate) fn mapping(&self, target: [i32; 2]) -> Result<Mapping, String> {
        let pose = self
            .pose
            .as_ref()
            .ok_or("Mask projection needs its captured placement")?;
        let [x, y] = self.origin;
        let shift = [
            1.,
            0.,
            0.,
            1.,
            f64::from(x - target[0]),
            f64::from(y - target[1]),
        ];
        Mapping::retained(
            pose.frame[0],
            pose.frame[1],
            pose.depth,
            crate::retained::compose(shift, pose.matrix),
            pose.projective,
            pose.warps(),
            self.pool,
        )
    }

    fn current_bounds(&self) -> [i32; 4] {
        bounds(
            self.current_origin,
            [self.current.width(), self.current.height()],
        )
    }

    pub(crate) fn reveals(&self, area: [i32; 4]) -> bool {
        self.revealed_bounds(area).is_some()
    }

    fn revealed_bounds(&self, area: [i32; 4]) -> Option<[i32; 4]> {
        intersect(area, self.bounds()).filter(|&overlap| {
            !contains(self.current_bounds(), overlap)
                && if self.transformed() {
                    !self.source.is_known_zero()
                } else {
                    self.source.has_physical_tiles(local(overlap, self.origin))
                }
        })
    }

    pub(crate) fn prepare(
        &self,
        area: [i32; 4],
        check: &mut dyn FnMut() -> Result<(), Error>,
    ) -> Result<ProjectedMask, String> {
        let prepare =
            |surface: &Surface, rect, origin, check: &mut dyn FnMut() -> Result<(), Error>| {
                let region = Region::new(local(rect, origin)).map_err(|e| e.to_string())?;
                surface
                    .prepare_region(region, check)
                    .map_err(|e| e.to_string())
            };
        let revealed = self.revealed_bounds(area);
        let original = if let Some(rect) = revealed.filter(|_| self.transformed()) {
            let [l, t, r, b] = rect;
            let shape = [(r - l) as u32, (b - t) as u32];
            if shape.iter().any(|&side| side == 0 || side > TILE) {
                return Err("Retained mask sampling exceeds one native tile".into());
            }
            let mapping = self.mapping([l, t])?;
            let checked = std::cell::RefCell::new(&mut *check);
            let tile = native::render_tile(
                self.source,
                &mapping,
                shape,
                (0, 0),
                None,
                self.pool,
                self.disk,
                &mut self.work.borrow_mut(),
                &|| checked.borrow_mut()(),
            )?;
            Some(OriginalRows::Projected(tile.into_bytes(), rect))
        } else if revealed.is_some() {
            Some(OriginalRows::Direct(
                prepare(self.source, area, self.origin, check)?,
                self.origin[1],
            ))
        } else {
            None
        };
        let current = intersect(area, self.current_bounds())
            .map(|rect| {
                prepare(self.current, rect, self.current_origin, check).map(|rows| (rect, rows))
            })
            .transpose()?;
        Ok(ProjectedMask {
            left: area[0],
            stride: usize::from(self.source.depth() / 8) * 4,
            original,
            current,
            current_top: self.current_origin[1],
        })
    }
}

enum OriginalRows {
    Direct(SurfaceRegion, i32),
    Projected(PinnedBytes, [i32; 4]),
}

pub(crate) struct ProjectedMask {
    left: i32,
    stride: usize,
    original: Option<OriginalRows>,
    current: Option<([i32; 4], SurfaceRegion)>,
    current_top: i32,
}

impl ProjectedMask {
    pub(crate) fn copy_row(&self, y: i32, output: &mut [u8]) -> Result<(), String> {
        match &self.original {
            Some(OriginalRows::Direct(rows, top)) => {
                rows.copy_row(y - top, output).map_err(|e| e.to_string())?
            }
            Some(OriginalRows::Projected(bytes, [l, t, r, b])) => {
                output.fill(0);
                if (*t..*b).contains(&y) {
                    let start = (l - self.left) as usize * self.stride;
                    let len = (r - l) as usize * self.stride;
                    let at = (y - t) as usize * TILE as usize * self.stride;
                    output
                        .get_mut(start..start + len)
                        .ok_or("Invalid projected mask row range")?
                        .copy_from_slice(
                            bytes
                                .get(at..at + len)
                                .ok_or("Missing projected mask row")?,
                        );
                }
            }
            None => output.fill(0),
        }
        if let Some(([l, t, r, b], current)) = &self.current {
            if (*t..*b).contains(&y) {
                // Current pixels, including explicit zeroes, win inside the old frame.
                let from = (l - self.left) as usize * self.stride;
                let to = (r - self.left) as usize * self.stride;
                current
                    .copy_row(y - self.current_top, &mut output[from..to])
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, Mask, MaskStep};
    use serde_json::json;

    #[test]
    fn resident_mask_expansion_erase_and_move_keep_off_frame_native_samples() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(9, 7, depth).unwrap();
            let layer = &mut doc.layers[0];
            let id = layer.id.clone();
            layer.x = 3;
            layer.y = 2;
            layer.pixels = Raster::new_depth(3, 2, depth);
            let mut raw = Raster::new_depth(11, 4, depth);
            raw.set16(0, 0, [0, 0, 0, 65535]);
            raw.set16(1, 0, [12347, 12347, 12347, 65535]);
            raw.set16(2, 1, [65535; 4]);
            raw.set16(10, 0, [23987, 23987, 23987, 65535]);
            let revealed = raw.get16(1, 0);
            let off_canvas = raw.get16(10, 0);
            let mut original = Original::from(&raw);
            original.matrix[4] = -2.;
            original.matrix[5] = -1.;
            let mut current = Raster::new_depth(3, 2, depth);
            current.retained = Some(Box::new(original));
            layer.mask = Some(Mask {
                enabled: true,
                linked: false,
                cache_key: crate::engine::id(),
                steps: vec![MaskStep {
                    id: crate::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 0.,
                    pixels: current,
                    settings: serde_json::Value::Null,
                }],
            });
            let color =
                json!({"op":"paint.fill","layer":id,"rect":[0,0,1,1],"color":[255,255,255,255]});
            let expanded = Engine::preview_edits(doc.clone(), &[color]).unwrap();
            let mask = &expanded.layers[0].mask.as_ref().unwrap().steps[0].pixels;
            assert_eq!(mask.get16(2, 1), revealed);
            assert_eq!(mask.get16(3, 2), [0; 4]);
            let erase = json!({"op":"paint","layer":id,"mask":true,"points":[[1.5,1.5]],
                "radius":0.5,"hardness":1,"flow":1,"opacity":1,"erase":true,"color":[0,0,0,255]});
            let preview = Engine::preview_edits(doc.clone(), &[erase.clone()]).unwrap();
            let mut engine = Engine::new();
            engine.resident_state_mut().unwrap().document = doc;
            engine
                .edit("human", &[erase], Some(0), None, "Erase retained mask")
                .unwrap();
            let layer = &engine.resident_state().unwrap().document.layers[0];
            assert_eq!((layer.x, layer.y), (0, 0));
            let mask = &layer.mask.as_ref().unwrap().steps[0].pixels;
            assert_eq!(
                *mask,
                preview.layers[0].mask.as_ref().unwrap().steps[0].pixels
            );
            assert_eq!(mask.get16(1, 1), [0; 4]);
            assert_eq!(mask.get16(2, 1), revealed);
            assert_eq!(mask.get16(3, 2), [0; 4]);
            let original = mask.retained.as_ref().unwrap();
            assert_eq!(original.pixels.get16(0, 0), [0; 4]);
            assert_eq!(original.pixels.get16(10, 0), off_canvas);
            assert_eq!(engine.history_lengths(), [1, 0]);

            engine
                .edit(
                    "human",
                    &[json!({"op":"move","layer":id,"dx":3,"selection_only":false})],
                    Some(1),
                    None,
                    "Reveal off-canvas mask",
                )
                .unwrap();
            let moved = &engine.resident_state().unwrap().document.layers[0];
            assert_eq!((moved.x, moved.y), (3, 0));
            assert_eq!(
                moved.mask.as_ref().unwrap().steps[0].pixels.get16(8, 1),
                off_canvas
            );
            assert_eq!(engine.history_lengths(), [2, 0]);
        }
    }
}
