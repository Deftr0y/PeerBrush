//! Derived live-preview pixels. Dirty updates preserve the full preview sampling grid.
//! Cache keys identify a gesture and its baseline/commands; this never edits source tiles/history.
use crate::{
    compositor::Plan,
    engine::{reframe_scope::union, Document, Engine},
    raster::Pixel,
};
use serde_json::Value;
pub(crate) mod native_grid;
pub(crate) mod regions;

#[derive(Default)]
pub struct Cache {
    key: Option<(String, String, u64, u32, Option<String>, bool)>,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    previous: Option<[i32; 4]>,
    full_upload: bool,
    canvas_job: Option<u64>,
    stroke: Option<Stroke>,
    // Replays can advance the stroke before their pixels reach this cache.
    stroke_damage: Option<[i32; 4]>,
    prepared_mask: Option<regions::PreparedMask>,
    prepared8: Option<regions::Prepared8>,
    prepared16: Option<regions::Prepared16>,
    selection_base: Option<SelectionBase>,
}
struct SelectionBase {
    gesture: String,
    document: String,
    revision: u64,
    edge: u32,
    target: Option<String>,
    mask: bool,
}
struct Stroke {
    key: String,
    prepared: Document,
    layer: usize,
    step: Option<usize>,
    session: crate::brush::Session,
}
pub struct Rendered {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub dirty: Option<[u32; 4]>,
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasPatch {
    pub base_job: u64,
    pub bounds: [u32; 4],
}

pub(crate) struct CanvasFrame {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub patch: Option<CanvasPatch>,
}

/// Filters with finite support can update a padded rectangle. Warps require a full render.
pub fn supports_dirty(doc: &Document) -> bool {
    dirty_padding(doc).is_some()
}

fn mask_padding(mask: &crate::engine::Mask) -> Option<i32> {
    mask.steps.iter().filter(|step| step.enabled).try_fold(0i32, |padding, step| {
        let reach = match step.kind.as_str() {
            "gaussian" => crate::effects::gaussian_radii(crate::effects::number(&step.settings, "radius", 8.))
                .iter().sum::<usize>() as i32,
            "blur" if step.value >= 0.5 => 3 * ((step.value.round().clamp(1., 64.) as i32 + 2) / 3),
            "fill" | "paint" | "invert" | "levels" | "curves" | "adjust" | "blur" => 0,
            _ => return None,
        };
        Some(padding.saturating_add(reach))
    })
}

/// Sum all active kernel reaches conservatively: nesting, clipping and adjustment backdrops
/// may compose several filters. This also includes disabled masks shown in isolation.
fn dirty_padding(doc: &Document) -> Option<i32> {
    // Profiled sources are protected; their full display path converts native
    // samples before projection and must not mix with raw regional pixels.
    if doc.icc_profile.is_some() {
        return None;
    }
    let gaussian = |radius| crate::effects::gaussian_radii(radius).iter().sum::<usize>() as i32;
    let mut padding = crate::filters::reach(doc)?;
    for layer in &doc.layers {
        for effect in layer.effects.iter().filter(|effect| effect.enabled) {
            let reach = match effect.kind.as_str() {
                "blur" => gaussian(crate::effects::number(&effect.settings, "radius", 8.)),
                "bloom" => gaussian(crate::effects::number(&effect.settings, "spread", 12.)),
                "levels" | "curves" | "adjust" | "color_balance" | "hsl" | "invert"
                | "grayscale" | "posterize" | "threshold" | "channel_clamp"
                | crate::effects::legacy_brightness::KIND | crate::effects::modern_brightness::KIND
                | crate::effects::exposure::KIND | crate::effects::channel_mixer::KIND
                | crate::effects::black_white::KIND | crate::effects::selective_color::KIND
                | crate::effects::vibrance::KIND | crate::effects::photo_filter::KIND
                | crate::effects::gradient_map::KIND | crate::effects::photoshop_color_balance::KIND
                | crate::effects::photoshop_hue_saturation::KIND | crate::effects::color_lookup::KIND => 0,
                _ => return None,
            };
            padding = padding.saturating_add(reach);
        }
        if let Some(mask) = &layer.mask {
            padding = padding.saturating_add(mask_padding(mask)?);
        }
    }
    Some(padding)
}
impl Cache {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    /// Reuse exact gesture coverage for a single paint command; other operations use the Engine.
    pub fn edit(
        &mut self,
        base: Document,
        commands: &[Value],
        gesture: &str,
    ) -> Result<Document, String> {
        self.edit_with_region(base, commands, gesture)
            .map(|(doc, _)| doc)
    }
    /// Replay and return the conservative document-space change since the
    /// cache's preceding render, including replays superseded before rendering.
    /// Smudge replays from the source and uses the engine's conservative stroke extent.
    /// `None` requires a full render when the existing baseline cannot be reused.
    pub fn edit_with_region(
        &mut self,
        base: Document,
        commands: &[Value],
        gesture: &str,
    ) -> Result<(Document, Option<[i32; 4]>), String> {
        if commands.len() != 1
            || commands[0]["op"] != "paint"
            || commands[0].get("preset").is_some()
            || gesture.is_empty()
            || base.selection_coverage.is_some()
            || !crate::edit_bounds::covers_canvas(
                &base,
                commands[0]["layer"].as_str().unwrap_or(""),
            )
        {
            if self.stroke.take().is_some() {
                self.key = None;
                self.stroke_damage = None;
            }
            let damage = match commands {
                [command]
                    if !gesture.is_empty() && command["op"] == "smudge" && command.get("preset").is_none() =>
                {
                    Engine::resident_command_scopes(&base, command)
                        .first()
                        .and_then(|scope| scope.rect)
                }
                _ => None,
            };
            return Engine::preview_edits(base, commands).map(|doc| (doc, damage));
        }
        let command = &commands[0];
        let mut settings = crate::brush::Settings::from_command(command)?;
        let (mut points, pressures) = crate::brush::points_from_command(command)?;
        let metadata: std::collections::BTreeMap<_, _> = command
            .as_object()
            .ok_or("Invalid paint command")?
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "points" | "pressures"))
            .collect();
        let key = serde_json::to_string(&serde_json::json!([
            gesture,
            base.id,
            base.revision,
            base.width,
            base.height,
            base.bit_depth,
            base.read_only,
            base.selection,
            base.selection_polygon,
            metadata
        ]))
        .map_err(|e| e.to_string())?;
        let reset = self.stroke.as_ref().is_none_or(|stroke| stroke.key != key);
        if reset {
            let rendered_baseline = self.key.as_ref().is_some_and(|key| {
                if key.1 != base.id || key.2 != base.revision {
                    return false;
                }
                if let Some(mask) = &self.prepared_mask {
                    base.layers
                        .iter()
                        .find(|layer| Some(layer.id.as_str()) == key.4.as_deref())
                        .is_some_and(|layer| mask.matches_baseline(&base, layer))
                } else if base.bit_depth == 16 {
                    self.prepared16.as_ref().is_some_and(|p| p.matches_baseline(&base))
                } else {
                    self.prepared8.as_ref().is_some_and(|p| p.matches_baseline(&base))
                }
            });
            // Preparation follows the same Engine target, mask-step, lock and input rules.
            let mut preparation = command.clone();
            preparation["opacity"] = serde_json::json!(0.0);
            let prepared = Engine::preview_edits(base, &[preparation])?;
            let layer = prepared
                .layers
                .iter()
                .position(|layer| Some(layer.id.as_str()) == command["layer"].as_str())
                .ok_or("Paint target no longer exists")?;
            let selected = &prepared.layers[layer];
            settings.texture_origin = [selected.x, selected.y];
            let mask = command["mask"].as_bool().unwrap_or(false);
            let step = if mask {
                let steps = &selected.mask.as_ref().ok_or("Layer has no mask")?.steps;
                let index = if let Some(id) = command["step"].as_str() {
                    steps
                        .iter()
                        .position(|step| step.kind == "paint" && step.id == id)
                } else {
                    steps.iter().rposition(|step| step.kind == "paint")
                };
                Some(index.ok_or("Add a paint step to the mask")?)
            } else {
                None
            };
            let clip = prepared.selection.map(|area| {
                [
                    area[0] - selected.x,
                    area[1] - selected.y,
                    area[2] - selected.x,
                    area[3] - selected.y,
                ]
            });
            let polygon = crate::selection::polygon(&prepared).map(|points| {
                points
                    .iter()
                    .map(|point| [point[0] - selected.x as f32, point[1] - selected.y as f32])
                    .collect::<Vec<_>>()
            });
            let raster = step.map_or(&selected.pixels, |index| {
                &selected.mask.as_ref().unwrap().steps[index].pixels
            });
            let session = crate::brush::Session::with_native_color(
                raster,
                settings,
                crate::engine::raster_color(command, raster.depth)?,
                command["erase"].as_bool().unwrap_or(false),
                clip,
                polygon.as_deref(),
            )?;
            self.stroke = Some(Stroke {
                key,
                prepared,
                layer,
                step,
                session,
            });
            self.stroke_damage = None;
            if let Some(key) = self.key.as_mut().filter(|_| rendered_baseline) {
                // Transfer only an exact baseline; render still checks size, isolation and mask.
                self.full_upload |= key.0 != gesture;
                key.0 = gesture.to_owned();
                self.previous = None;
            } else {
                self.key = None;
            }
        }
        let stroke = self.stroke.as_mut().unwrap();
        let selected = &stroke.prepared.layers[stroke.layer];
        for point in &mut points {
            point[0] -= selected.x as f32;
            point[1] -= selected.y as f32;
        }
        let raster = stroke
            .session
            .update(&points, pressures.as_deref())?
            .clone();
        if let Some(b) = stroke.session.last_update_bounds() {
            let bounds = [
                b[0].saturating_add(selected.x),
                b[1].saturating_add(selected.y),
                b[2].saturating_add(selected.x),
                b[3].saturating_add(selected.y),
            ];
            self.stroke_damage = Some(self.stroke_damage.map_or(bounds, |old| union(old, bounds)));
        }
        let mut doc = stroke.prepared.clone();
        let layer = &mut doc.layers[stroke.layer];
        if let Some(index) = stroke.step {
            let mask = layer.mask.as_mut().unwrap();
            mask.steps[index].pixels = raster;
            mask.cache_key = crate::engine::id();
        } else {
            layer.pixels = raster;
        }
        crate::mask::validate_budget(&doc.layers)?;
        crate::effects::validate_budget(&doc)?;
        if doc.bit_depth == 16 {
            crate::depth16::validate_budget(&doc)?;
        }
        crate::effects::invalidate(&mut doc, commands);
        Ok((doc, self.key.as_ref().and(self.stroke_damage)))
    }
    /// `dirty` is the complete document-space bounds of the replayed paint stroke.
    /// Use a new `key` when the baseline, isolation target or non-paint preview commands change.
    pub fn render(
        &mut self,
        doc: &Document,
        key: &str,
        dirty: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
    ) -> Result<Rendered, String> {
        self.render_checked(doc, key, dirty, edge, target, mask, &|| Ok(()))
    }
    pub(crate) fn render_checked(
        &mut self,
        doc: &Document,
        key: &str,
        dirty: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        check: &crate::render::Check<'_>,
    ) -> Result<Rendered, String> {
        self.canvas_job = None;
        let dirty = self.refresh(doc, key, dirty, edge, target, mask, check)?;
        Ok(Rendered {
            width: self.width,
            height: self.height,
            bytes: self.bytes.clone(),
            dirty,
        })
    }
    pub(crate) fn render_canvas(
        &mut self,
        doc: &Document,
        key: &str,
        dirty: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        presented: Option<u64>,
        job: u64,
        check: &crate::render::Check<'_>,
    ) -> Result<CanvasFrame, String> {
        let base = self.canvas_job.take().filter(|job| Some(*job) == presented);
        let dirty = self.refresh(doc, key, dirty, edge, target, mask, check)?;
        let patch = base.zip(dirty).and_then(|(base_job, bounds)| {
            let [left, top, right, bottom] = bounds;
            (left < right && top < bottom).then_some(CanvasPatch { base_job, bounds })
        });
        let bytes = if let Some(patch) = patch {
            let [left, top, right, bottom] = patch.bounds.map(|value| value as usize);
            let row_bytes = (right - left) * 4;
            let mut bytes = Vec::with_capacity(row_bytes * (bottom - top));
            for y in top..bottom {
                check()?;
                let start = (y * self.width as usize + left) * 4;
                bytes.extend_from_slice(&self.bytes[start..start + row_bytes]);
            }
            bytes
        } else {
            self.bytes.clone()
        };
        check()?;
        self.canvas_job = Some(job);
        Ok(CanvasFrame {
            width: self.width,
            height: self.height,
            bytes,
            patch,
        })
    }
    /// The gesture and source revision identify the committed artwork used for
    /// the draft. Coverage is applied to a copy so the cached pixels stay clean.
    pub(crate) fn render_selection_canvas(
        &mut self,
        base: &Document,
        selection: &Document,
        gesture: &str,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        mode: &str,
        check: &crate::render::Check<'_>,
    ) -> Result<CanvasFrame, String> {
        if !self.selection_base.as_ref().is_some_and(|prior| {
            prior.gesture == gesture && prior.document == base.id && prior.revision == base.revision
                && prior.edge == edge
                && prior.target.as_deref() == target && prior.mask == mask
        }) {
            self.clear();
            let (width, height, bytes, _) = base.preview_checked(None, edge, target, mask, check)?;
            self.width = width;
            self.height = height;
            self.bytes = bytes;
            self.selection_base = Some(SelectionBase {
                gesture: gesture.to_owned(), document: base.id.clone(), revision: base.revision,
                edge, target: target.map(str::to_owned), mask,
            });
        }
        let mut bytes = self.bytes.clone();
        crate::selection::display::apply_region_checked(
            selection, self.width, self.height, &mut bytes,
            [0, 0, selection.width as i32, selection.height as i32], mode, check,
        )?;
        Ok(CanvasFrame { width: self.width, height: self.height, bytes, patch: None })
    }
    fn refresh(
        &mut self,
        doc: &Document,
        key: &str,
        dirty: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        check: &crate::render::Check<'_>,
    ) -> Result<Option<[u32; 4]>, String> {
        self.selection_base = None;
        let result = self
            .render_inner(doc, key, dirty, edge, target, mask, check)
            .and_then(|bounds| check().map(|()| bounds));
        // A transferred stroke baseline may never have reached the UI.
        let full = std::mem::take(&mut self.full_upload);
        if result.is_err() {
            self.clear();
        } else {
            self.stroke_damage = None;
        }
        result.map(|bounds| if full { None } else { bounds })
    }
    fn render_inner(
        &mut self,
        doc: &Document,
        key: &str,
        dirty: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        check: &crate::render::Check<'_>,
    ) -> Result<Option<[u32; 4]>, String> {
        check()?;
        let identity = (
            key.to_owned(),
            doc.id.clone(),
            doc.revision,
            edge,
            target.map(String::from),
            mask,
        );
        let mask_layer = target.filter(|_| mask).map(|id| {
            doc.layers.iter().find(|layer| layer.id == id).ok_or("Layer no longer exists")
        }).transpose()?;
        let padding = mask_layer.map_or_else(|| dirty_padding(doc), |layer| {
            layer.mask.as_ref().map_or(Some(0), mask_padding)
        });
        let compatible = padding.is_some()
            && if let Some(layer) = mask_layer {
                self.prepared_mask.as_ref().is_some_and(|prepared| prepared.compatible(doc, layer))
            } else if doc.bit_depth == 16 {
                self.prepared16.as_ref().is_some_and(|p| p.compatible(doc))
            } else {
                self.prepared8.as_ref().is_some_and(|p| p.compatible(doc))
            };
        if self.key.as_ref() != Some(&identity)
            || dirty.is_none()
            || padding.is_none()
            || !compatible
        {
            let (width, height, bytes, _) = doc.preview_checked(None, edge, target, mask, check)?;
            self.key = Some(identity);
            self.width = width;
            self.height = height;
            self.bytes = bytes;
            self.previous = dirty;
            self.prepared_mask = mask_layer.filter(|_| padding.is_some())
                .map(|layer| regions::PreparedMask::new(doc, layer, check)).transpose()?;
            self.prepared8 = if mask_layer.is_none() && padding.is_some() && doc.bit_depth == 8 {
                Some(regions::Prepared8::new(doc, check)?)
            } else {
                None
            };
            self.prepared16 = if mask_layer.is_none() && padding.is_some() && doc.bit_depth == 16 {
                Some(regions::Prepared16::new(doc, check)?)
            } else {
                None
            };
            check()?;
            return Ok(None);
        }
        let damage = dirty.unwrap();
        let damage = self.stroke_damage.map_or(damage, |pending| union(damage, pending));
        let mut area = self.previous.map_or(damage, |previous| union(damage, previous));
        self.previous = Some(damage);
        let padding = padding.unwrap();
        area = [
            area[0].saturating_sub(padding),
            area[1].saturating_sub(padding),
            area[2].saturating_add(padding),
            area[3].saturating_add(padding),
        ];
        // Cached raw masks clamp their edge pixels beyond their local raster. When an edge
        // changes, update its entire outward strip as well, including offset small layers.
        if let Some(layer) = mask_layer.filter(|layer| {
                layer
                    .mask
                    .as_ref()
                    .is_some_and(crate::engine::Mask::needs_cache)
            })
        {
            if area[0] <= layer.x {
                area[0] = 0;
            }
            if area[1] <= layer.y {
                area[1] = 0;
            }
            if area[2] >= layer.x.saturating_add(layer.pixels.width as i32) {
                area[2] = doc.width as i32;
            }
            if area[3] >= layer.y.saturating_add(layer.pixels.height as i32) {
                area[3] = doc.height as i32;
            }
        }
        if let Some(layer) = mask_layer {
            if doc.bit_depth == 16 { crate::depth16::validate_budget(doc)?; }
            self.prepared_mask.as_mut().ok_or("Raw mask preview baseline is missing")?.update(layer, area, check)?;
        } else if doc.bit_depth == 16 {
            self.prepared16
                .as_mut()
                .ok_or("Native preview baseline is missing")?
                .update(doc, area, check)?;
        } else {
            self.prepared8
                .as_mut()
                .ok_or("Preview baseline is missing")?
                .update(doc, area, check)?;
        }
        let scale = (edge.clamp(1, 8192) as f32 / doc.width.max(doc.height) as f32).min(1.0);
        // Expand around the floor-sampled source grid. Extra border samples are harmless;
        // conservative mapping avoids seams at fractional preview scales.
        let bounds = [
            ((area[0].saturating_sub(2) as f32 * scale).floor() as i32).clamp(0, self.width as i32)
                as u32,
            ((area[1].saturating_sub(2) as f32 * scale).floor() as i32).clamp(0, self.height as i32)
                as u32,
            ((area[2].saturating_add(2) as f32 * scale).ceil() as i32).clamp(0, self.width as i32)
                as u32,
            ((area[3].saturating_add(2) as f32 * scale).ceil() as i32).clamp(0, self.height as i32)
                as u32,
        ];
        if bounds[0] < bounds[2] && bounds[1] < bounds[3] {
            check()?;
            if let Some(layer) = mask_layer {
                let prepared = self.prepared_mask.as_ref().ok_or("Raw mask preview baseline is missing")?;
                let native_scale = (edge.clamp(1, 8192) as f64 / doc.width.max(doc.height) as f64).min(1.);
                let coordinate = |value: u32| if doc.bit_depth == 16 { (value as f64 / native_scale) as i32 }
                    else { (value as f32 / scale) as i32 };
                crate::render::area8(&mut self.bytes, self.width, bounds, check, |x, y| {
                    prepared.sample(layer, coordinate(x), coordinate(y))
                })?;
                return Ok(Some(bounds));
            }
            if target.is_none() && !mask && crate::filters::active(doc) {
                let native_scale =
                    (edge.clamp(1, 8192) as f64 / doc.width.max(doc.height) as f64).min(1.);
                let coordinate = |x: u32| {
                    if doc.bit_depth == 16 {
                        (x as f64 / native_scale) as i32
                    } else {
                        (x as f32 / scale) as i32
                    }
                };
                let output = [
                    coordinate(bounds[0]),
                    coordinate(bounds[1]),
                    coordinate(bounds[2] - 1) + 1,
                    coordinate(bounds[3] - 1) + 1,
                ];
                if doc.bit_depth == 16 {
                    let (work, image) =
                        regions::filtered16(doc, self.prepared16.as_ref().unwrap(), output, check)?;
                    crate::render::area8(&mut self.bytes, self.width, bounds, check, |x, y| {
                        crate::depth16::display_pixel(
                            image.get(coordinate(x) - work[0], coordinate(y) - work[1]),
                        )
                    })?;
                } else {
                    let (work, image) =
                        regions::filtered8(doc, self.prepared8.as_ref().unwrap(), output, check)?;
                    crate::render::area8(&mut self.bytes, self.width, bounds, check, |x, y| {
                        image.get(coordinate(x) - work[0], coordinate(y) - work[1])
                    })?;
                }
                return Ok(Some(bounds));
            }
            let target_index = target
                .map(|id| {
                    doc.layers
                        .iter()
                        .position(|layer| layer.id == id)
                        .ok_or("Layer no longer exists")
                })
                .transpose()?;
            if doc.bit_depth == 16 {
                let prepared = self.prepared16.as_ref().unwrap();
                let plan =
                    crate::depth16::Plan16::with_prepared(doc, &prepared.masks, &prepared.colors);
                let native_scale =
                    (edge.clamp(1, 8192) as f64 / doc.width.max(doc.height) as f64).min(1.0);
                crate::render::area8(&mut self.bytes, self.width, bounds, check, |x, y| {
                    let sx = (x as f64 / native_scale) as i32;
                    let sy = (y as f64 / native_scale) as i32;
                    let word = if let Some(index) = target_index {
                        plan.layer(index, sx, sy)
                    } else {
                        plan.sample(0, sx, sy)
                    };
                    crate::depth16::display_pixel(word)
                })?;
                return Ok(Some(bounds));
            }
            let prepared = self.prepared8.as_ref().unwrap();
            let masks = &prepared.masks;
            if target.is_none() && !mask && scale < 1.0 {
                let xs: Vec<_> = (bounds[0]..bounds[2])
                    .map(|x| (x as f32 / scale) as i32)
                    .collect();
                let ys: Vec<_> = (bounds[1]..bounds[3])
                    .map(|y| (y as f32 / scale) as i32)
                    .collect();
                if let Some(bytes) =
                    crate::gpu::composite::try_render(doc, &prepared.colors, &xs, &ys)
                {
                    let row_bytes = xs.len() * 4;
                    for (row, source) in bytes.chunks_exact(row_bytes).enumerate() {
                        check()?;
                        let start = ((bounds[1] as usize + row) * self.width as usize
                            + bounds[0] as usize)
                            * 4;
                        self.bytes[start..start + row_bytes].copy_from_slice(source);
                    }
                    return Ok(Some(bounds));
                }
            }
            let plan = Plan::new(doc, masks, &prepared.colors);
            crate::render::area8(&mut self.bytes, self.width, bounds, check, |x, y| {
                let sx = (x as f32 / scale) as i32;
                let sy = (y as f32 / scale) as i32;
                if let Some(index) = target_index {
                    plan.layer(index, sx, sy)
                } else {
                    plan.sample(0, sx, sy)
                }
            })?;
        }
        Ok(Some(bounds))
    }
}

