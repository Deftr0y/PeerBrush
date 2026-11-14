use super::*;
use crate::{engine::native::Cancellation, raster::CanvasLimits};

type SizeReply = mpsc::Receiver<Result<[u32; 2], String>>;

fn dimensions(
    ui: &mut egui::Ui,
    width: &mut u32,
    height: &mut u32,
    edge: u32,
) -> [egui::Response; 2] {
    ui.horizontal(|ui| {
        [(width, "Width"), (height, "Height")].map(|(value, label)| {
            ui.horizontal(|ui| {
                let name = ui.label(label);
                let response = controls::numeric(ui, value, 1..=edge).labelled_by(name.id);
                ui.label("px");
                response
            }).inner
        })
    }).inner
}

fn channel_depth(ui: &mut egui::Ui, depth: &mut u16) {
    ui.horizontal_wrapped(|ui| {
        controls::label(ui, "Channel depth");
        for (value, icon, label, help) in [
            (8, Icon::ToneSteps, "8 bit", "256 values per channel"),
            (16, Icon::ToneRamp, "16 bit", "65,536 values per channel for finer tonal adjustments"),
        ] {
            if icons::labeled_choice(ui, icon, label, Some(*depth == value), 0.)
                .on_hover_text(help)
                .clicked()
            {
                *depth = value;
            }
        }
    });
}

pub(super) struct ProjectDimensions {
    document: String,
    revision: u64,
    pub(super) values: (u32, u32, u16),
}

impl ProjectDimensions {
    pub(super) fn new(doc: DocumentView<'_>) -> Self {
        Self {
            document: doc.id().into(),
            revision: doc.revision(),
            values: (doc.width(), doc.height(), doc.depth()),
        }
    }
}

