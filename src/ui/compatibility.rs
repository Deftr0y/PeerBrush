//! Source-bound, view-only compatibility inspection before saving a PSD/PSB.
use super::*;
use crate::compatibility::{Representation, StandardLayout};

impl PeerBrush {
    pub(super) fn open_save_report(&mut self) {
        let result = crate::compatibility::report(&self.shared.lock().unwrap().doc);
        match result {
            Ok(report) => self.save_report = Some(report),
            Err(error) => self.message = error,
        }
    }
    pub(super) fn compatibility_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(report) = self.save_report.as_ref() else {
            return;
        };
        if report.document_id != doc.id || report.revision != doc.revision {
            self.save_report = None;
            self.message =
                "Document changed · reopen PSD compatibility to review current sources".into();
            return;
        }
        let mut open = true;
        egui::Window::new("PSD / PSB compatibility")
            .id(egui::Id::new("save compatibility"))
            .open(&mut open).default_width(560.).resizable(true)
            .show(ctx, |ui| {
                ui.label(format!("{} bit · revision {}", report.bit_depth, report.revision));
                ui.label(match report.standard_layout {
                    StandardLayout::Protected => "This source is protected. Create an explicit compatible copy before saving.",
                    StandardLayout::LayerTree => "Standard output contains separate raster layers and supported folders.",
                    StandardLayout::CompositeOnly => "Standard output contains one baked composite layer. The original tree requires PeerBrush sources.",
                    StandardLayout::CompositeAndHiddenSources => "Standard output contains a visible filtered composite and hidden original layer roots.",
                });
                if report.save_allowed {
                    ui.label(format!("{} standard layers/folders · current merged composite · native channel depth", report.standard_layer_count));
                    ui.label("Standard rasters and masks preserve the saved result. Supplementary PeerBrush sources retain additional editable definitions; receiving applications may ignore them.");
                    ui.label(if report.output_profile == "embedded_srgb" { "Embedded sRGB profile · no profile conversion during saving" } else { "Untagged sRGB working space · no profile conversion during saving" });
                }
                egui::ScrollArea::vertical().id_salt("compatibility details").max_height(400.).show(ui, |ui| {
                    for reason in &report.protected_reasons { ui.label(RichText::new(reason).color(ACCENT)); }
                    if report.losses.is_empty() && report.save_allowed {
                        ui.label("No baking or supplementary-source limitation was found in this document's standard layer plan.");
                    }
                    for loss in &report.losses {
                        if let Some(layer) = loss.layer_id.as_ref().and_then(|id| report.layers.iter().find(|layer| &layer.id == id)) {
                            ui.label(RichText::new(&layer.name).strong());
                        }
                        ui.label(RichText::new(loss.message).color(ACCENT));
                        ui.add_space(4.);
                    }
                    ui.separator();
                    ui.label(RichText::new("Layer representation").strong());
                    for layer in &report.layers {
                        let representation = match layer.standard {
                            Representation::Raster => "editable raster",
                            Representation::Folder => "folder",
                            Representation::BakedFolderRaster => "folder baked to raster",
                            Representation::BakedIntoFolder => "included in a baked folder",
                            Representation::CompositeOnly => "included in the composite",
                            Representation::ProtectedPreview => "protected preview",
                        };
                        ui.label(format!("{} · {representation}{}", layer.name, if layer.standard_mask { " · raster mask" } else { "" }));
                        if matches!(layer.standard, Representation::Raster | Representation::BakedFolderRaster) {
                            if let Some([left, top, right, bottom]) = layer.standard_bounds { ui.label(format!("Standard bounds: {left}, {top} → {right}, {bottom}")); }
                        }
                        if !layer.peerbrush_sources.is_empty() {
                            let definitions = layer.peerbrush_sources.iter().map(|source| match *source {
                                "effect_stack" => "editable effects", "mask_stack" => "editable mask steps",
                                "editable_text_vector_source" => "editable text/vector definitions",
                                "transform_originals" => "original pixels and transform matrices",
                                "folder_hierarchy" => "original folder and children", _ => "original layer",
                            }).collect::<Vec<_>>().join(", ");
                            ui.label(format!("PeerBrush sources retain: {definitions}"));
                        }
                    }
                    if report.details_truncated { ui.label("Long protected-source reasons were shortened for this report."); }
                });
                ui.label("This report describes the codec's output. Receiving-application round trips and full Photoshop compatibility are not guaranteed.");
            });
        if !open {
            self.save_report = None;
        }
    }
}
