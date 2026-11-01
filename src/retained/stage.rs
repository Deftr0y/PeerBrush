//! Native transform preparation. Final Engine adoption remains mandatory.
use super::projection::Original;
use crate::{
    engine::{
        native::{
            CapturedEdit, OutputDraft, PreparedDelta, RasterPatch, RetainedUpdate, StagedOutput,
        },
        scene::{Grid36, Warp36},
    },
    large_document::{Error, FreshFragment},
    transform::native::{self, Mapping, Work},
    workspace::source_runtime::SourceSession,
};
use std::cell::RefCell;

impl Original<'_> {
    /// One generation-bound plane only. Whole commands must prepare linked
    /// masks, source definitions and selection metadata in the same transaction.
    pub(crate) fn prepare_plane(
        &self,
        captured: CapturedEdit,
        session: &SourceSession,
        matrix: [f64; 6],
        projective: Option<[f64; 9]>,
        warps: &[crate::warp::Stage],
        frame: [u32; 2],
        layer_origin: Option<[i32; 2]>,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<PreparedDelta, String> {
        if captured.base_key() != self.document().key() {
            return Err("Native transform input belongs to another captured document".into());
        }
        let cancellation = captured.cancellation();
        let checked = || {
            cancellation.check()?;
            session.check()?;
            check()?;
            cancellation.check()
        };
        checked()?;
        let pool = self.document().budget();
        if !session.resident().same_pool(pool)
            || !session.disk().same_pool(self.document().disk_budget())
        {
            return Err("Native transform session uses another document admission".into());
        }
        let source = self.source()?;
        let mapping = Mapping::retained(
            source.width(),
            source.height(),
            source.depth(),
            matrix,
            projective,
            warps,
            pool,
        )?;
        // Hold caller-owned fixed metadata and token-vector admission until the
        // candidate's own metadata/output charges have taken over.
        let metadata = pool
            .reserve_bytes(32 * 1024, 32 * 1024)
            .map_err(|e| e.to_string())?;
        let mut placement = Vec::new();
        placement
            .try_reserve_exact(warps.len())
            .map_err(|_| "Native retained metadata allocation refused")?;
        if placement
            .capacity()
            .checked_mul(std::mem::size_of::<Warp36>())
            .is_none_or(|n| n > 16 * 1024)
        {
            return Err("Native retained metadata exceeds admission".into());
        }
        for stage in warps {
            placement.push(Warp36 {
                before: stage.before,
                grid: Grid36 {
                    rect: stage.grid.rect,
                    points: stage.grid.points,
                },
            });
        }
        let output = self.stage_output(session, &mapping, frame, work, &checked)?;
        let patch = RasterPatch {
            target: self.current_slot(),
            output,
            retained: RetainedUpdate::Replace {
                source: self.original_slot(),
                // Original::render stores the coefficients after the same
                // near-integer snap that produced these output pixels.
                matrix: mapping.matrix(),
                projective,
                warps: placement,
            },
            layer_origin,
        };
        let result = match captured.raster_patch(patch, &checked) {
            Ok(prepared) => Ok(prepared),
            Err(refused) => {
                let reason = refused.reason().to_owned();
                match refused.abort(session, &|| Ok(())) {
                    Ok(()) => Err(reason),
                    Err(cleanup) => {
                        Err(format!("{reason}; provisional output retained: {cleanup}"))
                    }
                }
            }
        };
        drop(metadata);
        result
    }

    fn stage_output(
        &self,
        session: &SourceSession,
        mapping: &Mapping,
        frame: [u32; 2],
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<StagedOutput, String> {
        check()?;
        let source = self.source()?;
        let mut fragments = Vec::new();
        fragments
            .try_reserve_exact(1)
            .map_err(|_| "Native transform source token allocation refused")?;
        if fragments
            .capacity()
            .checked_mul(std::mem::size_of::<FreshFragment>())
            .is_none_or(|n| n > 16 * 1024)
        {
            return Err("Native transform token metadata exceeds admission".into());
        }
        let fragment = session.fresh_fragment()?;
        let failure = RefCell::new(None);
        let source_check = || match check() {
            Ok(()) => Ok(()),
            Err(reason) => {
                *failure.borrow_mut() = Some(reason);
                Err(Error::Cancelled)
            }
        };
        let result = native::write_plane(
            source,
            mapping,
            frame,
            fragment.store(),
            work,
            &source_check,
        );
        let output = match result {
            Ok(output) => output,
            Err(reason) => {
                return Err(retire(
                    session,
                    fragment,
                    failure.into_inner().unwrap_or(reason),
                ))
            }
        };
        if output.is_known_zero() {
            if let Err(cleanup) = session.abort_fragment(fragment, &mut || Ok(())) {
                return Err(format!("Empty native transform source retained: {cleanup}"));
            }
        } else {
            fragments.push(fragment);
        }
        match OutputDraft::new(output, fragments).seal(self.document(), session, check) {
            Ok(output) => Ok(output),
            Err(refused) => {
                let reason = refused.reason().to_owned();
                match refused.into_draft().abort(session, &|| Ok(())) {
                    Ok(()) => Err(reason),
                    Err(cleanup) => {
                        Err(format!("{reason}; provisional output retained: {cleanup}"))
                    }
                }
            }
        }
    }
}
fn retire(session: &SourceSession, fragment: FreshFragment, reason: String) -> String {
    match session.abort_fragment(fragment, &mut || Ok(())) {
        Ok(()) => reason,
        Err(cleanup) => format!("{reason}; provisional output retained: {cleanup}"),
    }
}