struct PendingSize {
    reply: SizeReply,
    cancel: Cancellation,
}
impl Drop for PendingSize {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Default)]
pub(super) struct Suggestion {
    pending: Option<PendingSize>,
    edited: bool,
    message: String,
}
impl Suggestion {
    fn from_request(request: Result<SizeReply, String>, cancel: Cancellation) -> Self {
        match request {
            Ok(reply) => Self {
                pending: Some(PendingSize { reply, cancel }),
                ..Default::default()
            },
            Err(error) => Self {
                message: format!("Clipboard size unavailable: {error}"),
                ..Default::default()
            },
        }
    }
    fn manual_edit(&mut self) {
        self.edited = true;
        self.pending = None;
        self.message.clear();
    }
    fn poll(&mut self, width: &mut u32, height: &mut u32, limits: CanvasLimits) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        let result = match pending.reply.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err("The clipboard is unavailable".into()),
        };
        self.pending = None;
        if self.edited {
            return false;
        }
        match result.and_then(|size| limits.check(size[0], size[1]).map(|_| size)) {
            Ok([w, h]) => {
                *width = w;
                *height = h;
                self.message = "Size suggested from the clipboard image.".into();
            }
            Err(error) => self.message = format!("Clipboard size unavailable: {error}"),
        }
        true
    }
}
impl PeerBrush {
    pub(super) fn project_dimensions_window(&mut self, ctx: &egui::Context, doc: DocumentView<'_>) {
        let Some(mut draft) = self.project_settings.take() else { return; };
        let limits = if doc.native().is_some() {
            CanvasLimits::Native
        } else {
            CanvasLimits::Resident
        };
        let stale = draft.document != doc.id() || draft.revision != doc.revision();
        let can_apply = !stale && !doc.read_only() && !self.busy && !self.edit_worker.pending();
        let (w, h, depth) = &mut draft.values;
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Project dimensions and depth")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                dialogs::body(ui, 430.);
                ui.add_enabled_ui(!stale && !doc.read_only(), |ui| {
                    dimensions(ui, w, h, limits.edge());
                    channel_depth(ui, depth);
                });
                ui.label("Canvas size preserves layer pixels and positions.");
                if *depth == 8 && doc.depth() == 16 {
                    ui.colored_label(ACCENT, "Converting to 8 bit quantizes the channels. Undo restores 16 bit.");
                }
                let size = limits.check(*w, *h);
                if let Err(error) = &size {
                    ui.colored_label(ACCENT, error);
                }
                if stale {
                    ui.colored_label(ACCENT, "The project changed. Reopen these settings to edit the current version.");
                } else if doc.read_only() {
                    ui.label("This project is read-only.");
                }
                apply = ui.add_enabled(
                    size.is_ok() && can_apply,
                    dialogs::primary("Apply dimensions and depth"),
                ).clicked();
            });
        if apply {
            let (w, h, depth) = draft.values;
            self.cancel_free_transform();
            self.close_proposal();
            self.apply_action(
                vec![json!({"op":"document.settings","width":w,"height":h,"bit_depth":depth})],
                "Project dimensions and depth",
                Some((&draft.document, draft.revision)),
                edit_job::Origin::Action,
            );
            self.frame_pending = true;
        } else if open {
            self.project_settings = Some(draft);
        }
    }

    pub(super) fn open_new_canvas(&mut self) {
        self.new_size.pending = None;
        let cancel = Cancellation::default();
        self.new_size = Suggestion::from_request(
            self.clipboard.image_dimensions_until(cancel.clone()),
            cancel,
        );
        self.show_new = true;
        self.lifecycle_frame = true;
    }
    pub(super) fn new_canvas_window(&mut self, ctx: &egui::Context) {
        if !self.show_new {
            return;
        }
        let configured = self.workspace.lock().unwrap().source_runtime.is_some();
        let limits = if configured {
            CanvasLimits::Native
        } else {
            CanvasLimits::Resident
        };
        let edge = limits.edge();
        let mut open = true;
        egui::Window::new("New canvas")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                dialogs::body(ui, 430.);
                let waiting = self.new_size.pending.is_some();
                let manual = dimensions(ui, &mut self.new_width, &mut self.new_height, edge)
                    .iter()
                    .any(|response| {
                        response.changed()
                            || response.has_focus()
                            || response.dragged()
                            || response.is_pointer_button_down_on()
                    });
                if manual {
                    self.new_size.manual_edit();
                }
                if self
                    .new_size
                    .poll(&mut self.new_width, &mut self.new_height, limits)
                {
                    ctx.request_repaint();
                }
                if self.new_size.pending.is_some() {
                    ui.label(RichText::new("Reading clipboard image size...").color(MUTED));
                } else if !self.new_size.message.is_empty() {
                    ui.label(RichText::new(&self.new_size.message).color(MUTED));
                }
                channel_depth(ui, &mut self.new_bit_depth);
                ui.label("Opens in a new project tab.");
                let size = limits.check(self.new_width, self.new_height);
                if let Err(error) = &size {
                    ui.label(RichText::new(error).color(ACCENT));
                }
                // Present received dimensions before accepting them for creation.
                let ready = (!waiting || manual) && size.is_ok() && !self.busy;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(ready, dialogs::primary("Create canvas"))
                        .clicked()
                    {
                        let source = self.shared.clone();
                        let initiating = self.project_id.clone();
                        let (width, height, depth) =
                            (self.new_width, self.new_height, self.new_bit_depth);
                        if configured || source.lock().unwrap().native_document().is_some() {
                            self.show_new = false;
                            self.job(move || {
                                crate::workspace::new_project(
                                    &source,
                                    width,
                                    height,
                                    depth,
                                    Some(&initiating),
                                )?;
                                Ok("Canvas created".into())
                            });
                            return;
                        }
                        let result = crate::workspace::new_project(
                            &source,
                            self.new_width,
                            self.new_height,
                            self.new_bit_depth,
                            Some(&initiating),
                        );
                        match result {
                            Ok(_) => {
                                self.texture = None;
                                self.canvas_pixels = None;
                                self.interrupt_animation();
                                self.show_new = false;
                                self.pan = Vec2::ZERO;
                                self.zoom = 1.0;
                                self.view = view::View::default();
                                self.last_preview = None;
                            }
                            Err(error) => self.message = error.into(),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_new = false;
                    }
                });
            });
        if !open || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.show_new = false;
        }
        if !self.show_new {
            self.new_size.pending = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::workspace_tests::{click, draw, fixture, key, label, source};
    use super::*;
    fn pending() -> (mpsc::Sender<Result<[u32; 2], String>>, Suggestion) {
        let (tx, rx) = mpsc::channel();
        (tx, Suggestion::from_request(Ok(rx), Cancellation::default()))
    }
    fn capture_readiness(app: &PeerBrush, ctx: &egui::Context, painted: u64) -> String {
        let time = ctx.input(|input| input.time);
        let (id, revision) = {
            let engine = app.shared.lock().unwrap();
            let document = crate::engine::view::current(&engine);
            (document.id().to_owned(), document.revision())
        };
        format!(
            "pending={}, exports_ready={}, painted_current={}, delay_elapsed={}, fading={}, progress={}, preview_current={}, preview_matches_requested={}",
            app.pending,
            app.exports_capture_ready(),
            painted == app.canvas_generation,
            time >= app.capture_ready_at,
            app.animation.fading(),
            app.animation.progress(time),
            app.canvas_preview.as_ref().is_some_and(|key| key.0 == id && key.1 == revision),
            app.canvas_preview.as_ref() == app.last_preview.as_ref(),
        )
    }
    fn settle_capture_preview(app: &mut PeerBrush, ctx: &egui::Context, size: Vec2) {
        for _ in 0..200 {
            let painted = app.canvas_generation;
            draw(app, ctx, size, vec![]);
            let (id, revision) = {
                let engine = app.shared.lock().unwrap();
                let document = crate::engine::view::current(&engine);
                (document.id().to_owned(), document.revision())
            };
            if !app.pending
                && app.exports_capture_ready()
                && painted == app.canvas_generation
                && !app.animation.fading()
                && app.animation.progress(ctx.input(|input| input.time)) >= 1.
                && app.canvas_preview.as_ref().is_some_and(|key| {
                    key.0 == id && key.1 == revision && app.last_preview.as_ref() == Some(key)
                })
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!(
            "Initial painted preview did not settle: {}",
            capture_readiness(app, ctx, app.canvas_generation)
        );
    }
    #[test]
    fn new_canvas_capture_queues_screenshot_while_dialog_stays_open_and_sources_unchanged() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture(depth);
            let size = Vec2::new(980., 640.);
            settle_capture_preview(&mut app, &ctx, size);
            let before = source(&app);
            app.open_new_canvas();
            let path = std::env::temp_dir().join(format!(
                "peerbrush-new-canvas-capture-{}.png",
                crate::engine::id()
            ));
            {
                let mut root = app.workspace_root.lock().unwrap();
                root.capture_ui = Some(path.clone());
                root.capture_panel = Some("workspace".into());
            }
            let mut requested = false;
            let mut last_readiness = String::new();
            for _ in 0..100 {
                let painted = app.canvas_generation;
                let output = draw(&mut app, &ctx, size, vec![]);
                assert!(app.show_new, "Capture must not dismiss the dialog");
                assert_eq!(source(&app), before);
                last_readiness = capture_readiness(&app, &ctx, painted);
                if output
                    .viewport_output
                    .values()
                    .flat_map(|viewport| &viewport.commands)
                    .any(|command| matches!(command, egui::ViewportCommand::Screenshot(_)))
                {
                    requested = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                requested,
                "Workspace capture did not request the open dialog screenshot: {last_readiness}"
            );
            assert!(app.workspace_root.lock().unwrap().capture_ui.is_none());
            assert!(!path.exists(), "A command is not a fabricated screenshot");
        }
    }
    #[test]
    fn new_canvas_capture_idle_dialog_keeps_polling_for_later_requests() {
        for depth in [8, 16] {
            let (mut app, ctx) = fixture(depth);
            let size = Vec2::new(980., 640.);
            settle_capture_preview(&mut app, &ctx, size);
            // The header's real-time arrival animation otherwise supplies
            // repaint requests during this short synthetic frame sequence.
            app.activity_changed = std::time::Instant::now() - Duration::from_secs(1);
            let before = source(&app);
            app.open_new_canvas();
            for _ in 0..20 {
                draw(&mut app, &ctx, size, vec![]);
                if app.new_size.pending.is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(app.new_size.pending.is_none());
            for _ in 0..10 {
                let painted = app.canvas_generation;
                let output = draw(&mut app, &ctx, size, vec![]);
                assert!(app.show_new);
                assert_eq!(source(&app), before);
                let repaint = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
                assert!(
                    repaint <= Duration::from_millis(200),
                    "Idle dialog stops polling workspace capture requests: repaint={repaint:?}; {}",
                    capture_readiness(&app, &ctx, painted)
                );
            }
        }
    }
    #[test]
    fn dimensions_are_read_once_and_manual_edits_reject_late_results() {
        let (tx, mut suggestion) = pending();
        let (mut width, mut height) = (1024, 768);
        assert!(!suggestion.poll(&mut width, &mut height, CanvasLimits::Resident));
        tx.send(Ok([641, 479])).unwrap();
        assert!(suggestion.poll(&mut width, &mut height, CanvasLimits::Resident));
        assert_eq!((width, height), (641, 479));
        tx.send(Ok([100, 200])).unwrap_err();
        let (tx, mut next) = pending();
        let cancel = next.pending.as_ref().unwrap().cancel.clone();
        next.manual_edit();
        assert!(cancel.check().is_err());
        tx.send(Ok([900, 600])).unwrap_err();
        assert!(!next.poll(&mut width, &mut height, CanvasLimits::Resident));
        assert_eq!((width, height), (641, 479));
    }
    #[test]
    fn unsupported_and_unavailable_images_preserve_defaults_and_explain_failure() {
        for result in [
            Ok([0, 10]),
            Ok([8193, 20]),
            Ok([8192, 8192]),
            Err("Clipboard unavailable".into()),
        ] {
            let (tx, mut suggestion) = pending();
            let (mut width, mut height) = (1024, 768);
            tx.send(result).unwrap();
            assert!(suggestion.poll(&mut width, &mut height, CanvasLimits::Resident));
            assert_eq!((width, height), (1024, 768));
            assert!(!suggestion.message.is_empty());
        }
        let (tx, mut suggestion) = pending();
        drop(tx);
        assert!(suggestion.poll(&mut 1024, &mut 768, CanvasLimits::Resident));
    }

    #[test]
    fn actual_file_and_tab_openings_read_new_clipboard_content_without_pasting() {
        for depth in [8, 16] {
            let size = Vec2::new(980., 640.);
            let (mut app, ctx) = fixture(depth);
            let image = |width, height| {
                Ok(crate::clipboard::Image {
                    width,
                    height,
                    bytes: vec![0; width as usize * height as usize * 4],
                    samples16: None,
                    origin: None,
                })
            };
            app.clipboard = crate::clipboard::Worker::memory_images(
                ctx.clone(),
                vec![
                    image(73, 41),
                    image(91, 63),
                    Err("no readable image".into()),
                    Err("clipboard read denied".into()),
                ],
            );
            let before = source(&app);
            let projects = crate::workspace::handles(&app.workspace_root).len();
            for (index, expected) in [[73, 41], [91, 63], [91, 63], [91, 63]]
                .into_iter()
                .enumerate()
            {
                for _ in 0..3 {
                    draw(&mut app, &ctx, size, vec![]);
                }
                let output = draw(&mut app, &ctx, size, vec![]);
                if index == 0 {
                    click(&mut app, &ctx, size, label(&output, "File"));
                    let output = draw(&mut app, &ctx, size, vec![]);
                    click(&mut app, &ctx, size, label(&output, "New canvas"));
                } else {
                    click(&mut app, &ctx, size, label(&output, "+"));
                }
                assert!(app.show_new);
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while app.new_size.pending.is_some() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Clipboard worker reply timed out"
                    );
                    draw(&mut app, &ctx, size, vec![]);
                    std::thread::sleep(Duration::from_millis(1));
                }
                let output = draw(&mut app, &ctx, size, vec![]);
                assert_eq!([app.new_width, app.new_height], expected);
                if index >= 2 {
                    assert!(app.new_size.message.contains("unavailable"));
                }
                click(&mut app, &ctx, size, label(&output, "Cancel"));
                assert!(!app.show_new);
                assert_eq!(
                    crate::workspace::handles(&app.workspace_root).len(),
                    projects
                );
                assert_eq!(source(&app), before);
            }
        }
    }

    #[test]
    fn tab_opening_frame_blocks_simultaneous_delete_and_undo() {
        for depth in [8, 16] {
            for undo in [false, true] {
                let size = Vec2::new(1360., 900.);
                let (mut app, ctx) = fixture(depth);
                for _ in 0..3 {
                    draw(&mut app, &ctx, size, vec![]);
                }
                let output = draw(&mut app, &ctx, size, vec![]);
                let plus = label(&output, "+");
                let before = source(&app);
                let projects = crate::workspace::handles(&app.workspace_root).len();
                draw(
                    &mut app,
                    &ctx,
                    size,
                    vec![
                        egui::Event::PointerMoved(plus),
                        egui::Event::PointerButton {
                            pos: plus,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: Default::default(),
                        },
                    ],
                );
                let modifiers = if undo {
                    egui::Modifiers {
                        command: true,
                        ctrl: !cfg!(target_os = "macos"),
                        mac_cmd: cfg!(target_os = "macos"),
                        ..Default::default()
                    }
                } else {
                    Default::default()
                };
                draw(
                    &mut app,
                    &ctx,
                    size,
                    vec![
                        egui::Event::PointerMoved(plus),
                        egui::Event::PointerButton {
                            pos: plus,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: Default::default(),
                        },
                        egui::Event::Key {
                            key: if undo {
                                egui::Key::Z
                            } else {
                                egui::Key::Delete
                            },
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers,
                        },
                    ],
                );
                assert!(
                    app.show_new,
                    "Actual tab action must open the dialog in this frame"
                );
                assert!(
                    app.lifecycle_frame,
                    "Opening must claim this frame before global actions"
                );
                assert_eq!(
                    crate::workspace::handles(&app.workspace_root).len(),
                    projects
                );
                assert_eq!(source(&app), before);
            }
        }
    }

    #[test]
    fn full_draw_owns_keys_and_canvas_through_numeric_edits_late_reply_and_cancellation() {
        for depth in [8, 16] {
            for dismissal in ["Cancel", "X", "Escape"] {
                let size = Vec2::new(1360., 900.);
                let (mut app, ctx) = fixture(depth);
                let before = source(&app);
                let projects = crate::workspace::handles(&app.workspace_root).len();
                let (late, suggestion) = pending();
                app.new_size = suggestion;
                app.show_new = true;
                for _ in 0..3 {
                    draw(&mut app, &ctx, size, vec![]);
                }
                // No text control owns focus: these keys would otherwise delete,
                // undo or switch the underlying document's current tool.
                let command = egui::Modifiers {
                    command: true,
                    ctrl: !cfg!(target_os = "macos"),
                    mac_cmd: cfg!(target_os = "macos"),
                    ..Default::default()
                };
                let tool = app.tool;
                key(&mut app, &ctx, size, egui::Key::Delete, Default::default());
                key(&mut app, &ctx, size, egui::Key::Z, command);
                key(&mut app, &ctx, size, egui::Key::W, Default::default());
                assert!(app.tool == tool);
                assert_eq!(source(&app), before);
                let output = draw(&mut app, &ctx, size, vec![]);
                let width = label(&output, "1024");
                click(&mut app, &ctx, size, width);
                click(&mut app, &ctx, size, width);
                assert!(
                    ctx.wants_keyboard_input(),
                    "Actual numeric editing must receive focus"
                );
                assert!(
                    late.send(Ok([600, 400])).is_err(),
                    "Manual focus discards late suggestions"
                );
                key(&mut app, &ctx, size, egui::Key::A, command);
                draw(&mut app, &ctx, size, vec![egui::Event::Text("47".into())]);
                key(&mut app, &ctx, size, egui::Key::Enter, Default::default());
                assert_eq!([app.new_width, app.new_height], [47, 768]);
                assert!(app.show_new);
                assert_eq!(source(&app), before);
                // Keep a live human draft and a newly delayed reply at dismissal.
                let (late, suggestion) = pending();
                let cancel = suggestion.pending.as_ref().unwrap().cancel.clone();
                app.new_size = suggestion;
                app.live_gesture = Some("unfinished human gesture".into());
                let output = draw(&mut app, &ctx, size, vec![]);
                match dismissal {
                    "Cancel" => click(&mut app, &ctx, size, label(&output, "Cancel")),
                    "Escape" => key(&mut app, &ctx, size, egui::Key::Escape, Default::default()),
                    _ => {
                        let title = label(&output, "New canvas");
                        let close = output
                            .shapes
                            .iter()
                            .find_map(|shape| match &shape.shape {
                                egui::Shape::LineSegment { points, .. } => {
                                    let center = points[0] + (points[1] - points[0]) * 0.5;
                                    (center.x > title.x
                                        && (center.y - title.y).abs() < 2.
                                        && (points[1].x - points[0].x).abs() > 5.
                                        && (points[1].y - points[0].y).abs() > 5.)
                                        .then_some(center)
                                }
                                _ => None,
                            })
                            .expect("Actual window close icon");
                        click(&mut app, &ctx, size, close);
                    }
                }
                assert!(!app.show_new);
                assert!(app.new_size.pending.is_none());
                assert!(cancel.check().is_err());
                assert!(late.send(Ok([900, 600])).is_err());
                assert_eq!(
                    app.live_gesture.as_deref(),
                    Some("unfinished human gesture")
                );
                assert_eq!([app.new_width, app.new_height], [47, 768]);
                assert_eq!(
                    crate::workspace::handles(&app.workspace_root).len(),
                    projects
                );
                assert_eq!(source(&app), before);
            }
        }
    }

    #[test]
    fn native_dialog_creates_only_after_confirming_visible_suggested_dimensions() {
        for (size, depth) in [
            (Vec2::new(980., 640.), 8),
            (Vec2::new(980., 640.), 16),
            (Vec2::new(1360., 900.), 8),
            (Vec2::new(1360., 900.), 16),
        ] {
            let (mut app, ctx) = super::super::tests::fixture();
            let before = serde_json::to_value(
                &app.shared
                    .lock()
                    .unwrap()
                    .resident_state()
                    .expect("resident fixture")
                    .document,
            )
            .unwrap();
            let initial = crate::workspace::handles(&app.workspace_root).len();
            let (tx, suggestion) = pending();
            app.new_size = suggestion;
            app.show_new = true;
            app.new_bit_depth = depth;
            let draw = |app: &mut PeerBrush, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.new_canvas_window(ctx),
                )
            };
            let position = |out: &egui::FullOutput, label: &str| {
                out.shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == label => {
                            Some(text.pos + text.galley.size() * 0.5)
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("Missing dialog action: {label}"))
            };
            let click = |app: &mut PeerBrush, at: Pos2| {
                for pressed in [true, false] {
                    draw(
                        app,
                        vec![
                            egui::Event::PointerMoved(at),
                            egui::Event::PointerButton {
                                pos: at,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: Default::default(),
                            },
                        ],
                    );
                }
            };
            draw(&mut app, vec![]);
            draw(&mut app, vec![]);
            let out = draw(&mut app, vec![]);
            click(&mut app, position(&out, "Create canvas"));
            assert!(app.show_new);
            assert_eq!(
                crate::workspace::handles(&app.workspace_root).len(),
                initial
            );
            tx.send(Ok([321, 123])).unwrap();
            draw(&mut app, vec![]);
            let out = draw(&mut app, vec![]);
            assert_eq!((app.new_width, app.new_height), (321, 123));
            click(&mut app, position(&out, "Create canvas"));
            assert!(!app.show_new);
            assert_eq!(
                crate::workspace::handles(&app.workspace_root).len(),
                initial + 1
            );
            let created = crate::workspace::active(&app.workspace_root).unwrap();
            let created = created.lock().unwrap();
            assert_eq!(
                (
                    created
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .width,
                    created
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .height,
                    created
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .bit_depth
                ),
                (321, 123, depth)
            );
            assert_eq!(
                created
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers
                    .len(),
                1
            );
            assert_eq!(
                created
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers[0]
                    .pixels
                    .bytes(),
                0
            );
            assert!(created
                .resident_state()
                .expect("resident fixture")
                .undo
                .is_empty());
            drop(created);
            assert_eq!(
                serde_json::to_value(
                    &app.shared
                        .lock()
                        .unwrap()
                        .resident_state()
                        .expect("resident fixture")
                        .document
                )
                .unwrap(),
                before
            );
            let (tx, suggestion) = pending();
            app.new_size = suggestion;
            app.show_new = true;
            draw(&mut app, vec![]);
            let out = draw(&mut app, vec![]);
            click(&mut app, position(&out, "Cancel"));
            assert!(!app.show_new);
            assert!(tx.send(Ok([600, 400])).is_err());
            assert_eq!(
                crate::workspace::handles(&app.workspace_root).len(),
                initial + 1
            );
        }
    }
}
