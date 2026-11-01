//! Private working planes. Every generated owner reaches the one final batch,
//! or registered retirement; no intermediate document is adopted.
use crate::{
    engine::native::{
        NativeDocument, RasterMetadata, RasterPatch, RasterSlot, RetainedUpdate, StagedOutput,
    },
    engine::scene::Warp36,
    raster::{Pixel, Surface},
    retained::{
        pose::Pose,
        transaction::{self, Session},
    },
    transform::native::{
        self,
        selected::{self, Placement, Selection},
        Mapping, Work,
    },
};

enum Pixels {
    Existing(RasterSlot),
    Generated(StagedOutput),
}
impl Pixels {
    fn surface<'a>(&'a self, base: &'a NativeDocument) -> Result<&'a Surface, String> {
        match self {
            Self::Existing(slot) => base.source(*slot),
            Self::Generated(output) => Ok(output.surface()),
        }
    }
    fn abort(self, session: &impl Session) -> Result<(), String> {
        match self {
            Self::Existing(_) => Ok(()),
            Self::Generated(output) => output.abort(session, &|| Ok(())),
        }
    }
}
struct Original {
    pixels: Pixels,
    pose: Pose,
    warps: Vec<Warp36>,
}
pub(super) struct Plane {
    target: RasterSlot,
    current: Pixels,
    original: Option<Original>,
    reframed: bool,
}
impl Plane {
    /// The command's packet envelope is already held before these bounded
    /// placement copies are allocated, and survives into CapturedEdit::batch.
    pub(super) fn new(base: &NativeDocument, metadata: RasterMetadata<'_>) -> Result<Self, String> {
        base.source(metadata.slot())?;
        let original = if let Some(original) = metadata.retained() {
            let source = base.source(original.pixels().slot())?;
            let pose = Pose::new(
                [source.width(), source.height()],
                source.depth(),
                Some(original),
                base.budget(),
            )?;
            let mut warps = Vec::new();
            warps
                .try_reserve_exact(original.warps().len())
                .map_err(|_| "Selected original metadata allocation refused")?;
            if warps
                .capacity()
                .checked_mul(std::mem::size_of::<Warp36>())
                .is_none_or(|n| n > 16 * 1024)
            {
                return Err("Selected original metadata exceeds admission".into());
            }
            warps.extend_from_slice(original.warps());
            Some(Original {
                pixels: Pixels::Existing(original.pixels().slot()),
                pose,
                warps,
            })
        } else {
            None
        };
        Ok(Self {
            target: metadata.slot(),
            current: Pixels::Existing(metadata.slot()),
            original,
            reframed: false,
        })
    }
    pub(super) fn frame(&self, base: &NativeDocument) -> Result<[u32; 2], String> {
        let p = self.current.surface(base)?;
        Ok([p.width(), p.height()])
    }
    pub(super) fn edit(
        &mut self,
        base: &NativeDocument,
        selection: &Selection<'_>,
        placement: &Placement,
        origin: [i32; 2],
        layer_frame: [u32; 2],
        fill: Option<Pixel>,
        session: &impl Session,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if placement.noop {
            return Ok(());
        }
        let mut cleared = false;
        let output = transaction::stage(base, session, check, |store, checked| {
            let written = selected::write(
                self.current.surface(base)?,
                selection,
                placement,
                origin,
                layer_frame,
                fill,
                store,
                work,
                checked,
            )?;
            cleared = written.cleared_retained;
            Ok(written.surface)
        })?;
        let old = std::mem::replace(&mut self.current, Pixels::Generated(output));
        old.abort(session)?;
        if cleared {
            if let Some(original) = self.original.take() {
                original.pixels.abort(session)?;
            }
        }
        Ok(())
    }
    pub(super) fn shift(
        &mut self,
        base: &NativeDocument,
        frame: [u32; 2],
        offset: [i32; 2],
        fill: Option<Pixel>,
        session: &impl Session,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.frame(base)? == frame && offset == [0, 0] && fill.is_none() {
            return Ok(());
        }
        let mut cleared = false;
        let output = transaction::stage(base, session, check, |store, checked| {
            let written = selected::shift(
                self.current.surface(base)?,
                frame,
                offset,
                fill,
                store,
                work,
                checked,
            )?;
            cleared = written.cleared_retained;
            Ok(written.surface)
        })?;
        let old = std::mem::replace(&mut self.current, Pixels::Generated(output));
        old.abort(session)?;
        if cleared {
            if let Some(original) = self.original.take() {
                original.pixels.abort(session)?;
            }
        } else if let Some(original) = &mut self.original {
            original.pose.matrix = crate::retained::compose(
                [1., 0., 0., 1., f64::from(offset[0]), f64::from(offset[1])],
                original.pose.matrix,
            );
            original.pose.validate()?;
        }
        Ok(())
    }
    /// Final linked-mask reframing is Original::render, not copy_shift. A newly
    /// edited mask can therefore become a new original with its own output token.
    pub(super) fn reframe(
        &mut self,
        base: &NativeDocument,
        frame: [u32; 2],
        offset: [i32; 2],
        session: &impl Session,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let had_original = self.original.is_some();
        let mut new_pose = if had_original {
            None
        } else {
            let p = self.current.surface(base)?;
            Some(Pose::new(
                [p.width(), p.height()],
                p.depth(),
                None,
                base.budget(),
            )?)
        };
        let pose = if let Some(original) = &mut self.original {
            &mut original.pose
        } else {
            new_pose.as_mut().ok_or("Missing linked-mask placement")?
        };
        pose.matrix = crate::retained::compose(
            [1., 0., 0., 1., f64::from(offset[0]), f64::from(offset[1])],
            pose.matrix,
        );
        let mapping = Mapping::retained(
            pose.frame[0],
            pose.frame[1],
            pose.depth,
            pose.matrix,
            pose.projective,
            pose.warps(),
            base.budget(),
        )?;
        // Original::render retains the snapped coefficients used for sampling.
        pose.matrix = mapping.matrix();
        let source = if let Some(original) = &self.original {
            &original.pixels
        } else {
            &self.current
        };
        let output = transaction::stage(base, session, check, |store, checked| {
            native::write_plane(source.surface(base)?, &mapping, frame, store, work, checked)
        })?;
        let old = std::mem::replace(&mut self.current, Pixels::Generated(output));
        if had_original {
            old.abort(session)?;
        } else {
            self.original = Some(Original {
                pixels: old,
                pose: new_pose.ok_or("Missing new linked-mask original")?,
                warps: Vec::new(),
            });
        }
        self.reframed = true;
        Ok(())
    }
    /// A retained mask reframe can restore samples beyond the selection. Check
    /// its actual document-space support against the same captured command.
    pub(super) fn check_reframed_write(
        &self,
        base: &NativeDocument,
        command: crate::engine::native::Command<'_>,
        before_origin: [i32; 2],
        after_origin: [i32; 2],
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if !self.reframed {
            return Ok(());
        }
        use crate::raster::{backed::Region, SurfaceRegion};
        let before = base.source(self.target)?;
        let after = self.current.surface(base)?;
        let bounds = [
            before_origin[0].min(after_origin[0]),
            before_origin[1].min(after_origin[1]),
            (before_origin[0] + before.width() as i32).max(after_origin[0] + after.width() as i32),
            (before_origin[1] + before.height() as i32)
                .max(after_origin[1] + after.height() as i32),
        ];
        let source_check = &mut || check().map_err(|_| crate::large_document::Error::Cancelled);
        let prepare = |source: &Surface,
                       origin: [i32; 2],
                       row: [i32; 4],
                       check: &mut dyn FnMut() -> crate::large_document::Result<()>|
         -> Result<Option<SurfaceRegion>, String> {
            let local = [
                (row[0] - origin[0]).max(0),
                (row[1] - origin[1]).max(0),
                (row[2] - origin[0]).min(source.width() as i32),
                (row[3] - origin[1]).min(source.height() as i32),
            ];
            if local[0] >= local[2] || local[1] >= local[3] {
                return Ok(None);
            }
            source
                .prepare_region(Region::new(local).map_err(|e| e.to_string())?, check)
                .map(Some)
                .map_err(|e| e.to_string())
        };
        let pixel = |source: &Surface,
                     prepared: &Option<SurfaceRegion>,
                     origin: [i32; 2],
                     x: i32,
                     y: i32|
         -> Result<[u16; 4], String> {
            let (x, y) = (x - origin[0], y - origin[1]);
            if x < 0 || y < 0 || x >= source.width() as i32 || y >= source.height() as i32 {
                return Ok([0; 4]);
            }
            prepared
                .as_ref()
                .ok_or("Missing reframed write footprint")?
                .pixel16(x, y)
                .map_err(|e| e.to_string())
        };
        let mut dirty: Option<[i32; 4]> = None;
        for top in (bounds[1]..bounds[3]).step_by(256) {
            for left in (bounds[0]..bounds[2]).step_by(256) {
                check()?;
                // Keep each admitted 64-KiPixel pair pinned while comparing it;
                // per-row pins would repeatedly decode/read the same tiles.
                let region = [
                    left,
                    top,
                    (left + 256).min(bounds[2]),
                    (top + 256).min(bounds[3]),
                ];
                let previous = prepare(before, before_origin, region, source_check)?;
                let current = prepare(after, after_origin, region, source_check)?;
                for y in region[1]..region[3] {
                    check()?;
                    for x in region[0]..region[2] {
                        work.claim(1)?;
                        if pixel(before, &previous, before_origin, x, y)?
                            != pixel(after, &current, after_origin, x, y)?
                        {
                            dirty = Some(dirty.map_or([x, y, x + 1, y + 1], |b| {
                                [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)]
                            }));
                        }
                    }
                }
            }
        }
        check()?;
        if let Some(bounds) = dirty {
            command
                .check_write(base, self.target, bounds)
                .map_err(|reason| format!("{reason}; linked-mask changed bounds {bounds:?}"))?;
        }
        Ok(())
    }
    pub(super) fn into_patch(self) -> Option<RasterPatch> {
        let output = match self.current {
            Pixels::Existing(_) => return None,
            Pixels::Generated(output) => output,
        };
        let retained = match self.original {
            None => RetainedUpdate::Clear,
            Some(original) => {
                let pose = original.pose;
                match original.pixels {
                    Pixels::Existing(source) => RetainedUpdate::Replace {
                        source,
                        matrix: pose.matrix,
                        projective: pose.projective,
                        warps: original.warps,
                    },
                    Pixels::Generated(source) => RetainedUpdate::ReplaceOutput {
                        source,
                        matrix: pose.matrix,
                        projective: pose.projective,
                        warps: original.warps,
                    },
                }
            }
        };
        Some(RasterPatch {
            target: self.target,
            output,
            retained,
            layer_origin: None,
        })
    }
    /// Selected Engine edits run retained::reconcile after the complete operator.
    /// Equal original metadata cannot survive a current-pixel change merely
    /// because a linked-mask reframe reattached it at the end of the operation.
    pub(super) fn reconcile(
        &mut self,
        base: &NativeDocument,
        old: RasterMetadata<'_>,
        session: &impl Session,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let (Some(original), Some(before)) = (&self.original, old.retained()) else {
            return Ok(());
        };
        let prior = before.warps();
        if original.pose.matrix != before.matrix()
            || original.pose.projective != before.projective()
            || original.warps.len() != prior.len()
            || !original.warps.iter().zip(prior).all(|(a, b)| {
                a.before == b.before && a.grid.rect == b.grid.rect && a.grid.points == b.grid.points
            })
        {
            return Ok(());
        }
        let previous = base.source(before.pixels().slot())?;
        let source = original.pixels.surface(base)?;
        if [source.width(), source.height()] != [previous.width(), previous.height()]
            || !same_tiles(previous, source, work, check)?
        {
            return Ok(());
        }
        let before = base.source(old.slot())?;
        let after = self.current.surface(base)?;
        let same = after.width() >= before.width()
            && after.height() >= before.height()
            && same_tiles(before, after, work, check)?;
        if !same {
            self.original
                .take()
                .ok_or("Missing selected original retirement")?
                .pixels
                .abort(session)?;
        }
        Ok(())
    }
    pub(super) fn abort(self, session: &impl Session) -> Result<(), String> {
        let current = self.current.abort(session);
        let original = self.original.map_or(Ok(()), |o| o.pixels.abort(session));
        current.and(original)
    }
}

/// Content equivalence is separate from source ownership: every physical tile,
/// including hidden channels and padding, participates; this performs no I/O.
fn same_tiles(
    a: &Surface,
    b: &Surface,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<bool, String> {
    if a.depth() != b.depth() {
        return Ok(false);
    }
    let (mut a, mut b) = (a.physical_tiles(), b.physical_tiles());
    loop {
        check()?;
        work.claim(1)?;
        match (a.next(), b.next()) {
            (None, None) => return Ok(true),
            (Some(a), Some(b))
                if a.coordinate() == b.coordinate()
                    && a.reference().spec() == b.reference().spec()
                    && a.reference().digest() == b.reference().digest() =>
            {
                ()
            }
            _ => return Ok(false),
        }
    }
}
