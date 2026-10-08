use super::*;
impl PeerBrush {
    pub(super) fn review_document(&mut self, current: &Document) -> Option<Document> {
        let id = self.proposal_review.as_ref()?;
        if self.proposal_original {
            return Some(current.clone());
        }
        let draft = self.shared.lock().unwrap().proposal_document(id);
        match draft {
            Ok(doc) => Some(doc),
            Err(reason) => {
                // Clear before the canvas paints, not just when a replacement worker finishes.
                self.proposal_original = true;
                self.proposal_error = Some(reason);
                self.proposal_rendered = None;
                self.texture = None;
                self.canvas_pixels = None;
                self.canvas_selection = None;
                self.last_preview = None;
                Some(current.clone())
            }
        }
    }
    pub(super) fn task_history_menu(&mut self, ui: &mut egui::Ui) {
        let (tasks, proposals, recovery) = {
            let mut e = self.shared.lock().unwrap();
            let p = e.proposals_state();
            (e.task_history(), p, e.task_recovery())
        };
        let pending = proposals
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["status"] == "pending")
            .count();
        let title = if pending == 0 {
            "AI tasks".into()
        } else {
            format!("AI tasks · {pending} to review")
        };
        ui.menu_button(RichText::new(title).color(AI_BLUE), |ui| {
            for proposal in proposals.as_array().unwrap().iter().rev() {
                let pending=proposal["status"]=="pending";
                let label=format!("{} · {}",proposal["label"].as_str().unwrap(),if pending {"Review proposal"}else{proposal["status"].as_str().unwrap()});
                if ui.button(RichText::new(label).color(AI_BLUE)).on_hover_text(proposal["reason"].as_str().unwrap()).clicked() {
                    if (self.proposal_review.is_some()||self.live_key().is_empty())&&self.geometry.is_none()&&self.source_editor.is_none() {
                        self.proposal_review=Some(proposal["id"].as_str().unwrap().into());self.proposal_original=false;self.proposal_error=None;self.proposal_rendered=None;self.last_preview=None;self.animation.cancel();
                    }else{self.message="Finish the current gesture or editor before reviewing a proposal".into();}
                    ui.close_menu();
                }
            }
            for task in recovery["active"].as_array().unwrap() {
                ui.label(RichText::new(format!("Working · {} · {}",task["owner"].as_str().unwrap(),task["description"].as_str().unwrap())).color(AI_BLUE)).on_hover_text(scope_text(&task["scopes"],&self.shared.lock().unwrap().doc));
            }
            for task in recovery["recent"].as_array().unwrap().iter().rev().take(8) {
                ui.label(format!("{} · {} · {}",task["actor"].as_str().unwrap(),task["status"].as_str().unwrap().replace('_'," "),task["description"].as_str().unwrap())).on_hover_text(format!("{}\nCommitted edits remain. Review task history below; no automatic rollback or reservation restart.",scope_text(&task["scopes"],&self.shared.lock().unwrap().doc)));
            }
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

fn scope_text(scopes: &Value, doc: &Document) -> String {
    scopes
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let target = s["target"].as_str();
            let name = target
                .and_then(|id| {
                    doc.layers
                        .iter()
                        .find(|l| l.id == id.strip_prefix("@visibility:").unwrap_or(id))
                })
                .map(|l| l.name.as_str())
                .unwrap_or_else(|| {
                    if target == Some("@selection") {
                        "Selection"
                    } else if target.is_some() {
                        "Layer"
                    } else {
                        "Document"
                    }
                });
            let region = s["rect"]
                .as_array()
                .map(|r| format!(" · ({}, {}) to ({}, {})", r[0], r[1], r[2], r[3]))
                .unwrap_or_default();
            format!("{name}{region}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
impl PeerBrush {
    pub(super) fn close_proposal(&mut self) {
        if self.proposal_review.take().is_some() {
            self.last_preview = None;
            self.proposal_error = None;
            self.proposal_original = false;
            self.proposal_rendered = None;
            self.animation.cancel();
            self.texture = None;
            self.canvas_pixels = None;
            self.canvas_selection = None;
        }
    }
    pub(super) fn proposal_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(id) = self.proposal_review.clone() else {
            return;
        };
        let proposals = self.shared.lock().unwrap().proposals_state();
        let Some(proposal) = proposals
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .cloned()
        else {
            self.close_proposal();
            return;
        };
        let pending = proposal["status"] == "pending";
        let mut open = true;
        let mut close = false;
        egui::Window::new("Review AI proposal")
            .open(&mut open)
            .collapsible(false)
            .default_width(390.)
            .show(ctx, |ui| {
                ui.label(RichText::new(proposal["label"].as_str().unwrap()).color(AI_BLUE));
                ui.label(format!(
                    "{} · {} commands · source revision {}",
                    proposal["actor"].as_str().unwrap(),
                    proposal["operations"].as_array().unwrap().len(),
                    proposal["source_revision"]
                ));
                if pending {
                    ui.label("Preview only. Accept commits the reviewed result as one edit.");
                } else {
                    ui.colored_label(ACCENT, proposal["reason"].as_str().unwrap());
                }
                egui::ScrollArea::vertical()
                    .max_height(120.)
                    .show(ui, |ui| {
                        for line in scope_text(&proposal["scopes"], doc).lines() {
                            ui.label(line);
                        }
                    });
                if pending {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_value(&mut self.proposal_original, true, "Original")
                            .changed()
                        {
                            self.last_preview = None;
                        }
                        if ui
                            .selectable_value(&mut self.proposal_original, false, "Proposed result")
                            .changed()
                        {
                            self.last_preview = None;
                        }
                    });
                }
                if let Some(error) = &self.proposal_error {
                    ui.colored_label(ACCENT, error);
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            pending
                                && !self.busy
                                && !self.proposal_original
                                && self.proposal_error.is_none()
                                && self.proposal_rendered.as_deref()
                                    == Some(self.live_key().as_str()),
                            egui::Button::new(RichText::new("Accept").color(AI_BLUE)),
                        )
                        .clicked()
                    {
                        let result = self.shared.lock().unwrap().accept_proposal(
                            "human",
                            &id,
                            &doc.id,
                            doc.revision,
                        );
                        match result {
                            Ok(_) => {
                                self.message =
                                    "AI proposal accepted · Undo restores the previous project"
                                        .into();
                                close = true;
                            }
                            Err(e) => {
                                self.proposal_error = Some(e);
                            }
                        }
                    }
                    if ui
                        .add_enabled(
                            proposal["status"] != "accepted",
                            egui::Button::new("Reject"),
                        )
                        .clicked()
                    {
                        let result = self.shared.lock().unwrap().reject_proposal("human", &id);
                        self.message = result
                            .map(|_| "Proposal discarded · current work preserved".into())
                            .unwrap_or_else(|e| e);
                        close = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        if !open || close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close_proposal();
        }
    }
}
