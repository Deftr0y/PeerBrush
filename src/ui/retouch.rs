use super::*;
pub(super) struct Anchor {
    pub project: String,
    pub layer: String,
    pub mask: bool,
    pub point: [f32; 2],
    pub destination: Option<[f32; 2]>,
}
impl PeerBrush {
    pub(super) fn begin_retouch(&mut self, doc: &Document, point: [f32; 2]) -> bool {
        if ![Tool::Clone, Tool::Heal].contains(&self.tool) {
            return true;
        }
        let Some(source) = &mut self.retouch_source else {
            self.message = "Alt-click to set a clone/heal source first".into();
            return false;
        };
        if source.project != doc.id
            || source.mask != self.mask
            || !doc.layers.iter().any(|l| l.id == source.layer)
        {
            self.retouch_source = None;
            self.message = "Alt-click to set a source in this project/channel".into();
            return false;
        }
        if self.retouch_aligned {
            source.destination.get_or_insert(point);
        }
        self.retouch_revision = Some(doc.revision);
        true
    }
    pub(super) fn retouch_toolbar(&mut self, ui: &mut egui::Ui) {
        if ![Tool::Clone, Tool::Heal].contains(&self.tool) {
            return;
        }
        ui.checkbox(&mut self.retouch_aligned, "Aligned");
        ui.add_enabled(
            !self.mask,
            egui::Checkbox::new(&mut self.retouch_merged, "Merged"),
        );
        if ui
            .selectable_label(self.retouch_pick, "Set source")
            .on_hover_text("Click an image source, or Alt-click the canvas")
            .clicked()
        {
            self.retouch_pick = !self.retouch_pick;
        }
        ui.label(
            RichText::new(
                self.retouch_source
                    .as_ref()
                    .map(|s| {
                        format!(
                            "Source {:.0}, {:.0} · Alt-click to reset",
                            s.point[0], s.point[1]
                        )
                    })
                    .unwrap_or_else(|| "Alt-click to set source".into()),
            )
            .size(11.)
            .color(MUTED),
        );
    }
    pub(super) fn retouch_command(&self, command: &mut Value, points: &[[f32; 2]]) {
        if ![Tool::Clone, Tool::Heal].contains(&self.tool) {
            return;
        }
        command["op"] = json!(if self.tool == Tool::Clone {
            "clone"
        } else {
            "heal"
        });
        if let Some(source) = &self.retouch_source {
            let mut point = source.point;
            if self.retouch_aligned {
                if let (Some(destination), Some(first)) = (source.destination, points.first()) {
                    point = [
                        point[0] + first[0] - destination[0],
                        point[1] + first[1] - destination[1],
                    ];
                }
            }
            command["source"] = json!(point);
            command["source_layer"] = json!(source.layer);
            command["document_id"] = json!(source.project);
        }
        command["sample_merged"] = json!(self.retouch_merged && !self.mask);
        if let Some(revision) = self.retouch_revision {
            command["source_revision"] = json!(revision);
        }
    }
}
