use super::*;
use crate::engine::Engine;
use crate::source::{Content, Source};

const SIZE: Vec2 = Vec2::new(1360., 1040.);

fn draw(
    app: &mut PeerBrush,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
            time: Some(ctx.input(|input| input.time) + 0.05),
            events,
            modifiers,
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    )
}

fn button(
    at: Pos2,
    button: egui::PointerButton,
    pressed: bool,
    modifiers: egui::Modifiers,
) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button,
        pressed,
        modifiers,
    }
}

fn key_event(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers,
    }
}

fn key(app: &mut PeerBrush, ctx: &egui::Context, value: egui::Key, modifiers: egui::Modifiers) {
    for pressed in [true, false] {
        draw(
            app,
            ctx,
            vec![key_event(value, pressed, modifiers)],
            modifiers,
        );
    }
}

fn click(app: &mut PeerBrush, ctx: &egui::Context, at: Pos2) {
    for pressed in [true, false] {
        draw(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(at),
                button(
                    at,
                    egui::PointerButton::Primary,
                    pressed,
                    Default::default(),
                ),
            ],
            Default::default(),
        );
    }
}

fn labels(output: &egui::FullOutput, expected: &str) -> Vec<Rect> {
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == expected => {
                Some(Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
        .collect()
}

fn label(output: &egui::FullOutput, expected: &str) -> Pos2 {
    labels(output, expected)
        .first()
        .unwrap_or_else(|| panic!("Missing visible {expected}"))
        .center()
}

fn point(app: &PeerBrush, value: [f32; 2]) -> Pos2 {
    let rect = app.view_rect.unwrap();
    let width = app.shared.lock().unwrap().doc.width;
    app.view.point(value, rect, rect.width() / width as f32)
}

fn state(app: &PeerBrush) -> Value {
    let e = app.shared.lock().unwrap();
    let history = |entries: &[crate::engine::History]| {
        entries.iter().map(|h|json!({"before":h.before,"after":h.after,"actor":h.actor,"label":h.label,"task":h.task,"scopes":h.scopes,"gesture":h.gesture,"reverted":h.reverted})).collect::<Vec<_>>()
    };
    json!({"doc":e.doc,"undo":history(&e.undo),"redo":history(&e.redo),"saved_revision":e.saved_revision,"path":e.path,"closed":e.closed})
}

struct WorkingFile {
    path: PathBuf,
    bytes: Vec<u8>,
}
impl WorkingFile {
    fn unchanged(&self) {
        assert_eq!(std::fs::read(&self.path).unwrap(), self.bytes);
    }
}
impl Drop for WorkingFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(self.path.parent().unwrap());
    }
}

fn fixture(depth: u16) -> (PeerBrush, egui::Context, WorkingFile) {
    let (mut app, ctx) = tests::fixture();
    app.recovery.open = false;
    app.clipboard = crate::clipboard::Worker::memory(ctx.clone());
    app.shortcuts = shortcuts::Shortcuts::load(Path::new(""));
    ctx.style_mut(|style| style.animation_time = 0.);
    app.color = [31, 97, 173, 255];
    app.frame_pending = false;
    let directory = std::env::temp_dir().join(format!("peerbrush-text-ui-{}", crate::engine::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("human-source.psd");
    let file = {
        let mut e = app.shared.lock().unwrap();
        e.doc = Document::new_depth(256, 192, depth).unwrap();
        app.selected = e.doc.layers[0].id.clone();
        e.doc.layers[0]
            .pixels
            .set16(1, 1, [12347, 33559, 51237, 65535]);
        for p in [[220, 160], [224, 164]] {
            e.edit("human",&[json!({"op":"paint","layer":app.selected,"points":[p],"radius":3,"color":[233,84,32,255]})],None,None,"Human stroke").unwrap();
        }
        e.undo("human").unwrap();
        e.doc.selection = Some([16, 12, 32, 28]);
        let bytes = crate::psd::encode(&e.doc).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        e.path = Some(path.clone());
        WorkingFile { path, bytes }
    };
    app.selection_layers = [app.selected.clone()].into_iter().collect();
    for _ in 0..3 {
        draw(&mut app, &ctx, vec![], Default::default());
    }
    (app, ctx, file)
}

fn text_tool(app: &mut PeerBrush, ctx: &egui::Context) -> Rect {
    key(app, ctx, egui::Key::T, Default::default());
    assert!(app.tool == Tool::Text);
    let output = draw(app, ctx, vec![], Default::default());
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Rect(r)
                if r.fill == Color32::from_rgb(74, 48, 71)
                    && r.rect.left() < 60.
                    && r.rect.size() == Vec2::new(42., 39.) =>
            {
                Some(r.rect)
            }
            _ => None,
        })
        .expect("Visible selected Text toolbar button")
}

