//! View-only PSD/PSB save reports derived from the codec's standard layer plan.
use crate::{engine::{view::MaskView, Document}, psd};
use serde::Serialize;
use std::collections::HashMap;
mod native;

fn mask_steps_baked(mask: MaskView<'_>, standard_mask: bool, standard_density: bool) -> bool {
    let paint = match mask.step_count().saturating_sub(usize::from(standard_density)) {
        0 => return false,
        1 => 0,
        // Photoshop's white default is the implicit starting coverage of a
        // standard mask. The import represents it as an explicit fill step.
        2 if standard_mask && mask.step(0).is_some_and(|fill| {
            fill.kind() == "fill" && fill.enabled() && fill.weight() == 1.
                && (fill.value() == 255. || standard_density && fill.value() == 0.)
                && fill.settings().is_null()
        }) => 1,
        _ => return true,
    };
    !mask.step(paint).is_some_and(|step| {
        step.kind() == "paint" && step.enabled() && step.weight() == 1.
    })
}

/// A native report retains its allocation admission through review and response
/// serialization. It cannot be converted into an uncharged owned report.
#[derive(Serialize)]
#[serde(transparent)]
pub(crate) struct PreparedReport {
    report: SaveReport,
    #[serde(skip)]
    _admission: Option<crate::large_document::Reservation>,
}
impl PreparedReport {
    pub(crate) fn data(&self) -> &SaveReport {
        &self.report
    }
}
impl From<SaveReport> for PreparedReport {
    fn from(report: SaveReport) -> Self {
        Self {
            report,
            _admission: None,
        }
    }
}
pub(crate) fn report_snapshot(
    snapshot: &crate::engine::view::DocumentSnapshot,
) -> Result<PreparedReport, String> {
    match snapshot {
        crate::engine::view::DocumentSnapshot::Resident(document) => {
            report(document).map(Into::into)
        }
        crate::engine::view::DocumentSnapshot::Native(document) => {
            native::report(document, &|| Ok(()))
        }
    }
}

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
    Adjustment,
    SolidColor,
    GradientFill,
    PatternFill,
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
    MaskLinkPrivate,
    TransformOriginalsPrivate,
    OpacityQuantized,
    AdjustmentRenderingApproximate,
}
const FILL_RASTERIZED: &str = "Solid Color fill remains editable in PeerBrush sources. Other editors receive its current raster appearance.";
const VIBRANCE_APPROXIMATION: &str = "The editable Legacy Vibrance settings are preserved. Photoshop can re-render this adjustment differently from the current merged composite.";
const PHOTO_FILTER_APPROXIMATION: &str = "The editable Photo Filter settings are preserved. Photoshop can clip bright colors differently with Preserve Luminosity enabled.";
const HUE_SATURATION_APPROXIMATION: &str = "The editable Hue/Saturation settings are preserved. Photoshop can render the adjustment differently from the current merged composite, especially at feathered color ranges.";
const COLOR_LOOKUP_APPROXIMATION: &str = "The embedded Color Lookup source is preserved. Photoshop may interpolate it differently from the current merged composite.";
const LEGACY_LOOKUP_ORDER: &str = "This earlier PeerBrush CUBE retains its original appearance and saved table-order field. Photoshop can interpret that field differently.";
const LOOKUP_WITHOUT_PROFILE: &str = "The embedded lookup table is preserved. Photoshop may need a compiled ICC profile to render the adjustment after reopening.";

