//! Native gesture capture shares UI command builders and the worker renderer.
use super::*;
use crate::engine::native::NativeDocument;
use crate::psd::memory36::MetadataCharge;
use crate::workspace::native_dispatch::retryable_source_read;
use std::sync::atomic::Ordering;
type Result<T> = std::result::Result<T, String>;

pub(super) fn covers_rect(outer: [i32; 4], inner: [i32; 4]) -> bool {
    outer[0] <= inner[0] && outer[1] <= inner[1] && outer[2] >= inner[2] && outer[3] >= inner[3]
}

fn area([left, top, right, bottom]: [i32; 4]) -> u64 {
    (right - left) as u64 * (bottom - top) as u64
}

fn pan_limit(buffered: [i32; 4]) -> u64 {
    // Limit alignment to 12.5% more work than the ordinary buffered crop.
    use crate::render::native_preview::{MAX_CANVAS_PIXELS, MAX_PREVIEW_PIXELS};
    let pixels = area(buffered);
    let limit = if pixels > MAX_PREVIEW_PIXELS { MAX_CANVAS_PIXELS } else { MAX_PREVIEW_PIXELS };
    (pixels * 9 / 8).min(limit)
}

fn buffered_region(
    visible: [i32; 4], size: [u32; 2], previous: Option<[i32; 4]>, scale: f32,
) -> [i32; 4] {
    let bounds = [0, 0, size[0] as i32, size[1] as i32];
    let limit = crate::render::native_preview::MAX_PREVIEW_PIXELS;
    let padded = |sample_margin| {
        let margin = |span: i32| (span / 8).min((span / 32).max(32)).min(sample_margin);
        let horizontal = margin(visible[2] - visible[0]);
        let vertical = margin(visible[3] - visible[1]);
        [(visible[0] - horizontal).max(0), (visible[1] - vertical).max(0),
            (visible[2] + horizontal).min(bounds[2]), (visible[3] + vertical).min(bounds[3])]
    };
    // At low zoom, keep roughly 32 screen points ahead of the viewport where
    // the crop dimensions and the materialized preview budget allow it.
    let margin = ((32. / scale).ceil() as i32).clamp(32, 256);
    let mut buffered = padded(margin);
    if margin > 32 && area(buffered) > limit {
        buffered = padded(32);
    }
    let reuse_limit = if area(visible) > limit { pan_limit(visible) } else { pan_limit(buffered).min(limit) };
    if let Some(previous) = previous.filter(|&rect| covers_rect(bounds, rect) && covers_rect(rect, visible)
        && area(rect) <= reuse_limit)
    {
        return previous;
    }
    // Do not push an exact viewport past the materialized preview limit just for pan reuse.
    if area(buffered) <= limit { buffered } else { visible }
}

fn aligned_pan_region(
    visible: [i32; 4],
    buffered: [i32; 4],
    displayed: [i32; 4],
    cell: [i32; 2],
    side: usize,
) -> [i32; 4] {
    let mut aligned = buffered;
    for axis in 0..2 {
        let end = axis + 2;
        let span = displayed[end] - displayed[axis];
        if displayed[axis] <= visible[axis] && displayed[end] >= visible[end]
            && span <= buffered[end] - buffered[axis]
        {
            aligned[axis] = displayed[axis];
            aligned[end] = displayed[end];
        } else if cell[axis] < span {
            let start = visible[axis] - (visible[axis] - displayed[axis]).rem_euclid(cell[axis]);
            if start < 0 {
                return buffered;
            }
            aligned[axis] = start;
        }
        // No tile can match on this axis without overlap on the old grid.
        let overlap = aligned[end].min(displayed[end]) - aligned[axis].max(displayed[axis]);
        if overlap < cell[axis] || (aligned[axis] - displayed[axis]).rem_euclid(cell[axis]) != 0 {
            return buffered;
        }
    }
    let size = [(aligned[2] - aligned[0]) as u32, (aligned[3] - aligned[1]) as u32];
    if area(aligned) <= pan_limit(buffered) && native_texture::viewport_tile_cell(size, side) == Some(cell) {
        aligned
    } else {
        buffered
    }
}