fn begin(app: &mut PeerBrush, ctx: &egui::Context, at: Pos2) {
    draw(
        app,
        ctx,
        vec![
            egui::Event::PointerMoved(at),
            button(at, egui::PointerButton::Primary, true, Default::default()),
        ],
        Default::default(),
    );
    assert!(app.source_editor.as_ref().unwrap().placement.is_some());
}

fn settle(app: &mut PeerBrush, ctx: &egui::Context, expected: &Document) {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        draw(app, ctx, vec![], Default::default());
        if !app.pending {
            if let Some((w, h, pixels)) = &app.canvas_pixels {
                let rendered = expected.preview(None, (*w).max(*h), None, false).unwrap().2;
                if *pixels == rendered && app.canvas_preview == app.last_preview {
                    return;
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Text preview did not show current shared-engine pixels"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn command_preview(app: &PeerBrush) -> Document {
    let command = app.source_editor.as_ref().unwrap().command();
    Engine::preview_edits(app.shared.lock().unwrap().doc.clone(), &[command]).unwrap()
}

fn replace_text(app: &mut PeerBrush, ctx: &egui::Context, old: &str, new: &str) {
    let output = draw(app, ctx, vec![], Default::default());
    let mut focused = false;
    for rect in labels(&output, old) {
        click(app, ctx, rect.center());
        focused = ctx
            .memory(|m| m.focused())
            .and_then(|id| ctx.read_response(id).map(|r| (id, r.rect)))
            .is_some_and(|(id, area)| {
                area.contains(rect.center()) && egui::TextEdit::load_state(ctx, id).is_some()
            });
        if focused {
            break;
        }
    }
    assert!(focused, "Visible text content was not editable");
    key(app, ctx, egui::Key::A, egui::Modifiers::COMMAND);
    draw(
        app,
        ctx,
        vec![egui::Event::Text(new.into())],
        Default::default(),
    );
    let Content::Text { text, .. } = &app.source_editor.as_ref().unwrap().source.content else {
        panic!("Text editor")
    };
    assert_eq!(text, new);
}

fn number(app: &mut PeerBrush, ctx: &egui::Context, name: &str, old: &str, new: &str) {
    let output = draw(app, ctx, vec![], Default::default());
    let values = labels(&output, old);
    let at = labels(&output, name)
        .into_iter()
        .find_map(|label| {
            values
                .iter()
                .filter(|r| {
                    r.left() > label.right()
                        && r.left() - label.right() < 80.
                        && (r.center().y - label.center().y).abs() < 5.
                })
                .min_by(|a, b| a.left().total_cmp(&b.left()))
        })
        .unwrap_or_else(|| panic!("Visible {old} adjacent to {name}"))
        .center();
    click(app, ctx, at);
    click(app, ctx, at);
    assert!(ctx.wants_keyboard_input());
    key(app, ctx, egui::Key::A, egui::Modifiers::COMMAND);
    draw(
        app,
        ctx,
        vec![egui::Event::Text(new.into())],
        Default::default(),
    );
    key(app, ctx, egui::Key::Enter, Default::default());
}

#[test]
fn text_toolbar_canvas_draft_controls_render_native_pixels_then_apply_once() {
    for depth in [8, 16] {
        let (mut app, ctx, file) = fixture(depth);
        let before = state(&app);
        let text = text_tool(&mut app, &ctx);
        key(&mut app, &ctx, egui::Key::B, Default::default());
        assert!(app.tool == Tool::Brush);
        click(&mut app, &ctx, text.center());
        assert!(app.tool == Tool::Text);
        let a = point(&app, [30., 30.]);
        let b = point(&app, [150., 110.]);
        let rect = app.view_rect.unwrap();
        let scale = rect.width() / app.shared.lock().unwrap().doc.width as f32;
        let anchor = app.view.document(a, rect, scale);
        let end = app.view.document(b, rect, scale);
        let left = anchor[0].min(end[0]).floor();
        let top = anchor[1].min(end[1]).floor();
        let width = (anchor[0].max(end[0]).ceil() - left) as u32;
        let height = (anchor[1].max(end[1]).ceil() - top) as u32;
        begin(&mut app, &ctx, a);
        draw(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(b)],
            Default::default(),
        );
        let expected = command_preview(&app);
        settle(&mut app, &ctx, &expected);
        assert!(app.source_editor.as_ref().unwrap().placement.is_some());
        assert_ne!(
            expected.export_png().unwrap(),
            app.shared.lock().unwrap().doc.export_png().unwrap()
        );
        assert_eq!(state(&app), before);
        draw(
            &mut app,
            &ctx,
            vec![button(
                b,
                egui::PointerButton::Primary,
                false,
                Default::default(),
            )],
            Default::default(),
        );
        draw(&mut app, &ctx, vec![], Default::default());
        let editor = app.source_editor.as_ref().unwrap();
        assert_eq!((editor.x, editor.y), (left as i32, top as i32));
        assert_eq!((editor.source.width, editor.source.height), (width, height));
        number(&mut app, &ctx, "Field width", &width.to_string(), "180");
        number(&mut app, &ctx, "Height", &height.to_string(), "96");
        number(&mut app, &ctx, "Letter spacing", "0", "2.5");
        number(
            &mut app,
            &ctx,
            "R",
            if depth == 16 { "7967" } else { "31" },
            if depth == 16 { "12501" } else { "203" },
        );
        replace_text(&mut app, &ctx, "Text", "Native title\nEditable");
        let draft = app.source_editor.as_ref().unwrap().source.clone();
        assert_eq!((draft.width, draft.height), (180, 96));
        let Content::Text {
            letter_spacing,
            color,
            ..
        } = &draft.content
        else {
            panic!("Text")
        };
        assert_eq!(*letter_spacing, 2.5);
        assert_eq!(color[0], if depth == 16 { 12501 } else { 203 * 257 });
        let expected = command_preview(&app);
        settle(&mut app, &ctx, &expected);
        assert_eq!(state(&app), before);
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Apply"));
        assert!(app.source_editor.is_none());
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.undo.len(), 2);
        assert!(e.redo.is_empty());
        assert_eq!(e.doc.layers[0].source.as_ref(), Some(&draft));
        assert_eq!(e.doc.bit_depth, depth);
        assert_eq!(e.doc.export_png().unwrap(), expected.export_png().unwrap());
        assert_eq!(
            e.doc.layers[1].pixels.get16(1, 1),
            if depth == 16 {
                [12347, 33559, 51237, 65535]
            } else {
                [48 * 257, 131 * 257, 199 * 257, 65535]
            }
        );
        e.undo("human").unwrap();
        let restored = json!(e.doc.layers);
        assert_eq!(restored, before["doc"]["layers"]);
        drop(e);
        file.unchanged();
    }
}

#[test]
fn text_placement_uses_ordered_press_drag_release_and_suppresses_initial_destructive_keys() {
    for depth in [8, 16] {
        for separate in [false, true] {
            let (mut app, ctx, file) = fixture(depth);
            text_tool(&mut app, &ctx);
            let before = state(&app);
            let a = point(&app, [40., 40.]);
            let far = point(&app, [160., 120.]);
            let near = a + Vec2::splat(0.25);
            let hover = point(&app, [210., 150.]);
            let mut events = vec![
                button(
                    hover,
                    egui::PointerButton::Primary,
                    false,
                    Default::default(),
                ),
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ];
            if separate {
                draw(&mut app, &ctx, events, Default::default());
                events = vec![];
            }
            events.extend([
                egui::Event::PointerMoved(far),
                egui::Event::PointerMoved(near),
                button(
                    near,
                    egui::PointerButton::Primary,
                    false,
                    Default::default(),
                ),
                egui::Event::PointerMoved(hover),
            ]);
            draw(&mut app, &ctx, events, Default::default());
            let editor = app.source_editor.as_ref().unwrap();
            assert!(editor.placement.is_none());
            assert_eq!(
                (
                    editor.x,
                    editor.y,
                    editor.source.width,
                    editor.source.height
                ),
                (40, 40, 1, 1)
            );
            assert_eq!(state(&app), before);
            file.unchanged();
        }
        let (mut app, ctx, file) = fixture(depth);
        text_tool(&mut app, &ctx);
        let before = state(&app);
        let a = point(&app, [40., 40.]);
        draw(
            &mut app,
            &ctx,
            vec![
                key_event(egui::Key::Delete, true, Default::default()),
                key_event(egui::Key::Z, true, egui::Modifiers::COMMAND),
                egui::Event::PointerMoved(a),
                button(a, egui::PointerButton::Primary, true, Default::default()),
            ],
            Default::default(),
        );
        assert!(app.source_editor.as_ref().unwrap().placement.is_some());
        assert_eq!(state(&app), before);
        file.unchanged();
    }
}

#[test]
fn text_tool_preserves_navigation_and_cancels_placement_on_real_input_or_new_sources() {
    for depth in [8, 16] {
        let (mut app, ctx, file) = fixture(depth);
        text_tool(&mut app, &ctx);
        let before = state(&app);
        for role in ["space", "middle", "zoom"] {
            let a = point(&app, [100., 80.]);
            let b = a + Vec2::new(23., 15.);
            let modifiers = if role == "zoom" {
                egui::Modifiers::ALT
            } else {
                Default::default()
            };
            let mouse = match role {
                "middle" => egui::PointerButton::Middle,
                "zoom" => egui::PointerButton::Secondary,
                _ => egui::PointerButton::Primary,
            };
            if role == "space" {
                draw(
                    &mut app,
                    &ctx,
                    vec![key_event(egui::Key::Space, true, Default::default())],
                    Default::default(),
                );
            }
            let (pan, zoom) = (app.pan, app.zoom);
            draw(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(a),
                    button(a, mouse, true, modifiers),
                ],
                modifiers,
            );
            draw(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(b)],
                modifiers,
            );
            draw(
                &mut app,
                &ctx,
                vec![
                    button(b, mouse, false, modifiers),
                    key_event(egui::Key::Space, false, Default::default()),
                ],
                Default::default(),
            );
            assert!(app.source_editor.is_none(), "{role} must not create text");
            if role == "zoom" {
                assert_ne!(app.zoom, zoom);
            } else {
                assert_ne!(app.pan, pan);
            }
            assert_eq!(state(&app), before);
        }
        file.unchanged();
        for reason in ["escape", "focus", "tool", "revision", "document"] {
            let (mut app, ctx, file) = fixture(depth);
            text_tool(&mut app, &ctx);
            let a = point(&app, [40., 40.]);
            let b = point(&app, [160., 120.]);
            begin(&mut app, &ctx, a);
            draw(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(b)],
                Default::default(),
            );
            let mut events = vec![];
            match reason {
                "escape" => events.push(key_event(egui::Key::Escape, true, Default::default())),
                "focus" => events.push(egui::Event::WindowFocused(false)),
                "tool" => events.push(button(
                    b,
                    egui::PointerButton::Extra1,
                    true,
                    Default::default(),
                )),
                "revision" => {
                    app.shared.lock().unwrap().edit("human",&[json!({"op":"fill","layer":app.selected,"rect":[3,4,8,9],"color":[71,83,97,255]})],None,None,"Later human pixels").unwrap();
                }
                _ => {
                    let mut e = app.shared.lock().unwrap();
                    e.doc = Document::new_depth(256, 192, depth).unwrap();
                    e.doc.layers[0]
                        .pixels
                        .set16(7, 9, [23457, 34569, 45671, 65535]);
                }
            }
            let after_human = state(&app);
            draw(&mut app, &ctx, events, Default::default());
            assert!(app.source_editor.is_none(), "{reason}");
            assert_eq!(state(&app), after_human, "{reason}");
            if reason == "tool" {
                assert!(app.tool == Tool::Eraser);
            }
            file.unchanged();
        }
    }
}

