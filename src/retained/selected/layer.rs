use super::state::Plane;
use crate::{
    engine::native::{LayerMetadata, LayerPatch, NativeDocument, RasterBatch, SourceUpdate},
    retained::transaction::Session,
    transform::native::{
        selected::{Placement, Selection},
        Work,
    },
};
use serde_json::Value;

struct Mask<'a> {
    id: &'a str,
    kind: &'a str,
    pixels: Plane,
}
pub(super) struct Layer<'a> {
    pub metadata: LayerMetadata<'a>,
    pub origin: [i32; 2],
    pub frame: [u32; 2],
    initial: [i32; 2],
    initial_frame: [u32; 2],
    color: Plane,
    masks: Vec<Mask<'a>>,
    fill: bool,
    materialized: bool,
}
impl<'a> Layer<'a> {
    pub(super) fn new(base: &NativeDocument, metadata: LayerMetadata<'a>) -> Result<Self, String> {
        let count = metadata.mask().map_or(0, |m| m.steps().len());
        let mut masks = Vec::new();
        masks
            .try_reserve_exact(count)
            .map_err(|_| "Selected mask state allocation refused")?;
        if let Some(mask) = metadata.mask() {
            for step in mask.steps() {
                masks.push(Mask {
                    id: step.id(),
                    kind: step.kind(),
                    pixels: Plane::new(base, step.pixels())?,
                });
            }
        }
        Ok(Self {
            metadata,
            origin: metadata.origin(),
            frame: [metadata.pixels().width(), metadata.pixels().height()],
            initial: metadata.origin(),
            initial_frame: [metadata.pixels().width(), metadata.pixels().height()],
            color: Plane::new(base, metadata.pixels())?,
            masks,
            fill: metadata.kind() == "fill",
            materialized: false,
        })
    }
    fn plain(&self) -> Result<(), String> {
        if self
            .masks
            .iter()
            .any(|m| !matches!(m.kind, "fill" | "paint"))
        {
            return Err("Independent or selected linked transforms require a fill/paint mask; mask effects are preserved unchanged. Unlink and choose a paint step for a mask-only transform.".into());
        }
        Ok(())
    }
    pub(super) fn apply(
        &mut self,
        base: &NativeDocument,
        selection: &Selection<'_>,
        placement: &Placement,
        command: &Value,
        select_mask: bool,
        session: &impl Session,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let linked = self.metadata.mask().is_some_and(|m| m.linked());
        let mask = select_mask && !linked;
        if !linked && mask && command["step"].as_str().is_some() && placement.noop {
            // selection_impl returns its geometry before looking up a named
            // mask step. The all-paint and linked routes still validate/reframe.
            return Ok(());
        }
        let paints = self.masks.iter().filter(|m| m.kind == "paint").count();
        if linked || (mask && command["step"].is_null()) {
            self.plain()?;
            if paints == 0 {
                return Err("Select a paint mask step to transform".into());
            }
            let count = paints
                .checked_mul(1 + self.masks.len())
                .ok_or("Selected mask work overflow")?;
            crate::transform::work_budget(self.frame[0], self.frame[1], count)?;
        }
        if linked {
            let (mut masked_origin, mut masked_frame) = (self.origin, self.frame);
            self.edit_color(base, selection, placement, false, session, work, check)?;
            for index in 0..self.masks.len() {
                if self.masks[index].kind == "paint" {
                    self.edit_mask(
                        base,
                        selection,
                        placement,
                        index,
                        &mut masked_origin,
                        &mut masked_frame,
                        false,
                        session,
                        work,
                        check,
                    )?;
                    crate::transform::work_budget(
                        masked_frame[0],
                        masked_frame[1],
                        paints * (1 + self.masks.len()),
                    )?;
                }
            }
            let mut reframe = masked_origin != self.origin || masked_frame != self.frame;
            for mask in &self.masks {
                reframe |= mask.pixels.frame(base)? != self.frame;
            }
            if reframe {
                let offset = [
                    masked_origin[0] - self.origin[0],
                    masked_origin[1] - self.origin[1],
                ];
                for mask in &mut self.masks {
                    mask.pixels
                        .reframe(base, self.frame, offset, session, work, check)?;
                }
            }
        } else if mask {
            if self.metadata.mask().is_none() {
                return Err("Layer has no mask".into());
            }
            let named = command["step"].as_str();
            let count = self
                .masks
                .iter()
                .filter(|m| m.kind == "paint" && named.is_none_or(|id| id == m.id))
                .count();
            if count == 0 {
                return Err(if named.is_some() {
                    "No paint mask step"
                } else {
                    "Select a paint mask step to transform"
                }
                .into());
            }
            let (mut origin, mut frame) = (self.origin, self.frame);
            for index in 0..self.masks.len() {
                if self.masks[index].kind == "paint"
                    && named.is_none_or(|id| id == self.masks[index].id)
                {
                    self.edit_mask(
                        base,
                        selection,
                        placement,
                        index,
                        &mut origin,
                        &mut frame,
                        true,
                        session,
                        work,
                        check,
                    )?;
                    if named.is_none() {
                        crate::transform::work_budget(
                            frame[0],
                            frame[1],
                            count * (1 + self.masks.len()),
                        )?;
                    }
                }
            }
            self.origin = origin;
            self.frame = frame;
        } else {
            self.edit_color(base, selection, placement, true, session, work, check)?;
        }
        Ok(())
    }
    fn edit_color(
        &mut self,
        base: &NativeDocument,
        selection: &Selection<'_>,
        placement: &Placement,
        shift_masks: bool,
        session: &impl Session,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let (next, frame) = after(self.origin, self.frame, placement)?;
        let count = if shift_masks { 1 + self.masks.len() } else { 1 };
        if !placement.noop {
            crate::transform::work_budget(frame[0], frame[1], count)?;
        }
        self.color.edit(
            base,
            selection,
            placement,
            self.origin,
            self.frame,
            self.fill.then_some(self.metadata.color()),
            session,
            work,
            check,
        )?;
        if !placement.noop && self.fill {
            self.fill = false;
            self.materialized = true;
        }
        if shift_masks && (next != self.origin || frame != self.frame) {
            let offset = [self.origin[0] - next[0], self.origin[1] - next[1]];
            for mask in &mut self.masks {
                mask.pixels
                    .shift(base, frame, offset, None, session, work, check)?;
            }
        }
        self.origin = next;
        self.frame = frame;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn edit_mask(
        &mut self,
        base: &NativeDocument,
        selection: &Selection<'_>,
        placement: &Placement,
        index: usize,
        origin: &mut [i32; 2],
        frame: &mut [u32; 2],
        shift_color: bool,
        session: &impl Session,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let (next, next_frame) = after(*origin, *frame, placement)?;
        if !placement.noop {
            crate::transform::work_budget(next_frame[0], next_frame[1], 1 + self.masks.len())?;
        }
        self.masks[index].pixels.edit(
            base, selection, placement, *origin, *frame, None, session, work, check,
        )?;
        if next != *origin || next_frame != *frame {
            let offset = [origin[0] - next[0], origin[1] - next[1]];
            for other in 0..self.masks.len() {
                if other != index {
                    self.masks[other]
                        .pixels
                        .shift(base, next_frame, offset, None, session, work, check)?;
                }
            }
            if shift_color {
                self.color.shift(
                    base,
                    next_frame,
                    offset,
                    self.fill.then_some(self.metadata.color()),
                    session,
                    work,
                    check,
                )?;
                if self.fill {
                    self.fill = false;
                    self.materialized = true;
                }
            }
        }
        *origin = next;
        *frame = next_frame;
        Ok(())
    }
    pub(super) fn expanded(&self) -> bool {
        self.origin != self.initial || self.frame != self.initial_frame
    }
    pub(super) fn reconcile(
        &mut self,
        base: &NativeDocument,
        session: &impl Session,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.color
            .reconcile(base, self.metadata.pixels(), session, work, check)?;
        if let Some(mask) = self.metadata.mask() {
            for (state, old) in self.masks.iter_mut().zip(mask.steps()) {
                state
                    .pixels
                    .reconcile(base, old.pixels(), session, work, check)?;
            }
        }
        Ok(())
    }
    pub(super) fn check_reframed_writes(
        &self,
        base: &NativeDocument,
        command: crate::engine::native::Command<'_>,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        for mask in &self.masks {
            mask.pixels.check_reframed_write(
                base,
                command,
                self.initial,
                self.origin,
                work,
                check,
            )?;
        }
        Ok(())
    }
    pub(super) fn offset(&self) -> [i32; 2] {
        [
            self.initial[0] - self.origin[0],
            self.initial[1] - self.origin[1],
        ]
    }
    pub(super) fn pack(self, batch: &mut RasterBatch) {
        if let Some(patch) = self.color.into_patch() {
            batch.rasters.push(patch);
        }
        for mask in self.masks {
            if let Some(patch) = mask.pixels.into_patch() {
                batch.rasters.push(patch);
            }
        }
        batch.layers.push(LayerPatch {
            target: self.metadata.pixels().slot(),
            origin: Some(self.origin),
            source: SourceUpdate::Keep,
            materialize_fill: self.materialized,
        });
    }
    pub(super) fn abort(self, session: &impl Session) -> Result<(), String> {
        let mut result = self.color.abort(session);
        for mask in self.masks {
            if let Err(reason) = mask.pixels.abort(session) {
                result = Err(reason);
            }
        }
        result
    }
}
pub(super) fn after(
    origin: [i32; 2],
    frame: [u32; 2],
    placement: &Placement,
) -> Result<([i32; 2], [u32; 2]), String> {
    if placement.noop {
        return Ok((origin, frame));
    }
    let [l, t, r, b] = placement.bounds;
    let next = [origin[0].min(l), origin[1].min(t)];
    let frame = [
        (origin[0] + frame[0] as i32).max(r) - next[0],
        (origin[1] + frame[1] as i32).max(b) - next[1],
    ];
    if frame.iter().any(|v| *v <= 0) {
        return Err("Invalid selected transform frame".into());
    }
    let frame = frame.map(|v| v as u32);
    crate::raster::check_size(frame[0], frame[1])?;
    Ok((next, frame))
}
