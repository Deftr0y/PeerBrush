use super::*;
use crate::engine::scene::{Retained36, Warp36};

pub enum RetainedUpdate {
    Keep,
    Clear,
    Replace {
        source: RasterSlot,
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
        self.patch.output.into_draft().abort(session, cleanup)
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
                if let Err(_) = prepared.outputs.try_reserve_exact(1) {
                    return Err(RefusedPatch {
                        reason: "Native output owner allocation refused".into(),
                        patch,
                    });
                }
                prepared.outputs.push(patch.output);
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
        let check = &checked;
        check()?;
        let old = self.base.source(patch.target)?;
        let output = patch.output.surface();
        if output.depth() != old.depth()
            || !output.uses_budget(self.base.budget())
            || !output.uses_disk_budget(self.base.disk_budget())
        {
            return Err("Native output depth or admission differs".into());
        }
        let mut old_reference = None;
        self.base.core.metadata.visit_rasters(|reference| {
            if reference.id == patch.target.ordinal {
                old_reference = Some(reference);
            }
            Ok(())
        })?;
        let old_reference = old_reference.ok_or("Missing native patch target")?;
        if matches!(patch.retained, RetainedUpdate::Keep)
            && old_reference.retained.is_some()
            && old.key() != output.key()
        {
            return Err("Changed retained pixels require explicit source reconciliation".into());
        }
        let layer = self
            .base
            .core
            .metadata
            .layers
            .iter()
            .find(|layer| layer_contains(layer, patch.target.ordinal));
        if let Some(layer) = layer {
            unlocked(&self.base.core.metadata, &layer.id)?;
            if layer.pixels.id == patch.target.ordinal
                && layer.source.is_some()
                && old.key() != output.key()
            {
                return Err(
                    "Native editable source pixels require an explicit source-definition update"
                        .into(),
                );
            }
        }
        let layer_id = layer
            .map(|layer| layer.id.clone())
            .unwrap_or_else(|| "@selection".into());
        if let Some(origin) = patch.layer_origin {
            if origin.iter().any(|v| v.unsigned_abs() > 100_000)
                || !layer.is_some_and(|l| l.pixels.id == patch.target.ordinal)
            {
                return Err("Native layer origin does not match its raster target".into());
            }
        }
        let intent = Intent::layers(
            if patch.layer_origin.is_some() {
                IntentKind::Placement
            } else {
                IntentKind::Pixels
            },
            &[layer_id],
            self.base.budget(),
        )?;
        let working = MetadataCharge::reserve(self.base.budget(), 32 * 1024, check)?;
        let retained = match &patch.retained {
            RetainedUpdate::Keep => old_reference.retained.clone(),
            RetainedUpdate::Clear => None,
            RetainedUpdate::Replace {
                source,
                matrix,
                projective,
                warps,
            } => {
                let original = self.base.source(*source)?;
                if warps.len() > crate::warp::MAX_STAGES {
                    return Err("Retained Warp stage limit exceeded".into());
                }
                let mut stages = Vec::new();
                stages
                    .try_reserve_exact(warps.len())
                    .map_err(|_| "Retained placement allocation refused")?;
                for warp in warps {
                    stages.push(crate::warp::Stage {
                        before: warp.before,
                        grid: crate::warp::Grid {
                            rect: warp.grid.rect,
                            points: warp.grid.points,
                        },
                    });
                }
                crate::retained::geometry::validate(
                    original.width(),
                    original.height(),
                    original.depth(),
                    output.depth(),
                    *matrix,
                    *projective,
                    &stages,
                )?;
                Some(Box::new(Retained36 {
                    pixels: RasterRef36 {
                        id: source.ordinal,
                        width: original.width(),
                        height: original.height(),
                        depth: original.depth(),
                        retained: None,
                    },
                    matrix: *matrix,
                    projective: *projective,
                    warps: warps.clone(),
                }))
            }
        };
        let copied = copy_metadata(&self.base.core.metadata, self.base.budget(), check)?;
        let virtual_id = u32::try_from(self.base.core.roster.roster.len())
            .map_err(|_| "Native source ordinal overflow")?
            .checked_add(1)
            .ok_or("Native source ordinal overflow")?;
        let metadata = copied.map_unique(|mut metadata, _| {
            let mut found = false;
            metadata.remap_rasters(|reference| {
                if reference.id == patch.target.ordinal && !found {
                    reference.id = virtual_id;
                    reference.width = output.width();
                    reference.height = output.height();
                    reference.depth = output.depth();
                    reference.retained = retained.clone();
                    found = true;
                }
                Ok(())
            })?;
            if !found {
                return Err("Native patch target disappeared".into());
            }
            if let Some(origin) = patch.layer_origin {
                let layer = metadata
                    .layers
                    .iter_mut()
                    .find(|l| l.pixels.id == virtual_id)
                    .ok_or("Native layer origin target disappeared")?;
                layer.x = origin[0];
                layer.y = origin[1];
            }
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            Ok(metadata)
        })?;
        // Move the provisional header while preserving every temporary admission.
        let resident = self.base.core.resident.clone();
        let disk = self.base.core.disk.clone();
        let source = &self.base.core.roster.roster;
        let mut result = None;
        metadata.map_unique(|mut header, _| {
            let roster = rebuild_from(
                &mut header,
                source.len() + 1,
                &resident,
                &disk,
                check,
                |reference| {
                    if reference.id == virtual_id {
                        Ok(output.clone())
                    } else {
                        source.get(reference).cloned()
                    }
                },
            )?;
            let fresh = copy_metadata(&header, &resident, check)?;
            // The final header has a fresh complete owner, without an old-ticket chain.
            result = Some(fresh.map_unique(|metadata, _| {
                Ok(Core {
                    metadata,
                    roster,
                    resident,
                    disk,
                    key: key(),
                    render_key: OnceLock::new(),
                })
            })?);
            Ok(())
        })?;
        drop(working);
        check()?;
        Ok(PreparedDelta {
            base: self.base.key(),
            candidate: NativeDocument {
                core: result.ok_or("Native header handoff failed")?,
            },
            cancellation: self.cancellation.clone(),
            intent,
            outputs: vec![],
        })
    }
}
fn contains(reference: &RasterRef36, id: u32) -> bool {
    reference.id == id
        || reference
            .retained
            .as_ref()
            .is_some_and(|o| o.pixels.id == id)
}
fn layer_contains(layer: &Layer36, id: u32) -> bool {
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
fn unlocked(metadata: &Document36, target: &str) -> Result<()> {
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
