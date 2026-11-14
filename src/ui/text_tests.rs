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

pub(super) fn wait_edit(app: &mut PeerBrush) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while app.edit_worker.pending() {
        app.poll_edit_job();
        assert!(
            std::time::Instant::now() < deadline,
            "source edit did not return a terminal result"
        );
        std::thread::yield_now();
    }
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

fn canvas_action(app: &mut PeerBrush, ctx: &egui::Context, label: &str) {
    use egui::accesskit::{Action, ActionRequest, Role};

    ctx.enable_accesskit();
    let update = draw(app, ctx, vec![], Default::default()).platform_output.accesskit_update.unwrap();
    let (target, node) = update.nodes.iter().find(|(_, node)| node.label() == Some(label)).unwrap();
    assert_eq!(node.role(), Role::Button);
    assert!(node.supports_action(Action::Click));
    draw(app, ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
        action: Action::Click, target: *target, data: None,
    })], Default::default());
}

fn point(app: &PeerBrush, value: [f32; 2]) -> Pos2 {
    let rect = app.view_rect.unwrap();
    let width = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.width;
    app.view.point(value, rect, rect.width() / width as f32)
}

fn state(app: &PeerBrush) -> Value {
    let e = app.shared.lock().unwrap();
    let history = |entries: &[crate::engine::History]| {
        entries.iter().map(|h|json!({"before":h.before,"after":h.after,"actor":h.actor,"label":h.label,"task":h.task,"scopes":h.scopes,"gesture":h.gesture,"reverted":h.reverted})).collect::<Vec<_>>()
    };
    json!({"doc":e.resident_state().expect("resident fixture").document,"undo":history(&e.resident_state().expect("resident fixture").undo),"redo":history(&e.resident_state().expect("resident fixture").redo),"saved_revision":e.saved_revision,"path":e.path,"closed":e.closed})
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
        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(256, 192, depth).unwrap();
        app.selected = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
        e.resident_state_mut().expect("resident fixture").document.layers[0]
            .pixels
            .set16(1, 1, [12347, 33559, 51237, 65535]);
        for p in [[220, 160], [224, 164]] {
            e.edit("human",&[json!({"op":"paint","layer":app.selected,"points":[p],"radius":3,"color":[233,84,32,255]})],None,None,"Human stroke").unwrap();
        }
        e.undo("human").unwrap();
        e.resident_state_mut().expect("resident fixture").document.selection = Some([16, 12, 32, 28]);
        let bytes = crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap();
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
    Engine::preview_edits(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone(), &[command]).unwrap()
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

fn toggle_field_dimensions(app: &mut PeerBrush, ctx: &egui::Context) {
    let output = draw(app, ctx, vec![], Default::default());
    click(app, ctx, label(&output, "Field dimensions"));
    draw(app, ctx, vec![], Default::default());
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
        let scale = rect.width() / app.shared.lock().unwrap().resident_state().expect("resident fixture").document.width as f32;
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
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap()
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
        assert!(matches!(editor.source.content, Content::Text { layout: crate::source::TextLayout::Paragraph, .. }));
        let output = draw(&mut app, &ctx, vec![], Default::default());
        assert!(labels(&output, "Field width").is_empty());
        click(&mut app, &ctx, label(&output, "Ubuntu Sans regular"));
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Search installed fonts"));
        let output = draw(&mut app, &ctx, vec![egui::Event::Text("Ubuntu".into())], Default::default());
        assert!(!labels(&output, "Ubuntu").is_empty());
        key(&mut app, &ctx, egui::Key::Escape, Default::default());
        assert!(app.source_editor.is_some());
        assert!(!ctx.memory(|memory| memory.any_popup_open()));
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Ubuntu Sans regular"));
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Ubuntu Sans medium"));
        number(&mut app, &ctx, "Size", "48", "42");
        let output = draw(&mut app, &ctx, vec![], Default::default());
        let alignment = label(&output, "Alignment");
        let color_label = labels(&output, "Color")
            .into_iter()
            .find(|rect| {
                rect.center().x > alignment.x && (rect.center().y - alignment.y).abs() < 5.
            })
            .expect("Text color swatch label");
        click(&mut app, &ctx, egui::pos2(color_label.right() + 24., color_label.center().y));
        number(&mut app, &ctx, "HEX", "1F61AD", "CB61AD");
        toggle_field_dimensions(&mut app, &ctx);
        number(&mut app, &ctx, "Field width", &width.to_string(), "180");
        number(&mut app, &ctx, "Height", &height.to_string(), "96");
        toggle_field_dimensions(&mut app, &ctx);
        number(&mut app, &ctx, "Letter spacing", "0", "2.5");
        replace_text(&mut app, &ctx, "Text", "Native title\nEditable");
        let draft = app.source_editor.as_ref().unwrap().source.clone();
        assert_eq!((draft.width, draft.height), (180, 96));
        let Content::Text {
            font,
            size,
            letter_spacing,
            color,
            ..
        } = &draft.content
        else {
            panic!("Text")
        };
        assert_eq!(font, &crate::fonts::TextFont::from("medium"));
        assert_eq!(*size, 42.);
        assert_eq!(*letter_spacing, 2.5);
        assert_eq!(color[0], 203 * 257);
        let expected = command_preview(&app);
        settle(&mut app, &ctx, &expected);
        assert_eq!(state(&app), before);
        canvas_action(&mut app, &ctx, "Apply text on canvas");
        wait_edit(&mut app);
        assert!(app.source_editor.is_none());
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
        assert!(e.resident_state().expect("resident fixture").redo.is_empty());
        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].source.as_deref(), Some(&draft));
        assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, depth);
        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), expected.export_png().unwrap());
        assert_eq!(
            e.resident_state().expect("resident fixture").document.layers[1].pixels.get16(1, 1),
            if depth == 16 {
                [12347, 33559, 51237, 65535]
            } else {
                [48 * 257, 131 * 257, 199 * 257, 65535]
            }
        );
        e.undo("human").unwrap();
        let restored = json!(e.resident_state().expect("resident fixture").document.layers);
        assert_eq!(restored, before["doc"]["layers"]);
        drop(e);
        file.unchanged();
    }
}

