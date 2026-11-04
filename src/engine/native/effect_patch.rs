//! Explicit effect settings and native clip bindings for an atomic batch.
use super::*;
use crate::engine::scene::{Effect36, EffectClip36};
use serde_json::Value;

pub(crate) struct EffectPatch {
    /// None selects a document filter; Some must be this generation's primary
    /// color slot. Native clip pixels never enter settings as inline JSON.
    pub layer: Option<RasterSlot>,
    pub id: String,
    pub settings: Option<Admitted<Value>>,
    pub enabled: Option<bool>,
    pub weight: Option<f32>,
    pub clips: Option<Vec<EffectClipPatch>>,
}
pub(crate) struct EffectClipPatch {
    pub stroke: u32,
    pub coverage: CoveragePatch,
}
pub(super) fn admission(base: &NativeDocument, patches: &[EffectPatch]) -> Result<(usize, usize)> {
    let mut bytes = patches
        .len()
        .checked_mul(32 * 1024)
        .ok_or("Native effect footprint overflow")?;
    let mut locations = 0usize;
    for patch in patches {
        bytes = bytes
            .checked_add(patch.id.len())
            .ok_or("Native effect footprint overflow")?;
        if let Some(slot) = patch.layer {
            base.source(slot)?;
            if let Some(layer) = base
                .core
                .metadata
                .layers
                .iter()
                .find(|layer| layer.pixels.id == slot.ordinal)
            {
                bytes = bytes
                    .checked_add(layer.id.len())
                    .ok_or("Native effect scope overflow")?;
            }
        }
        if let Some(settings) = &patch.settings {
            if !settings.owner().uses_budget(base.budget()) {
                return Err("Effect settings use another admission".into());
            }
            bytes = bytes
                .checked_add(settings.owner().bytes())
                .ok_or("Native effect footprint overflow")?;
        }
        if let Some(clips) = &patch.clips {
            locations = locations
                .checked_add(
                    clips
                        .len()
                        .checked_mul(2)
                        .ok_or("Native clip count overflow")?,
                )
                .ok_or("Native clip count overflow")?;
            for clip in clips {
                if !clip.coverage.contours.owner().uses_budget(base.budget()) {
                    return Err("Effect clip contours use another admission".into());
                }
                bytes = bytes
                    .checked_add(clip.coverage.contours.owner().bytes())
                    .and_then(|n| n.checked_add(32 * 1024))
                    .ok_or("Native effect footprint overflow")?;
            }
        }
    }
    Ok((bytes, locations))
}
fn selected<'a>(
    base: &'a NativeDocument,
    patch: &EffectPatch,
) -> Result<(&'a Effect36, Option<&'a Layer36>)> {
    let layer = patch
        .layer
        .map(|slot| {
            base.source(slot)?;
            base.core
                .metadata
                .layers
                .iter()
                .find(|layer| layer.pixels.id == slot.ordinal)
                .ok_or_else(|| String::from("Effect update requires a primary layer slot"))
        })
        .transpose()?;
    if let Some(layer) = layer {
        super::patch::unlocked(&base.core.metadata, &layer.id)?;
    }
    let effects = layer.map_or(&base.core.metadata.filters, |layer| &layer.effects);
    let effect = effects
        .iter()
        .find(|effect| effect.id == patch.id)
        .ok_or("Unknown native effect")?;
    Ok((effect, layer))
}
pub(super) fn scopes(
    base: &NativeDocument,
    patches: &[EffectPatch],
    scopes: &mut Vec<String>,
) -> Result<bool> {
    let mut global = false;
    for (index, patch) in patches.iter().enumerate() {
        if patches[..index]
            .iter()
            .any(|old| old.layer == patch.layer && old.id == patch.id)
        {
            return Err("Duplicate native effect target".into());
        }
        let (_, layer) = selected(base, patch)?;
        if patch
            .weight
            .is_some_and(|weight| !weight.is_finite() || !(0.0..=1.0).contains(&weight))
        {
            return Err("Invalid native effect weight".into());
        }
        if let Some(layer) = layer {
            scopes.push(layer.id.clone());
        } else {
            global = true;
        }
    }
    Ok(global)
}
pub(super) fn apply<'a>(
    base: &NativeDocument,
    metadata: &mut Document36,
    patches: &'a [EffectPatch],
    outputs: &mut Vec<&'a Surface>,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    for patch in patches {
        let effects = if patch.layer.is_some() {
            // A color replacement has already changed its temporary ordinal.
            // Select by the captured layer identity, never by that new ordinal.
            let (_, old) = selected(base, patch)?;
            let id = &old.ok_or("Missing native effect layer")?.id;
            &mut metadata
                .layers
                .iter_mut()
                .find(|layer| layer.id == *id)
                .ok_or("Missing native effect layer")?
                .effects
        } else {
            &mut metadata.filters
        };
        let effect = effects
            .iter_mut()
            .find(|effect| effect.id == patch.id)
            .ok_or("Missing native effect target")?;
        if let Some(settings) = &patch.settings {
            effect.settings = crate::engine::scene::MetadataPayload::copy_from(
                &**settings,
                base.budget(),
                check,
            )?;
        }
        if let Some(enabled) = patch.enabled {
            effect.enabled = enabled;
        }
        if let Some(weight) = patch.weight {
            effect.weight = weight;
        }
        if let Some(clips) = &patch.clips {
            let mut replacement = Vec::new();
            replacement
                .try_reserve_exact(clips.len())
                .map_err(|_| "Native clip metadata allocation refused")?;
            for clip in clips {
                replacement.push(EffectClip36 {
                    stroke: clip.stroke,
                    coverage: super::batch::coverage_ref(base, &clip.coverage, outputs, check)?,
                });
            }
            effect.clips = replacement;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::output::tests::Session;
    use super::*;
    use crate::engine::scene::{Coverage36, PendingScene};
    use serde_json::json;
    fn admitted<T>(pool: &ResidentBudget, value: T) -> Admitted<T> {
        Admitted::from_parts(
            Arc::new(value),
            MetadataCharge::reserve(pool, 64 * 1024, &|| Ok(())).unwrap(),
        )
    }
    fn document(pool: &ResidentBudget) -> NativeDocument {
        let base = super::super::tests::document(pool);
        let owner = MetadataCharge::reserve(pool, 64 * 1024, &|| Ok(())).unwrap();
        let mut metadata = wire_copy(&base.core.metadata);
        let effect = |id: &str, ordinal| Effect36 {
            id: id.into(),
            kind: "liquify".into(),
            enabled: true,
            weight: 1.,
            settings: json!({"strokes":[{"x":1.,"y":2.,"coverage":null}]}).into(),
            clips: vec![EffectClip36 {
                stroke: 0,
                coverage: Coverage36 {
                    mask: RasterRef36 {
                        id: ordinal,
                        width: 8,
                        height: 8,
                        depth: 8,
                        retained: None,
                    },
                    bounds: [0, 0, 4, 4],
                    origin: [0, 0],
                    contours: vec![vec![[0., 0.], [4., 0.], [0., 4.]]].into(),
                },
            }],
        };
        metadata.layers[0].effects = vec![effect("local", 2)];
        metadata.filters = vec![effect("global", 3)];
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(vec![
                Surface::zero(8, 8, 16).unwrap(),
                Surface::zero(8, 8, 8).unwrap(),
                Surface::zero(8, 8, 8).unwrap(),
            ])
            .unwrap(),
            pool.clone(),
            base.disk_budget().clone(),
        )
        .unwrap();
        NativeDocument::from_pending(pending).unwrap()
    }
    #[test]
    fn local_settings_clip_translation_and_origin_publish_with_one_revision() {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let layer = base.metadata().layer(0).unwrap();
        let clip = layer.effects().next().unwrap().clips().next().unwrap().1;
        let batch = RasterBatch {
            rasters: vec![],
            layers: vec![LayerPatch {
                target: layer.pixels().slot(),
                origin: Some([-3, 3]),
                source: SourceUpdate::Keep,
                materialize_fill: false,
            }],
            selection: None,
            effects: vec![EffectPatch {
                layer: Some(layer.pixels().slot()),
                id: "local".into(),
                settings: Some(admitted(
                    &pool,
                    json!({"strokes":[{"x":-2.,"y":5.,"coverage":null}]}),
                )),
                enabled: Some(true),
                weight: Some(0.75),
                clips: Some(vec![EffectClipPatch {
                    stroke: 0,
                    coverage: CoveragePatch {
                        pixels: CoveragePixels::Existing(clip.mask().slot()),
                        bounds: [-3, 3, 1, 7],
                        origin: [-3, 3],
                        contours: admitted(&pool, vec![vec![[-3., 3.], [1., 3.], [-3., 7.]]]),
                    },
                }]),
            }],
        };
        let prepared = base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        let current = base.publish("human", &[], 1, false, prepared).unwrap();
        assert_eq!(current.metadata().revision(), 1);
        let layer = current.metadata().layer(0).unwrap();
        assert_eq!(layer.origin(), [-3, 3]);
        let effect = layer.effects().next().unwrap();
        assert_eq!(effect.weight(), 0.75);
        assert_eq!(effect.settings()["strokes"][0]["x"], -2.);
        let clip = effect.clips().next().unwrap().1;
        assert_eq!(clip.origin(), [-3, 3]);
        assert_eq!(clip.bounds(), [-3, 3, 1, 7]);
        assert_eq!(
            current.source(clip.mask().slot()).unwrap().key(),
            base.source(
                base.metadata()
                    .layer(0)
                    .unwrap()
                    .effects()
                    .next()
                    .unwrap()
                    .clips()
                    .next()
                    .unwrap()
                    .1
                    .mask()
                    .slot()
            )
            .unwrap()
            .key()
        );
        drop(current);
        drop(base);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn global_filter_scope_preserves_all_layer_reservations() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let batch = RasterBatch {
            rasters: vec![],
            layers: vec![],
            selection: None,
            effects: vec![EffectPatch {
                layer: None,
                id: "global".into(),
                settings: None,
                enabled: None,
                weight: Some(0.5),
                clips: None,
            }],
        };
        let prepared = base
            .capture_edit()
            .unwrap()
            .batch(batch, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()));
        assert_eq!(prepared.intent().scopes()[0].target, None);
        let lease = crate::engine::Lease {
            id: "held".into(),
            owner: "other".into(),
            description: "layer".into(),
            scopes: vec![crate::engine::Scope::layer(
                base.metadata().layer(0).unwrap().id(),
            )],
            expires: 100,
        };
        let refused = base
            .publish("worker", &[lease], 1, false, prepared)
            .unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        assert_eq!(base.metadata().revision(), 0);
    }
    #[test]
    fn invalid_clip_graph_inline_pixels_and_foreign_settings_fail_closed() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let base = document(&pool);
        let target = base.metadata().layer(0).unwrap().pixels().slot();
        let batch = |settings| RasterBatch {
            rasters: vec![],
            layers: vec![],
            selection: None,
            effects: vec![EffectPatch {
                layer: Some(target),
                id: "local".into(),
                settings: Some(settings),
                enabled: None,
                weight: None,
                clips: None,
            }],
        };
        let inline = batch(admitted(
            &pool,
            json!({"strokes":[{"coverage":{"mask":{"tiles":[]}}}]}),
        ));
        let refused = base
            .capture_edit()
            .unwrap()
            .batch(inline, &|| Ok(()))
            .err()
            .unwrap();
        assert!(refused.reason().contains("Inline Liquify"));
        refused.abort(&Session, &|| Ok(())).unwrap();
        let missing = batch(admitted(&pool, json!({"strokes":[]})));
        assert!(base
            .capture_edit()
            .unwrap()
            .batch(missing, &|| Ok(()))
            .err()
            .unwrap()
            .reason()
            .contains("clip source"));
        let foreign = ResidentBudget::new(1024 * 1024).unwrap();
        let wrong = batch(admitted(&foreign, json!({"strokes":[]})));
        assert!(base
            .capture_edit()
            .unwrap()
            .batch(wrong, &|| Ok(()))
            .err()
            .unwrap()
            .reason()
            .contains("another admission"));
        assert_eq!(base.metadata().revision(), 0);
        assert_eq!(base.disk_budget().used().unwrap(), 0);
    }
}
