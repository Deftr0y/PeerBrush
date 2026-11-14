//! Document-bound, non-destructive filter browsing. Preview buffers never enter history.
use super::effects::{effect_icon, menu_effect};
use super::*;
use crate::filter_library::{Preset, THUMBNAIL_SIZE};
use std::{
    collections::VecDeque,
    hash::{BuildHasher, RandomState},
};

const THUMBNAIL_LIMIT: usize = 64;

#[derive(Clone, Copy)]
enum PhotoJob {
    Preset(u64),
    Draft { key: u64, generation: u64 },
}

type PhotoReply = (PhotoJob, Result<Vec<u8>, String>);

fn reference_texture(ctx: &egui::Context) -> Result<TextureHandle, String> {
    let id = egui::Id::new("unfiltered reference photo");
    if let Some(texture) = ctx.data(|data| data.get_temp::<TextureHandle>(id)) {
        return Ok(texture);
    }
    let rgba = crate::filter_library::reference_thumbnail()?;
    let texture = ctx.load_texture(
        "Unfiltered reference photo",
        egui::ColorImage::from_rgba_unmultiplied(THUMBNAIL_SIZE.map(|n| n as usize), &rgba),
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    Ok(texture)
}

fn compare_photo(
    ui: &mut egui::Ui,
    result_label: &str,
    preview: Option<&Result<TextureHandle, String>>,
    pending_photo: Option<&TextureHandle>,
    max_width: f32,
) {
    let reference = reference_texture(ui.ctx());
    let available = ui.available_width();
    let spacing = ui.spacing().item_spacing.x;
    let width = ((available - spacing) / 2.).clamp(1., max_width);
    let size = Vec2::new(width, width * THUMBNAIL_SIZE[1] as f32 / THUMBNAIL_SIZE[0] as f32);
    let caption_height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(Vec2::new(2. * width + spacing, caption_height + size.y),
        egui::Layout::left_to_right(egui::Align::Min), |ui| {
        let filtered = if preview.is_none() && pending_photo.is_some() {
            "Updating preview…"
        } else {
            result_label
        };
        for (label, photo, pending) in [
            ("Original photo", Some(&reference), None), (filtered, preview, pending_photo),
        ] {
            ui.allocate_ui_with_layout(Vec2::new(width, caption_height + size.y),
                egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(width);
                ui.add(egui::Label::new(label).truncate());
                match photo {
                    Some(Ok(texture)) => {
                        ui.add(egui::Image::new((texture.id(), size)).alt_text(label));
                    }
                    Some(Err(error)) => {
                        ui.add_sized(size, egui::Label::new("Preview unavailable").truncate())
                            .on_hover_text(error);
                    }
                    None => {
                        if let Some(texture) = pending {
                            ui.add(egui::Image::new((texture.id(), size)).alt_text("Previous filter result"));
                        } else {
                            ui.add_sized(size, egui::Label::new("Rendering preview…").truncate());
                        }
                    }
                }
            });
        }
    });
}

pub(super) struct Editor {
    pub document: String,
    pub revision: u64,
    native: Option<crate::engine::native::DocumentKey>,
    depth: u16,
    pub target: Option<String>,
    pub kind: String,
    pub settings: Value,
    pub weight: f32,
    pub preset: Option<String>,
    pub name: String,
    pub category: String,
    pub search: String,
    pub category_filter: String,
    pub original: bool,
    pub unfiltered: bool,
    pub error: Option<String>,
    apply_error: Option<String>,
    thumbnails: VecDeque<(u64, Result<TextureHandle, String>)>,
    draft_result: Option<(u64, Result<TextureHandle, String>)>,
    last_draft_photo: Option<TextureHandle>,
    thumbnail_keys: RandomState,
    thumbnail_pending: Option<u64>,
    draft_pending: Option<u64>,
    draft_generation: u64,
    draft_photo: bool,
    token: egui::Id,
    preview_drag: Option<egui::Id>,
    applying: Option<bool>,
    tx: mpsc::Sender<PhotoReply>,
    rx: mpsc::Receiver<PhotoReply>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::sync::Mutex;

    #[test]
    fn a_failed_thumbnail_allows_the_next_preset_to_render() {
        let ctx = egui::Context::default();
        let doc = Document::new_depth(12, 8, 16).unwrap();
        let mut editor = Editor::new(&doc);
        let valid = crate::filter_library::curated().remove(0);
        let mut invalid = valid.clone();
        invalid.kind = "invalid-test-filter".into();

        assert!(editor.thumbnail(&ctx, &invalid).is_none());
        let (job, result) = editor.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(result.is_err());
        editor.cache_thumbnail(&ctx, job, result);
        assert!(matches!(editor.thumbnail(&ctx, &invalid), Some(Err(_))));
        assert!(editor.thumbnail_pending.is_none());

        assert!(editor.thumbnail(&ctx, &valid).is_none());
        let (job, result) = editor.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(result.as_ref().unwrap().len(), 96 * 64 * 4);
        editor.cache_thumbnail(&ctx, job, result);
        assert!(matches!(editor.thumbnail(&ctx, &valid), Some(Ok(_))));
    }

    #[test]
    fn draft_photo_progress_does_not_wait_for_or_evict_preset_thumbnails() {
        let ctx = egui::Context::default();
        let doc = Document::new_depth(12, 8, 16).unwrap();
        let mut editor = Editor::new(&doc);
        let preset = crate::filter_library::curated().remove(0);
        let key = editor.photo_key(&preset.kind, &preset.settings, preset.weight);
        assert!(editor.thumbnail(&ctx, &preset).is_none());
        assert!(editor.draft_thumbnail(&ctx).is_none());
        assert!(editor.draft_pending.is_none());
        editor.weight = 0.5;
        assert!(editor.draft_thumbnail(&ctx).is_none());
        assert!(editor.thumbnail_pending.is_some() && editor.draft_pending.is_some());

        editor.weight = 0.25;
        for _ in 0..2 {
            let (job, result) = editor.rx.recv_timeout(Duration::from_secs(5)).unwrap();
            editor.cache_thumbnail(&ctx, job, result);
        }
        assert!(editor.last_draft_photo.is_none());
        assert!(editor.draft_result.is_none());
        assert!(editor.photo_index(key).is_some());

        assert!(editor.draft_thumbnail(&ctx).is_none());
        let (job, result) = editor.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        editor.cache_thumbnail(&ctx, job, result);
        assert!(matches!(editor.draft_thumbnail(&ctx), Some(Ok(_))));
        assert_eq!(editor.thumbnails.len(), 1);
        assert!(editor.photo_index(key).is_some());
    }