#[test]
fn text_field_drag_can_begin_in_the_workspace() {
    for depth in [8, 16] {
        let (mut app, ctx, file) = fixture(depth);
        text_tool(&mut app, &ctx);
        app.zoom = 0.65;
        draw(&mut app, &ctx, vec![], Default::default());
        let before = state(&app);
        let start = point(&app, [-12., 38.]);
        let end = point(&app, [142., 104.]);
        assert!(app.canvas_input_region.unwrap().0.contains(start));
        assert!(!app.view_rect.unwrap().contains(start));

        click(&mut app, &ctx, start);
        assert!(app.source_editor.is_none());
        begin(&mut app, &ctx, start);
        assert!(!app.source_editor.as_ref().unwrap().previewing());
        draw(&mut app, &ctx, vec![egui::Event::PointerMoved(end)], Default::default());
        assert!(app.source_editor.as_ref().unwrap().previewing());
        draw(&mut app, &ctx, vec![button(end, egui::PointerButton::Primary, false, Default::default())], Default::default());
        let editor = app.source_editor.as_ref().unwrap();
        assert!(editor.placement.is_none());
        assert!(editor.x < 0 && i64::from(editor.x) + i64::from(editor.source.width) > 0);
        let expected = command_preview(&app);
        settle(&mut app, &ctx, &expected);
        assert_eq!(state(&app), before);
        file.unchanged();
    }
}

