use super::*;
fn metadata_admission(
    doc: DocumentView<'_>,
) -> Result<Option<Arc<crate::psd::memory36::MetadataCharge>>, String> {
    doc.native()
        .map(|native| {
            if doc.layer_count() > 100 {
                return Err("Transform toolbar exceeds its layer bound".into());
            }
            let base = doc
                .id()
                .len()
                .checked_mul(256)
                .and_then(|n| n.checked_add(65536))
                .ok_or("Transform toolbar metadata overflow")?;
            let bytes = doc
                .layers()
                .try_fold(base, |sum, layer| {
                    layer
                        .id()
                        .len()
                        .checked_mul(64)
                        .and_then(|n| sum.checked_add(n))
                })
                .filter(|n| *n <= 64 * 1024 * 1024)
                .ok_or("Transform toolbar metadata exceeds admission")?;
            crate::psd::memory36::MetadataCharge::reserve(native.budget(), bytes, &|| Ok(()))
        })
        .transpose()
}
impl PeerBrush {
    fn toolbar_transform_bounds(
        &mut self,
        doc: DocumentView<'_>,
        roots: &[String],
        ctx: &egui::Context,
    ) -> Result<Option<[i32; 4]>, String> {
        if roots.is_empty() {
            return Ok(None);
        }
        if let Some(selection) = doc.selection().filter(|_| !self.transform_entire_pixel_copy(doc)) {
            return Ok(Some(selection));
        }
        match doc {
            DocumentView::Resident(resident) => Ok(Some(
                crate::transform::tree_bounds(resident, roots).unwrap_or([
                    0,
                    0,
                    doc.width() as i32,
                    doc.height() as i32,
                ]),
            )),
            DocumentView::Native(native) => self
                .transform_bounds
                .get(
                    &self.shared,
                    &self.project_id,
                    native,
                    roots,
                    self.mask,
                    self.mask_step.as_deref(),
                    ctx,
                )
                .map(|result| result.map(|geometry| geometry.bounds)),
        }
    }
    pub(super) fn transform_snap_toolbar<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        let increment = match self.tool {
            Tool::Move => "8 px",
            Tool::Rotate => "15°",
            Tool::Scale => "10% · uniform",
            _ => return,
        };
        let _charge = match metadata_admission(doc) {
            Ok(charge) => charge,
            Err(error) => {
                self.message = error.into();
                return;
            }
        };
        let roots = crate::tree::roots_view(doc, &self.selection_layers);
        let targets = crate::transform::tree_ids_view(doc, &roots);
        let enabled = !self.busy
            && !doc.read_only()
            && !roots.is_empty()
            && doc
                .layers()
                .filter(|l| targets.iter().any(|id| id == l.id()))
                .all(|l| !l.locked());
        let bounds_ready = match self.toolbar_transform_bounds(doc, &roots, ui.ctx()) {
            Ok(bounds) => bounds.is_some(),
            Err(error) => {
                self.message = error.into();
                false
            }
        };
        let enabled = enabled && bounds_ready;
        for (icon, label, parameters) in [
            (
                Icon::MirrorHorizontal,
                "Mirror horizontally",
                json!({"mirror_x":true}),
            ),
            (
                Icon::MirrorVertical,
                "Mirror vertically",
                json!({"mirror_y":true}),
            ),
            (
                Icon::QuarterTurnLeft,
                "Rotate 90° counterclockwise",
                json!({"angle":-90.}),
            ),
            (
                Icon::QuarterTurnRight,
                "Rotate 90° clockwise",
                json!({"angle":90.}),
            ),
        ] {
            let response = ui
                .add_enabled_ui(enabled, |ui| icons::small_button(ui, icon, label))
                .inner;
            if response.clicked() {
                self.transform_toolbar_action(doc, parameters, label);
            }
        }
        let active = ui.input(|i| i.modifiers.shift);
        ui.label(RichText::new(format!("{} · {increment}", if active {"Snap"} else {"Shift"})).size(11.).color(if active {ACCENT} else {MUTED}))
            .on_hover_text("Hold Shift to snap this gesture. Release Shift for free transformation. Move snaps in document pixels; scaling keeps both axes uniform while Shift is held.");
    }

    fn transform_toolbar_action<'a>(
        &mut self,
        doc: impl Into<DocumentView<'a>>,
        parameters: Value,
        label: &str,
    ) {
        let doc = doc.into();
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; transform controls preserved".into();
            return;
        }
        if self.free_transform_action(&parameters) {
            return;
        }
        let _charge = match metadata_admission(doc) {
            Ok(charge) => charge,
            Err(error) => {
                self.message = error.into();
                return;
            }
        };
        let roots = crate::tree::roots_view(doc, &self.selection_layers);
        let ctx = self.edit_context.clone();
        let bounds = match self.toolbar_transform_bounds(doc, &roots, &ctx) {
            Ok(Some(bounds)) => bounds,
            Ok(None) => {
                self.message = "Preparing transform bounds; current controls preserved".into();
                return;
            }
            Err(error) => {
                self.message = error.into();
                return;
            }
        };
        let pivot = [
            (bounds[0] as f64 + bounds[2] as f64) / 2.,
            (bounds[1] as f64 + bounds[3] as f64) / 2.,
        ];
        let selection_only = doc.selection().is_some() && !self.transform_entire_pixel_copy(doc);
        let commands = roots
            .iter()
            .map(|id| {
                let mut c = parameters.clone();
                c["op"] = json!("transform");
                c["layer"] = json!(id);
                c["pivot"] = json!(pivot);
                c["mask"] = json!(self.mask);
                c["selection_only"] = json!(selection_only);
                c["document_id"] = json!(doc.id());
                c["source_revision"] = json!(doc.revision());
                if roots.len() == 1 {
                    c["step"] = json!(self.mask_step);
                }
                c
            })
            .collect::<Vec<_>>();
        // A toolbar action ends the pending presentation gesture before editing.
        self.select_tool(self.tool);
        self.close_proposal();
        self.apply_action(
            commands,
            label,
            Some((&doc.id(), doc.revision())),
            edit_job::Origin::Action,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_toolbar_uses_captured_selection_bounds_and_refuses_unadmitted_work() {
        let (mut app, ctx) = crate::ui::workspace_tests::fixture(16);
        let mut doc = Document::new_depth(19, 13, 16).unwrap();
        doc.selection = Some([2, 3, 15, 11]);
        let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let native = crate::tree::presentation_tests::native(&doc, &pool);
        let roots = vec![doc.layers[0].id.clone()];
        assert_eq!(
            app.toolbar_transform_bounds((&native).into(), &roots, &ctx)
                .unwrap(),
            doc.selection
        );
        let used = pool.used();
        let held = pool
            .reserve_bytes(pool.limit() - used, pool.limit())
            .unwrap();
        app.proposal_review = Some("preserved-proposal".into());
        app.points = vec![[3., 4.]];
        app.transform_toolbar_action(&native, json!({"angle":90}), "Rotate");
        assert!(metadata_admission((&native).into()).is_err());
        assert!(!app.message.is_empty());
        assert_eq!(app.proposal_review.as_deref(), Some("preserved-proposal"));
        assert_eq!(app.points, vec![[3., 4.]]);
        drop(held);
        assert_eq!(pool.used(), used);
    }
    #[test]
    fn toolbar_transform_rejects_same_revision_document_replacement_and_allows_fresh_retry() {
        for depth in [8, 16] {
            for selected in [false, true] {
                for parameters in [
                    json!({"mirror_x":true}),
                    json!({"mirror_y":true}),
                    json!({"angle":90.}),
                    json!({"angle":-90.}),
                ] {
                    let (mut app, _) = super::super::tests::fixture();
                    let snapshot = {
                        let mut engine = app.shared.lock().unwrap();
                        engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                        engine.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(5, 6, [12347, 23459, 34571, 65535]);
                        engine.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(11, 12, [51347, 10359, 22371, 65535]);
                        engine.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(25, 25, [43219, 19237, 22349, 65535]);
                        if selected {
                            engine.edit("human", &[json!({"op":"selection","kind":"rectangle","rect":[4,5,12,13]})], None, None, "Fixture").unwrap();
                        }
                        engine.resident_state_mut().expect("resident fixture").undo.clear();
                        engine.resident_state().expect("resident fixture").document.clone()
                    };
                    app.select_content(&snapshot.layers[0].id);
                    app.select_tool(Tool::Rotate);
                    let mut replacement = snapshot.clone();
                    replacement.id = "replacement-transform-project".into();
                    replacement.layers[0]
                        .pixels
                        .set16(6, 7, [53417, 23149, 13257, 65535]);
                    assert_eq!(replacement.revision, snapshot.revision);
                    app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = replacement.clone();
                    app.transform_toolbar_action(&snapshot, parameters.clone(), "Stale action");
                    {
                        let engine = app.shared.lock().unwrap();
                        assert!(
                            engine.resident_state().expect("resident fixture").undo.is_empty(),
                            "{depth} {selected} {parameters}: stale action reached replacement"
                        );
                        assert_eq!(
                            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            serde_json::to_value(&replacement).unwrap()
                        );
                    }
                    assert!(app.message.contains("source changed"), "{}", app.message);
                    let bounds = replacement.selection.unwrap_or(
                        crate::transform::tree_bounds(
                            &replacement,
                            &[replacement.layers[0].id.clone()],
                        )
                        .unwrap(),
                    );
                    let mut command = parameters.clone();
                    command["op"] = json!("transform");
                    command["layer"] = json!(replacement.layers[0].id);
                    command["pivot"] = json!([
                        (bounds[0] as f64 + bounds[2] as f64) / 2.,
                        (bounds[1] as f64 + bounds[3] as f64) / 2.
                    ]);
                    command["selection_only"] = json!(selected);
                    let expected =
                        crate::engine::Engine::preview_edits(replacement.clone(), &[command])
                            .unwrap();
                    app.transform_toolbar_action(&replacement, parameters, "Fresh action");
                    let mut engine = app.shared.lock().unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1, "{}", app.message);
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        expected.export_png().unwrap()
                    );
                    assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
                    engine.undo("human").unwrap();
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        replacement.export_png().unwrap()
                    );
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        replacement.layers[0].pixels.rgba16()
                    );
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.selection_coverage,
                        replacement.selection_coverage
                    );
                }
            }
        }
    }

    #[test]
    fn toolbar_actions_click_through_real_controls_and_commit_one_native_edit() {
        use egui::accesskit::{Action, ActionRequest, Role};

        for depth in [8, 16] {
            for tool in [Tool::Move, Tool::Rotate, Tool::Scale] {
                for label in ["Mirror horizontally", "Mirror vertically", "Rotate 90° counterclockwise", "Rotate 90° clockwise"] {
                    let ctx = egui::Context::default();
                    ctx.enable_accesskit();
                    let mut e = crate::engine::Engine::new();
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                    for y in 4..28 {
                        for x in 4..28 {
                            e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                                x,
                                y,
                                [10001 + x as u16 * 97, 20003 + y as u16 * 137, 30007, 65535],
                            );
                        }
                    }
                    let before = e.resident_state().expect("resident fixture").document.clone();
                    let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    let shared = Arc::new(std::sync::Mutex::new(e));
                    let connection = crate::server::Connection {
                        port: 0,
                        token: String::new(),
                        state_dir: std::path::PathBuf::new(),
                        instance_lock: None,
                        availability: Default::default(),
                    };
                    let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
                    app.selected = id.clone();
                    app.selection_layers = [id.clone()].into_iter().collect();
                    app.select_tool(tool);
                    let mut time = 0.;
                    let mut draw = |app: &mut PeerBrush, events| {
                        time += 0.05;
                        ctx.run(
                            egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(
                                    Pos2::ZERO,
                                    Vec2::new(1300., 900.),
                                )),
                                time: Some(time),
                                events,
                                ..Default::default()
                            },
                            |ctx| app.draw(ctx),
                        )
                    };
                    draw(&mut app, vec![]);
                    let tree = draw(&mut app, vec![]).platform_output.accesskit_update.unwrap();
                    let (target, node) = tree.nodes.iter()
                        .find(|(_, node)| node.label() == Some(label))
                        .expect("Transform button visible");
                    assert_eq!(node.role(), Role::Button);
                    assert!(node.supports_action(Action::Click));
                    draw(&mut app, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Click, target: *target, data: None,
                    })]);
                    let after = shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    assert_eq!(
                        shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(),
                        1,
                        "{label}: {}",
                        app.message
                    );
                    assert_eq!(after.bit_depth, depth);
                    let mut c = json!({"op":"transform","layer":id,"pivot":[16.,16.],"selection_only":false,"mask":false});
                    match label {
                        "Mirror horizontally" => c["mirror_x"] = json!(true),
                        "Mirror vertically" => c["mirror_y"] = json!(true),
                        "Rotate 90° counterclockwise" => c["angle"] = json!(-90.),
                        _ => c["angle"] = json!(90.),
                    };
                    let expected =
                        crate::engine::Engine::preview_edits(before.clone(), &[c]).unwrap();
                    assert_eq!(after.export_png().unwrap(), expected.export_png().unwrap());
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        draw(&mut app, vec![]);
                        if app.canvas_pixels.as_ref().is_some_and(|(w, h, p)| {
                            after.preview(None, (*w).max(*h), None, false).unwrap().2 == *p
                        }) {
                            break;
                        }
                        assert!(std::time::Instant::now() < deadline, "Action canvas absent");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let mut e = shared.lock().unwrap();
                    e.undo("human").unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                }
            }
        }
    }

    #[test]
    fn toolbar_rotation_repeats_on_a_pixel_copy_outside_its_original_marquee() {
        let (mut app, _) = super::super::tests::fixture();
        let mut doc = Document::new_depth(32, 32, 16).unwrap();
        doc.layers[0].pixels.set16(2, 2, [11223, 22334, 33445, u16::MAX]);
        doc.layers[0].pixels.set16(3, 2, [33445, 22334, 11223, u16::MAX]);
        doc.selection = Some([1, 1, 5, 4]);
        app.shared.lock().unwrap().resident_state_mut().unwrap().document = doc.clone();
        app.select_content(&doc.layers[0].id);
        app.duplicate_with_transform((&doc).into());
        let copy = app.selected.clone();
        app.select_tool(Tool::Rotate);
        app.edit(vec![json!({"op":"move", "layer":copy, "dx":10, "selection_only":false})], "Move copy");
        let snapshot = |app: &PeerBrush| app.shared.lock().unwrap().resident_state().unwrap().document.clone();
        let moved = snapshot(&app);
        app.transform_toolbar_action(&moved, json!({"angle":90.}), "Rotate copy");
        let rotated = snapshot(&app);
        let mut expected = moved.layers[0].clone();
        let command = json!({"op":"transform", "layer":copy, "angle":90., "pivot":[13., 2.5], "selection_only":false});
        crate::transform::layer(&mut expected, &command).unwrap();
        assert_eq!(rotated.layers[0].pixels, expected.pixels);
        app.duplicate_with_transform((&rotated).into());
        let repeated = snapshot(&app);
        crate::transform::layer(&mut expected, &command).unwrap();
        assert_ne!(app.selected, copy);
        assert_eq!(repeated.layers[0].pixels, expected.pixels);
        assert_eq!(repeated.selection, doc.selection);
        assert_eq!(repeated.layers.last().unwrap().pixels, doc.layers[0].pixels);
    }

    #[test]
    fn shift_wer_toggles_snapping_previews_real_native_pixels_and_commits_once() {
        for depth in [8, 16] {
            for tool in [Tool::Move, Tool::Rotate, Tool::Scale] {
                for zoom in [0.8, 1.4] {
                    for selected in [false, true] {
                        let ctx = egui::Context::default();
                        let mut e = crate::engine::Engine::new();
                        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                        for y in 8..24 {
                            for x in 8..24 {
                                e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                                    x,
                                    y,
                                    [10001 + x as u16 * 37, 30003 + y as u16 * 53, 50007, 65535],
                                );
                            }
                        }
                        let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                        if selected {
                            e.edit(
                                "human",
                                &[json!({"op":"selection","rect":[8,8,24,24],"feather":1.})],
                                None,
                                None,
                                "Selection",
                            )
                            .unwrap();
                        }
                        e.resident_state_mut().expect("resident fixture").undo.clear();
                        let before = e.resident_state().expect("resident fixture").document.clone();
                        let shared = Arc::new(std::sync::Mutex::new(e));
                        let connection = crate::server::Connection {
                            port: 0,
                            token: String::new(),
                            state_dir: std::path::PathBuf::new(),
                            instance_lock: None,
                            availability: Default::default(),
                        };
                        let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
                        app.selected = layer.clone();
                        app.selection_layers = [layer].into_iter().collect();
                        app.select_tool(tool);
                        app.zoom = zoom;
                        let mut time = 0.;
                        let mut draw = |app: &mut PeerBrush, events, modifiers| {
                            time += 0.05;
                            ctx.run(
                                egui::RawInput {
                                    screen_rect: Some(Rect::from_min_size(
                                        Pos2::ZERO,
                                        Vec2::new(1100., 900.),
                                    )),
                                    time: Some(time),
                                    events,
                                    modifiers,
                                    ..Default::default()
                                },
                                |ctx| app.draw(ctx),
                            )
                        };
                        draw(&mut app, vec![], Default::default());
                        draw(&mut app, vec![], Default::default());
                        let rect = app.view_rect.unwrap();
                        let scale = rect.width() / 32.;
                        let center = rect.min + Vec2::new(16., 16.) * scale;
                        let (start, end) = match tool {
                            Tool::Rotate => {
                                let angle = 22.4f32.to_radians();
                                (
                                    center + Vec2::new(58., 0.),
                                    center + Vec2::new(angle.cos(), angle.sin()) * 58.,
                                )
                            }
                            Tool::Scale => {
                                (center + Vec2::new(65., 0.), center + Vec2::new(96., 0.))
                            }
                            _ => (center, center + Vec2::new(13.4, 3.1) * scale),
                        };
                        let button = |pos, pressed, modifiers| egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers,
                        };
                        draw(
                            &mut app,
                            vec![
                                egui::Event::PointerMoved(start),
                                button(start, true, Default::default()),
                            ],
                            Default::default(),
                        );
                        draw(
                            &mut app,
                            vec![egui::Event::PointerMoved(end)],
                            Default::default(),
                        );
                        let free = app.transient.clone();
                        assert_eq!(free.len(), 1);
                        let shift = egui::Modifiers {
                            shift: true,
                            ..Default::default()
                        };
                        let output = draw(&mut app, vec![], shift);
                        let snapped = app.transient.clone();
                        assert_eq!(snapped.len(), 1);
                        assert_eq!(snapped[0]["snap"], true);
                        assert_ne!(free, snapped);
                        assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.job.text.contains("Snap"))));
                        match tool {
                            Tool::Move => {
                                assert_eq!(snapped[0]["dx"], 16.);
                                assert_eq!(snapped[0]["dy"], 0.);
                            }
                            Tool::Rotate => assert_eq!(snapped[0]["angle"], 15.),
                            _ => {
                                assert!(
                                    (snapped[0]["scale_x"].as_f64().unwrap() - 1.5).abs() < 0.0001
                                );
                                assert_eq!(snapped[0]["scale_x"], snapped[0]["scale_y"]);
                            }
                        }
                        draw(&mut app, vec![], Default::default());
                        assert_eq!(
                            app.transient, free,
                            "Releasing Shift restores free parameters"
                        );
                        draw(&mut app, vec![], shift);
                        let preview =
                            crate::engine::Engine::preview_edits(before.clone(), &app.transient)
                                .unwrap();
                        let deadline = std::time::Instant::now() + Duration::from_secs(5);
                        loop {
                            draw(&mut app, vec![], shift);
                            if app.canvas_pixels.as_ref().is_some_and(|(w, h, pixels)| {
                                preview.preview(None, (*w).max(*h), None, false).unwrap().2
                                    == *pixels
                            }) {
                                break;
                            }
                            assert!(
                                std::time::Instant::now() < deadline,
                                "Rendered snapped preview missing"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        {
                            let e = shared.lock().unwrap();
                            assert!(e.resident_state().expect("resident fixture").undo.is_empty());
                            assert_eq!(e.resident_state().expect("resident fixture").document.revision, before.revision);
                            assert_eq!(
                                e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                                before.layers[0].pixels.rgba16()
                            );
                        }
                        draw(&mut app, vec![button(end, false, shift)], shift);
                        let mut e = shared.lock().unwrap();
                        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}: {}", tool.label(), app.message);
                        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), preview.export_png().unwrap());
                        e.undo("human").unwrap();
                        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                        assert_eq!(e.resident_state().expect("resident fixture").document.selection, before.selection);
                    }
                }
            }
        }
    }
    #[test]
    fn toolbar_action_preserves_controls_and_review_while_an_edit_is_pending() {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.proposal_review = Some("preserved-review".into());
        app.tool = Tool::Rotate;
        let shared = app.shared.clone();
        let guard = shared.lock().unwrap();
        let history = guard.history_lengths();
        app.submit_edit_job(
            edit_job::Request {
                project: app.project_id.clone(),
                document: doc.id.clone(),
                revision: doc.revision,
                label: "Pending rename".into(),
                selection_edit: false,
                origin: edit_job::Origin::Action,
            },
            edit_job::Commands::resident(vec![
                json!({"op":"layer.update","layer":app.selected,"name":"Renamed"}),
            ]),
            &ctx,
        )
        .unwrap();
        app.transform_toolbar_action(&doc, json!({"angle":90}), "Rotate");
        assert_eq!(app.proposal_review.as_deref(), Some("preserved-review"));
        assert!(app.tool == Tool::Rotate);
        assert_eq!(guard.history_lengths(), history);
        drop(guard);
        super::super::text_tests::wait_edit(&mut app);
        let engine = shared.lock().unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels, doc.layers[0].pixels);
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].name, "Renamed");
        assert_eq!(engine.history_lengths(), [history[0] + 1, 0]);
    }
}
