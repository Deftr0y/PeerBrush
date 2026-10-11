//! View-only explanations for protected Photoshop documents.
use super::*;

fn explanation(warning: &str) -> Option<&'static str> {
    if warning.ends_with(": lrFX") || warning.ends_with(": lfx2") || warning.ends_with(": lmfx") {
        Some("Photoshop layer styles are not yet supported by the editable codec.")
    } else if warning.contains("Blend If") {
        Some("These ranges change how layer colors blend with the backdrop.")
    } else if warning.ends_with(": Patt") {
        Some("Embedded Photoshop patterns may be used by effects or source layers.")
    } else if warning.ends_with(": lyvr") {
        Some("This layer requires a newer Photoshop interpretation; its source is protected.")
    } else {
        None
    }
}

impl PeerBrush {
    pub(super) fn protection_window(&mut self, ctx: &egui::Context, doc: &Document) {
        if !doc.read_only || self.read_only_info.as_deref() != Some(doc.id.as_str()) {
            self.read_only_info = None;
            return;
        }
        let scroll_id = egui::Id::new(("protected-source-scroll", doc.id.as_str()));
        let mut source_requested =
            ctx.data_mut(|data| data.remove_temp::<bool>(scroll_id).unwrap_or(false));
        let mut open = true;
        egui::Window::new("Photoshop document protection")
            .id(egui::Id::new("peerbrush-protection"))
            .open(&mut open)
            .default_width(520.)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(RichText::new("Showing the saved appearance").strong());
                ui.label("The original Photoshop file still contains its layers. PeerBrush is showing its saved composite because these features cannot yet be preserved through editing:");
                if !doc.source_structure.is_empty() {
                    source_requested |= ui.button(format!("Source layers ({})", doc.source_structure.len())).clicked();
                }
                ui.add_space(8.);
                egui::ScrollArea::vertical().max_height(340.).show(ui, |ui| {
                    for warning in &doc.warnings {
                        ui.label(RichText::new(warning).color(ACCENT));
                        if let Some(detail) = explanation(warning) {
                            ui.label(detail);
                        }
                        ui.add_space(6.);
                    }
                    if !doc.source_structure.is_empty() {
                        ui.separator();
                        if source_requested { ui.scroll_to_cursor(Some(egui::Align::Min)); }
                        ui.label(RichText::new("Original source layers").strong());
                        ui.label("These are source records, not editable layers. Coordinates come from the original file.");
                        for layer in &doc.source_structure {
                            let parent = layer.parent_index.and_then(|index| doc.source_structure.iter().find(|other| other.source_index == index));
                            egui::CollapsingHeader::new(format!("{} · {}", layer.name, layer.kind))
                                .id_salt(("protected-source", layer.source_index))
                                .default_open(!layer.warnings.is_empty())
                                .show(ui, |ui| {
                                    if let Some(parent) = parent { ui.label(format!("Folder: {}", parent.name)); }
                                    let [left, top, right, bottom] = layer.bounds;
                                    ui.label(format!("Bounds: {left}, {top} → {right}, {bottom}"));
                                    ui.label(format!("{} · {}", if layer.visible { "Visible" } else { "Hidden" }, if layer.has_mask { "Source mask present" } else { "No source mask" }));
                                    for warning in &layer.warnings { ui.label(RichText::new(warning).color(ACCENT)); }
                                    if layer.warnings.is_empty() { ui.label("No unsupported setting was found in this layer record. Document-level restrictions still apply."); }
                                    if !layer.tags.is_empty() { ui.label(format!("Source tags: {}", layer.tags.join(", "))); }
                                    if layer.details_truncated { ui.label("Long source details were shortened for this report."); }
                                });
                        }
                    }
                });
                ui.add_space(8.);
                ui.label(format!("Source precision: {} bit. Opening did not flatten or overwrite the original file.", doc.bit_depth));
                ui.label("The copy action explicitly flattens the saved appearance at its original precision. Save the copy under a new name; it does not recover editable Photoshop layers.");
            });
        if !open {
            self.read_only_info = None;
        }
    }

    #[cfg(test)]
    pub(super) fn protection_visible(&self, doc: &Document) -> bool {
        self.read_only_info.as_deref() == Some(doc.id.as_str()) && doc.read_only
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protection_explanations_preserve_raw_unknown_feature_reasons() {
        assert!(explanation("Unsupported Photoshop feature: lmfx")
            .unwrap()
            .contains("layer styles"));
        assert!(explanation("Unsupported Photoshop feature: lrFX").is_some());
        assert!(explanation("Unsupported global Photoshop feature: Patt").is_some());
        assert!(explanation("Unsupported Photoshop feature: arbitrary").is_none());
    }
}