fn text_source() -> Source {
    Source {
        width: 180,
        height: 96,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content::Text {
            text: "Existing".into(),
            font: "regular".into(),
            size: 24.,
            line_height: 1.2,
            letter_spacing: 0.,
            align: "left".into(),
            color: [12501, 23003, 34507, 65535],
        },
    }
}

fn existing_field(app: &mut PeerBrush) -> (String, String, [f32; 2]) {
    let source = text_source();
    let mut e = app.shared.lock().unwrap();
    e.edit(
        "human",
        &[json!({"op":"layer.add","kind":"group","name":"Text group"})],
        None,
        None,
        "Group fixture",
    )
    .unwrap();
    let group = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"source.add","source":source,"parent":group,"x":40,"y":40})],
        None,
        None,
        "Text fixture",
    )
    .unwrap();
    let id = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"transform","layer":id,"angle":90.,"mirror_x":true,"selection_only":false})],
        None,
        None,
        "Affine text fixture",
    )
    .unwrap();
    let layer = &e.doc.layers[0];
    let blank = crate::path::document_point(layer, [160., 80.]);
    assert_eq!(
        layer.pixels.get16(
            (blank[0] - layer.x as f32) as i32,
            (blank[1] - layer.y as f32) as i32
        ),
        [0; 4]
    );
    (id, group, blank)
}