/// Baseline compositor retained for performance/equivalence measurements.
pub fn reference(doc: &Document, edge: u32) -> Result<(u32, u32, Vec<u8>), String> {
    crate::mask::validate_budget(&doc.layers)?;
    let masks: Vec<_> = doc
        .layers
        .iter()
        .map(|layer| {
            layer
                .mask
                .as_ref()
                .and_then(|mask| mask.prepare(layer.pixels.width, layer.pixels.height))
        })
        .collect();
    let colors = crate::effects::prepare(doc, &masks)?;
    let scale = (edge.clamp(1, 8192) as f32 / doc.width.max(doc.height) as f32).min(1.0);
    let width = ((doc.width as f32 * scale).round() as u32).max(1);
    let height = ((doc.height as f32 * scale).round() as u32).max(1);
    let mut bytes = vec![0; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let pixel: Pixel = crate::compositor::sample_group(
                doc,
                None,
                (x as f32 / scale) as i32,
                (y as f32 / scale) as i32,
                &masks,
                &colors,
            );
            let at = ((y * width + x) * 4) as usize;
            bytes[at..at + 4].copy_from_slice(&pixel);
        }
    }
    Ok((width, height, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{Mask, MaskStep},
        raster::Raster,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn selection_canvas_reuses_clean_pixels_and_refreshes_for_a_new_revision() {
        for depth in [8, 16] {
            let mut base = Document::new_depth(32, 24, depth).unwrap();
            base.layers[0].pixels.set16(7, 7, [12537, 25671, 53213, 65535]);
            let mut cache = Cache::default();
            for rect in [[2, 2, 12, 12], [14, 3, 26, 16], [2, 2, 12, 12]] {
                let draft = Engine::preview_edits(base.clone(), &[serde_json::json!({
                    "op":"selection", "kind":"rectangle", "rect":rect, "feather":1,
                })]).unwrap();
                let frame = cache.render_selection_canvas(&base, &draft, "gesture", 32, None, false, "overlay", &|| Ok(())).unwrap();
                let (_, _, mut expected, _) = base.preview(None, 32, None, false).unwrap();
                assert_eq!(cache.bytes, expected);
                crate::selection::display::apply(&draft, frame.width, frame.height, &mut expected, "overlay");
                assert_eq!(frame.bytes, expected);
            }
            base.revision += 1;
            base.layers[0].pixels.set16(7, 7, [62119, 9233, 33445, 65535]);
            let frame = cache.render_selection_canvas(&base, &base, "gesture", 32, None, false, "overlay", &|| Ok(())).unwrap();
            assert_eq!(frame.bytes, base.preview(None, 32, None, false).unwrap().2);
        }
    }

    #[test]
    fn canvas_rectangles_require_the_last_delivered_base() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(96, 80, depth).unwrap();
            let mut cache = Cache::default();
            let check = || Ok(());
            let base = cache.render_canvas(&doc, "stroke", None, 96, None, false, None, 10, &check).unwrap();
            assert!(base.patch.is_none());
            let mut displayed = base.bytes;

            doc.layers[0].pixels.set16(13, 15, [31271, 12347, 51481, 65535]);
            let update = cache.render_canvas(
                &doc, "stroke", Some([13, 15, 14, 16]), 96, None, false, Some(10), 11, &check,
            ).unwrap();
            let patch = update.patch.unwrap();
            assert_eq!(patch.base_job, 10);
            let [left, top, right, bottom] = patch.bounds;
            let row_bytes = (right - left) as usize * 4;
            assert_eq!(update.bytes.len(), row_bytes * (bottom - top) as usize);
            assert!(update.bytes.len() < displayed.len());
            for (y, row) in (top..bottom).zip(update.bytes.chunks_exact(row_bytes)) {
                let start = (y * update.width + left) as usize * 4;
                displayed[start..start + row_bytes].copy_from_slice(row);
            }
            assert_eq!(displayed, doc.preview(None, 96, None, false).unwrap().2);

            doc.layers[0].pixels.set16(68, 57, [19001, 30029, 47857, 65535]);
            let skipped = cache.render_canvas(
                &doc, "stroke", Some([68, 57, 69, 58]), 96, None, false, Some(10), 12, &check,
            ).unwrap();
            assert!(skipped.patch.is_none());
            assert_eq!(skipped.bytes, doc.preview(None, 96, None, false).unwrap().2);
        }
    }

    #[test]
    fn isolated_mask_patches_keep_native_samples_without_preparing_unrelated_effects() {
        for depth in [8, 16] {
            for (edge, [x, y], [width, height]) in [(83, [0, 0], [83, 65]), (41, [7, 11], [53, 39])] {
                let mut doc = Document::new_depth(83, 65, depth).unwrap();
                let layer = &mut doc.layers[0];
                layer.x = x;
                layer.y = y;
                layer.pixels = Raster::new_depth(width, height, depth);
                let target = layer.id.clone();
                layer.mask = Some(Mask {
                    enabled: false, linked: false, cache_key: crate::engine::id(),
                    steps: vec![
                        MaskStep {
                            id: "paint".into(), kind: "paint".into(), enabled: true, weight: 1., value: 0.,
                            pixels: Raster::new_depth(width, height, depth), settings: serde_json::json!({}),
                        },
                        MaskStep {
                            id: "feather".into(), kind: "gaussian".into(), enabled: true, weight: 1., value: 0.,
                            pixels: Raster::new_depth(width, height, depth), settings: serde_json::json!({"radius":4.}),
                        },
                    ],
                });
                let mut unrelated = crate::engine::Layer::new("Unrelated", "paint", 83, 65);
                unrelated.pixels = Raster::new_depth(83, 65, depth);
                unrelated.effects.push(crate::effects::Effect {
                    id: crate::engine::id(), kind: "drop_shadow".into(), enabled: true, weight: 1.,
                    settings: crate::effects::defaults("drop_shadow").into(),
                });
                doc.layers.push(unrelated);
                assert!(!supports_dirty(&doc));
                let mut cache = Cache::default();
                let check = || Ok(());
                let base = cache.render_canvas(&doc, "", None, edge, Some(&target), true, None, 1, &check).unwrap();
                assert!(cache.prepared_mask.is_some() && cache.prepared8.is_none() && cache.prepared16.is_none());
                let mut shown = base.bytes;
                if x == 0 {
                    let points = [[9., 15.], [52., 24.], [71., 35.]];
                    for count in 1..=points.len() {
                        let command = serde_json::json!({"op":"paint","layer":target,"mask":true,
                            "step":"paint","radius":2.,"color":[0,0,0,255],"points":points[..count]});
                        let (draft, dirty) = cache
                            .edit_with_region(doc.clone(), &[command], "stroke")
                            .unwrap();
                        assert!(dirty.is_some(), "The stroke should reuse the displayed mask baseline");
                        let job = count as u64 + 1;
                        let frame = cache.render_canvas(
                            &draft, "stroke", dirty, edge, Some(&target), true, Some(job - 1), job, &check,
                        ).unwrap();
                        assert_eq!(frame.patch.is_some(), count > 1);
                        let expected = draft.preview(None, edge, Some(&target), true).unwrap().2;
                        assert_eq!(cache.bytes, expected, "depth {depth}, stroke samples {count}");
                    }
                } else {
                    for (job, mask_x) in [(2, 0), (3, 26)] {
                        let mask = doc.layers[0].mask.as_mut().unwrap();
                        mask.steps[0].pixels.set16(mask_x, 15, [18371, 18371, 18371, 65535]);
                        mask.cache_key = crate::engine::id();
                        let dirty = Some([x + mask_x, y + 15, x + mask_x + 1, y + 16]);
                        let frame = cache.render_canvas(&doc, "", dirty, edge, Some(&target), true, Some(job - 1), job, &check).unwrap();
                        let patch = frame.patch.unwrap();
                        assert_eq!(patch.base_job, job - 1);
                        let [left, top, right, bottom] = patch.bounds;
                        let row_bytes = ((right - left) * 4) as usize;
                        for (row, pixels) in (top..bottom).zip(frame.bytes.chunks_exact(row_bytes)) {
                            let start = ((row * frame.width + left) * 4) as usize;
                            shown[start..start + row_bytes].copy_from_slice(pixels);
                        }
                        assert_eq!(shown, doc.preview(None, edge, Some(&target), true).unwrap().2);
                    }
                }
            }
        }
    }

    #[test]
    fn skipped_stroke_replays_keep_damage_until_the_canvas_is_rendered() {
        for depth in [8, 16] {
            let doc = Document::new_depth(128, 64, depth).unwrap();
            let target = &doc.layers[0].id;
            let mut cache = Cache::default();
            let check = || Ok(());
            cache
                .render_canvas(&doc, "", None, 128, None, false, None, 1, &check)
                .unwrap();
            let paint = |points: &[[f32; 2]], color: [u8; 4]| {
                serde_json::json!({"op":"paint", "layer":target, "radius":2., "color":color, "points":points})
            };
            let points = [[12., 20.], [58., 20.], [92., 20.]];
            let color = [180, 40, 91, 255];
            let (draft, dirty) = cache
                .edit_with_region(doc.clone(), &[paint(&points[..1], color)], "stroke")
                .unwrap();
            cache
                .render_canvas(&draft, "stroke", dirty, 128, None, false, Some(1), 2, &check)
                .unwrap();
            cache
                .edit_with_region(doc.clone(), &[paint(&points[..2], color)], "stroke")
                .unwrap();
            let latest = paint(&points, color);
            let (draft, dirty) = cache
                .edit_with_region(doc.clone(), std::slice::from_ref(&latest), "stroke")
                .unwrap();
            let frame = cache
                .render_canvas(&draft, "stroke", dirty, 128, None, false, Some(2), 4, &check)
                .unwrap();
            let patch = frame.patch.unwrap();
            assert_eq!(patch.base_job, 2);
            assert!(patch.bounds[0] <= 35 && patch.bounds[2] > 92);
            let expected = Engine::preview_edits(doc.clone(), &[latest]).unwrap();
            assert_eq!(cache.bytes, expected.preview(None, 128, None, false).unwrap().2);

            // A different stroke preparation cannot patch over pixels from the old one.
            let points = [[65., 42.], [78., 42.]];
            let color = [40, 180, 91, 255];
            cache
                .edit_with_region(doc.clone(), &[paint(&points[..1], color)], "stroke")
                .unwrap();
            let latest = paint(&points, color);
            let (draft, dirty) = cache
                .edit_with_region(doc.clone(), std::slice::from_ref(&latest), "stroke")
                .unwrap();
            assert!(dirty.is_none());
            let frame = cache
                .render_canvas(&draft, "stroke", dirty, 128, None, false, Some(4), 6, &check)
                .unwrap();
            let expected = Engine::preview_edits(doc.clone(), &[latest]).unwrap();
            assert!(frame.patch.is_none());
            assert_eq!(frame.bytes, expected.preview(None, 128, None, false).unwrap().2);
            assert_eq!(doc.layers[0].pixels.get16(35, 20), [0; 4]);
        }
    }

    #[test]
    fn smudge_replay_uses_the_captured_selection_for_regional_pixels() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(128, 64, depth).unwrap();
            doc.selection = Some([20, 12, 92, 40]);
            let layer = &mut doc.layers[0];
            for y in 16..30 {
                for x in 14..55 {
                    layer.pixels.set16(x, y, [12039, 51233, 24837, 65535]);
                }
            }
            let command = |end: f32| {
                serde_json::json!({"op":"smudge", "layer":doc.layers[0].id,
                    "radius":4., "wetness":1., "load":1., "pickup":0.,
                    "color":[205,61,90,255], "points":[[14.,23.],[end,23.]]})
            };
            let mut cache = Cache::default();
            let check = || Ok(());
            let (first, dirty) = cache
                .edit_with_region(doc.clone(), &[command(42.)], "smudge")
                .unwrap();
            cache
                .render_canvas(&first, "smudge", dirty, 128, None, false, None, 1, &check)
                .unwrap();
            let (draft, dirty) = cache
                .edit_with_region(doc.clone(), &[command(78.)], "smudge")
                .unwrap();
            assert_eq!(dirty, Some([20, 17, 84, 29]));
            let frame = cache
                .render_canvas(&draft, "smudge", dirty, 128, None, false, Some(1), 2, &check)
                .unwrap();
            assert_eq!(frame.patch.unwrap().base_job, 1);
            assert_eq!(cache.bytes, draft.preview(None, 128, None, false).unwrap().2);
            assert_ne!(draft.layers[0].pixels.get16(65, 23), doc.layers[0].pixels.get16(65, 23));
            assert_eq!(draft.layers[0].pixels.get16(19, 23), doc.layers[0].pixels.get16(19, 23));
            assert_eq!(draft.selection, doc.selection);
            assert_eq!(draft.revision, doc.revision);
        }
    }

    #[test]
    fn unresolved_stroke_presets_use_the_full_shared_engine_preview() {
        for depth in [8, 16] {
            let doc = Document::new_depth(96, 64, depth).unwrap();
            for op in ["paint", "smudge"] {
                let command = serde_json::json!({"op":op, "layer":doc.layers[0].id,
                    "preset":"basic-hard-round", "points":[[32.,32.],[48.,32.]],
                    "wetness":1., "load":1., "pickup":0., "color":[205,61,90,255]});
                let commands = std::slice::from_ref(&command);
                let mut cache = Cache::default();
                cache.render(&doc, "stroke", None, 96, None, false).unwrap();
                let (draft, dirty) = cache
                    .edit_with_region(doc.clone(), commands, "stroke")
                    .unwrap();
                let expected = Engine::preview_edits(doc.clone(), commands).unwrap();
                assert!(dirty.is_none());
                assert!(cache.stroke.is_none());
                assert_ne!(expected.layers[0].pixels.get16(44, 45), [0; 4]);
                assert_eq!(draft.layers[0].pixels, expected.layers[0].pixels);
            }
        }
    }

    #[test]
    fn cancelled_feather_preparation_leaves_no_partial_mask_in_the_cache() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(64, 64, depth).unwrap();
            let mut paint = Raster::new_depth(64, 64, depth);
            paint.set16(31, 32, [7347, 7347, 7347, 65535]);
            doc.layers[0].mask = Some(Mask {
                enabled: true,
                linked: true,
                cache_key: crate::engine::id(),
                steps: vec![
                    MaskStep {
                        id: "paint".into(),
                        kind: "paint".into(),
                        enabled: true,
                        weight: 1.,
                        value: 0.,
                        pixels: paint,
                        settings: serde_json::json!({}),
                    },
                    MaskStep {
                        id: "feather".into(),
                        kind: "gaussian".into(),
                        enabled: true,
                        weight: 1.,
                        value: 0.,
                        pixels: Raster::new_depth(64, 64, depth),
                        settings: serde_json::json!({"radius":4.0}),
                    },
                ],
            });
            for _ in 0..2 {
                let checks = AtomicUsize::new(0);
                let check = || {
                    // One tile of paint is sampled before the first feather pass.
                    if checks.fetch_add(1, Ordering::Relaxed) < 76 {
                        Ok(())
                    } else {
                        Err("Superseded feather".into())
                    }
                };
                let result = if depth == 8 {
                    crate::mask::prepare_layers(&doc.layers, &check).map(drop)
                } else {
                    crate::depth16::prepare_masks_checked(&doc, &check).map(drop)
                };
                assert_eq!(result.unwrap_err(), "Superseded feather");
            }
            let mask = doc.layers[0].mask.as_ref().unwrap();
            if depth == 8 {
                let prepared = crate::mask::prepare_layers(&doc.layers, &|| Ok(())).unwrap();
                let actual = prepared[0].as_ref().unwrap();
                let expected = crate::mask::evaluate(mask, 64, 64);
                assert!(actual.value(31, 32) < 1.0);
                for y in 0..64 {
                    for x in 0..64 {
                        assert_eq!(actual.value(x, y), expected.value(x, y));
                    }
                }
            } else {
                let prepared = crate::depth16::prepare_masks(&doc).unwrap();
                let actual = prepared[0].as_ref().unwrap();
                let expected = crate::depth16::mask::evaluate(mask, 64, 64).unwrap();
                assert!(actual.get(31, 32) < 65535);
                assert_eq!(actual.words, expected.words);
            }
        }
    }

    #[test]
    fn cancelled_dirty_rows_force_an_exact_full_retry() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(48, 48, depth).unwrap();
            let mut cache = Cache::default();
            cache.render(&doc, "stroke", None, 48, None, false).unwrap();
            for y in 4..16 {
                for x in 4..16 {
                    doc.layers[0].pixels.set16(x, y, [31271, 12347, 51481, 65535]);
                }
            }
            let dirty = Some([4, 4, 16, 16]);
            let checks = AtomicUsize::new(0);
            let check = || {
                if checks.fetch_add(1, Ordering::Relaxed) < 8 {
                    Ok(())
                } else {
                    Err("Superseded frame".into())
                }
            };
            assert_eq!(
                cache.render_checked(&doc, "stroke", dirty, 48, None, false, &check).err().as_deref(),
                Some("Superseded frame"),
            );
            assert!(cache.bytes.is_empty());
            let retried = cache.render(&doc, "stroke", dirty, 48, None, false).unwrap();
            assert_eq!(retried.dirty, None);
            assert_eq!(retried.bytes, doc.preview(None, 48, None, false).unwrap().2);
        }
    }
}
