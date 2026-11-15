use super::*;
pub(super) struct Anchor {
    pub project: String,
    pub layer: String,
    pub mask: bool,
    pub point: [f32; 2],
    pub destination: Option<[f32; 2]>,
}
impl PeerBrush {
    pub(super) fn begin_retouch<'a>(
        &mut self,
        doc: impl Into<DocumentView<'a>>,
        point: [f32; 2],
    ) -> bool {
        let doc = doc.into();
        if ![Tool::Clone, Tool::Heal].contains(&self.tool) {
            return true;
        }
        let Some(source) = &mut self.retouch_source else {
            self.message = "Alt-click to set a clone/heal source first".into();
            return false;
        };
        if source.project != doc.id()
            || source.mask != self.mask
            || doc.find_layer(&source.layer).is_none()
        {
            self.retouch_source = None;
            self.message = "Alt-click to set a source in this project/channel".into();
            return false;
        }
        if self.retouch_aligned {
            source.destination.get_or_insert(point);
        }
        self.retouch_revision = Some(doc.revision());
        true
    }
    pub(super) fn retouch_toolbar(&mut self, ui: &mut egui::Ui) {
        if ![Tool::Clone, Tool::Heal].contains(&self.tool) {
            return;
        }
        ui.horizontal(|ui| {
            controls::label(ui, "Source tracking");
            ui.spacing_mut().item_spacing.x = 2.;
            for (aligned, icon, label, help) in [
                (true, Icon::RetouchAligned, "Keep source offset", "Keep the same distance and direction between source and brush across strokes."),
                (false, Icon::RetouchRestart, "Restart at source", "Start each stroke from the chosen source point."),
            ] {
                if icons::selectable(ui, icon, self.retouch_aligned == aligned, label)
                    .on_hover_text(help).clicked()
                {
                    self.retouch_aligned = aligned;
                }
            }
        });
        if self.mask {
            ui.label("Sample mask")
                .on_hover_text("Clone and Heal sample the evaluated layer mask while mask editing is active.");
        } else {
            controls::layer_sampling(
                ui,
                &mut self.retouch_merged,
                "Sample source layer",
                "Sample the chosen source layer’s original pixels.",
                "Sample the visible composite, including effects and masks.",
            );
        }
        icons::toggle(ui, Icon::Picker, &mut self.retouch_pick, "Set source")
            .on_hover_text("Click the canvas to pick the source, or Alt-click to pick directly.");
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
    pub(super) fn retouch_command(&self, command: &mut Value, first: Option<[f32; 2]>) {
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
                if let (Some(destination), Some(first)) = (source.destination, first) {
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
