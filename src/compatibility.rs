//! View-only PSD/PSB save reports derived from the codec's standard layer plan.
use crate::{engine::Document, psd};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StandardLayout {
    LayerTree,
    CompositeOnly,
    CompositeAndHiddenSources,
    Protected,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Representation {
    Raster,
    Folder,
    BakedFolderRaster,
    BakedIntoFolder,
    CompositeOnly,
    ProtectedPreview,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LossCode {
    DocumentFlattened,
    FiltersBaked,
    FolderBaked,
    EffectsBaked,
    DisabledEffectsPrivate,
    EditableSourceRasterized,
    MaskStackBaked,
    TransformOriginalsPrivate,
    OpacityQuantized,
}

#[derive(Clone, Debug, Serialize)]
pub struct Loss {
    pub code: LossCode,
    pub layer_id: Option<String>,
    pub message: &'static str,
}
#[derive(Clone, Debug, Serialize)]
pub struct LayerReport {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub standard: Representation,
    pub standard_container: Option<String>,
    pub standard_mask: bool,
    pub standard_bounds: Option<[i32; 4]>,
    pub peerbrush_sources: Vec<&'static str>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SaveReport {
    pub document_id: String,
    pub revision: u64,
    pub bit_depth: u16,
    pub save_allowed: bool,
    pub has_editability_losses: bool,
    pub standard_layout: StandardLayout,
    pub standard_records: usize,
    pub standard_layer_count: usize,
    pub current_merged_composite: bool,
    pub profile: serde_json::Value,
    pub output_profile: &'static str,
    pub profile_conversion: bool,
    pub layers: Vec<LayerReport>,
    pub losses: Vec<Loss>,
    pub protected_reasons: Vec<String>,
    pub details_truncated: bool,
}

/// Acceptance applies only to the exact source whose output was reviewed.
/// It never overrides protection, destination conflicts or final save guards.
pub(crate) fn authorize_save(doc: &Document, request: &serde_json::Value) -> Result<(), String> {
    let accepted = request
        .get("accept_editability_loss")
        .map(|value| {
            value
                .as_bool()
                .ok_or("accept_editability_loss must be boolean")
        })
        .transpose()?
        .unwrap_or(false);
    let report = report(doc)?;
    if !report.save_allowed {
        return Err(
            "Cannot save an unsupported PSD without explicitly creating a compatible copy".into(),
        );
    }
    if accepted
        && (request["document_id"].as_str() != Some(doc.id.as_str())
            || request["expected_revision"].as_u64() != Some(doc.revision))
    {
        return Err(
            "Save-loss acceptance requires the reviewed document_id and expected_revision".into(),
        );
    }
    if report.has_editability_losses && !accepted {
        return Err("PSD/PSB output changes editability or standard layer settings. Request document.save_report, review its losses, then confirm accept_editability_loss=true for that document_id and expected_revision. Current work remains unchanged.".into());
    }
    Ok(())
}

pub fn report(doc: &Document) -> Result<SaveReport, String> {
    psd::validate(doc)?;
    let mut report = SaveReport {
        document_id: doc.id.clone(),
        revision: doc.revision,
        bit_depth: doc.bit_depth,
        save_allowed: !doc.read_only,
        has_editability_losses: false,
        standard_layout: StandardLayout::Protected,
        standard_records: 0,
        standard_layer_count: 0,
        current_merged_composite: !doc.read_only,
        profile: crate::color_profile::summary(doc.icc_profile.as_deref().map(Vec::as_slice)),
        output_profile: if doc.read_only {
            "source_protected"
        } else if doc.srgb_tagged {
            "embedded_srgb"
        } else {
            "untagged_srgb"
        },
        profile_conversion: false,
        layers: vec![],
        losses: vec![],
        protected_reasons: vec![],
        details_truncated: false,
    };
    if doc.read_only {
        report.details_truncated = doc.warnings.len() > 64;
        for reason in doc.warnings.iter().take(64) {
            let shortened = reason.chars().take(1024).collect::<String>();
            report.details_truncated |= shortened.len() != reason.len();
            report.protected_reasons.push(shortened);
        }
        report.layers = doc
            .layers
            .iter()
            .map(|layer| LayerReport {
                id: layer.id.clone(),
                name: layer.name.clone(),
                parent: layer.parent.clone(),
                standard: Representation::ProtectedPreview,
                standard_container: None,
                standard_mask: false,
                standard_bounds: None,
                peerbrush_sources: vec![],
            })
            .collect();
        return Ok(report);
    }
    let plan = psd::standard_plan(doc);
    let tree = !plan.composite || plan.hidden_sources;
    let order = if tree {
        psd::standard_order(doc)
    } else {
        vec![]
    };
    report.standard_records = (order.len() + usize::from(plan.composite)).max(1);
    if report.standard_records > 200 {
        return Err("Too many PSD layer records".into());
    }
    report.standard_layout = if plan.hidden_sources {
        StandardLayout::CompositeAndHiddenSources
    } else if plan.composite {
        StandardLayout::CompositeOnly
    } else {
        StandardLayout::LayerTree
    };
    if plan.composite {
        report.losses.push(Loss {
            code: if plan.hidden_sources { LossCode::FiltersBaked } else { LossCode::DocumentFlattened },
            layer_id: None,
            message: if plan.hidden_sources {
                "Whole-image filters are baked into the visible standard composite. Original standard layer roots are hidden; editable filters and original visibility require PeerBrush sources."
            } else {
                "Adjustments or folder clipping require one standard composite layer. The original editable layer tree requires PeerBrush sources."
            },
        });
    }
    let represented = order
        .iter()
        .filter(|(_, section)| *section != 3)
        .map(|(layer, section)| (layer.id.as_str(), *section))
        .collect::<HashMap<_, _>>();
    let by_id = doc
        .layers
        .iter()
        .map(|layer| (layer.id.as_str(), layer))
        .collect::<HashMap<_, _>>();
    for layer in &doc.layers {
        let mut container = None;
        let mut parent = layer.parent.as_deref();
        while let Some(id) = parent {
            let ancestor = by_id.get(id).ok_or("Missing save-report parent")?;
            if represented.get(id) == Some(&0) && ancestor.kind == "group" {
                container = Some(id.to_owned());
            }
            parent = ancestor.parent.as_deref();
        }
        let standard = if !tree {
            Representation::CompositeOnly
        } else if represented.contains_key(layer.id.as_str()) {
            if layer.kind == "group" {
                if represented[layer.id.as_str()] == 0 {
                    Representation::BakedFolderRaster
                } else {
                    Representation::Folder
                }
            } else {
                Representation::Raster
            }
        } else {
            Representation::BakedIntoFolder
        };
        let mut sources = if represented.contains_key(layer.id.as_str()) {
            vec![]
        } else {
            vec!["original_layer"]
        };
        let mut loss = |code, message| {
            report.losses.push(Loss {
                code,
                layer_id: Some(layer.id.clone()),
                message,
            })
        };
        if standard == Representation::BakedFolderRaster {
            sources.push("folder_hierarchy");
            loss(LossCode::FolderBaked, "Folder color effects bake the folder and its children into one standard raster. The original folder tree and effects require PeerBrush sources.");
        }
        if !layer.effects.is_empty() {
            sources.push("effect_stack");
            if standard == Representation::Raster && crate::effects::active(layer) {
                loss(LossCode::EffectsBaked, "Layer effects are baked into the standard raster; editable effect settings require PeerBrush sources.");
            } else if !crate::effects::active(layer) {
                loss(
                    LossCode::DisabledEffectsPrivate,
                    "Disabled effect settings are retained only in PeerBrush sources.",
                );
            }
        }
        if layer.source.is_some() {
            sources.push("editable_text_vector_source");
            loss(LossCode::EditableSourceRasterized, "Text, vector or path definitions have a standard raster appearance; editable definitions require PeerBrush sources. Photoshop text/vector objects are not written.");
        }
        if let Some(mask) = &layer.mask {
            sources.push("mask_stack");
            if mask.steps.len() > 1
                || mask
                    .steps
                    .iter()
                    .any(|step| step.kind != "paint" || !step.enabled || step.weight != 1.)
            {
                loss(LossCode::MaskStackBaked, "Mask steps are combined into a standard raster mask where this layer remains separate. Editable step settings require PeerBrush sources.");
            }
        }
        if layer.pixels.retained.is_some()
            || layer
                .mask
                .as_ref()
                .is_some_and(|mask| mask.steps.iter().any(|step| step.pixels.retained.is_some()))
        {
            sources.push("transform_originals");
            loss(LossCode::TransformOriginalsPrivate, "Standard rasters contain the current transformed result. Retained originals and transform matrices require PeerBrush sources.");
        }
        if represented.contains_key(layer.id.as_str())
            && (layer.opacity * 255.).round() / 255. != layer.opacity
        {
            loss(LossCode::OpacityQuantized, "Standard PSD opacity uses 256 levels. PeerBrush sources retain the original value; another editor recomposing the layers may differ slightly from the current merged composite.");
        }
        report.layers.push(LayerReport {
            id: layer.id.clone(),
            name: layer.name.clone(),
            parent: layer.parent.clone(),
            standard_mask: layer.mask.is_some() && represented.contains_key(layer.id.as_str()),
            standard_bounds: represented
                .get(layer.id.as_str())
                .map(|section| psd::standard_bounds(doc, layer, *section)),
            standard,
            standard_container: container,
            peerbrush_sources: sources,
        });
    }
    report.has_editability_losses = !report.losses.is_empty();
    report.standard_layer_count = (report
        .layers
        .iter()
        .filter(|layer| {
            matches!(
                layer.standard,
                Representation::Raster | Representation::Folder | Representation::BakedFolderRaster
            )
        })
        .count()
        + usize::from(plan.composite))
    .max(1);
    Ok(report)
}