fn append<'a>(
    commands: &mut [&'a Value; 100],
    count: &mut usize,
    command: &'a Value,
) -> Result<()> {
    let slot = commands
        .get_mut(*count)
        .ok_or("Use at most 100 native preview commands")?;
    *slot = command;
    *count += 1;
    Ok(())
}

impl PeerBrush {
    pub(super) fn native_canvas_offscreen(&self) -> bool {
        self.view_rect.zip(self.canvas_viewport).is_some_and(|(canvas, viewport)| {
            !self.view.overlaps_viewport(canvas, viewport)
        })
    }

    pub(super) fn native_canvas_region(&self, document: &NativeDocument) -> Option<[i32; 4]> {
        self.visible_native_region([document.metadata().width(), document.metadata().height()])
    }
    pub(super) fn visible_native_region(&self, size: [u32; 2]) -> Option<[i32; 4]> {
        let canvas = self.view_rect?;
        self.view.visible_region(canvas, self.canvas_viewport?, size)
    }
    // Owned hover and stroke values coexist with their admitted command copy.
    fn native_command_working_bytes(&self) -> Result<usize> {
        let points = self
            .points
            .len()
            .checked_mul(256)
            .and_then(|n| {
                self.stroke_pressures
                    .len()
                    .checked_mul(64)
                    .and_then(|p| n.checked_add(p))
            })
            .ok_or("Native gesture working size overflow")?;
        let strings = [
            self.selected.as_str(),
            self.doc_snapshot.view().id(),
            self.mask_step.as_deref().unwrap_or(""),
            self.straight.document.as_deref().unwrap_or(""),
            self.liquify_effect.as_deref().unwrap_or(""),
            self.retouch_source
                .as_ref()
                .map_or("", |source| source.layer.as_str()),
            self.retouch_source
                .as_ref()
                .map_or("", |source| source.project.as_str()),
        ];
        let hover = self.blend_hover.iter().flat_map(|hover| {
            let mode = match hover.setting {
                super::BlendSetting::Mode(mode) => mode,
                super::BlendSetting::Channels(_) => "",
            };
            hover.layers.iter().flat_map(move |layer| [layer.as_str(), mode])
        });
        strings
            .into_iter()
            .chain(hover)
            .try_fold(points, |sum, value| {
                value
                    .len()
                    .checked_mul(4)
                    .and_then(|n| sum.checked_add(n))
                    .ok_or("Native gesture identifier size overflow")
            })
            .and_then(|bytes| {
                bytes
                    .checked_add(32768)
                    .and_then(|bytes| bytes.checked_add(self.blend_hover.as_ref().map_or(0, |hover| hover.layers.len() * 1024)))
                    .filter(|n| *n <= 64 * 1024 * 1024)
                    .ok_or("Native gesture metadata exceeds its working bound")
            })
            .map_err(str::to_owned)
    }

