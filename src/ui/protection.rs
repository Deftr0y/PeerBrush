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
        let mut open = true;
        egui::Window::new("Photoshop document protection")
            .id(egui::Id::new("peerbrush-protection"))
            .open(&mut open)
            .default_width(520.)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(RichText::new("Showing the saved appearance").strong());
                ui.label("The original Photoshop file still contains its layers. PeerBrush is showing its saved composite because these features cannot yet be preserved through editing:");
                ui.add_space(8.);
                egui::ScrollArea::vertical().max_height(340.).show(ui, |ui| {
                    for warning in &doc.warnings {
                        ui.label(RichText::new(warning).color(ACCENT));
                        if let Some(detail) = explanation(warning) {
                            ui.label(detail);
                        }
                        ui.add_space(6.);
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