fn standard_adjustment_approximation(layer: crate::engine::view::LayerView<'_>) -> Option<&'static str> {
    let effect = layer.effect(0)?;
    let settings = effect.settings();
    match effect.kind() {
        crate::effects::vibrance::KIND if settings["vibrance"].as_f64().is_some_and(|value| value != 0.) =>
            Some(VIBRANCE_APPROXIMATION),
        crate::effects::photo_filter::KIND if settings["preserve_luminosity"].as_bool().unwrap_or(true)
            && settings["density"].as_f64().unwrap_or(25.) > 0. => Some(PHOTO_FILTER_APPROXIMATION),
        crate::effects::photoshop_hue_saturation::KIND => Some(HUE_SATURATION_APPROXIMATION),
        crate::effects::color_lookup::KIND => {
            if settings["lut_format"].as_str().unwrap_or("").is_empty()
                && settings["profile"].as_str().unwrap_or("").is_empty()
                && settings["table_order"] == "rgb"
            {
                Some(LEGACY_LOOKUP_ORDER)
            } else if crate::effects::color_lookup::uses_table(settings)
                && crate::effects::color_lookup::source_size(settings, "profile") == 0
                && settings["embed_profile"] != true
            {
                Some(LOOKUP_WITHOUT_PROFILE)
            } else {
                Some(COLOR_LOOKUP_APPROXIMATION)
            }
        }
        _ => None,
    }
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
    let accepted = save_acceptance(request)?;
    authorize_report(&report(doc)?, request, accepted)
}
pub(crate) fn authorize_snapshot(
    snapshot: &crate::engine::view::DocumentSnapshot,
    request: &serde_json::Value,
) -> Result<(), String> {
    let accepted = save_acceptance(request)?;
    authorize_report(report_snapshot(snapshot)?.data(), request, accepted)
}
fn save_acceptance(request: &serde_json::Value) -> Result<bool, String> {
    request
        .get("accept_editability_loss")
        .map(|value| {
            value
                .as_bool()
                .ok_or("accept_editability_loss must be boolean")
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
        .map_err(String::from)
}
fn authorize_report(
    report: &SaveReport,
    request: &serde_json::Value,
    accepted: bool,
) -> Result<(), String> {
    if !report.save_allowed {
        return Err(
            "Cannot save an unsupported PSD without explicitly creating a compatible copy".into(),
        );
    }
    if accepted
        && (request["document_id"].as_str() != Some(report.document_id.as_str())
            || request["expected_revision"].as_u64() != Some(report.revision))
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
    let pattern_geometry = psd::patterns::Geometry::Rotation;
    let standard_patterns = psd::resident_patterns(doc, &plan, pattern_geometry, psd::PLACED_PATTERN_OVERLAY_FORMAT, &|| Ok(()))?;
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
        let standard_style = represented.get(layer.id.as_str()).and_then(|section| {
            psd::standard_styles(doc, layer, *section, &plan, &standard_patterns)
        });
        let standard = if !tree {
            Representation::CompositeOnly
        } else if represented.contains_key(layer.id.as_str()) {
            if layer.kind == "group" {
                if represented[layer.id.as_str()] == 0 {
                    Representation::BakedFolderRaster
                } else {
                    Representation::Folder
                }
            } else if psd::adjustments::standard_invert(
                layer.into(), doc.width, doc.height, !plan.composite,
            ) || psd::adjustments::standard_tone(
                layer.into(), doc.width, doc.height, !plan.composite, psd::adjustments::ToneRecords::current(doc.bit_depth),
            ).is_some() {
                Representation::Adjustment
            } else if (layer.kind == "fill" && standard_style.is_some()) || (!plan.composite
                && psd::adjustments::standard_solid(layer.into(), doc.width, doc.height).is_some())
            {
                Representation::SolidColor
            } else if !plan.composite && psd::gradient_fill::standard(layer.into(), doc.width, doc.height).is_some() {
                Representation::GradientFill
            } else if psd::patterns::standard(layer.into(), doc.width, doc.height, pattern_geometry)
                .and_then(|fill| standard_patterns.record(fill)).is_some() {
                Representation::PatternFill
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
        if standard == Representation::Adjustment {
            if let Some(message) = standard_adjustment_approximation(layer.into()) {
                loss(LossCode::AdjustmentRenderingApproximate, message);
            }
        }
        if !layer.effects.is_empty() && standard_style.is_none()
            && !matches!(standard, Representation::Adjustment | Representation::GradientFill | Representation::PatternFill)
        {
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
        if layer.kind == "fill" && !matches!(standard, Representation::SolidColor | Representation::GradientFill | Representation::PatternFill) {
            sources.push("editable_solid_color");
            loss(LossCode::EditableSourceRasterized, FILL_RASTERIZED);
        }
        if let Some(mask) = &layer.mask {
            sources.push("mask_stack");
            if !represented.contains_key(layer.id.as_str()) {
                sources.push("mask_link");
                if !mask.linked {
                    loss(LossCode::MaskLinkPrivate, "The unlinked mask choice requires PeerBrush sources because this layer has no separate standard mask record.");
                }
            }
            let standard_mask = represented.contains_key(layer.id.as_str());
            let density = standard_mask && psd::standard_mask_density(layer.into()).is_some();
            if mask_steps_baked(MaskView::Resident(mask), standard_mask, density) {
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
                .map(|section| psd::standard_bounds(doc, layer, *section, standard_style)),
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
                Representation::Raster
                    | Representation::SolidColor
                    | Representation::GradientFill
                    | Representation::PatternFill
                    | Representation::Adjustment
                    | Representation::Folder
                    | Representation::BakedFolderRaster
            )
        })
        .count()
        + usize::from(plan.composite))
    .max(1);
    Ok(report)
}