    fn fixture() -> (PeerBrush, egui::Context) {
        let ctx = egui::Context::default();
        let mut e = Engine::new();
        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(12, 8, 16).unwrap();
        e.resident_state_mut().expect("resident fixture").document.layers[0]
            .pixels
            .set16(2, 2, [12345, 23456, 45678, 65535]);
        let app = PeerBrush::init(
            &ctx,
            Arc::new(Mutex::new(e)),
            Connection {
                port: 0,
                token: String::new(),
                state_dir: std::env::temp_dir(),
                instance_lock: None,
                availability: Default::default(),
            },
        );
        (app, ctx)
    }
    fn draw<'a>(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let doc = doc.into();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                events,
                ..Default::default()
            },
            |ctx| app.filters_window(ctx, doc),
        )
    }
    #[test]
    fn held_filter_strength_coalesces_native_preview_and_applies_the_latest_draft() {
        use crate::engine::{authority::DocumentAuthority, native::{worker::tests::Fixture, NativeDocument}};

        for depth in [8, 16] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let document = NativeDocument::new_blank(
                64, 48, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
            ).unwrap();
            let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
            app.set_preview_quality(false).unwrap();
            app.workspace.lock().unwrap().source_runtime = Some(runtime);
            app.shared.lock().unwrap().authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
            app.doc_snapshot = document.clone().into();
            let project = app.project_id.clone();
            app.open_filters(&document);
            let editor = app.filter_editor.as_mut().unwrap();
            editor.kind = "posterize".into();
            editor.settings = json!({"levels":3});
            editor.weight = 0.5;
            draw(&mut app, &ctx, &document, vec![]);
            let output = draw(&mut app, &ctx, &document, vec![]);
            let start = super::super::workspace_tests::label(&output, "50%");
            let button = |pos, pressed| egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
            };
            draw(&mut app, &ctx, &document, vec![egui::Event::PointerMoved(start), button(start, true)]);
            draw(&mut app, &ctx, &document, vec![egui::Event::PointerMoved(start + egui::vec2(16., 0.))]);
            let first_weight = app.filter_editor.as_ref().unwrap().weight;
            assert!(app.filter_editor.as_ref().unwrap().pointer_preview_key(&ctx, (&document).into()).is_some());
            let first = app.capture_native_canvas(&ctx, &project, &document).unwrap().unwrap();
            app.queue_native_canvas(first, &ctx).unwrap();

            let end = start + egui::vec2(36., 0.);
            draw(&mut app, &ctx, &document, vec![egui::Event::PointerMoved(end)]);
            assert_ne!(app.filter_editor.as_ref().unwrap().weight, first_weight);
            assert!(app.capture_native_canvas(&ctx, &project, &document).unwrap().is_none());
            assert!(app.native_dispatch.gesture_deferred);
            assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);

            draw(&mut app, &ctx, &document, vec![button(end, false)]);
            let weight = app.filter_editor.as_ref().unwrap().weight;
            assert!(app.filter_editor.as_ref().unwrap().preview_drag.is_none());
            assert!(app.capture_native_canvas(&ctx, &project, &document).unwrap().is_some());
            app.apply_filter_editor(None);
            super::super::text_tests::wait_edit(&mut app);
            assert!(app.filter_editor.is_none(), "{}", app.message);
            let engine = app.shared.lock().unwrap();
            let doc = crate::engine::view::current(&engine);
            assert_eq!(doc.filter(0).unwrap().weight(), weight);
            assert_eq!(engine.history_lengths(), [1, 0]);
        }
    }
    #[test]
    fn browsing_strength_comparison_cancel_and_apply_use_one_native_engine_edit() {
        let (mut app, ctx) = fixture();
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.open_filters(&doc);
        let p = crate::filter_library::curated()
            .into_iter()
            .find(|p| p.id == "posterize")
            .unwrap();
        let e = app.filter_editor.as_mut().unwrap();
        e.choose(&p);
        e.weight = 0.5;
        e.settings = json!({"levels":3});
        draw(&mut app, &ctx, &doc, vec![]);
        let output = draw(&mut app, &ctx, &doc, vec![]);
        for label in [
            "Whole-image filters",
            "Posterize",
            "Levels",
            "50%",
            "Filtered",
            "Original",
        ] {
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text==label)),
                "Missing {label}"
            );
        }
        let preview = Engine::preview_edits(doc.clone(), &app.transient).unwrap();
        assert_ne!(preview.export_png().unwrap(), doc.export_png().unwrap());
        assert_eq!(preview.layers[0].pixels, doc.layers[0].pixels);
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        app.filter_editor.as_mut().unwrap().original = true;
        draw(&mut app, &ctx, &doc, vec![]);
        assert!(app.transient.is_empty());
        app.filter_editor.as_mut().unwrap().original = false;
        draw(&mut app, &ctx, &doc, vec![]);
        app.apply_filter_editor(None);
        assert!(app.filter_editor.is_none());
        let changed = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        assert_eq!(changed.export_png().unwrap(), preview.export_png().unwrap());
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        app.open_filters(&changed);
        app.filter_editor.as_mut().unwrap().unfiltered = true;
        draw(&mut app, &ctx, &changed, vec![]);
        let original = Engine::preview_edits(changed.clone(), &app.transient).unwrap();
        assert_eq!(original.export_png().unwrap(), doc.export_png().unwrap());
        draw(
            &mut app,
            &ctx,
            &changed,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!(app.filter_editor.is_none());
        assert!(app.transient.is_empty());
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            changed.export_png().unwrap()
        );
        app.shared.lock().unwrap().undo("human").unwrap();
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            doc.export_png().unwrap()
        );

        app.edit(vec![json!({"op":"filter.add","kind":"invert"})], "Add filter");
        let id = app.shared.lock().unwrap().resident().unwrap().filters[0].id.clone();
        app.edit(vec![json!({"op":"filter.update","filter":id,"enabled":false})], "Bypass filter");
        let bypassed = app.shared.lock().unwrap().resident().unwrap().clone();
        app.open_filters(&bypassed);
        app.filter_editor.as_mut().unwrap().weight = 0.5;
        draw(&mut app, &ctx, &bypassed, vec![]);
        let preview = Engine::preview_edits(bypassed.clone(), &app.transient).unwrap();
        assert_ne!(preview.export_png().unwrap(), bypassed.export_png().unwrap());
        app.apply_filter_editor(None);
        let engine = app.shared.lock().unwrap();
        let filter = &engine.resident().unwrap().filters[0];
        assert_eq!(filter.weight, 0.5);
        assert!(!filter.enabled);
    }
    #[test]
    fn stale_preview_tab_switch_locks_reservations_and_native_close_preserve_work() {
        let (mut app, ctx) = fixture();
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.open_filters(&doc);
        app.request_close(lifecycle::Intent::Exit);
        assert!(app.lifecycle.is_none());
        assert!(app.filter_editor.is_some());
        let id = doc.layers[0].id.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":id,"name":"Human"})],
                None,
                None,
                "Human",
            )
            .unwrap();
        app.apply_filter_editor(None);
        assert!(app.filter_editor.as_ref().unwrap().apply_error.is_some());
        app.pending = true;
        app.request_preview(&ctx, &doc);
        assert!(app.filter_editor.is_none());
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].name, "Human");
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.open_filters(&doc);
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.layers[0].locked = true;
        app.apply_filter_editor(None);
        assert!(app.filter_editor.as_ref().unwrap().apply_error.is_some());
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.layers[0].locked = false;
        app.shared
            .lock()
            .unwrap()
            .reserve(
                "other",
                "Reserved",
                vec![crate::engine::Scope {
                    target: Some(id),
                    rect: Some([0, 0, 1, 1]),
                }],
            )
            .unwrap();
        app.apply_filter_editor(None);
        assert!(app.filter_editor.as_ref().unwrap().apply_error.is_some());
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
        let other = crate::workspace::register_in(&app.workspace, Engine::new(), None).unwrap();
        let project = other.lock().unwrap().project_id.clone();
        app.switch_project(&project);
        assert!(app.filter_editor.is_none());
    }
    #[test]
    fn stack_strength_drag_renders_native_result_and_commits_once_on_release() {
        let (mut app, ctx) = fixture();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"filter.add","kind":"invert"})],
                None,
                None,
                "Invert",
            )
            .unwrap();
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let filter = before.filters[0].id.clone();
        let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
        let mut bar = Rect::NOTHING;
        let mut draw = |app: &mut PeerBrush, events: Vec<egui::Event>| {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 200.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let mut value = app.parameter_gesture.as_ref().map_or(100., |e| {
                            e.commands[0]["weight"].as_f64().unwrap() as f32 * 100.
                        });
                        let response = controls::range(
                            ui,
                            "filter strength test",
                            &mut value,
                            0.0..=100.0,
                            180.,
                            "%",
                            0,
                            false,
                        );
                        bar = response.rect;
                        if response.changed() {
                            app.layer_parameter(
                                json!({"filter":filter,"weight":value/100.}),
                                "Filter strength",
                                &response,
                            );
                        }
                    });
                },
            );
            bar
        };
        let pos = draw(&mut app, vec![]).center();
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        draw(
            &mut app,
            vec![egui::Event::PointerMoved(pos - Vec2::new(25., 0.))],
        );
        let commands = app.parameter_gesture.as_ref().unwrap().commands.clone();
        assert!(commands[0].get("layer").is_none());
        let preview = Engine::preview_edits(before.clone(), &commands).unwrap();
        assert_ne!(preview.export_png().unwrap(), before.export_png().unwrap());
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.filters[0].weight, 1.);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history);
        draw(
            &mut app,
            vec![egui::Event::PointerButton {
                pos: pos - Vec2::new(25., 0.),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        app.finish_parameter(false);
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history + 1);
        app.shared.lock().unwrap().undo("human").unwrap();
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            before.export_png().unwrap()
        );
    }
    #[test]
    fn filter_apply_and_stack_keep_drafts_until_guarded_completion() {
        for stack in [false, true] {
            for outcome in ["commit", "cancel", "reserved", "replacement"] {
                let (mut app, ctx) = fixture();
                app.edit(
                    vec![json!({"op":"filter.add","kind":"invert"})],
                    "Initial filter",
                );
                let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                app.open_filters(&doc);
                let editor = app.filter_editor.as_mut().unwrap();
                editor.weight = 0.375;
                let preset = &crate::filter_library::curated_values()[0];
                let key = editor.photo_key(&preset.kind, &preset.settings, preset.weight);
                editor.cache_thumbnail(&ctx, PhotoJob::Preset(key), crate::filter_library::reference_thumbnail());
                let token = editor.token;
                let draft = editor.command();
                let command = if stack {
                    json!({"op":"filter.update","filter":doc.filters[0].id,"enabled":false,
                        "document_id":doc.id,"source_revision":doc.revision})
                } else {
                    draft.clone()
                };
                editor.applying = Some(stack);
                let expected = Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
                let shared = app.shared.clone();
                if outcome == "reserved" {
                    shared
                        .lock()
                        .unwrap()
                        .reserve(
                            "agent",
                            "Protected filters",
                            vec![crate::engine::Scope::layer(&doc.layers[0].id)],
                        )
                        .unwrap();
                }
                let mut guard = shared.lock().unwrap();
                if outcome == "replacement" {
                    guard.resident_state_mut().expect("resident fixture").document.id = crate::engine::id();
                }
                let before = serde_json::to_value(&guard.resident_state().expect("resident fixture").document).unwrap();
                let history = guard.history_lengths();
                app.submit_edit_job(
                    edit_job::Request {
                        project: app.project_id.clone(),
                        document: doc.id.clone(),
                        revision: doc.revision,
                        label: "Filter edit".into(),
                        selection_edit: false,
                        origin: edit_job::Origin::Filter(token, None),
                    },
                    edit_job::Commands::resident(vec![command]),
                    &ctx,
                )
                .unwrap();
                app.filter_edit_finished(egui::Id::new("other filter draft"), Ok(()));
                app.open_filters(&doc);
                app.apply_filter_editor(None);
                assert!(app.filter_apply_pending());
                assert_eq!(app.filter_editor.as_ref().unwrap().command(), draft);
                assert_eq!(guard.history_lengths(), history);
                if outcome == "cancel" {
                    app.cancel_filters();
                    assert!(app.filter_apply_pending());
                }
                drop(guard);
                super::super::text_tests::wait_edit(&mut app);
                let engine = shared.lock().unwrap();
                assert!(!app.filter_apply_pending());
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels, doc.layers[0].pixels);
                if outcome == "commit" {
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        expected.export_png().unwrap()
                    );
                    assert_eq!(engine.history_lengths(), [history[0] + 1, 0]);
                    if stack {
                        let editor = app.filter_editor.as_ref().unwrap();
                        assert_ne!(editor.token, token);
                        assert_eq!(editor.revision, engine.resident_state().expect("resident fixture").document.revision);
                        assert_eq!(editor.document, engine.resident_state().expect("resident fixture").document.id);
                        assert!(editor.error.is_none());
                        assert_eq!(editor.photo_key(&preset.kind, &preset.settings, preset.weight), key);
                        assert!(matches!(editor.preset_photo(key), Some(Ok(_))));
                    } else {
                        assert!(app.filter_editor.is_none());
                    }
                } else {
                    let editor = app.filter_editor.as_ref().unwrap();
                    assert_eq!(editor.token, token);
                    assert_eq!(editor.command(), draft);
                    assert!(editor.apply_error.is_some());
                    assert_eq!(engine.history_lengths(), history);
                    assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
                }
            }
        }
    }
    #[test]
    fn stale_filter_editor_refuses_the_same_revision_from_a_new_native_source() {
        use crate::engine::authority::DocumentAuthority;

        let (mut app, _) = fixture();
        let resident = Document::new_depth(12, 8, 16).unwrap();
        let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let make = || crate::tree::presentation_tests::native(&resident, &pool);
        let first = make();
        let replacement = make();
        app.doc_snapshot = first.clone().into();
        app.open_filters(&first);
        assert_eq!(app.filter_editor.as_ref().unwrap().preview_commands(&first).len(), 1);
        assert!(app.filter_editor.as_ref().unwrap().preview_commands(&replacement).is_empty());
        app.shared.lock().unwrap().authority = DocumentAuthority::from_native(replacement.clone(), 4).unwrap();
        app.doc_snapshot = replacement.clone().into();

        app.apply_filter_editor(None);
        assert!(!app.edit_worker.pending());
        assert!(app.filter_editor.as_ref().unwrap().apply_error.is_some());
        let engine = app.shared.lock().unwrap();
        assert_eq!(engine.native_document().unwrap().key(), replacement.key());
        assert_eq!(engine.history_lengths(), [0, 0]);
    }

    #[test]
    fn pending_filter_window_remains_visible_through_lifecycle_gate() {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.open_filters(&doc);
        let editor = app.filter_editor.as_mut().unwrap();
        editor.applying = Some(false);
        let token = editor.token;
        let (release, held) = mpsc::channel();
        app.edit_worker
            .submit(
                edit_job::Request {
                    project: app.project_id.clone(),
                    document: doc.id.clone(),
                    revision: doc.revision,
                    label: "Whole-image filter".into(),
                    selection_edit: false,
                    origin: edit_job::Origin::Filter(token, None),
                },
                edit_job::Commands::resident(vec![]),
                move |_, _| {
                    held.recv().unwrap();
                    Err("Apply cancelled".into())
                },
                || {},
            )
            .unwrap();
        let output =
            super::super::workspace_tests::draw(&mut app, &ctx, Vec2::new(1360., 900.), vec![]);
        super::super::workspace_tests::label(&output, "Applying filter changes…");
        super::super::workspace_tests::label(&output, "Cancel apply");
        assert!(app.filter_apply_pending());
        release.send(()).unwrap();
        super::super::text_tests::wait_edit(&mut app);
        assert!(app.filter_editor.is_some());
        assert!(!app.filter_apply_pending());
    }
}
impl Editor {
    pub fn new<'a>(doc: impl Into<DocumentView<'a>>) -> Self {
        let doc = doc.into();
        let (tx, rx) = mpsc::channel();
        let p = crate::filter_library::curated_values()[0].clone();
        let mut editor = Self {
            document: doc.id().into(),
            revision: doc.revision(),
            native: doc.native().map(|native| native.key()),
            depth: doc.depth(),
            target: None,
            kind: p.kind,
            settings: p.settings,
            weight: p.weight,
            preset: Some(p.id),
            name: p.name,
            category: p.category,
            search: String::new(),
            category_filter: String::new(),
            original: false,
            unfiltered: false,
            error: None,
            apply_error: None,
            thumbnails: VecDeque::new(),
            draft_result: None,
            last_draft_photo: None,
            thumbnail_keys: RandomState::new(),
            thumbnail_pending: None,
            draft_pending: None,
            draft_generation: 0,
            draft_photo: true,
            token: egui::Id::new(crate::engine::id()),
            preview_drag: None,
            applying: None,
            tx,
            rx,
        };
        if let Some(f) = doc
            .filter_count()
            .checked_sub(1)
            .and_then(|index| doc.filter(index))
        {
            editor.select_filter(f);
        }
        editor
    }

    fn select_filter(&mut self, filter: crate::engine::view::EffectView<'_>) {
        self.preview_drag = None;
        self.reset_draft_photo();
        self.target = Some(filter.id().into());
        self.kind = filter.kind().into();
        self.settings = filter.settings().clone();
        self.weight = filter.weight();
        self.preset = None;
        self.name = effects::effect_name(filter.kind()).into();
        self.category = crate::effects::catalog::get(filter.kind())
            .map_or("Custom", |entry| entry.category)
            .into();
        self.error = None;
        self.show_filtered();
    }
    pub fn command(&self) -> Value {
        json!({"op":if self.target.is_some(){"filter.update"}else{"filter.add"},"filter":self.target,"kind":self.kind,"settings":self.settings,"weight":self.weight,"document_id":self.document,"source_revision":self.revision})
    }
    pub(super) fn observes(&self, doc: DocumentView<'_>) -> bool {
        self.document == doc.id() && self.revision == doc.revision() && self.depth == doc.depth()
            && self.native == doc.native().map(|native| native.key())
    }
    pub fn preview_commands<'a>(&self, doc: impl Into<DocumentView<'a>>) -> Vec<Value> {
        self.preview_commands_with(doc.into(), || self.command())
    }

    fn preview_commands_with(&self, doc: DocumentView<'_>, command: impl FnOnce() -> Value) -> Vec<Value> {
        if !self.observes(doc) {
            vec![]
        } else if self.unfiltered {
            (0..doc.filter_count()).filter_map(|index| doc.filter(index)).map(|f|json!({"op":"filter.update","filter":f.id(),"enabled":false,"document_id":self.document,"source_revision":self.revision})).collect()
        } else if self.original {
            vec![]
        } else {
            let mut draft = command();
            if self.target.is_some() {
                draft["enabled"] = json!(true);
            }
            vec![draft]
        }
    }
    pub(super) fn pointer_preview_key(
        &self,
        ctx: &egui::Context,
        doc: DocumentView<'_>,
    ) -> Option<(u64, u64, Option<&str>, &str)> {
        let drag = self.preview_drag.filter(|&id| ctx.dragged_id() == Some(id))?;
        (!self.original && !self.unfiltered && self.applying.is_none()
            && self.observes(doc)
            && ctx.input(|input| {
                input.focused && input.pointer.primary_down() && !input.key_pressed(egui::Key::Escape)
                    && !input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
            }))
            .then_some((self.token.value(), drag.value(), self.target.as_deref(), self.kind.as_str()))
    }

    fn track_preview_drag(&mut self, response: &egui::Response) {
        if response.changed() && response.dragged_by(egui::PointerButton::Primary) {
            self.preview_drag = response.ctx.dragged_id();
        }
    }

    fn choose(&mut self, p: &Preset) {
        self.preview_drag = None;
        self.reset_draft_photo();
        self.kind = p.kind.clone();
        self.settings = p.settings.clone();
        self.weight = p.weight;
        self.preset = Some(p.id.clone());
        self.name = p.name.clone();
        self.category = p.category.clone();
        self.error = None;
    }

    fn show_filtered(&mut self) {
        self.original = false;
        self.unfiltered = false;
    }

    fn thumbnail(
        &mut self,
        ctx: &egui::Context,
        p: &Preset,
    ) -> Option<&Result<TextureHandle, String>> {
        let key = self.photo_key(&p.kind, &p.settings, p.weight);
        if self.thumbnail_pending.is_none() && self.draft_pending != Some(key)
            && self.preset_photo(key).is_none()
        {
            self.queue_photo(ctx, PhotoJob::Preset(key), p.kind.clone(), p.settings.clone(), p.weight);
        }
        self.preset_photo(key)
    }
    fn photo_key(&self, kind: &str, settings: &Value, weight: f32) -> u64 {
        self.thumbnail_keys.hash_one((kind, settings, weight.to_bits()))
    }
    fn photo_index(&self, key: u64) -> Option<usize> {
        self.thumbnails.iter().position(|(cached, _)| *cached == key)
    }
    fn preset_photo(&self, key: u64) -> Option<&Result<TextureHandle, String>> {
        self.photo_index(key).map(|index| &self.thumbnails[index].1)
            .or_else(|| self.draft_result.as_ref().filter(|(cached, _)| *cached == key)
                .map(|(_, preview)| preview))
    }
    fn queue_photo(&mut self, ctx: &egui::Context, job: PhotoJob, kind: String, settings: Value, weight: f32) {
        let tx = self.tx.clone();
        let repaint = ctx.clone();
        let depth = self.depth;
        let worker = std::thread::Builder::new().name("peerbrush-filter-photo".into()).spawn(move || {
            let result = std::panic::catch_unwind(|| {
                crate::filter_library::thumbnail_for(&kind, &settings, weight, depth)
            }).unwrap_or_else(|_| Err("Filter photo preview could not complete".into()));
            if tx.send((job, result)).is_ok() {
                repaint.request_repaint();
            }
        });
        match worker {
            Ok(_) => match job {
                PhotoJob::Preset(key) => self.thumbnail_pending = Some(key),
                PhotoJob::Draft { key, .. } => self.draft_pending = Some(key),
            },
            Err(error) => {
                self.cache_thumbnail(ctx, job, Err(format!("Could not start filter preview: {error}")));
                ctx.request_repaint();
            }
        }
    }

    fn reset_draft_photo(&mut self) {
        self.draft_generation = self.draft_generation.wrapping_add(1);
        self.draft_result = None;
        self.last_draft_photo = None;
    }

    fn draft_thumbnail(&mut self, ctx: &egui::Context) -> Option<&Result<TextureHandle, String>> {
        let key = self.photo_key(&self.kind, &self.settings, self.weight);
        if let Some(index) = self.photo_index(key) {
            self.last_draft_photo = self.thumbnails[index].1.as_ref().ok().cloned();
            return Some(&self.thumbnails[index].1);
        }
        if self.draft_result.as_ref().is_some_and(|(cached, _)| *cached == key) {
            if let Some((_, Ok(texture))) = &self.draft_result {
                self.last_draft_photo = Some(texture.clone());
            }
            return self.draft_result.as_ref().map(|(_, texture)| texture);
        }
        if self.draft_pending.is_none() && self.thumbnail_pending != Some(key) {
            let job = PhotoJob::Draft { key, generation: self.draft_generation };
            self.queue_photo(ctx, job, self.kind.clone(), self.settings.clone(), self.weight);
        }
        None
    }
    fn cache_thumbnail(&mut self, ctx: &egui::Context, job: PhotoJob, result: Result<Vec<u8>, String>) {
        if let PhotoJob::Draft { key, generation } = job {
            self.draft_pending = None;
            if generation != self.draft_generation
                || key != self.photo_key(&self.kind, &self.settings, self.weight)
            {
                return;
            }
        }
        let preview = result.map(|rgba| {
            ctx.load_texture(
                "filter thumbnail",
                egui::ColorImage::from_rgba_unmultiplied(THUMBNAIL_SIZE.map(|n| n as usize), &rgba),
                egui::TextureOptions::LINEAR,
            )
        });
        match job {
            PhotoJob::Preset(key) => {
                self.thumbnail_pending = None;
                if self.thumbnails.len() >= THUMBNAIL_LIMIT {
                    self.thumbnails.pop_front();
                }
                self.thumbnails.push_back((key, preview));
            }
            PhotoJob::Draft { key, .. } => {
                if let Ok(texture) = &preview {
                    self.last_draft_photo = Some(texture.clone());
                }
                self.draft_result = Some((key, preview));
            }
        }
    }
}
impl PeerBrush {
    pub(super) fn document_filter_stack<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        let pending = doc
            .resident()
            .and_then(|resident| self.pending_parameter_document(ui.ctx(), resident));
        let display = pending.as_deref().map(DocumentView::from).unwrap_or(doc);
        let numbers = self.animation.controls(display, ui.input(|i| i.time));
        ui.set_min_width(290.);
        ui.add_enabled_ui(!doc.read_only() && !self.busy, |ui| {
            for index in (0..display.filter_count()).rev() {
                let f = display.filter(index).unwrap();
                ui.push_id(&f.id(), |ui| {
                    ui.horizontal(|ui| {
                        let mut enabled = f.enabled();
                        let name = effects::effect_name(f.kind());
                        let visibility = if enabled { Icon::Eye } else { Icon::EyeOff };
                        if icons::toggle(ui, visibility, &mut enabled, &format!("Enable {name}"))
                            .changed()
                        {
                            self.edit(
                                vec![
                                    json!({"op":"filter.update","filter":f.id(),"enabled":enabled}),
                                ],
                                "Toggle whole-image filter",
                            );
                        }
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::splat(18.), egui::Sense::hover());
                        icons::paint(ui.painter(), rect, effect_icon(&f.kind()), f.enabled());
                        ui.label(name);
                        let mut value = numbers.effect_weight(None, f) * 100.;
                        let response = controls::range(
                            ui,
                            "strength",
                            &mut value,
                            0.0..=100.0,
                            95.,
                            "%",
                            0,
                            false,
                        );
                        if response.changed() || response.double_clicked() {
                            self.layer_parameter(
                                json!({"filter":f.id(),"weight":value/100.}),
                                "Filter strength",
                                &response,
                            );
                        }
                        if index + 1 < doc.filter_count()
                            && icons::small_button(ui, Icon::Up, "Move filter up").clicked()
                        {
                            self.edit(
                                vec![
                                    json!({"op":"filter.reorder","filter":f.id(),"index":index+1}),
                                ],
                                "Reorder filters",
                            );
                        }
                        if index > 0
                            && icons::small_button(ui, Icon::Down, "Move filter down").clicked()
                        {
                            self.edit(
                                vec![
                                    json!({"op":"filter.reorder","filter":f.id(),"index":index-1}),
                                ],
                                "Reorder filters",
                            );
                        }
                        if icons::small_button(ui, Icon::Trash, "Remove filter").clicked() {
                            self.edit(
                                vec![json!({"op":"filter.delete","filter":f.id()})],
                                "Remove filter",
                            );
                        }
                    })
                });
            }
        });
        ui.label(
            RichText::new("Top filters run last · edit settings in Whole-image filters")
                .size(11.)
                .color(MUTED),
        );
    }
    pub(super) fn open_filters<'a>(&mut self, doc: impl Into<DocumentView<'a>>) {
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; filter draft preserved".into();
            return;
        }
        self.cancel_geometry();
        self.cancel_source();
        self.cancel_refinement();
        self.finish_parameter(true);
        self.points.clear();
        self.drag_start = None;
        self.gizmo_handle = 0;
        self.layer_drag = None;
        self.eye_sweep = None;
        self.interrupt_animation();
        self.isolate = false;
        self.mask = false;
        self.filter_editor = Some(Editor::new(doc));
        self.last_preview = None;
    }
    pub(super) fn filter_apply_pending(&self) -> bool {
        self.filter_editor
            .as_ref()
            .is_some_and(|editor| editor.applying.is_some())
    }
    pub(super) fn cancel_filters(&mut self) {
        if self.filter_apply_pending() {
            self.cancel_pending_edit();
            return;
        }
        self.filter_editor = None;
        self.transient
            .retain(|c| !c["op"].as_str().is_some_and(|op| op.starts_with("filter.")));
        self.last_preview = None;
    }
    fn apply_filter_editor(&mut self, stack: Option<Value>) {
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; filter draft preserved".into();
            return;
        }
        let Some(editor) = self.filter_editor.as_mut() else {
            return;
        };
        let is_stack = stack.is_some();
        let mut command = stack.unwrap_or_else(|| editor.command());
        command["document_id"] = json!(editor.document);
        command["source_revision"] = json!(editor.revision);
        let document = editor.document.clone();
        let revision = editor.revision;
        let token = editor.token;
        let native = editor.native;
        editor.apply_error = None;
        editor.applying = Some(is_stack);
        self.apply_action(
            vec![command],
            if is_stack {
                "Edit whole-image filter stack"
            } else {
                "Whole-image filter"
            },
            Some((&document, revision)),
            edit_job::Origin::Filter(token, native),
        );
    }
    pub(super) fn filter_edit_finished(&mut self, token: egui::Id, result: Result<(), String>) {
        let Some(editor) = self
            .filter_editor
            .as_mut()
            .filter(|editor| editor.token == token && editor.applying.is_some())
        else {
            return;
        };
        let stack = editor.applying.take().unwrap();
        match result {
            Err(error) => editor.apply_error = Some(error),
            Ok(()) if stack => {
                let engine = self.shared.lock().unwrap();
                let doc = crate::engine::view::current(&engine);
                if doc.id() == editor.document {
                    let mut refreshed = Editor::new(doc);
                    if refreshed.depth == editor.depth {
                        refreshed.thumbnails = std::mem::take(&mut editor.thumbnails);
                        std::mem::swap(&mut refreshed.thumbnail_keys, &mut editor.thumbnail_keys);
                    }
                    self.filter_editor = Some(refreshed);
                    self.transient.clear();
                    self.last_preview = None;
                } else {
                    editor.apply_error =
                        Some("Filter source changed; current work preserved".into());
                }
            }
            Ok(()) => self.cancel_filters(),
        }
    }
    pub(super) fn filters_window<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        if self.filter_apply_pending() {
            egui::Window::new("Whole-image filters")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("Applying filter changes…");
                    if ui.button("Cancel apply").clicked() {
                        self.cancel_pending_edit();
                    }
                });
            return;
        }
        let Some(mut editor) = self.filter_editor.take() else {
            return;
        };
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_filters();
            ctx.request_repaint();
            return;
        }
        if !editor.observes(doc) {
            self.cancel_filters();
            self.message = "Filter preview cancelled · project changed".into();
            ctx.request_repaint();
            return;
        }
        while let Ok((job, result)) = editor.rx.try_recv() {
            editor.cache_thumbnail(ctx, job, result);
        }
        let library = self.shared.lock().unwrap().filter_library.clone();
        let (custom_presets, library_error) = {
            let l = library.lock().unwrap();
            (l.custom_snapshot(), l.error.clone())
        };
        let presets = || crate::filter_library::curated_values().iter().chain(custom_presets.iter());
        let before = editor.command();
        let comparison = (editor.original, editor.unfiltered);
        let was_dragging = editor.preview_drag.is_some();
        if editor.pointer_preview_key(ctx, doc).is_none() {
            editor.preview_drag = None;
        }
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut stack_edit = None;
        egui::Window::new("Whole-image filters")
            .open(&mut open).collapsible(false).resizable(true).default_width(650.0)
            .default_pos(egui::pos2((ctx.screen_rect().right()-660.0).max(8.0),80.0))
            .show(ctx,|ui| {
                ui.horizontal_wrapped(|ui| {
                    for (icon, label, help, state) in [
                        (
                            Icon::Adjust,
                            "Filtered",
                            "Preview the draft settings on the project",
                            (false, false),
                        ),
                        (
                            Icon::LayerStack,
                            "Current image",
                            "Show the project’s filter stack before this draft",
                            (true, false),
                        ),
                        (
                            Icon::EyeOff,
                            "Original",
                            "Bypass every whole-image filter for comparison",
                            (false, true),
                        ),
                    ] {
                        if icons::labeled_choice(ui, icon, label, Some(comparison == state), 0.)
                            .on_hover_text(help)
                            .clicked()
                        {
                            (editor.original, editor.unfiltered) = state;
                        }
                    }
                });
                if doc.filter_count() > 0 {
                    egui::ScrollArea::vertical().id_salt("document filter stack").max_height(100.).show(ui,|ui| {
                        for (index,f) in (0..doc.filter_count()).rev().filter_map(|index| doc.filter(index).map(|f| (index, f))) {
                            ui.push_id(f.id(),|ui|ui.horizontal(|ui| {
                                let mut enabled=f.enabled();
                                let name = effects::effect_name(f.kind());
                                let visibility = if enabled { Icon::Eye } else { Icon::EyeOff };
                                if icons::toggle(ui, visibility, &mut enabled, &format!("Enable {name}")).changed() {
                                    stack_edit = Some(json!({"op":"filter.update","filter":f.id(),"enabled":enabled}));
                                }
                                if ui.selectable_label(editor.target.as_deref() == Some(f.id()), name).clicked() {
                                    editor.select_filter(f);
                                }
                                ui.label(format!("{:.0}%",f.weight()*100.));
                                if index+1<doc.filter_count()&&icons::small_button(ui,Icon::Up,"Move filter up").clicked(){stack_edit=Some(json!({"op":"filter.reorder","filter":f.id(),"index":index+1}));}
                                if index>0&&icons::small_button(ui,Icon::Down,"Move filter down").clicked(){stack_edit=Some(json!({"op":"filter.reorder","filter":f.id(),"index":index-1}));}
                                if icons::small_button(ui,Icon::Trash,"Remove filter").clicked(){stack_edit=Some(json!({"op":"filter.delete","filter":f.id()}));}
                            }));
                        }
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    let search = ui.add(egui::TextEdit::singleline(&mut editor.search)
                        .hint_text("Search filters…").desired_width(220.).frame(false));
                    search.widget_info(|| egui::WidgetInfo {
                        label: Some("Search filters".into()),
                        ..egui::WidgetInfo::text_edit(search.enabled(), &editor.search, &editor.search)
                    });
                    let mut categories = presets().map(|p| p.category.as_str()).collect::<Vec<_>>();
                    categories.sort_unstable();
                    categories.dedup();
                    let category_menu = egui::ComboBox::from_id_salt("filter category").selected_text(if editor.category_filter.is_empty(){"All categories"}else{&editor.category_filter}).show_ui(ui,|ui| {
                        ui.selectable_value(&mut editor.category_filter,String::new(),"All categories");
                        for category in categories {
                            if ui.selectable_label(editor.category_filter == category, category).clicked() {
                                editor.category_filter = category.into();
                            }
                        }
                    });
                    let response = category_menu.response;
                    response.widget_info(|| {
                        let mut info = egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, response.enabled(), "Filter category");
                        info.current_text_value = Some(if editor.category_filter.is_empty() {
                            "All categories".into()
                        } else {
                            editor.category_filter.clone()
                        });
                        info
                    });
                    if ui.button("New filter").clicked() {
                        editor.reset_draft_photo();
                        editor.target = None;
                        editor.error = None;
                        editor.show_filtered();
                    }
                });
                let body_height = (ui.available_height() - 88.).clamp(96., 385.);
                ui.columns(2,|columns| {
                    let query: Vec<_> = editor.search.split_whitespace().map(str::to_lowercase).collect();
                    let filtered = presets().filter(|p| {
                        if !editor.category_filter.is_empty() && editor.category_filter != p.category {
                            return false;
                        }
                        if query.is_empty() {
                            return true;
                        }
                        let description = format!("{} {} {}", p.name, p.category, p.kind).to_lowercase();
                        query.iter().all(|word| description.contains(word.as_str()))
                    }).collect::<Vec<_>>();
                    let [width, height] = THUMBNAIL_SIZE.map(|n| n as f32);
                    let mut next_preset = None;
                    let mut hovered_preset = None;
                    let mut focused_preset = None;
                    let mut chosen_preset = None;
                    egui::ScrollArea::vertical().id_salt("filter library").max_height(body_height.min(280.)).show_rows(&mut columns[0],height,filtered.len(),|ui,rows| {
                        for row in rows {
                            let preset = filtered[row];
                            ui.push_id(&preset.id, |ui| {
                                let selected = editor.preset.as_deref() == Some(&preset.id);
                                let key = editor.photo_key(&preset.kind, &preset.settings, preset.weight);
                                let preview = editor.preset_photo(key);
                                if preview.is_none() && next_preset.is_none() {
                                    next_preset = Some(preset);
                                }
                                let button = match preview {
                                    Some(Ok(texture)) => egui::Button::image_and_text(
                                        (texture.id(), Vec2::new(width, height)), &preset.name,
                                    ),
                                    _ => egui::Button::new(&preset.name),
                                };
                                let response = ui.add(button.frame(false).selected(selected).truncate()
                                    .min_size(Vec2::new(ui.available_width(), height)));
                                response.widget_info(|| egui::WidgetInfo::selected(
                                    egui::WidgetType::SelectableLabel, response.enabled(), selected, &preset.name,
                                ));
                                icons::paint_focus(ui, &response);
                                if response.gained_focus() {
                                    response.scroll_to_me(None);
                                }
                                if response.hovered() && preview.is_none() {
                                    hovered_preset = Some(preset);
                                }
                                if response.has_focus() && response.enabled() && !selected
                                    && ui.clip_rect().intersects(response.rect)
                                {
                                    focused_preset = Some(preset);
                                }
                                let response = response.on_hover_ui(|ui| {
                                    ui.set_min_width(3. * width + ui.spacing().item_spacing.x);
                                    ui.label(format!("{} · {}", preset.name, preset.category));
                                    compare_photo(ui, "Filtered photo", preview, None, width * 1.5);
                                });
                                if response.clicked() {
                                    chosen_preset = Some(preset);
                                }
                            });
                        }
                    });
                    if let Some(preset) = chosen_preset {
                        editor.choose(preset);
                        editor.show_filtered();
                        focused_preset = None;
                    }
                    let focused_job = focused_preset.filter(|preset| {
                        let key = editor.photo_key(&preset.kind, &preset.settings, preset.weight);
                        editor.preset_photo(key).is_none()
                    });
                    if let Some(preset) = hovered_preset.or(focused_job).or(next_preset) {
                        editor.thumbnail(ctx, preset);
                    }
                    if filtered.is_empty(){columns[0].label("No matching filters");}
                    let ui = &mut columns[1];
                    let controls_top = ui.cursor().min.y;
                    ui.horizontal(|ui| {
                        let current = effects::effect_name(&editor.kind);
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.), egui::Sense::hover());
                        icons::paint(ui.painter(), rect, effect_icon(&editor.kind), true);
                        let menu = egui::ComboBox::from_id_salt("filter kind")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                let mut choice = None;
                                for entry in crate::effects::catalog::ENTRIES.iter()
                                    .filter(|entry| crate::filters::supports(entry.kind))
                                {
                                    let selected = editor.kind == entry.kind;
                                    if menu_effect(ui, entry.kind, entry.name, Some(selected)).clicked() {
                                        choice = Some(entry);
                                        ui.close_menu();
                                    }
                                }
                                choice
                            });
                        let response = menu.response.on_hover_text("Change the effect applied to the whole image.");
                        response.widget_info(|| {
                            let mut info = egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, response.enabled(), "Effect type");
                            info.current_text_value = Some(current.to_owned());
                            info
                        });
                        if let Some(entry) = menu.inner.flatten().filter(|entry| editor.kind != entry.kind) {
                            editor.kind = entry.kind.into();
                            editor.settings = crate::effects::defaults(entry.kind);
                            editor.preset = None;
                            editor.reset_draft_photo();
                            editor.name = entry.name.into();
                            editor.category = entry.category.into();
                            editor.error = None;
                            editor.show_filtered();
                        }
                    });
                    ui.toggle_value(&mut editor.draft_photo, "Compare on reference photo")
                        .on_hover_text("Keep the original and filtered photo beside the effect settings");
                    let draft_key = (editor.draft_photo && focused_preset.is_none())
                        .then(|| editor.photo_key(&editor.kind, &editor.settings, editor.weight));
                    if editor.draft_photo || focused_preset.is_some() {
                        let photo_width = (body_height * 0.4).clamp(32., width * 1.5);
                        if let Some(preset) = focused_preset {
                            let key = editor.photo_key(&preset.kind, &preset.settings, preset.weight);
                            compare_photo(ui, &preset.name, editor.preset_photo(key), None, photo_width);
                        } else if let Some(key) = draft_key {
                            compare_photo(ui, "Filtered photo", editor.preset_photo(key), editor.last_draft_photo.as_ref(), photo_width);
                        }
                    }
                    let controls_height = (body_height - (ui.cursor().min.y - controls_top)).max(60.);
                    egui::ScrollArea::vertical()
                        .id_salt("whole-image filter controls")
                        .max_height(controls_height)
                        .drag_to_scroll(false)
                        .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        controls::label(ui, "Strength");
                        let mut value=editor.weight*100.;
                        let response = controls::range(ui, "filter strength", &mut value, 0.0..=100.0, 110., "%", 0, false);
                        if response.changed() {
                            editor.weight = value / 100.;
                            editor.show_filtered();
                        }
                        editor.track_preview_drag(&response);
                    });
                    let response = effects::settings(ui, &editor.kind, &mut editor.settings, doc.depth());
                    if let Some(response) = response {
                        if response.changed() {
                            editor.show_filtered();
                        }
                        editor.track_preview_drag(&response);
                    }
                    if editor.target.as_deref().is_some_and(|target| {
                        (0..doc.filter_count()).filter_map(|index| doc.filter(index))
                            .any(|filter| filter.id() == target && !filter.enabled())
                    }) {
                        ui.label(RichText::new("Bypassed in the stack. Filtered previews it enabled.").small());
                    }
                    ui.add(egui::TextEdit::singleline(&mut editor.name).hint_text("Preset name").desired_width(230.).char_limit(80).frame(false));
                    ui.add(egui::TextEdit::singleline(&mut editor.category).hint_text("Preset category").desired_width(230.).char_limit(40).frame(false));
                    let custom=editor.preset.as_ref().is_some_and(|id|custom_presets.iter().any(|p|&p.id==id));
                    ui.horizontal(|ui| {
                        if ui.button("Save new preset").clicked(){match library.lock().unwrap().save(None,&editor.name,&editor.category,&editor.kind,editor.settings.clone(),editor.weight){Ok(p)=>editor.choose(&p),Err(e)=>editor.error=Some(e)}}
                        if custom&&ui.button("Update").clicked(){match library.lock().unwrap().save(editor.preset.as_deref(),&editor.name,&editor.category,&editor.kind,editor.settings.clone(),editor.weight){Ok(p)=>editor.choose(&p),Err(e)=>editor.error=Some(e)}}
                    });
                    ui.horizontal(|ui| {
                        if custom&&ui.button("Rename").clicked(){match library.lock().unwrap().rename(editor.preset.as_deref().unwrap(),&editor.name){Ok(p)=>editor.preset=Some(p.id),Err(e)=>editor.error=Some(e)}}
                        if custom&&ui.button("Delete preset").clicked(){match library.lock().unwrap().delete(editor.preset.as_deref().unwrap()){Ok(())=>editor.preset=None,Err(e)=>editor.error=Some(e)}}
                        if editor.preset.is_some()&&ui.button("Export…").clicked(){if let Some(path)=rfd::FileDialog::new().add_filter("PeerBrush filter",&["json"]).set_file_name("filter.json").save_file(){let result=library.lock().unwrap().export(editor.preset.as_deref().unwrap()).and_then(|data|serde_json::to_vec_pretty(&data).map_err(|e|e.to_string())).and_then(|bytes|server::atomic_write(&path,&bytes));if let Err(e)=result{editor.error=Some(e);}}}
                    });
                    if ui.button("Import preset…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("PeerBrush filter", &["json"]).pick_file() {
                            let result = crate::filter_library::read(&path)
                                .and_then(|data| library.lock().unwrap().import(&data));
                            match result {
                                Ok(preset) => {
                                    editor.choose(&preset);
                                    editor.show_filtered();
                                }
                                Err(error) => editor.error = Some(error),
                            }
                        }
                    }
                    });
                    if let Some(key) = draft_key {
                        // Process controls first so the worker sees this frame's final settings.
                        if editor.draft_thumbnail(ctx).is_some()
                            && key != editor.photo_key(&editor.kind, &editor.settings, editor.weight)
                        {
                            ctx.request_repaint();
                        }
                    }
                });
                if let Some(error)=library_error.as_ref().or(editor.error.as_ref()).or(editor.apply_error.as_ref()){ui.colored_label(ACCENT,error);}
                ui.horizontal(|ui| {
                    apply=ui.add_enabled(!self.busy&&!doc.read_only()&&editor.error.is_none(),egui::Button::new(if editor.target.is_some(){"Apply changes"}else{"Apply filter"})).clicked();
                    cancel=ui.button("Cancel").clicked();
                });
            });
        let command = editor.command();
        let changed_comparison = comparison != (editor.original, editor.unfiltered);
        let changed_target = before["filter"] != command["filter"] || before["kind"] != command["kind"];
        let finished_drag = was_dragging && editor.preview_drag.is_none();
        let changed_draft = command != before || changed_comparison;
        if changed_comparison || changed_target || finished_drag {
            self.last_preview = None;
        }
        if changed_draft {
            editor.error = None;
            editor.apply_error = None;
        }
        if changed_draft || finished_drag {
            // The canvas scheduler observes the new draft on the next frame.
            ctx.request_repaint();
        }
        if !open || cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_filters();
            ctx.request_repaint();
        } else if stack_edit.is_some() || apply {
            self.filter_editor = Some(editor);
            self.apply_filter_editor(stack_edit);
        } else {
            self.transient = editor.preview_commands_with(doc, || command);
            self.filter_editor = Some(editor);
        }
    }
}