#[test]
fn native_text_field_placement_uses_wide_frames_and_handles_admission_refusal() {
    use crate::engine::native::{worker::tests::Fixture, NativeDocument};

    for (depth, [width, height]) in [8, 16]
        .into_iter()
        .flat_map(|depth| [[16000, 128], [128, 16000]].map(|size| (depth, size)))
    {
        let fixture = Fixture::new();
        let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 8 * 1024 * 1024);
        let doc = NativeDocument::new_blank(
            width, height, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
        ).unwrap();
        let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
        app.tool = Tool::Text;
        let scale = 0.0625;
        let rect = Rect::from_min_size(Pos2::new(100., 20.), Vec2::new(width as f32, height as f32) * scale);
        let start = rect.min + Vec2::splat(16. * scale);
        let end = rect.max - Vec2::splat(16. * scale);
        let press = || button(start, egui::PointerButton::Primary, true, Default::default());
        let release = |at| button(at, egui::PointerButton::Primary, false, Default::default());
        let canvas = |app: &mut PeerBrush, events: Vec<egui::Event>| {
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400., 1100.))),
                events,
                ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                    let painter = ui.painter().clone();
                    app.text_canvas(ui, &doc, &response, &painter, rect, scale);
                });
            });
        };
        canvas(&mut app, vec![]);
        let pool = doc.budget();
        let remaining = pool.limit() - pool.used();
        let pressure = pool.reserve_bytes(remaining, remaining).unwrap();
        let before = app.message.clone();
        canvas(&mut app, vec![egui::Event::PointerMoved(start), press(), release(start)]);
        assert!(app.source_editor.is_none());
        assert_ne!(app.message, before, "The press must reach source admission");
        drop(pressure);

        canvas(&mut app, vec![press(), egui::Event::PointerMoved(end), release(end)]);
        let editor = app.source_editor.as_ref().unwrap();
        assert!(editor.placement.is_none());
        assert_eq!((editor.x, editor.y), (16, 16));
        assert_eq!([editor.source.width, editor.source.height], [width - 32, height - 32]);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
                ..Default::default()
            }, |ctx| app.source_window(ctx, &doc));
        }
        let editor = app.source_editor.as_ref().unwrap();
        assert_eq!([editor.source.width, editor.source.height], [width - 32, height - 32]);
        let command = editor.owned_command().unwrap();
        assert_eq!(command.value()["source"]["width"], width - 32);
        assert_eq!(command.value()["source"]["height"], height - 32);
        assert_eq!(app.native_source_preview.as_ref().unwrap().signature(), command.signature());
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
        drop((command, app, doc, runtime, connection));
    }
}