    pub(super) fn capture_native_canvas(
        &mut self,
        ctx: &egui::Context,
        project: &str,
        document: &NativeDocument,
    ) -> Result<Option<native_dispatch::Input>> {
        self.native_dispatch.gesture_deferred = false;
        self.native_dispatch
            .observe_canvas(self.native_canvas.current().map(|canvas| canvas.tag()));
        if let Some(id) = self.proposal_review.as_deref() {
            return self.capture_native_proposal(
                document,
                project,
                (!self.proposal_original).then_some(id),
            );
        }
        self.refresh_sampled_preview(document.into());
        let has_stroke = self.has_preview_stroke();
        let has_hover = self.blend_hover.is_some();
        let source = self.native_source_preview.as_ref();
        if source.is_some_and(|command| {
            !command.value().owner().uses_budget(document.budget())
                || self.native_source_preview_at > self.transient.len()
        }) {
            return Err("Native source preview no longer belongs to this command capture".into());
        }
        let mut commands = [&Value::Null; 100];
        let mut count = 0;
        for command in &self.transient {
            append(&mut commands, &mut count, command)?;
        }
        for command in self
            .sampled_input
            .as_ref()
            .map_or(&[][..], |input| input.preview_commands())
        {
            append(&mut commands, &mut count, command)?;
        }
        let other_canvas_edit = has_stroke || has_hover || self.geometry.is_some();
        let mixed_refinement = other_canvas_edit || self.layer_drag.is_some() || self.eye_sweep.is_some();
        let mixed_canvas = other_canvas_edit || self.refinement.is_some();
        let mixed_edit = mixed_refinement || self.refinement.is_some();
        let hierarchy_only = !mixed_canvas
            && count != 0
            && count == self.transient.len()
            && source.is_none()
            && self.parameter_gesture.is_none()
            && self.free_transform.is_none()
            && self.source_editor.is_none()
            && self.filter_editor.is_none();
        let mixed_parameter = mixed_edit || count != 0 || source.is_some()
            || self.free_transform.is_some() || self.source_editor.is_some() || self.filter_editor.is_some();
        let mixed_transient = mixed_edit || count != self.transient.len() || self.parameter_gesture.is_some();
        let mixed_tool = mixed_transient || self.filter_editor.is_some();
        if let Some(edit) = &self.parameter_gesture {
            for command in &edit.commands {
                append(&mut commands, &mut count, command)?;
            }
        }
        let hover_count = self.blend_hover.as_ref().map_or(0, |hover| hover.layers.len());
        if count + usize::from(source.is_some()) + hover_count + usize::from(has_stroke) > 100 {
            return Err("Use at most 100 native preview commands".into());
        }
        let source = source.map(|source| (self.native_source_preview_at, source));
        let has_commands = count != 0 || has_hover || has_stroke;
        let size = geometry::canvas_size(
            [document.metadata().width(), document.metadata().height()],
            commands[..count].iter().copied(),
            crate::raster::CanvasLimits::Native,
        )?;
        let full_edge = size[0].max(size[1]).max(8192);
        let time = ctx.input(|i| i.time);
        let editing = has_commands
            || source.is_some()
            || self.gizmo_handle != 0
            || self.animation.progress(time) < 1.
            || self.free_transform.is_some()
            || self.source_editor.is_some()
            || self.filter_editor.is_some()
            || self.geometry.is_some();
        let visible = self.visible_native_region(size);
        let visible_edge = visible.map_or(size[0].max(size[1]), |[l, t, r, b]| (r - l).max(b - t) as u32);
        let display_edge = preview_quality::region_display_edge(
            self.view_rect, size[0], visible_edge, ctx.pixels_per_point(),
        );
        let reduced = self.preview_quality.reduced_edge(visible_edge, display_edge, editing);
        let region = visible.map(|visible| {
            if reduced.is_some() || !self.native_dispatch.allows_pan_margin(document)
                || size != [document.metadata().width(), document.metadata().height()]
            {
                return visible;
            }
            let previous = self.native_dispatch.requested_region(
                document, project, self.preview_epoch.load(Ordering::Relaxed),
            );
            let scale = self.view_rect.map_or(1., |rect| rect.width() / size[0] as f32);
            let buffered = buffered_region(visible, size, previous, scale);
            if previous == Some(buffered) || has_commands || source.is_some()
                || self.animation.active(time) || self.animation.fading()
            {
                return buffered;
            }
            let Some(canvas) = self.native_canvas.current() else { return buffered; };
            let key = canvas.key();
            if key.source != document.key() || key.identity.project_id != project
                || key.identity.document_id != document.metadata().id()
                || key.identity.revision != document.metadata().revision()
                || key.width as i32 != key.rect[2] - key.rect[0]
                || key.height as i32 != key.rect[3] - key.rect[1]
            {
                return buffered;
            }
            let Some(cell) = canvas.reusable_tile_cell() else { return buffered; };
            aligned_pan_region(visible, buffered, key.rect, cell, ctx.input(|i| i.max_texture_side))
        });
        let frame_rect = region.unwrap_or([0, 0, size[0] as i32, size[1] as i32]);
        let target = self.isolate.then_some(self.selected.as_str());
        let mask = self.mask && self.isolate;
        let projection = refinement::preview_projection(self.refinement.as_ref(),
            self.parameter_gesture.as_ref(), self.selection_drag_preview_key().is_some());
        let (target, mask, selection_view) = match projection {
            Some(refinement::PreviewProjection::Selection(mode)) => (target, mask, Some(mode)),
            Some(refinement::PreviewProjection::LayerMask(layer)) => (Some(layer), true, None),
            None => (target, mask, None),
        };
        let presentation = if !has_commands && source.is_none() && selection_view.is_none() {
            self.animation.native_presentation(document, time)?
        } else {
            None
        };
        let can_fade = presentation.is_none() && !has_commands && source.is_none() && selection_view.is_none();
        let baseline = if can_fade {
            self.animation.native_before(document).and_then(|before| {
                self.native_dispatch
                    .fade_baseline(before, project, target, mask)
                    .filter(|(_, baseline, _)| {
                        baseline.tag().key().rect == frame_rect && baseline.tag().matches_edge(full_edge)
                    })
            })
        } else {
            None
        };
        let streamed_base = if can_fade && baseline.is_none() {
            self.animation.native_before(document).and_then(|before| {
                let painted = self.native_canvas.current()?.tag();
                let shown = self.native_receipt.as_ref()
                    .and_then(|receipt| receipt.streamed_before(project, document, painted))
                    .unwrap_or(painted);
                let key = shown.key();
                (key.source == before.key() && key.identity.project_id == project
                    && key.identity.document_id == before.metadata().id() && key.identity.revision == before.metadata().revision()
                    && key.rect == frame_rect && key.target.as_deref() == target && key.mask == mask
                    && key.selection_view.is_none() && shown.matches_region(region))
                    .then(|| (before.clone(), shown.clone()))
            })
        } else { None };
        let blend = if let Some((_, before, generation)) = baseline {
            self.animation.native_blend(document, time)?.map(|blend| (blend, before, generation))
        } else {
            None
        };
        let streamed_blend = if let Some((before, shown)) = streamed_base {
            self.animation.native_blend(document, time)?.map(|blend| (blend, before, shown))
        } else {
            None
        };
        if presentation.is_some() || blend.is_some() || streamed_blend.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        let reduced = reduced.filter(|_| blend.is_none() && streamed_blend.is_none());
        let epoch = self.preview_epoch.load(Ordering::Relaxed);
        if !has_commands
            && source.is_none()
            && selection_view.is_none()
            && presentation.is_none()
            && blend.is_none()
            && streamed_blend.is_none()
            && reduced.is_none()
            && self.native_dispatch.matches_base_capture(
                document, project, full_edge, region, target, mask, epoch,
            )
        {
            self.preview_quality.edge(&[], time, None, full_edge);
            return Ok(None);
        }
        let live_stroke = self.live_gesture.as_ref()
            .filter(|_| has_stroke && count == 0 && !has_hover && source.is_none())
            .map(|gesture| native_dispatch::fingerprint(
                &(gesture, self.tool.label(), &self.selected, self.mask, &self.mask_step),
                1, target, mask, selection_view,
            ))
            .transpose()?
            .map(native_dispatch::LiveGesture::Stroke);
        let live_parameter = self.parameter_gesture.as_ref()
            .filter(|edit| !mixed_parameter && count != 0 && edit.typing.is_none()
                && edit.engine.as_ptr() == Arc::as_ptr(&self.shared)
                && edit.document == document.metadata().id()
                && edit.revision == document.metadata().revision())
            .map(|edit| {
                let mut targets = [[None; 5]; 100];
                for (target, command) in targets.iter_mut().zip(&commands[..count]) {
                    *target = ["op", "layer", "effect", "step", "filter"].map(|key| command[key].as_str());
                }
                native_dispatch::fingerprint(
                    &(&edit.gesture, &edit.label, &targets[..count]), 1, target, mask, selection_view,
                )
            })
            .transpose()?
            .map(native_dispatch::LiveGesture::Parameter);
        let live_transform = self.free_transform.as_ref()
            .filter(|editor| !mixed_tool && editor.observes(document.into()) && if editor.edits_text_draft() {
                count == 0 && source.is_some()
            } else {
                count != 0 && source.is_none()
            })
            .and_then(|editor| editor.pointer_preview_key())
            .map(|key| native_dispatch::fingerprint(&key, 1, target, mask, selection_view))
            .transpose()?;
        let live_gizmo = self.gizmo_source.as_ref().zip(self.live_gesture.as_ref())
            .filter(|(gizmo, _)| !mixed_tool && self.gizmo_handle != 0
                && self.free_transform.is_none() && source.is_none() && count != 0
                && gizmo.observes(document.into())
                && commands[..count].iter().all(|command| matches!(command["op"].as_str(), Some("move" | "transform"))))
            .map(|(gizmo, gesture)| native_dispatch::fingerprint(
                &(gesture, &gizmo.roots, gizmo.tool.label(), gizmo.mask, &gizmo.step),
                1, target, mask, selection_view,
            ))
            .transpose()?;
        let live_transform = live_transform.or(live_gizmo).map(native_dispatch::LiveGesture::Transform);
        let live_crop = self.crop_preview_key()
            .filter(|_| !has_stroke && !has_hover && source.is_none() && count == 1
                && commands[0]["op"] == "crop")
            .map(|key| native_dispatch::fingerprint(&key, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::Crop);
        let live_primitive = self.primitive_drag.as_ref()
            .filter(|drag| !mixed_tool && self.free_transform.is_none() && source.is_none()
                && count == 1 && matches!(commands[0]["op"].as_str(), Some("shape" | "gradient"))
                && drag.observes(self, document.into()))
            .and_then(primitive_drag::Drag::live_key)
            .map(|key| native_dispatch::fingerprint(&key, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::Primitive);
        let live_selection = self.selection_drag_preview_key()
            .filter(|_| !mixed_tool && source.is_none() && count == 1)
            .map(|key| native_dispatch::fingerprint(&key, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::Selection);
        let live_refinement = self.refinement.as_ref()
            .filter(|_| !mixed_refinement && self.parameter_gesture.is_none() && self.free_transform.is_none()
                && self.source_editor.is_none() && self.filter_editor.is_none() && source.is_none()
                && count == 1 && count == self.transient.len()
                && matches!(commands[0]["op"].as_str(), Some("selection.refine" | "mask.refine")))
            .and_then(|editor| editor.pointer_preview_key(ctx, document.into()))
            .map(|key| native_dispatch::fingerprint(&key, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::Refinement);
        let live_filter = self.filter_editor.as_ref()
            .filter(|_| !mixed_transient && self.free_transform.is_none() && source.is_none()
                && count == 1 && matches!(commands[0]["op"].as_str(), Some("filter.add" | "filter.update")))
            .and_then(|editor| editor.pointer_preview_key(ctx, document.into()))
            .map(|key| native_dispatch::fingerprint(&key, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::Filter);
        let live_text = self.source_editor.as_ref()
            .filter(|_| !mixed_tool && self.free_transform.is_none() && source.is_some() && count == 0)
            .and_then(|editor| editor.canvas_preview_key(ctx, document.into()))
            .map(|key| native_dispatch::fingerprint(&key, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::CanvasText);
        let live_pen = if !mixed_tool && self.free_transform.is_none() && source.is_none() && count == 1 {
            self.pen_preview_key(document.into(), commands[0])
                .map(|key| native_dispatch::fingerprint(&key, 1, target, mask, selection_view))
                .transpose()?
                .map(native_dispatch::LiveGesture::Pen)
        } else {
            None
        };
        let current = |id: &str, revision, native| {
            id == document.metadata().id()
                && revision == document.metadata().revision()
                && native == Some(document.key())
        };
        let live_reorder = self.layer_drag.as_ref()
            .filter(|drag| {
                hierarchy_only && self.eye_sweep.is_none()
                    && current(&drag.document, drag.revision, drag.native)
                    && self.transient == drag.commands
            })
            .map(|drag| native_dispatch::fingerprint(&drag.gesture, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::LayerReorder);
        let live_visibility = self.eye_sweep.as_ref()
            .filter(|sweep| {
                hierarchy_only && self.layer_drag.is_none()
                    && current(&sweep.document, sweep.revision, sweep.native)
                    && count == sweep.ids.len()
                    && self.transient.iter().all(|command| {
                        command["op"] == "layer.update"
                            && command["visible"].as_bool() == Some(sweep.visible)
                            && command["layer"].as_str().is_some_and(|id| sweep.ids.contains(id))
                    })
            })
            .map(|sweep| native_dispatch::fingerprint(&sweep.gesture, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::LayerVisibility);
        let live_hover = self.blend_hover.as_ref()
            .filter(|_| count == 0 && source.is_none() && !has_stroke && self.refinement.is_none())
            .map(|hover| native_dispatch::fingerprint(&hover.layers, 1, target, mask, selection_view))
            .transpose()?
            .map(native_dispatch::LiveGesture::BlendHover);
        let gesture = live_stroke.or(live_parameter).or(live_transform).or(live_crop)
            .or(live_primitive).or(live_selection).or(live_refinement).or(live_filter)
            .or(live_text).or(live_pen).or(live_reorder).or(live_visibility).or(live_hover);
        if reduced.is_none() && live_stroke.is_some_and(|stroke| {
            self.native_dispatch.pending_gesture(document, project, stroke, full_edge, region, epoch)
        }) {
            self.preview_quality.edge(&[], time, None, full_edge);
            return Ok(None);
        }

        let _working = (has_stroke || has_hover)
            .then(|| MetadataCharge::reserve(document.budget(), self.native_command_working_bytes()?, &|| Ok(())))
            .transpose()?;
        let hover = self.blend_hover.as_ref()
            .map(|hover| hover.commands().collect::<Vec<_>>());
        let stroke = self.preview_stroke();
        for command in hover.as_deref().unwrap_or_default() {
            append(&mut commands, &mut count, command)?;
        }
        if let Some(command) = &stroke {
            append(&mut commands, &mut count, command)?;
        }
        let finish_signature = |signature| {
            native_dispatch::blend_signature(
                native_dispatch::presentation_signature(
                    native_dispatch::region_signature(signature, region), presentation.as_ref()),
                blend.as_ref().map(|(blend, _, _)| blend)
                    .or_else(|| streamed_blend.as_ref().map(|(blend, _, _)| blend)),
            )
        };
        let capture = native_dispatch::CommandCapture::new(
            &commands[..count],
            source,
            target,
            mask,
            selection_view,
        )?;
        let signature = finish_signature(capture.signature(full_edge)?);
        let edge = self.preview_quality.edge(&signature, time, reduced, full_edge);
        self.preview_quality.repaint_when_settled(ctx, time, edge != full_edge);
        let signature = if edge == full_edge {
            signature
        } else {
            finish_signature(capture.signature(edge)?)
        };
        if presentation.is_some() || blend.is_some() || streamed_blend.is_some() {
            self.animation.native_captured(document, signature);
        }
        if self.native_dispatch.matches_capture(document, project, signature, gesture, epoch) {
            return Ok(None);
        }
        if let Some(gesture) = gesture.filter(|&gesture| {
            self.native_dispatch.pending_gesture(document, project, gesture, edge, region, epoch)
        }) {
            self.native_dispatch.gesture_deferred = !matches!(gesture, native_dispatch::LiveGesture::Stroke(_));
            return Ok(None);
        }
        capture
            .admit(document, project, edge)
            .and_then(|input| input.with_gesture(gesture))
            .and_then(|input| input.with_region(document, region))
            .and_then(|input| input.with_presentation(document, presentation))
            .and_then(|input| input.with_blend(document, blend))
            .and_then(|input| input.with_streamed_blend(document, streamed_blend))
            .map(Some)
    }

    pub(super) fn request_native_canvas(
        &mut self,
        ctx: &egui::Context,
        project: &str,
        document: &NativeDocument,
    ) {
        if self.native_canvas_offscreen() {
            let had_transform_preview = self.free_transform.as_mut()
                .is_some_and(|editor| editor.rendered.take().is_some());
            if self.native_dispatch.pending() || had_transform_preview {
                self.preview_epoch.fetch_add(1, Ordering::Relaxed);
                self.native_dispatch.clear();
            }
            self.animation.stop_native_canvas();
            self.pending = false;
            return;
        }
        let captured = self.capture_native_canvas(ctx, project, document);
        let result = match captured {
            Ok(Some(input)) => self
                .queue_native_canvas(input, ctx)
                .and_then(|_| self.present_native_canvas(project, document, ctx)),
            Ok(None) => {
                let was_pending = self.native_dispatch.pending();
                let shown = self.present_native_canvas(project, document, ctx);
                if was_pending && !self.native_dispatch.pending() {
                    ctx.request_repaint();
                }
                shown
            }
            Err(error) if retryable_source_read(&error) => {
                ctx.request_repaint_after(INVENTORY_RETRY_DELAY);
                Ok(false)
            }
            Err(error) => {
                self.preview_epoch.fetch_add(1, Ordering::Relaxed);
                self.native_dispatch.clear();
                self.animation.stop_native_canvas();
                self.pending = false;
                Err(error)
            }
        };
        if self.native_dispatch.gesture_deferred {
            // A compatible render can show drag progress. Apply needs the
            // newest transform to have reached the canvas.
            if let Some(editor) = &mut self.free_transform {
                editor.rendered = None;
            }
            return;
        }
        if matches!(result, Ok(false)) {
            if self.native_dispatch.pending() {
                if let Some(editor) = &mut self.free_transform {
                    editor.rendered = None;
                }
            }
            return;
        }
        let error = result.err();
        if self.proposal_review.is_some() && error.is_some() {
            self.proposal_error = error.clone();
            self.proposal_rendered = None;
        }
        if let Some(editor) = &mut self.free_transform {
            editor.error = error.clone();
            let full_detail = self.native_canvas.current().is_some_and(|canvas| {
                let key = canvas.key();
                let [left, top, right, bottom] = key.rect;
                key.width == (right - left) as u32 && key.height == (bottom - top) as u32
            });
            editor.rendered = if error.is_none() && full_detail {
                editor.key().ok()
            } else {
                None
            };
        }
        if let Some(editor) = &mut self.geometry {
            editor.error = error.clone();
        }
        if let Some(editor) = &mut self.refinement {
            editor.error = error.clone();
        }
        if let Some(editor) = &mut self.source_editor {
            if editor.error != error {
                ctx.request_repaint();
            }
            editor.error = error.clone();
        }
        if let Some(editor) = &mut self.filter_editor {
            editor.error = error.clone();
        }
        if let Some(error) = error {
            self.message = error.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offscreen_check_accounts_for_a_rotated_canvas() {
        let (mut app, _) = crate::ui::tests::fixture();
        app.view_rect = Some(Rect::from_center_size(Pos2::ZERO, Vec2::new(20., 200.)));
        app.canvas_viewport = Some(Rect::from_min_max(Pos2::new(40., -5.), Pos2::new(60., 5.)));
        assert!(app.native_canvas_offscreen());
        app.view.angle = 90.;
        assert!(!app.native_canvas_offscreen());
        app.view.angle = 45.;
        app.view.mirror_v = true;
        app.canvas_viewport = Some(Rect::from_min_max(Pos2::new(55., 55.), Pos2::new(65., 65.)));
        assert!(!app.native_canvas_offscreen());
        app.canvas_viewport = Some(Rect::from_min_max(Pos2::new(55., -65.), Pos2::new(65., -55.)));
        assert!(app.native_canvas_offscreen());
    }

    #[test]
    fn pan_margin_is_clipped_and_does_not_push_a_view_past_the_sampling_budget() {
        assert_eq!(buffered_region([0, 0, 80, 64], [16000; 2], None, 1.), [0, 0, 90, 72]);
        let near_limit = [100, 100, 8292, 8292];
        assert_eq!(buffered_region(near_limit, [16000; 2], None, 0.2), near_limit);
        assert_eq!(buffered_region([400, 400, 464, 464], [16000; 2], Some([0, 0, 4096, 4096]), 1.),
            [392, 392, 472, 472]);
        let visible = [1000, 1000, 8000, 7000];
        assert_eq!(buffered_region(visible, [16000; 2], None, 0.2), [840, 840, 8160, 7160]);
        let limited = [1000, 1000, 9000, 9000];
        assert_eq!(buffered_region(limited, [16000; 2], None, 0.2), [968, 968, 9032, 9032]);
    }

    #[test]
    fn native_pan_keeps_overlapping_tile_origins_when_the_display_budget_allows_it() {
        let size = [16000; 2];
        let side = 8192;
        let previous = buffered_region([100, 100, 1300, 900], size, None, 1.);
        let cell = native_texture::viewport_tile_cell([1264, 864], side).unwrap();
        assert_eq!(cell, [1264, 101]);
        let visible = [100, 210, 1300, 1010];
        let ordinary = buffered_region(visible, size, Some(previous), 1.);
        assert_eq!(ordinary, [68, 178, 1332, 1042]);
        let aligned = aligned_pan_region(visible, ordinary, previous, cell, side);
        assert_eq!(aligned, [68, 169, 1332, 1042]);
        assert!(covers_rect(aligned, visible));
        assert_eq!(buffered_region(visible, size, Some(aligned), 1.), aligned);

        let near_edge = [100, 20, 1300, 820];
        let ordinary = buffered_region(near_edge, size, None, 1.);
        assert_eq!(aligned_pan_region(near_edge, ordinary, previous, cell, side), ordinary);
        let distant = [100, 1600, 1300, 2400];
        let ordinary = buffered_region(distant, size, None, 1.);
        assert_eq!(aligned_pan_region(distant, ordinary, previous, cell, side), ordinary);

        let visible = [61, 0, 125, 20];
        let ordinary = buffered_region(visible, size, None, 1.);
        assert_eq!(
            aligned_pan_region(visible, ordinary, [0, 0, 124, 20], [62, 20], 64),
            ordinary,
        );
        assert_eq!(aligned_pan_region([100, 210, 1300, 1010], [68, 178, 1332, 1042], previous, cell, 1024),
            [68, 178, 1332, 1042]);

        let previous = [0, 100, 12000, 10000];
        let visible = [0, 131, 12000, 10031];
        let cell = native_texture::viewport_tile_cell([12000, 9900], side).unwrap();
        assert_eq!(cell, [8190, 14]);
        let aligned = aligned_pan_region(visible, visible, previous, cell, side);
        assert_eq!(aligned, [0, 128, 12000, 10031]);
        assert_eq!(buffered_region(visible, size, Some(aligned), 1.), aligned);
    }
}