#[test]
fn text_affine_blank_field_hits_visibility_locks_and_reserved_apply_preserve_the_typed_draft() {
    for depth in [8, 16] {
        let (mut app, ctx, file) = fixture(depth);
        let (id, _group, blank) = existing_field(&mut app);
        app.view.angle = 30.;
        app.view.mirror_h = true;
        draw(&mut app, &ctx, vec![], Default::default());
        text_tool(&mut app, &ctx);
        let before = state(&app);
        let at = point(&app, blank);
        click(&mut app, &ctx, at);
        assert_eq!(
            app.source_editor.as_ref().unwrap().layer.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(state(&app), before);
        replace_text(&mut app, &ctx, "Existing", "Typed draft\nRetain exactly");
        number(&mut app, &ctx, "Letter spacing", "0", "3.25");
        let draft = app.source_editor.as_ref().unwrap().command();
        let expected = command_preview(&app);
        settle(&mut app, &ctx, &expected);
        let task = app
            .shared
            .lock()
            .unwrap()
            .reserve(
                "agent",
                "Reserved text",
                vec![crate::engine::Scope::layer(&id)],
            )
            .unwrap()
            .id;
        let before = state(&app);
        let selected = (
            app.selected.clone(),
            app.selection_layers.clone(),
            app.selection_anchor.clone(),
        );
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Apply"));
        assert_eq!(app.source_editor.as_ref().unwrap().command(), draft);
        assert_eq!(state(&app), before);
        assert_eq!(
            (
                app.selected.clone(),
                app.selection_layers.clone(),
                app.selection_anchor.clone()
            ),
            selected
        );
        app.shared
            .lock()
            .unwrap()
            .leases
            .retain(|lease| lease.id != task);
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Apply"));
        assert!(app.source_editor.is_none());
        assert_eq!(
            app.shared.lock().unwrap().doc.export_png().unwrap(),
            expected.export_png().unwrap()
        );
        assert_eq!(
            app.shared.lock().unwrap().undo.len(),
            before["undo"].as_array().unwrap().len() + 1
        );
        file.unchanged();
        for guard in ["hidden", "locked_layer", "locked_ancestor"] {
            let (mut app, ctx, file) = fixture(depth);
            let (id, group, blank) = existing_field(&mut app);
            {
                let mut e = app.shared.lock().unwrap();
                if guard == "locked_layer" {
                    e.doc
                        .layers
                        .iter_mut()
                        .find(|layer| layer.id == id)
                        .unwrap()
                        .locked = true;
                } else {
                    let group = e
                        .doc
                        .layers
                        .iter_mut()
                        .find(|layer| layer.id == group)
                        .unwrap();
                    if guard == "hidden" {
                        group.visible = false;
                    } else {
                        group.locked = true;
                    }
                }
            }
            draw(&mut app, &ctx, vec![], Default::default());
            text_tool(&mut app, &ctx);
            let before = state(&app);
            let at = point(&app, blank);
            click(&mut app, &ctx, at);
            if guard == "hidden" {
                assert!(app
                    .source_editor
                    .as_ref()
                    .is_none_or(|editor| editor.layer.as_deref() != Some(id.as_str())));
            } else {
                assert!(app.source_editor.is_none());
                assert!(app.message.contains("Unlock"));
            }
            assert_eq!(state(&app), before);
            file.unchanged();
        }
    }
}

#[test]
fn overlapping_text_field_hit_follows_visible_group_order_instead_of_flat_storage() {
    for depth in [8, 16] {
        let (mut app, ctx, file) = fixture(depth);
        let top = {
            let mut e = app.shared.lock().unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.add","kind":"group","name":"Lower group"})],
                None,
                None,
                "Group fixture",
            )
            .unwrap();
            let lower = e.doc.layers[0].id.clone();
            e.edit("human",&[json!({"op":"source.add","source":text_source(),"name":"Upper text","x":40,"y":40})],None,None,"Upper text fixture").unwrap();
            let top = e.doc.layers[0].id.clone();
            e.edit("human",&[json!({"op":"source.add","source":text_source(),"parent":lower,"name":"Lower child","x":40,"y":40})],None,None,"Child text fixture").unwrap();
            assert_eq!(e.doc.layers[0].parent.as_deref(), Some(lower.as_str()));
            assert_eq!(e.doc.layers[1].id, top);
            top
        };
        draw(&mut app, &ctx, vec![], Default::default());
        text_tool(&mut app, &ctx);
        let before = state(&app);
        let at = point(&app, [200., 120.]);
        click(&mut app, &ctx, at);
        assert_eq!(
            app.source_editor.as_ref().unwrap().layer.as_deref(),
            Some(top.as_str())
        );
        assert_eq!(state(&app), before);
        file.unchanged();
    }
}
