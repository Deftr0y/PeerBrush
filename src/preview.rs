//! Derived live-preview pixels. Dirty updates preserve the full preview sampling grid.
//! Cache keys identify a gesture and its baseline/commands; this never edits source tiles/history.
use crate::{
    compositor::Plan,
    engine::{Document, Engine},
    raster::Pixel,
};
use serde_json::Value;
pub(crate) mod regions;

#[derive(Default)]
pub struct Cache {
    key: Option<(String, String, u64, u32, Option<String>, bool)>,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    previous: Option<[i32; 4]>,
    stroke: Option<Stroke>,
    prepared8: Option<regions::Prepared8>,
    prepared16: Option<regions::Prepared16>,
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

/// Filters with finite support can update a padded rectangle. Warps require a full render.
pub fn supports_dirty(doc: &Document) -> bool {
    dirty_padding(doc).is_some()
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
                | "grayscale" | "posterize" | "channel_clamp" => 0,
                _ => return None,
            };
            padding = padding.saturating_add(reach);
        }
        if let Some(mask) = &layer.mask {
            for step in mask.steps.iter().filter(|step| step.enabled) {
                let reach = match step.kind.as_str() {
                    "gaussian" => gaussian(crate::effects::number(&step.settings, "radius", 8.)),
                    "blur" if step.value >= 0.5 => {
                        3 * ((step.value.round().clamp(1., 64.) as i32 + 2) / 3)
                    }
                    "fill" | "paint" | "invert" | "levels" | "curves" | "adjust" | "blur" => 0,
                    _ => return None,
                };
                padding = padding.saturating_add(reach);
            }
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
        if commands.len() != 1
            || commands[0]["op"] != "paint"
            || gesture.is_empty()
            || base.selection_coverage.is_some()
            || !crate::edit_bounds::covers_canvas(
                &base,
                commands[0]["layer"].as_str().unwrap_or(""),
            )
        {
            self.stroke = None;
            return Engine::preview_edits(base, commands);
        }
        let command = &commands[0];
        let settings = crate::brush::Settings::from_command(command)?;
        let (mut points, pressures) = crate::brush::points_from_command(command)?;
        let mut metadata = command.clone();
        metadata
            .as_object_mut()
            .ok_or("Invalid paint command")?
            .remove("points");
        metadata.as_object_mut().unwrap().remove("pressures");
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
        if self.stroke.as_ref().is_none_or(|stroke| stroke.key != key) {
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
            let mut color = crate::engine::color(command);
            if mask {
                color = [color[0], color[0], color[0], color[3]];
            }
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
            let session = crate::brush::Session::new(
                raster,
                settings,
                color,
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
        let mut doc = stroke.prepared.clone();
        let layer = &mut doc.layers[stroke.layer];
        if let Some(index) = stroke.step {
            layer.mask.as_mut().unwrap().steps[index].pixels = raster;
        } else {
            layer.pixels = raster;
        }
        crate::mask::validate_budget(&doc.layers)?;
        crate::effects::validate_budget(&doc)?;
        if doc.bit_depth == 16 {
            crate::depth16::validate_budget(&doc)?;
        }
        crate::effects::invalidate(&mut doc, commands);
        Ok(doc)
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
        let result = self.render_inner(doc, key, dirty, edge, target, mask);
        if result.is_err() {
            self.clear();
        }
        result
    }
    fn render_inner(
        &mut self,
        doc: &Document,
        key: &str,
        dirty: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
    ) -> Result<Rendered, String> {
        let identity = (
            key.to_owned(),
            doc.id.clone(),
            doc.revision,
            edge,
            target.map(String::from),
            mask,
        );
        let padding = dirty_padding(doc);
        let compatible = padding.is_some()
            && if doc.bit_depth == 16 {
                self.prepared16.as_ref().is_some_and(|p| p.compatible(doc))
            } else {
                self.prepared8.as_ref().is_some_and(|p| p.compatible(doc))
            };
        if self.key.as_ref() != Some(&identity)
            || dirty.is_none()
            || padding.is_none()
            || !compatible
        {
            let (width, height, bytes, _) = doc.preview(None, edge, target, mask)?;
            self.key = Some(identity);
            self.width = width;
            self.height = height;
            self.bytes = bytes;
            self.previous = dirty;
            self.prepared8 = if padding.is_some() && doc.bit_depth == 8 {
                Some(regions::Prepared8::new(doc)?)
            } else {
                None
            };
            self.prepared16 = if padding.is_some() && doc.bit_depth == 16 {
                Some(regions::Prepared16::new(doc)?)
            } else {
                None
            };
            return Ok(Rendered {
                width,
                height,
                bytes: self.bytes.clone(),
                dirty: None,
            });
        }
        let mut area = dirty.unwrap();
        if let Some(previous) = self.previous {
            area = [
                area[0].min(previous[0]),
                area[1].min(previous[1]),
                area[2].max(previous[2]),
                area[3].max(previous[3]),
            ];
        }
        self.previous = dirty;
        let padding = padding.unwrap();
        area = [
            area[0].saturating_sub(padding),
            area[1].saturating_sub(padding),
            area[2].saturating_add(padding),
            area[3].saturating_add(padding),
        ];
        // Cached raw masks clamp their edge pixels beyond their local raster. When an edge
        // changes, update its entire outward strip as well, including offset small layers.
        if let Some(layer) = target
            .filter(|_| mask)
            .and_then(|target| doc.layers.iter().find(|layer| layer.id == target))
            .filter(|layer| {
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
        if doc.bit_depth == 16 {
            self.prepared16
                .as_mut()
                .ok_or("Native preview baseline is missing")?
                .update(doc, area)?;
        } else {
            self.prepared8
                .as_mut()
                .ok_or("Preview baseline is missing")?
                .update(doc, area)?;
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
                        regions::filtered16(doc, self.prepared16.as_ref().unwrap(), output)?;
                    crate::render::area8(&mut self.bytes, self.width, bounds, |x, y| {
                        crate::depth16::display_pixel(
                            image.get(coordinate(x) - work[0], coordinate(y) - work[1]),
                        )
                    })?;
                } else {
                    let (work, image) =
                        regions::filtered8(doc, self.prepared8.as_ref().unwrap(), output)?;
                    crate::render::area8(&mut self.bytes, self.width, bounds, |x, y| {
                        image.get(coordinate(x) - work[0], coordinate(y) - work[1])
                    })?;
                }
                return Ok(Rendered {
                    width: self.width,
                    height: self.height,
                    bytes: self.bytes.clone(),
                    dirty: Some(bounds),
                });
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
                crate::render::area8(&mut self.bytes, self.width, bounds, |x, y| {
                    let sx = (x as f64 / native_scale) as i32;
                    let sy = (y as f64 / native_scale) as i32;
                    let word = if let Some(index) = target_index {
                        if mask {
                            let v = (plan.mask(index, sx, sy, true) * 65535.)
                                .round()
                                .clamp(0., 65535.) as u16;
                            [v, v, v, 65535]
                        } else {
                            plan.layer(index, sx, sy)
                        }
                    } else {
                        plan.sample(0, sx, sy)
                    };
                    crate::depth16::display_pixel(word)
                })?;
                return Ok(Rendered {
                    width: self.width,
                    height: self.height,
                    bytes: self.bytes.clone(),
                    dirty: Some(bounds),
                });
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
                        let start = ((bounds[1] as usize + row) * self.width as usize
                            + bounds[0] as usize)
                            * 4;
                        self.bytes[start..start + row_bytes].copy_from_slice(source);
                    }
                    return Ok(Rendered {
                        width: self.width,
                        height: self.height,
                        bytes: self.bytes.clone(),
                        dirty: Some(bounds),
                    });
                }
            }
            let plan = Plan::new(doc, masks, &prepared.colors);
            crate::render::area8(&mut self.bytes, self.width, bounds, |x, y| {
                let sx = (x as f32 / scale) as i32;
                let sy = (y as f32 / scale) as i32;
                let pixel = if let Some(index) = target_index {
                    if mask {
                        let layer = &doc.layers[index];
                        let value = (layer.mask_value_prepared(
                            sx - layer.x,
                            sy - layer.y,
                            masks[index].as_deref(),
                            true,
                        ) * 255.0) as u8;
                        [value, value, value, 255]
                    } else {
                        plan.layer(index, sx, sy)
                    }
                } else {
                    plan.sample(0, sx, sy)
                };
                pixel
            })?;
        }
        Ok(Rendered {
            width: self.width,
            height: self.height,
            bytes: self.bytes.clone(),
            dirty: Some(bounds),
        })
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
