use super::*;
use crate::engine::scene::Warp36;

pub enum RetainedUpdate {
    Keep,
    Clear,
    Replace {
        source: RasterSlot,
        matrix: [f64; 6],
        projective: Option<[f64; 9]>,
        warps: Vec<Warp36>,
    },
    ReplaceOutput {
        source: StagedOutput,
        matrix: [f64; 6],
        projective: Option<[f64; 9]>,
        warps: Vec<Warp36>,
    },
}
pub struct RasterPatch {
    pub target: RasterSlot,
    pub output: StagedOutput,
    pub retained: RetainedUpdate,
    pub layer_origin: Option<[i32; 2]>,
}
#[must_use]
pub struct RefusedPatch {
    reason: String,
    patch: RasterPatch,
}
impl RefusedPatch {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn into_patch(self) -> RasterPatch {
        self.patch
    }
    pub(crate) fn abort(
        self,
        session: &impl super::super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let mut outputs = Vec::new();
        outputs.try_reserve_exact(2).map_err(|_| {
            "Native retirement allocation refused; provisional bytes remain charged"
        })?;
        outputs.push(self.patch.output);
        if let RetainedUpdate::ReplaceOutput { source, .. } = self.patch.retained {
            outputs.push(source);
        }
        super::output::abort_outputs(outputs, session, cleanup)
    }
}
impl CapturedEdit {
    /// Output construction and all metadata/source validation happen outside the
    /// Engine lock. A failure preserves the producer's owned retirement tokens.
    pub(crate) fn raster_patch(
        self,
        patch: RasterPatch,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<PreparedDelta, RefusedPatch> {
        match self.prepare_raster(&patch, check) {
            Ok(mut prepared) => {
                let count = if matches!(patch.retained, RetainedUpdate::ReplaceOutput { .. }) {
                    2
                } else {
                    1
                };
                if prepared.outputs.try_reserve_exact(count).is_err() {
                    return Err(RefusedPatch {
                        reason: "Native output owner allocation refused".into(),
                        patch,
                    });
                }
                prepared.outputs.push(patch.output);
                if let RetainedUpdate::ReplaceOutput { source, .. } = patch.retained {
                    prepared.outputs.push(source);
                }
                Ok(prepared)
            }
            Err(reason) => Err(RefusedPatch { reason, patch }),
        }
    }
    fn prepare_raster(
        &self,
        patch: &RasterPatch,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<PreparedDelta> {
        let checked = || {
            self.cancellation.check()?;
            check()?;
            self.cancellation.check()
        };
        self.prepare_batch(std::slice::from_ref(patch), &[], None, &[], &checked)
    }
}

fn contains(reference: &RasterRef36, id: u32) -> bool {
    reference.id == id
        || reference
            .retained
            .as_ref()
            .is_some_and(|o| o.pixels.id == id)
}
pub(super) fn layer_contains(layer: &Layer36, id: u32) -> bool {
    contains(&layer.pixels, id)
        || layer
            .mask
            .as_ref()
            .is_some_and(|m| m.steps.iter().any(|s| contains(&s.pixels, id)))
        || layer
            .effects
            .iter()
            .any(|e| e.clips.iter().any(|c| contains(&c.coverage.mask, id)))
}
pub(super) fn unlocked(metadata: &Document36, target: &str) -> Result<()> {
    let mut current = Some(target);
    for _ in 0..=16 {
        let Some(id) = current else { return Ok(()) };
        let layer = metadata
            .layers
            .iter()
            .find(|layer| layer.id == id)
            .ok_or("Invalid native layer parent")?;
        if layer.locked {
            return Err("Unlock the layer or its folder before changing pixels".into());
        }
        current = layer.parent.as_deref();
    }
    Err("Native group nesting limit exceeded".into())
}

/// Legacy retained reconciliation permits an unshifted, nonshrinking frame
/// expansion with identical physical data. Store identity is deliberately not
/// pixel equality; seals bind these immutable descriptors to owned bytes.
pub(super) fn same_current(before: &Surface, after: &Surface) -> bool {
    if before.depth() != after.depth()
        || after.width() < before.width()
        || after.height() < before.height()
    {
        return false;
    }
    let mut before = before.physical_tiles();
    let mut after = after.physical_tiles();
    loop {
        match (before.next(), after.next()) {
            (None, None) => return true,
            (Some(before), Some(after))
                if before.coordinate() == after.coordinate()
                    && before.reference().spec() == after.reference().spec()
                    && before.reference().digest() == after.reference().digest() => {}
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::output::tests::{draft, Session};
    use super::*;
    use crate::raster::backed::Region;
    fn patch(base: &NativeDocument) -> (RasterPatch, std::path::PathBuf) {
        let target = base.metadata().layers().next().unwrap().pixels().slot();
        let (draft, path) = draft(base);
        let output = draft
            .seal(base, &Session, &|| Ok(()))
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        (
            RasterPatch {
                target,
                output,
                retained: RetainedUpdate::Keep,
                layer_origin: Some([-2, -3]),
            },
            path,
        )
    }
    #[test]
    fn pixels_and_placement_publish_together_after_final_guards() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let (patch, path) = patch(&base);
        let prepared = base
            .capture_edit()
            .unwrap()
            .raster_patch(patch, &|| Ok(()))
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        assert_eq!(base.metadata().layers().next().unwrap().origin(), [0, 0]);
        let current = base.publish("human", &[], 1, false, prepared).unwrap();
        let layer = current.metadata().layers().next().unwrap();
        assert_eq!(layer.origin(), [-2, -3]);
        let view = current
            .source(layer.pixels().slot())
            .unwrap()
            .prepare_region(Region::new([0, 0, 1, 1]).unwrap(), &mut || Ok(()))
            .unwrap();
        assert_eq!(view.pixel16(0, 0).unwrap(), [12345, 23456, 34567, 65535]);
        drop(view);
        assert_eq!(current.metadata().revision(), 1);
        drop(current);
        drop(base);
        std::fs::remove_dir_all(path).unwrap();
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn lease_refusal_returns_owned_pixel_output_for_registered_cleanup() {
        use crate::engine::{Lease, Scope};
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let (patch, path) = patch(&base);
        let layer = base.metadata().layers().next().unwrap().id().to_string();
        let prepared = base
            .capture_edit()
            .unwrap()
            .raster_patch(patch, &|| Ok(()))
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        let lease = Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "source edit".into(),
            scopes: vec![Scope::layer(&layer)],
            expires: 100,
        };
        let refused = base
            .publish("worker", &[lease], 1, false, prepared)
            .unwrap_err();
        assert_eq!(base.metadata().revision(), 0);
        refused.abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        std::fs::remove_dir(path).unwrap();
    }
    #[test]
    fn foreign_generation_refusal_preserves_retirement_token() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let other = super::super::tests::document(&pool);
        let (mut patch, path) = patch(&base);
        patch.target = other.metadata().layers().next().unwrap().pixels().slot();
        let refused = base
            .capture_edit()
            .unwrap()
            .raster_patch(patch, &|| Ok(()))
            .err()
            .unwrap();
        assert!(refused.reason().contains("another immutable document"));
        refused.abort(&Session, &|| Ok(())).unwrap();
        assert_eq!(base.disk_budget().used().unwrap(), 0);
        std::fs::remove_dir(path).unwrap();
    }
    #[test]
    fn retained_replacement_uses_validated_original_slot_and_canonical_roster() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = super::super::tests::document(&pool);
        let target = base.metadata().layers().next().unwrap().pixels().slot();
        let output = OutputDraft::new(Surface::zero(8, 8, 16).unwrap(), vec![])
            .seal(&base, &Session, &|| Ok(()))
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        let patch = RasterPatch {
            target,
            output,
            retained: RetainedUpdate::Replace {
                source: target,
                matrix: crate::retained::IDENTITY,
                projective: None,
                warps: vec![],
            },
            layer_origin: None,
        };
        let prepared = base
            .capture_edit()
            .unwrap()
            .raster_patch(patch, &|| Ok(()))
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        let current = base.publish("human", &[], 1, false, prepared).unwrap();
        let metadata = current.metadata();
        let raster = metadata.layers().next().unwrap().pixels();
        assert_eq!(raster.ordinal(), 1);
        assert_eq!(raster.retained().unwrap().pixels().ordinal(), 2);
        assert!(current
            .source(raster.retained().unwrap().pixels().slot())
            .unwrap()
            .is_known_zero());
    }
}
