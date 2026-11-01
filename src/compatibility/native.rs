//! Pure metadata reporting from the same standard-record plan as native Save.
//! No raster sampling, source-session acquisition or CMS execution belongs here.
use super::*;
use crate::{
    engine::{
        native::NativeDocument,
        scene::{SceneIdentity, SceneSnapshot},
    },
    psd::{scene_save36::ScenePlan, standard36::RowSource},
};

pub(super) fn report(
    document: &NativeDocument,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedReport, String> {
    check()?;
    let doc = document.metadata();
    if doc.layers().len() > 200 {
        return Err("Too many source-backed save-report layers".into());
    }
    // Copies are limited to names/IDs and bounded warning excerpts. Settings,
    // embedded images, profiles, contours and native pixels remain borrowed.
    let mut text = doc.id().len();
    for layer in doc.layers() {
        text = text
            .checked_add(layer.id().len())
            .and_then(|n| n.checked_add(layer.name().len()))
            .and_then(|n| n.checked_add(layer.parent().map_or(0, str::len)))
            .ok_or("Native save-report text accounting overflow")?;
    }
    let bytes = text
        .checked_mul(16)
        .and_then(|n| n.checked_add(1024 * 1024))
        .ok_or("Native save-report admission overflow")?;
    let admission = document
        .budget()
        .reserve_bytes(bytes, 8 * 1024 * 1024)
        .map_err(|e| e.to_string())?;
    let mut report = SaveReport {
        document_id: doc.id().into(),
        revision: doc.revision(),
        bit_depth: doc.depth(),
        save_allowed: !doc.read_only(),
        has_editability_losses: false,
        standard_layout: StandardLayout::Protected,
        standard_records: 0,
        standard_layer_count: 0,
        current_merged_composite: !doc.read_only(),
        profile: if doc.profile().is_some() {
            // Protected-source reporting never builds a global CMS cache.
            serde_json::json!({"source":"embedded ICC", "preview":"source-protected", "conversion_available":false})
        } else {
            crate::color_profile::summary(None)
        },
        output_profile: if doc.read_only() {
            "source_protected"
        } else if doc.srgb_tagged() {
            "embedded_srgb"
        } else {
            "untagged_srgb"
        },
        profile_conversion: false,
        layers: Vec::new(),
        losses: Vec::new(),
        protected_reasons: Vec::new(),
        details_truncated: false,
    };
    report
        .layers
        .try_reserve_exact(doc.layers().len())
        .map_err(|_| "Native save-report layer allocation refused")?;
    report
        .losses
        .try_reserve_exact(doc.layers().len() * 8 + 1)
        .map_err(|_| "Native save-report loss allocation refused")?;
    if doc.read_only() {
        report
            .protected_reasons
            .try_reserve_exact(doc.warnings().len().min(64))
            .map_err(|_| "Native protected-report allocation refused")?;
        report.details_truncated = doc.warnings().len() > 64;
        for reason in doc.warnings().iter().take(64) {
            check()?;
            let excerpt: String = reason.chars().take(1024).collect();
            report.details_truncated |= excerpt.len() != reason.len();
            report.protected_reasons.push(excerpt);
        }
        for layer in doc.layers() {
            report.layers.push(LayerReport {
                id: layer.id().into(),
                name: layer.name().into(),
                parent: layer.parent().map(Into::into),
                standard: Representation::ProtectedPreview,
                standard_container: None,
                standard_mask: false,
                standard_bounds: None,
                peerbrush_sources: Vec::new(),
            });
        }
        check()?;
        return Ok(PreparedReport {
            report,
            _admission: Some(admission),
        });
    }
    // This snapshot is only a borrowed metadata-plan owner. It supplies no
    // session, filesystem or publication authority, and never escapes reporting.
    let scene = SceneSnapshot::from_native(
        SceneIdentity {
            project_id: String::new(),
            document_id: doc.id().into(),
            revision: doc.revision(),
        },
        document.clone(),
        Vec::new(),
        None,
        None,
    )?;
    let plan = ScenePlan::for_report(&scene, check)?;
    let records = plan.standard().records();
    report.standard_records = records.len();
    report.standard_layer_count = records.iter().filter(|r| r.section != 3).count();
    report.standard_layout = if plan.has_fallback() {
        StandardLayout::CompositeAndHiddenSources
    } else {
        StandardLayout::LayerTree
    };
    if plan.has_fallback() {
        report.losses.push(Loss {
            code: if doc.filters().len() != 0 { LossCode::FiltersBaked } else { LossCode::DocumentFlattened },
            layer_id: None,
            message: "The visible standard composite contains the current rendered result. Original standard layer roots are hidden; editable settings and original visibility require PeerBrush sources.",
        });
    }
    for (index, layer) in doc.layers().enumerate() {
        check()?;
        let record = records
            .iter()
            .find(|r| {
                r.section != 3
                    && match r.source {
                        RowSource::Layer(i) | RowSource::OriginalLayer(i) => i == index,
                        _ => false,
                    }
            })
            .ok_or("Native save-report layer is missing from its actual standard plan")?;
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(8)
            .map_err(|_| "Native save-report source allocation refused")?;
        if matches!(record.source, RowSource::OriginalLayer(_)) {
            sources.push("original_layer");
        }
        let mut loss = |code, message| {
            report.losses.push(Loss {
                code,
                layer_id: Some(layer.id().into()),
                message,
            })
        };
        if layer.effects().len() != 0 {
            sources.push("effect_stack");
            if layer
                .effects()
                .any(|effect| effect.enabled() && effect.weight() > 0.)
            {
                loss(LossCode::EffectsBaked, "Layer effects are baked into the standard appearance; editable effect settings require PeerBrush sources.");
            } else {
                loss(
                    LossCode::DisabledEffectsPrivate,
                    "Disabled effect settings are retained only in PeerBrush sources.",
                );
            }
        }
        if layer.source().is_some() {
            sources.push("editable_text_vector_source");
            loss(LossCode::EditableSourceRasterized, "Text, vector or path definitions have a standard raster appearance; editable definitions require PeerBrush sources. Photoshop text/vector objects are not written.");
        }
        if let Some(mask) = layer.mask() {
            sources.extend(["mask_stack", "mask_link"]);
            if !mask.linked() {
                loss(LossCode::MaskLinkPrivate, "The unlinked mask choice is retained in PeerBrush sources. Photoshop mask-link editing behavior is not written or verified; another editor may link the mask. Standard mask pixels and the current composite are preserved.");
            }
            if mask.steps().len() > 1
                || mask
                    .steps()
                    .any(|s| s.kind() != "paint" || !s.enabled() || s.weight() != 1.)
            {
                loss(LossCode::MaskStackBaked, "Mask steps are combined into a standard raster mask. Editable step settings require PeerBrush sources.");
            }
        }
        if layer.pixels().retained().is_some()
            || layer
                .mask()
                .is_some_and(|m| m.steps().any(|s| s.pixels().retained().is_some()))
        {
            sources.push("transform_originals");
            loss(LossCode::TransformOriginalsPrivate, "Standard rasters contain the current transformed result. Retained originals and transform matrices require PeerBrush sources.");
        }
        if (layer.opacity() * 255.).round() / 255. != layer.opacity() {
            loss(LossCode::OpacityQuantized, "Standard PSD opacity uses 256 levels. PeerBrush sources retain the original value; another editor recomposing the layers may differ slightly from the current merged composite.");
        }
        report.layers.push(LayerReport {
            id: layer.id().into(),
            name: layer.name().into(),
            parent: layer.parent().map(Into::into),
            standard: if record.section == 1 {
                Representation::Folder
            } else {
                Representation::Raster
            },
            standard_container: None,
            standard_mask: record.mask.is_some(),
            standard_bounds: Some(record.bounds),
            peerbrush_sources: sources,
        });
    }
    report.has_editability_losses = !report.losses.is_empty();
    check()?;
    Ok(PreparedReport {
        report,
        _admission: Some(admission),
    })
}

#[cfg(test)]
mod tests;