#[test]
fn native_paragraph_typing_and_resize_present_progress_and_submit_the_latest_draft() {
    use crate::engine::{authority::DocumentAuthority, native::{worker::tests::Fixture, NativeDocument}};

    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 32 * 1024 * 1024);
        let document = NativeDocument::new_blank(256, 160, depth, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
        app.set_preview_quality(false).unwrap();
        app.workspace.lock().unwrap().source_runtime = Some(runtime);
        app.shared.lock().unwrap().authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
        app.doc_snapshot = document.clone().into();
        app.select_content(document.metadata().layer(0).unwrap().id());
        app.tool = Tool::Text;
        let rect = Rect::from_min_size(egui::pos2(80., 60.), egui::vec2(256., 160.));
        app.view_rect = Some(rect);
        app.canvas_viewport = Some(rect);
        let project = app.project_id.clone();
        let canvas = |app: &mut PeerBrush, events| {
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)), events, ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                    let painter = ui.painter().clone();
                    app.text_canvas(ui, &document, &response, &painter, rect, 1.);
                });
            });
        };
        let finish_preview = |app: &mut PeerBrush| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while app.native_dispatch.pending() {
                app.request_native_canvas(&ctx, &project, &document);
                assert!(std::time::Instant::now() < deadline, "{}", app.message);
                std::thread::yield_now();
            }
        };
        let primary = |position, pressed| button(position, egui::PointerButton::Primary, pressed, Default::default());
        let start = rect.min + egui::vec2(12., 18.);
        let end = start + egui::vec2(150., 100.);
        canvas(&mut app, vec![]);
        canvas(&mut app, vec![egui::Event::PointerMoved(start), primary(start, true), primary(end, false)]);
        let editor = app.source_editor.as_ref().unwrap();
        assert_eq!([editor.source.width, editor.source.height], [150, 100]);
        assert!(matches!(editor.source.content, Content::Text { layout: crate::source::TextLayout::Paragraph, .. }));
        canvas(&mut app, vec![egui::Event::Text("One two three".into())]);
        assert_eq!(app.native_source_preview.as_ref().unwrap().value()["source"]["content"]["text"], "One two three");
        let first = app.capture_native_canvas(&ctx, &project, &document).unwrap().unwrap();
        app.queue_native_canvas(first, &ctx).unwrap();
        let generation = app.canvas_generation;
        let added = " four five six seven eight nine";
        canvas(&mut app, vec![egui::Event::Text(added.into())]);
        let latest = "One two three four five six seven eight nine";
        assert_eq!(app.native_source_preview.as_ref().unwrap().value()["source"]["content"]["text"], latest);
        finish_preview(&mut app);
        assert!(app.native_dispatch.gesture_deferred);
        assert_eq!(app.canvas_generation, generation + 1);

        let active = app.capture_native_canvas(&ctx, &project, &document).unwrap().unwrap();
        app.queue_native_canvas(active, &ctx).unwrap();
        finish_preview(&mut app);
        assert!(!app.canvas_capture_ready((&document).into()));
        let time = ctx.input(|input| input.time) + 0.3;
        let _ = ctx.run(egui::RawInput { time: Some(time), ..Default::default() }, |_| {});
        assert!(app.source_editor.as_mut().unwrap().finish_typing_preview(&ctx));
        let settled = app.capture_native_canvas(&ctx, &project, &document).unwrap().unwrap();
        app.queue_native_canvas(settled, &ctx).unwrap();
        finish_preview(&mut app);
        assert!(app.canvas_capture_ready((&document).into()));

        let inside_text = start + egui::vec2(30., 26.);
        canvas(&mut app, vec![egui::Event::PointerMoved(inside_text), primary(inside_text, true)]);
        canvas(&mut app, vec![primary(inside_text, false)]);
        let input = ctx.memory(|memory| memory.focused()).unwrap();
        let state = egui::TextEdit::load_state(&ctx, input).unwrap();
        let range = state.cursor.char_range().unwrap();
        assert_eq!(range.primary.index, range.secondary.index);
        assert!((1..latest.chars().count()).contains(&range.primary.index));

        let editor = app.source_editor.as_mut().unwrap();
        editor.error = Some("Text render refused".into());
        assert!(editor.owned_command().is_some());

        let handle = start + egui::vec2(150., 50.);
        canvas(&mut app, vec![egui::Event::PointerMoved(handle), primary(handle, true)]);
        canvas(&mut app, vec![egui::Event::PointerMoved(handle - egui::vec2(40., 0.))]);
        assert!(app.source_editor.as_ref().unwrap().error.is_none());
        assert_eq!(app.native_source_preview.as_ref().unwrap().value()["source"]["width"], 110);
        canvas(&mut app, vec![egui::Event::WindowFocused(false), egui::Event::WindowFocused(true), primary(handle - egui::vec2(40., 0.), false)]);
        assert_eq!(app.source_editor.as_ref().unwrap().source.width, 150);
        assert_eq!(app.native_source_preview.as_ref().unwrap().value()["source"]["width"], 150);
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
        canvas(&mut app, vec![egui::Event::PointerMoved(handle), primary(handle, true)]);
        canvas(&mut app, vec![egui::Event::PointerMoved(handle - egui::vec2(40., 0.))]);
        let first = app.capture_native_canvas(&ctx, &project, &document).unwrap().unwrap();
        app.queue_native_canvas(first, &ctx).unwrap();
        let generation = app.canvas_generation;

        canvas(&mut app, vec![egui::Event::PointerMoved(handle - egui::vec2(80., 0.))]);
        assert_eq!(app.native_source_preview.as_ref().unwrap().value()["source"]["width"], 70);
        finish_preview(&mut app);
        assert!(app.native_dispatch.gesture_deferred);
        assert_eq!(app.canvas_generation, generation + 1);
        assert!(!app.canvas_capture_ready((&document).into()));
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);

        let release = handle - egui::vec2(90., 0.);
        canvas(&mut app, vec![primary(release, false), egui::Event::PointerMoved(handle)]);
        assert_eq!(app.native_source_preview.as_ref().unwrap().value()["source"]["width"], 60);
        assert!(app.capture_native_canvas(&ctx, &project, &document).unwrap().is_some());
        let modifiers = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        let _ = ctx.run(egui::RawInput {
            events: vec![key_event(egui::Key::Enter, true, modifiers)], modifiers, ..Default::default()
        }, |ctx| app.source_window(ctx, &document));
        wait_edit(&mut app);
        assert!(app.source_editor.is_none(), "{}", app.message);
        let engine = app.shared.lock().unwrap();
        let doc = crate::engine::view::current(&engine);
        assert_eq!(doc.layers().find_map(|layer| layer.source()).unwrap().dimensions(), [60, 100]);
        assert_eq!(engine.history_lengths(), [1, 0]);
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
        let at = point(&app, [100., 80.]);
        click(&mut app, &ctx, at);
        let draft = app.source_editor.as_ref().unwrap().command();
        for (mouse, modifiers) in [
            (egui::PointerButton::Middle, egui::Modifiers::NONE),
            (egui::PointerButton::Secondary, egui::Modifiers::ALT),
        ] {
            let start = point(&app, [100., 80.]);
            let end = start + Vec2::new(25., 10.);
            let (pan, zoom) = (app.pan, app.zoom);
            draw(&mut app, &ctx, vec![egui::Event::PointerMoved(start),
                button(start, mouse, true, modifiers)], modifiers);
            draw(&mut app, &ctx, vec![egui::Event::PointerMoved(end)], modifiers);
            draw(&mut app, &ctx, vec![button(end, mouse, false, modifiers)], modifiers);
            draw(&mut app, &ctx, vec![], Default::default());
            if mouse == egui::PointerButton::Middle {
                assert_ne!(app.pan, pan);
            } else {
                assert_ne!(app.zoom, zoom);
            }
            let editor = app.source_editor.as_ref().unwrap();
            assert_eq!(editor.command(), draft);
            assert!(editor.has_canvas_focus(&ctx));
        }
        draw(&mut app, &ctx, vec![egui::Event::Text("Still editing".into())], Default::default());
        let Content::Text { text, .. } = &app.source_editor.as_ref().unwrap().source.content else {
            panic!("Text draft must remain editable after navigation");
        };
        assert_eq!(text, "Still editing");
        assert_eq!(state(&app), before);
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
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(256, 192, depth).unwrap();
                    e.resident_state_mut().expect("resident fixture").document.layers[0]
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
            layout: crate::source::TextLayout::Legacy,
            font: "regular".into(),
            size: 24.,
            line_height: 1.2,
            letter_spacing: 0.,
            align: "left".into(),
            color: [12501, 23003, 34507, 65535],
        },
    }
}

