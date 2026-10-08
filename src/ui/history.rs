use super::*;
impl PeerBrush {
    pub(super) fn task_history_menu(&mut self, ui: &mut egui::Ui) {
        let tasks = self.shared.lock().unwrap().task_history();
        ui.menu_button(RichText::new("AI tasks").color(AI_BLUE), |ui| {
            if tasks.as_array().unwrap().is_empty() {
                ui.label("No agent tasks in history");
            }
            for task in tasks.as_array().unwrap().iter().rev() {
                let active = task["active_batches"].as_u64().unwrap();
                let label = format!(
                    "{} · {}{}",
                    task["label"].as_str().unwrap(),
                    task["actor"].as_str().unwrap(),
                    if active == 0 { " · undone" } else { "" }
                );
                if ui
                    .add_enabled(active > 0, egui::Button::new(label))
                    .on_hover_text(task["task"].as_str().unwrap())
                    .clicked()
                {
                    let result = self.shared.lock().unwrap().inspect_task_undo(
                        "human",
                        task["actor"].as_str().unwrap(),
                        task["task"].as_str().unwrap(),
                    );
                    match result {
                        Ok(mut result) => {
                            result["document"] = json!(self.shared.lock().unwrap().doc.id);
                            result["label"] = task["label"].clone();
                            self.task_undo_review = Some(result);
                        }
                        Err(error) => self.message = error,
                    }
                    ui.close_menu();
                }
            }
            ui.label(
                RichText::new("Review scope before undoing a task")
                    .size(11.0)
                    .color(MUTED),
            );
        });
    }
    pub(super) fn task_history_review(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(review) = self.task_undo_review.clone() else {
            return;
        };
        let mut open = true;
        let mut dismiss = false;
        egui::Window::new("Undo AI task").id(egui::Id::new("task undo review")).open(&mut open).collapsible(false).default_width(430.).show(ctx,|ui| {
            ui.label(RichText::new(review["label"].as_str().unwrap_or("Agent task")).color(AI_BLUE));
            ui.label(format!("{} · {} batches",review["actor"].as_str().unwrap(),review["batches"].as_array().unwrap().len()));
            let stale=review["revision"]!=doc.revision||review["document"]!=doc.id;
            if stale {ui.label("The project changed. Close and review the task again.");}
            egui::ScrollArea::vertical().max_height(260.).show(ui,|ui| {
                for scope in review["scopes"].as_array().unwrap() {
                    let target=scope["target"].as_str();
                    let name=target.and_then(|id|doc.layers.iter().find(|l|l.id==id.strip_prefix("@visibility:").unwrap_or(id))).map(|l|l.name.as_str()).unwrap_or_else(||if target==Some("@selection"){"Selection"}else{"Document"});
                    let region=scope["rect"].as_array().map(|r|format!(" · ({}, {}) to ({}, {})",r[0],r[1],r[2],r[3])).unwrap_or_default();
                    ui.label(format!("{name}{region}"));
                }
                for conflict in review["conflicts"].as_array().unwrap() {
                    let field=conflict["field"].as_str().unwrap();let reason=conflict["reason"].as_str().unwrap();
                    let target=conflict["target"].as_str().and_then(|id|doc.layers.iter().find(|l|l.id==id));
                    let name=target.map(|l|l.name.as_str()).unwrap_or("Document");
                    let label=if field=="pixels" {"Color pixels".into()}else if field.starts_with("mask.") {"Mask source".into()}else if field.starts_with("layer.effects.") {format!("Color effect · {}",field.rsplit('.').next().unwrap().replace('_'," "))}else{field.strip_prefix("layer.").unwrap_or(field).replace('_'," ")};
                    let region=conflict["rect"].as_array().map(|r|format!(" · ({}, {}) to ({}, {})",r[0],r[1],r[2],r[3])).unwrap_or_default();
                    ui.label(RichText::new(format!("{name} · {label}{region}: {reason}")).color(ACCENT));
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy&&!stale&&review["can_undo"]==true,egui::Button::new("Undo task")).clicked() {
                    let shared=self.shared.clone();let actor=review["actor"].clone();let task=review["task"].clone();let revision=review["revision"].clone();
                    self.job(move||{server::dispatch(&shared,"history",&json!({"action":"undo_task","actor":"human","task_actor":actor,"task":task,"expected_revision":revision,"feedback":"request"}))?;Ok("Task undone · later work preserved · Undo to restore".into())});
                    dismiss=true;
                }
                if ui.button("Cancel").clicked() {dismiss=true;}
            });
        });
        if !open || dismiss {
            self.task_undo_review = None;
        }
    }
}
