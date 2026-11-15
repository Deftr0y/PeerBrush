use super::*;

pub(super) struct ProposalFrame {
    pub id: String,
    state: Option<Value>,
    draft: Option<Arc<Document>>,
}

impl ProposalFrame {
    pub(super) fn capture(engine: &crate::engine::Engine, id: &str) -> Self {
        let proposal = engine.proposals.iter().find(|proposal| proposal.id == id);
        Self {
            id: id.into(),
            state: proposal.map(crate::collaboration::Proposal::state),
            draft: proposal.and_then(|proposal| proposal.draft.clone()),
        }
    }

    pub(super) fn resident_draft(&self) -> Result<Arc<Document>, String> {
        self.draft.clone().ok_or_else(|| {
            self.state.as_ref().and_then(|state| state["reason"].as_str())
                .unwrap_or("Proposal not found in this project").to_owned()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    #[test]
    fn proposal_preview_worker_restamps_its_copy_and_keeps_the_review_snapshot_immutable() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
            app.preview_quality = preview_quality::Quality::load(Path::new(""));
            app.set_preview_quality(true).unwrap();
            let source = {
                let mut engine = app.shared.lock().unwrap();
                let resident = engine.resident_state_mut().unwrap();
                resident.document = Engine::preview_edits(resident.document.clone(), &[json!({
                    "op":"canvas.resize", "width":2048, "height":24, "anchor":"top_left",
                })]).unwrap();
                resident.document.clone()
            };
            app.doc_snapshot = source.clone().into();
            let state = crate::collaboration::propose(&app.shared, "agent", &json!({
                "document_id":source.id,"expected_revision":source.revision,
                "commands":[{"op":"paint.fill","layer":app.selected,"rect":[3,4,11,12],"color":[17,79,151,193]}],
            })).unwrap();
            let id = state["id"].as_str().unwrap();
            app.proposal_review = Some(id.into());
            app.proposal_original = false;
            let snapshot = ProposalFrame::capture(&app.shared.lock().unwrap(), id)
                .resident_draft().unwrap();
            assert!(Arc::ptr_eq(
                &snapshot,
                &app.shared.lock().unwrap().proposal_snapshot(id).unwrap()
            ));
            let frozen = serde_json::to_value(&*snapshot).unwrap();
            let expected = snapshot.preview(None, 8192, None, false).unwrap();
            let before = super::super::workspace_tests::source(&app);
            for original in [false, true] {
                app.proposal_original = original;
                app.last_preview = None;
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    app.request_preview(&ctx, &source);
                    if !app.pending && app.canvas_pixels.is_some() {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Actual proposal preview did not finish"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                let expected = if original {
                    source.preview(None, 8192, None, false).unwrap()
                } else {
                    expected.clone()
                };
                assert_eq!(app.canvas_pixels.as_ref().unwrap().0, 2048);
                assert_eq!(
                    app.canvas_pixels.as_ref().unwrap(),
                    &(expected.0, expected.1, expected.2)
                );
                assert_eq!(serde_json::to_value(&*snapshot).unwrap(), frozen);
                assert_eq!(snapshot.revision, source.revision + 1);
                assert_eq!(super::super::workspace_tests::source(&app), before);
            }
        }
    }
}
impl PeerBrush {
    pub(super) fn reconcile_reservation_message(&mut self) {
        if !self.message.starts_with("Reserved by ") {
            return;
        }
        let Ok(e) = self.shared.try_lock() else {
            return;
        };
        let still_reserved = e
            .leases
            .iter()
            .filter(|l| l.expires > crate::engine::now())
            .any(|l| {
                self.message.as_str() == format!("Reserved by {}: {}", l.owner, l.description)
                    || self.message.as_str()
                        == format!(
                            "Reserved by {}: {}. Release the reservation or edit elsewhere.",
                            l.owner, l.description
                        )
            });
        if !still_reserved {
            self.message = "Reservation released · ready to edit".into();
        }
    }
    pub(super) fn resolve_proposal_document(
        &mut self,
        draft: Result<Arc<Document>, String>,
    ) -> Option<Arc<Document>> {
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
                None
            }
        }
    }
    pub(super) fn task_history_menu(&mut self, ui: &mut egui::Ui) {
        let pending = {
            let Ok(mut engine) = self.shared.try_lock() else {
                ui.ctx().request_repaint_after(Duration::from_millis(16));
                return;
            };
            engine.expire();
            engine.proposals.iter().filter(|proposal| proposal.status == "pending").count()
        };
        let title = if pending == 0 {
            "AI tasks".into()
        } else {
            format!("AI tasks · {pending} to review")
        };
        ui.menu_button(RichText::new(title).color(AI_BLUE), |ui| {
            let (tasks, proposals, recovery, code, names) = {
                let Ok(mut engine) = self.shared.try_lock() else {
                    ui.label("Working on this project…");
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                    return;
                };
                let proposals = engine.proposals_state();
                let names: HashMap<String, String> = crate::engine::view::current(&engine)
                    .layers()
                    .map(|layer| (layer.id().to_owned(), layer.name().to_owned()))
                    .collect();
                (
                    engine.task_history(),
                    proposals,
                    engine.task_recovery(),
                    engine.code_states(),
                    names,
                )
            };
            if let Err(error)=&code {ui.label(RichText::new(error).color(AI_BLUE));}
            for run in code.as_ref().ok().and_then(|code|code.as_array()).into_iter().flatten().rev() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} · {}",run["status"].as_str().unwrap(),run["description"].as_str().unwrap())).color(AI_BLUE))
                        .on_hover_text(run["error"].as_str().map(str::to_owned).unwrap_or_else(||scope_text_with(&run["scopes"],|id|names.get(id).map(String::as_str))));
                    if run["running"]==true && ui.button("Cancel").clicked() {
                        self.shared.lock().unwrap().cancel_code(run["run"].as_str().unwrap());
                        self.message="Canceling AI work · current canvas preserved".into();
                    }
                });
            }
            for proposal in proposals.as_array().unwrap().iter().rev() {
                let pending=proposal["status"]=="pending";
                let label=format!("{} · {}",proposal["label"].as_str().unwrap(),if pending {"Review proposal"}else{proposal["status"].as_str().unwrap()});
                if ui.button(RichText::new(label).color(AI_BLUE)).on_hover_text(proposal["reason"].as_str().unwrap()).clicked() {
                    if (self.proposal_review.is_some()||self.live_key().is_empty())&&self.geometry.is_none()&&self.source_editor.is_none() {
                        self.proposal_review=Some(proposal["id"].as_str().unwrap().into());self.proposal_original=false;self.proposal_error=None;self.proposal_rendered=None;self.last_preview=None;self.interrupt_animation();
                    }else{self.message="Finish the current gesture or editor before reviewing a proposal".into();}
                    ui.close_menu();
                }
            }
            for task in recovery["active"].as_array().unwrap() {
                ui.label(RichText::new(format!("Working · {} · {}",task["owner"].as_str().unwrap(),task["description"].as_str().unwrap())).color(AI_BLUE)).on_hover_text(scope_text_with(&task["scopes"],|id|names.get(id).map(String::as_str)));
            }
            for task in recovery["recent"].as_array().unwrap().iter().rev().take(8) {
                ui.label(format!("{} · {} · {}",task["actor"].as_str().unwrap(),task["status"].as_str().unwrap().replace('_'," "),task["description"].as_str().unwrap())).on_hover_text(format!("{}\nCommitted edits remain. Review task history below; no automatic rollback or reservation restart.",scope_text_with(&task["scopes"],|id|names.get(id).map(String::as_str))));
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
                    let result = {
                        let mut engine = self.shared.lock().unwrap();
                        let document = crate::engine::view::current(&engine).id().to_owned();
                        engine.inspect_task_undo("human",task["actor"].as_str().unwrap(),task["task"].as_str().unwrap())
                            .map(|mut result| {result["document"]=json!(document);result})
                    };
                    match result {
                        Ok(mut result) => {
                            result["label"] = task["label"].clone();
                            self.task_undo_review = Some(result);
                        }
                        Err(error) => self.message = error.into(),
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
    pub(super) fn task_history_review<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        let Some(review) = self.task_undo_review.clone() else {
            return;
        };
        let mut open = true;
        let mut dismiss = false;
        egui::Window::new("Undo AI task").id(egui::Id::new("task undo review")).open(&mut open).collapsible(false).default_width(430.).show(ctx,|ui| {
            ui.label(RichText::new(review["label"].as_str().unwrap_or("Agent task")).color(AI_BLUE));
            ui.label(format!("{} · {} batches",review["actor"].as_str().unwrap(),review["batches"].as_array().unwrap().len()));
            let stale=review["revision"]!=doc.revision()||review["document"]!=doc.id();
            if stale {ui.label("The project changed. Close and review the task again.");}
            egui::ScrollArea::vertical().max_height(260.).show(ui,|ui| {
                for scope in review["scopes"].as_array().unwrap() {
                    let target=scope["target"].as_str();
                    let name=target.and_then(|id|doc.find_layer(id.strip_prefix("@visibility:").unwrap_or(id))).map(|l|l.name()).unwrap_or_else(||if target==Some("@selection"){"Selection"}else{"Document"});
                    let region=scope["rect"].as_array().map(|r|format!(" · ({}, {}) to ({}, {})",r[0],r[1],r[2],r[3])).unwrap_or_default();
                    ui.label(format!("{name}{region}"));
                }
                for conflict in review["conflicts"].as_array().unwrap() {
                    let field=conflict["field"].as_str().unwrap();let reason=conflict["reason"].as_str().unwrap();
                    let target=conflict["target"].as_str().and_then(|id|doc.find_layer(id));
                    let name=target.map(|l|l.name()).unwrap_or("Document");
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

fn scope_text<'a>(scopes: &Value, doc: impl Into<DocumentView<'a>>) -> String {
    let doc = doc.into();
    scope_text_with(scopes, |id| doc.find_layer(id).map(|layer| layer.name()))
}
fn scope_text_with<'a>(scopes: &Value, name: impl Fn(&str) -> Option<&'a str>) -> String {
    scopes
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let target = s["target"].as_str();
            let name = target
                .and_then(|id| name(id.strip_prefix("@visibility:").unwrap_or(id)))
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
            self.interrupt_animation();
            self.texture = None;
            self.canvas_pixels = None;
            self.canvas_selection = None;
            self.preview_epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.native_dispatch.clear();
            self.native_canvas.clear();
            self.native_receipt = None;
            self.pending = false;
        }
    }
    pub(super) fn accept_reviewed_proposal(
        &mut self,
        ctx: &egui::Context,
        doc: DocumentView<'_>,
        id: &str,
    ) -> Result<bool, String> {
        if doc.native().is_some() {
            if !self.native_proposal_canvas_current(doc) || self.proposal_original {
                return Err("Wait for the current proposed result before accepting".into());
            }
            self.submit_edit_job(
                edit_job::Request {
                    project: self.project_id.clone(),
                    document: doc.id().into(),
                    revision: doc.revision(),
                    label: "AI proposal accepted · Undo restores the previous project".into(),
                    selection_edit: false,
                    origin: edit_job::Origin::Proposal(egui::Id::new(id)),
                },
                edit_job::Commands::resident(vec![]),
                ctx,
            )?;
            Ok(false)
        } else {
            self.shared.lock().unwrap().accept_proposal(
                "human", id, doc.id(), doc.revision(),
            )?;
            self.message = "AI proposal accepted · Undo restores the previous project".into();
            Ok(true)
        }
    }
    pub(super) fn proposal_window<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
        frame: Option<&ProposalFrame>,
    ) {
        let doc = doc.into();
        let Some(id) = self.proposal_review.clone() else {
            return;
        };
        let Some(frame) = frame.filter(|frame| frame.id == id) else {
            ctx.request_repaint();
            return;
        };
        let Some(proposal) = &frame.state else {
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
                    ui.horizontal_wrapped(|ui| {
                        for (original, icon, label, help) in [
                            (true, Icon::CanvasBounds, "Original", "Show the document before the proposed edit."),
                            (false, Icon::Adjust, "Proposed result", "Show the rendered changes that Accept will apply."),
                        ] {
                            let selected = self.proposal_original == original;
                            if icons::labeled_choice(ui, icon, label, Some(selected), 0.)
                                .on_hover_text(help)
                                .clicked() && !selected
                            {
                                self.proposal_original = original;
                                self.last_preview = None;
                            }
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
                                && !self.edit_worker.pending()
                                && !self.proposal_original
                                && (doc.native().is_none() || self.native_proposal_canvas_current(doc))
                                && self.proposal_error.is_none()
                                && self.proposal_rendered.as_deref()
                                    == Some(self.live_key().as_str()),
                            egui::Button::new(RichText::new("Accept").color(AI_BLUE)),
                        )
                        .clicked()
                    {
                        let result = self.accept_reviewed_proposal(ctx, doc, &id);
                        match result {
                            Ok(finished) => close = finished,
                            Err(e) => {
                                self.proposal_error = Some(e);
                            }
                        }
                    }
                    if ui
                        .add_enabled(
                            proposal["status"] != "accepted" && !self.edit_worker.pending(),
                            egui::Button::new("Reject"),
                        )
                        .clicked()
                    {
                        let result = self.shared.lock().unwrap().reject_proposal("human", &id);
                        self.message = result
                            .map(|_| "Proposal discarded · current work preserved".into())
                            .unwrap_or_else(|e| e)
                            .into();
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

#[cfg(test)]
mod metadata_tests {
    use super::*;
    #[test]
    fn native_task_scope_names_match_resident_metadata_without_rendering_or_document_copies() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(9, 7, depth).unwrap();
            doc.layers[0].name = "Artwork tree".into();
            let pool = crate::large_document::ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let owner = crate::engine::view::DocumentSnapshot::from(
                crate::tree::presentation_tests::native(&doc, &pool),
            );
            let admitted = pool.used();
            let scopes = json!([{"target":format!("@visibility:{}",doc.layers[0].id),"rect":[1,2,3,4]},
                {"target":"@selection"},{"target":"missing"},{}]);
            let expected = "Artwork tree · (1, 2) to (3, 4)\nSelection\nLayer\nDocument";
            assert_eq!(scope_text(&scopes, &doc), expected);
            assert_eq!(scope_text(&scopes, owner.view()), expected);
            let names: HashMap<String, String> = owner
                .view()
                .layers()
                .map(|l| (l.id().to_owned(), l.name().to_owned()))
                .collect();
            assert_eq!(
                scope_text_with(&scopes, |id| names.get(id).map(String::as_str)),
                expected
            );
            assert_eq!(pool.used(), admitted);
        }
    }
}
