use super::*;
impl PeerBrush {
    pub(super) fn transform_snap_toolbar(&mut self, ui: &mut egui::Ui, doc: &Document) {
        let increment = match self.tool {
            Tool::Move => "8 px",
            Tool::Rotate => "15°",
            Tool::Scale => "10% · uniform",
            _ => return,
        };
        let roots = crate::tree::roots(doc, &self.selection_layers.iter().cloned().collect());
        let targets = crate::transform::tree_ids(doc, &roots);
        let enabled = !self.busy
            && !doc.read_only
            && !roots.is_empty()
            && doc
                .layers
                .iter()
                .filter(|l| targets.contains(&l.id))
                .all(|l| !l.locked);
        for (label, tooltip, parameters) in [
            (
                "Mirror H",
                "Mirror horizontally around the current target center",
                json!({"mirror_x":true}),
            ),
            (
                "Mirror V",
                "Mirror vertically around the current target center",
                json!({"mirror_y":true}),
            ),
            (
                "↶ 90°",
                "Rotate a quarter turn counterclockwise",
                json!({"angle":-90.}),
            ),
            (
                "↷ 90°",
                "Rotate a quarter turn clockwise",
                json!({"angle":90.}),
            ),
        ] {
            if ui.add_enabled(enabled, egui::Button::new(label).frame(false)).on_hover_text(format!("{tooltip}. Uses the selected Color or paint Mask step and active selection; Undo restores the result.")).clicked() {
                self.transform_toolbar_action(doc, parameters, label);
            }
        }
        let active = ui.input(|i| i.modifiers.shift);
        ui.label(RichText::new(format!("{} · {increment}", if active {"Snap"} else {"Shift"})).size(11.).color(if active {ACCENT} else {MUTED}))
            .on_hover_text("Hold Shift to snap this gesture. Release Shift for free transformation. Move snaps in document pixels; scaling keeps both axes uniform while Shift is held.");
    }

    fn transform_toolbar_action(&mut self, doc: &Document, parameters: Value, label: &str) {
        let roots = crate::tree::roots(doc, &self.selection_layers.iter().cloned().collect());
        let bounds = doc
            .selection
            .or_else(|| crate::transform::tree_bounds(doc, &roots))
            .unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
        let pivot = [
            (bounds[0] as f64 + bounds[2] as f64) / 2.,
            (bounds[1] as f64 + bounds[3] as f64) / 2.,
        ];
        let commands = roots
            .iter()
            .map(|id| {
                let mut c = parameters.clone();
                c["op"] = json!("transform");
                c["layer"] = json!(id);
                c["pivot"] = json!(pivot);
                c["mask"] = json!(self.mask);
                c["selection_only"] = json!(doc.selection.is_some());
                if roots.len() == 1 {
                    c["step"] = json!(self.mask_step);
                }
                c
            })
            .collect::<Vec<_>>();
        // A toolbar action ends the pending presentation gesture before editing.
        self.select_tool(self.tool);
        self.close_proposal();
        let result =
            self.shared
                .lock()
                .unwrap()
                .edit("human", &commands, Some(doc.revision), None, label);
        self.message = result.map(|_| label.into()).unwrap_or_else(|e| e);
        self.last_preview = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toolbar_actions_click_through_real_controls_and_commit_one_native_edit() {
        for depth in [8, 16] {
            for tool in [Tool::Move, Tool::Rotate, Tool::Scale] {
                for label in ["Mirror H", "Mirror V", "↶ 90°", "↷ 90°"] {
                    let ctx = egui::Context::default();
                    let mut e = crate::engine::Engine::new();
                    e.doc = Document::new_depth(32, 32, depth).unwrap();
                    for y in 4..28 {
                        for x in 4..28 {
                            e.doc.layers[0].pixels.set16(
                                x,
                                y,
                                [10001 + x as u16 * 97, 20003 + y as u16 * 137, 30007, 65535],
                            );
                        }
                    }
                    let before = e.doc.clone();
                    let id = e.doc.layers[0].id.clone();
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
                    let output = draw(&mut app, vec![]);
                    let pos = output
                        .shapes
                        .iter()
                        .find_map(|s| {
                            if let egui::Shape::Text(t) = &s.shape {
                                if t.galley.job.text == label {
                                    return Some(t.pos + t.galley.size() * 0.5);
                                }
                            }
                            None
                        })
                        .expect("Transform button visible");
                    let button = |pressed| egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    };
                    draw(&mut app, vec![egui::Event::PointerMoved(pos), button(true)]);
                    draw(&mut app, vec![button(false)]);
                    let after = shared.lock().unwrap().doc.clone();
                    assert_eq!(
                        shared.lock().unwrap().undo.len(),
                        1,
                        "{label}: {}",
                        app.message
                    );
                    assert_eq!(after.bit_depth, depth);
                    let mut c = json!({"op":"transform","layer":id,"pivot":[16.,16.],"selection_only":false,"mask":false});
                    match label {
                        "Mirror H" => c["mirror_x"] = json!(true),
                        "Mirror V" => c["mirror_y"] = json!(true),
                        "↶ 90°" => c["angle"] = json!(-90.),
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
                    assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                }
            }
        }
    }
    #[test]
    fn shift_wer_toggles_snapping_previews_real_native_pixels_and_commits_once() {
        for depth in [8, 16] {
            for tool in [Tool::Move, Tool::Rotate, Tool::Scale] {
                for zoom in [0.8, 1.4] {
                    for selected in [false, true] {
                        let ctx = egui::Context::default();
                        let mut e = crate::engine::Engine::new();
                        e.doc = Document::new_depth(32, 32, depth).unwrap();
                        for y in 8..24 {
                            for x in 8..24 {
                                e.doc.layers[0].pixels.set16(
                                    x,
                                    y,
                                    [10001 + x as u16 * 37, 30003 + y as u16 * 53, 50007, 65535],
                                );
                            }
                        }
                        let layer = e.doc.layers[0].id.clone();
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
                        e.undo.clear();
                        let before = e.doc.clone();
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
                            assert!(e.undo.is_empty());
                            assert_eq!(e.doc.revision, before.revision);
                            assert_eq!(
                                e.doc.layers[0].pixels.rgba16(),
                                before.layers[0].pixels.rgba16()
                            );
                        }
                        draw(&mut app, vec![button(end, false, shift)], shift);
                        let mut e = shared.lock().unwrap();
                        assert_eq!(e.undo.len(), 1, "{}: {}", tool.label(), app.message);
                        assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
                        e.undo("human").unwrap();
                        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                        assert_eq!(e.doc.selection, before.selection);
                    }
                }
            }
        }
    }
}