#[test]
fn explicit_shaped_layout_control_previews_native_pixels_caches_unchanged_validation_and_applies_once(
) {
    for depth in [8, 16] {
        let (mut app, ctx, file) = fixture(depth);
        let (id, _, local) = existing_field(&mut app);
        let before = state(&app);
        text_tool(&mut app, &ctx);
        let at = point(&app, local);
        click(&mut app, &ctx, at);
        assert_eq!(
            app.source_editor.as_ref().unwrap().layer.as_deref(),
            Some(id.as_str())
        );
        replace_text(&mut app, &ctx, "Existing", "office a\u{0301}");
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Ligatures and joining"));
        assert!(matches!(
            app.source_editor.as_ref().unwrap().source.content,
            Content::Text {
                layout: crate::source::TextLayout::Shaped,
                ..
            }
        ));
        let expected = command_preview(&app);
        settle(&mut app, &ctx, &expected);
        let runs = app.source_editor.as_ref().unwrap().validation_runs;
        assert_eq!(runs, 1);
        for _ in 0..8 {
            draw(&mut app, &ctx, vec![], Default::default());
        }
        assert_eq!(app.source_editor.as_ref().unwrap().validation_runs, runs);
        assert_eq!(state(&app), before);
        number(&mut app, &ctx, "Letter spacing", "0", "2.5");
        let expected = command_preview(&app);
        settle(&mut app, &ctx, &expected);
        assert_eq!(
            app.source_editor.as_ref().unwrap().validation_runs,
            runs + 1
        );
        assert_eq!(state(&app), before);
        let draft = app.source_editor.as_ref().unwrap().source.clone();
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Apply text"));
        wait_edit(&mut app);
        assert!(app.source_editor.is_none());
        {
            let mut engine = app.shared.lock().unwrap();
            let applied = engine
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .find(|layer| layer.id == id)
                .unwrap();
            assert_eq!(applied.source, Some(draft.into()));
            assert_eq!(applied.pixels.depth, depth);
            assert_eq!(
                engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                expected.export_png().unwrap()
            );
            assert_eq!(
                engine.resident_state().expect("resident fixture").undo.len(),
                before["undo"].as_array().unwrap().len() + 1
            );
            engine.undo("human").unwrap();
            assert_eq!(json!(engine.resident_state().expect("resident fixture").document.layers), before["doc"]["layers"]);
            engine.redo("human").unwrap();
            assert_eq!(
                engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                expected.export_png().unwrap()
            );
        }
        let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.selected = id;
        app.open_source(&current, "edit");
        draw(&mut app, &ctx, vec![], Default::default());
        let before_cancel = state(&app);
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Ligatures and joining"));
        canvas_action(&mut app, &ctx, "Cancel text on canvas");
        assert!(app.source_editor.is_none());
        assert_eq!(state(&app), before_cancel);
        file.unchanged();
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
    let group = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"source.add","source":source,"parent":group,"x":40,"y":40})],
        None,
        None,
        "Text fixture",
    )
    .unwrap();
    let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"transform","layer":id,"angle":90.,"mirror_x":true,"selection_only":false})],
        None,
        None,
        "Affine text fixture",
    )
    .unwrap();
    let layer = &e.resident_state().expect("resident fixture").document.layers[0];
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
fn affine_text_frame_handles_rotate_about_center_and_resize_from_the_opposite_edge() {
    for depth in [8, 16] {
        let (mut app, ctx, file) = fixture(depth);
        let (id, _, blank) = existing_field(&mut app);
        app.view.angle = 30.;
        app.view.mirror_h = true;
        text_tool(&mut app, &ctx);
        let at = point(&app, blank);
        click(&mut app, &ctx, at);
        let before = state(&app);
        let draft = app.source_editor.as_ref().unwrap();
        assert_eq!(draft.layer.as_deref(), Some(id.as_str()));
        let original = draft.source.clone();
        let origin = [draft.x as f32, draft.y as f32];
        let project = |matrix: [f32; 6], [x, y]: [f32; 2]| {
            let [a, b, c, d, tx, ty] = matrix;
            [origin[0] + a * x + c * y + tx, origin[1] + b * x + d * y + ty]
        };
        let press = point(&app, project(original.matrix, [1., 48.]));
        let motion = point(&app, project(original.matrix, [21., 48.]));
        let release = point(&app, project(original.matrix, [-11., 52.]));
        let primary = |position, pressed| button(position, egui::PointerButton::Primary, pressed, Default::default());
        let corner = point(&app, project(original.matrix, [180., 96.]));
        let shrink = point(&app, project(original.matrix, [139., 64.5]));
        draw(&mut app, &ctx, vec![
            egui::Event::PointerMoved(corner), primary(corner, true),
            button(shrink, egui::PointerButton::Primary, false, egui::Modifiers::SHIFT),
        ], Default::default());
        let proportional = &app.source_editor.as_ref().unwrap().source;
        assert_eq!([proportional.width, proportional.height], [135, 72]);
        assert_eq!(proportional.matrix, original.matrix);
        let shrunk_corner = point(&app, project(original.matrix, [135., 72.]));
        draw(&mut app, &ctx, vec![
            egui::Event::PointerMoved(shrunk_corner), primary(shrunk_corner, true), primary(corner, false),
        ], Default::default());
        assert_eq!(app.source_editor.as_ref().unwrap().source, original);
        draw(&mut app, &ctx, vec![egui::Event::PointerMoved(press), primary(press, true)], Default::default());
        draw(&mut app, &ctx, vec![egui::Event::PointerMoved(motion)], Default::default());
        assert_eq!(app.source_editor.as_ref().unwrap().source.width, 160);
        assert_eq!(state(&app), before);
        key(&mut app, &ctx, egui::Key::Escape, Default::default());
        draw(&mut app, &ctx, vec![primary(motion, false)], Default::default());
        assert_eq!(app.source_editor.as_ref().unwrap().source, original);

        let center = project(original.matrix, [90., 48.]);
        let edge = point(&app, project(original.matrix, [90., 96.]));
        let bottom = (edge - point(&app, center)).normalized();
        let rotate = edge + bottom * 19.;
        let center_screen = point(&app, center);
        let arm = rotate - center_screen;
        let angle = 32_f32.to_radians();
        let (sin, cos) = angle.sin_cos();
        let rotated = center_screen + Vec2::new(cos * arm.x - sin * arm.y, sin * arm.x + cos * arm.y);
        draw(&mut app, &ctx, vec![egui::Event::PointerMoved(rotate), primary(rotate, true)], Default::default());
        draw(&mut app, &ctx, vec![egui::Event::PointerMoved(rotated)], egui::Modifiers::SHIFT);
        let matrix = app.source_editor.as_ref().unwrap().source.matrix;
        let [a, b, ..] = original.matrix;
        let angle = 30_f32.to_radians() * if app.view.mirror_h { -1. } else { 1. };
        let (sin, cos) = angle.sin_cos();
        assert!((matrix[0] - (cos * a - sin * b)).abs() < 0.001);
        assert!((matrix[1] - (sin * a + cos * b)).abs() < 0.001);
        for (actual, expected) in project(matrix, [90., 48.]).into_iter().zip(center) {
            assert!((actual - expected).abs() < 0.001);
        }
        key(&mut app, &ctx, egui::Key::Escape, Default::default());
        draw(&mut app, &ctx, vec![primary(rotated, false)], Default::default());
        assert_eq!(app.source_editor.as_ref().unwrap().source, original);

        draw(&mut app, &ctx, vec![
            egui::Event::PointerMoved(press), primary(press, true),
            egui::Event::PointerMoved(motion), primary(release, false),
            egui::Event::PointerMoved(motion),
        ], Default::default());
        let resized = &app.source_editor.as_ref().unwrap().source;
        assert_eq!([resized.width, resized.height], [192, 96]);
        assert_eq!(resized.matrix[..4], original.matrix[..4]);
        let opposite_before = project(original.matrix, [180., 48.]);
        let opposite_after = project(resized.matrix, [192., 48.]);
        for axis in 0..2 {
            assert!((opposite_before[axis] - opposite_after[axis]).abs() < 0.001);
        }
        assert_eq!(state(&app), before);
        let expected = command_preview(&app);
        settle(&mut app, &ctx, &expected);
        let output = draw(&mut app, &ctx, vec![], Default::default());
        click(&mut app, &ctx, label(&output, "Apply text"));
        wait_edit(&mut app);
        assert!(app.source_editor.is_none());
        let mut engine = app.shared.lock().unwrap();
        let committed = engine.resident_state().unwrap();
        assert_eq!(committed.document.export_png().unwrap(), expected.export_png().unwrap());
        assert_eq!(committed.undo.len(), before["undo"].as_array().unwrap().len() + 1);
        engine.undo("human").unwrap();
        assert_eq!(json!(engine.resident_state().unwrap().document.layers), before["doc"]["layers"]);
        file.unchanged();
    }
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
        click(&mut app, &ctx, label(&output, "Apply text"));
        wait_edit(&mut app);
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
        click(&mut app, &ctx, label(&output, "Apply text"));
        wait_edit(&mut app);
        assert!(app.source_editor.is_none());
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            expected.export_png().unwrap()
        );
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(),
            before["undo"].as_array().unwrap().len() + 1
        );
        file.unchanged();
        for guard in ["hidden", "locked_layer", "locked_ancestor"] {
            let (mut app, ctx, file) = fixture(depth);
            let (id, group, blank) = existing_field(&mut app);
            {
                let mut e = app.shared.lock().unwrap();
                if guard == "locked_layer" {
                    e.resident_state_mut().expect("resident fixture").document
                        .layers
                        .iter_mut()
                        .find(|layer| layer.id == id)
                        .unwrap()
                        .locked = true;
                } else {
                    let group = e
                        .resident_state_mut().expect("resident fixture").document
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
            let lower = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            e.edit("human",&[json!({"op":"source.add","source":text_source(),"name":"Upper text","x":40,"y":40})],None,None,"Upper text fixture").unwrap();
            let top = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
            e.edit("human",&[json!({"op":"source.add","source":text_source(),"parent":lower,"name":"Lower child","x":40,"y":40})],None,None,"Child text fixture").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].parent.as_deref(), Some(lower.as_str()));
            assert_eq!(e.resident_state().expect("resident fixture").document.layers[1].id, top);
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

#[test]
fn pending_source_apply_preserves_its_draft_until_real_terminal_completion() {
    for (depth, cancel) in [8, 16]
        .into_iter()
        .flat_map(|depth| [false, true].map(|cancel| (depth, cancel)))
    {
        let (mut app, ctx, file) = fixture(depth);
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.open_source(&doc, "text");
        let token = app.source_editor.as_ref().unwrap().token();
        let draft = app.source_editor.as_ref().unwrap().command();
        let project = app.project_id.clone();
        let shared = app.shared.clone();
        let guard = shared.lock().unwrap();
        let revision = guard.resident_state().expect("resident fixture").document.revision;
        let history = guard.resident_state().expect("resident fixture").undo.len();
        let render = |app: &mut PeerBrush, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
                    events,
                    ..Default::default()
                },
                |ctx| app.source_window(ctx, &doc),
            )
        };
        // egui uses the first frame of a new window to measure its contents.
        render(&mut app, vec![]);
        let output = render(&mut app, vec![]);
        let at = label(&output, "Apply");
        for pressed in [true, false] {
            render(
                &mut app,
                vec![
                    egui::Event::PointerMoved(at),
                    button(
                        at,
                        egui::PointerButton::Primary,
                        pressed,
                        Default::default(),
                    ),
                ],
            );
        }
        assert!(app.edit_worker.pending());
        assert_eq!(guard.resident_state().expect("resident fixture").document.revision, revision);
        assert_eq!(guard.resident_state().expect("resident fixture").undo.len(), history);
        app.cancel_source();
        assert_eq!(app.source_editor.as_ref().unwrap().token(), token);
        app.open_source(&doc, "ellipse");
        assert_eq!(app.source_editor.as_ref().unwrap().command(), draft);
        app.switch_project("another-project");
        assert_eq!(app.project_id, project);
        app.request_close(lifecycle::Intent::Exit);
        assert!(app.lifecycle.is_none());
        assert!(!app.exit_approved);
        app.source_edit_finished_nonce(token.nonce().wrapping_add(1), Ok(()));
        assert_eq!(app.source_editor.as_ref().unwrap().token(), token);
        if cancel {
            app.cancel_pending_edit();
            assert!(app.message.starts_with("Cancelling"));
        }
        drop(guard);
        wait_edit(&mut app);
        if cancel {
            assert_eq!(app.source_editor.as_ref().unwrap().command(), draft);
            let engine = shared.lock().unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.revision, revision);
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history);
            assert!(app.message.contains("cancelled"), "{}", app.message);
            drop(engine);
            app.cancel_source();
            assert!(app.source_editor.is_none());
            file.unchanged();
            continue;
        }
        assert!(app.source_editor.is_none(), "{}", app.message);
        let mut engine = shared.lock().unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.revision, revision + 1);
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history + 1);
        let id = app.selected.clone();
        assert!(engine
            .resident_state().expect("resident fixture").document
            .layers
            .iter()
            .any(|layer| layer.id == id && layer.source.is_some()));
        engine.undo("human").unwrap();
        assert!(engine.resident_state().expect("resident fixture").document.layers.iter().all(|layer| layer.id != id));
        engine.redo("human").unwrap();
        assert!(engine
            .resident_state().expect("resident fixture").document
            .layers
            .iter()
            .any(|layer| layer.id == id && layer.source.is_some()));
        drop(engine);
        file.unchanged();
    }
}
