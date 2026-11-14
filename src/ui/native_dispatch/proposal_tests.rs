use super::*;
use crate::{
    collaboration::native,
    engine::{
        native::worker::tests::Fixture,
        scene::{CapturedScene, LegacyTileCache},
        Document, Engine,
    },
    ui::PeerBrush,
    workspace::{self, source_runtime::SourceRuntime},
};
use serde_json::json;
use std::time::{Duration, Instant};

fn app(
    runtime: &Arc<SourceRuntime>,
    depth: u16,
) -> (PeerBrush, egui::Context, NativeDocument, Document, String) {
    let mut original = Document::new_depth(19, 13, depth).unwrap();
    original.layers[0]
        .pixels
        .set16(2, 3, [12347, 23459, 34571, 61379]);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let staged =
        CapturedScene::from_parts(original.clone(), "review-ui".into(), vec![], None, None)
            .stage(runtime, &cache, &|| Ok(()))
            .unwrap();
    let document =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(
        Engine::from_native(document.clone(), 8).unwrap(),
    ));
    let registry = workspace::attach(&shared);
    registry.lock().unwrap().source_runtime = Some(runtime.clone());
    let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
    app.project_id = shared.lock().unwrap().project_id.clone();
    app.shared = shared.clone();
    app.workspace_root = shared;
    app.workspace = registry;
    app.doc_snapshot = document.clone().into();
    app.selected = original.layers[0].id.clone();
    let id = native::create_stored(
        &app.shared,
        "agent",
        &commands(&original),
        &original.id,
        0,
        "Reviewed edit",
        None,
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    app.proposal_review = Some(id.clone());
    app.proposal_original = false;
    (app, ctx, document, original, id)
}
fn commands(original: &Document) -> Vec<Value> {
    vec![
        json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[4,3,10,9],"color":[31,73,149,177]}),
    ]
}
fn prepare(app: &mut PeerBrush, ctx: &egui::Context, document: &NativeDocument) -> (Input, Output) {
    let input = app
        .capture_native_canvas(ctx, &app.project_id.clone(), document)
        .unwrap()
        .unwrap();
    let output = render(
        &app.shared,
        Input {
            stamp: input.stamp.clone(),
            commands: input.commands.clone(),
            source: None,
            presentation: None,
            blend: None,
            streamed_blend: None,
            cache: false,
        },
        &app.native_dispatch.retirement,
        &app.native_dispatch.endpoint,
        1024,
        &|| Ok(()),
    )
    .unwrap();
    output.validate(&input.stamp).unwrap();
    (input, output)
}
fn publish(
    app: &mut PeerBrush,
    ctx: &egui::Context,
    document: &NativeDocument,
    input: &Input,
    output: Output,
    job: u64,
) -> Result<bool> {
    app.preview_epoch.store(job, Ordering::Relaxed);
    app.preview_job = job;
    app.native_dispatch.expected = Some((job, input.stamp.clone(), true));
    app.native_dispatch.replies.complete(Reply {
        job,
        stamp: input.stamp.clone(),
        result: Ok(output),
    });
    app.present_native_canvas(&app.project_id.clone(), document, ctx)
}

#[test]
fn native_review_uploads_actual_proposed_pixels_and_accepts_on_the_shared_edit_worker() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (mut app, ctx, document, original, id) = app(&runtime, depth);
        app.isolate = true;
        app.mask = true;
        let (input, output) = prepare(&mut app, &ctx, &document);
        assert!(input.stamp.target.is_none());
        assert!(!input.stamp.mask);
        assert!(!input.cache);
        assert!(output.baseline.is_none());
        assert!(output.origin.is_some());
        assert_ne!(output.uploads.tag().key().source, document.key());
        assert!(publish(&mut app, &ctx, &document, &input, output, 1).unwrap());
        assert!(app.native_proposal_canvas_current((&document).into()));
        assert!(!app.canvas_capture_ready((&document).into()));
        assert_eq!(
            app.proposal_rendered.as_deref(),
            Some(app.live_key().as_str())
        );
        let texture = app
            .native_canvas
            .current()
            .unwrap()
            .texture_ids()
            .next()
            .unwrap();
        let delta = ctx.tex_manager().write().take_delta();
        let image = &delta
            .set
            .iter()
            .find(|(id, _)| *id == texture)
            .unwrap()
            .1
            .image;
        let egui::ImageData::Color(image) = image else {
            panic!("Canvas color upload required")
        };
        let mut oracle = Engine::from_document(original.clone());
        oracle
            .edit(
                "agent",
                &commands(&original),
                Some(0),
                None,
                "Reviewed edit",
            )
            .unwrap();
        let expected = oracle
            .resident_state()
            .unwrap()
            .document
            .preview(None, 8192, None, false)
            .unwrap();
        let colors: Vec<_> = expected
            .2
            .chunks_exact(4)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();
        assert_eq!(image.size, [19, 13]);
        assert_eq!(image.pixels, colors);
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
        app.proposal_original = true;
        assert!(!app.native_proposal_canvas_current((&document).into()));
        assert!(app
            .accept_reviewed_proposal(&ctx, (&document).into(), &id)
            .is_err());
        let (baseline, output) = prepare(&mut app, &ctx, &document);
        assert!(baseline.stamp.proposal.is_none());
        assert_eq!(output.uploads.tag().key().source, document.key());
        assert!(publish(&mut app, &ctx, &document, &baseline, output, 2).unwrap());
        app.proposal_original = false;
        assert!(!app.native_proposal_canvas_current((&document).into()));
        let (input, output) = prepare(&mut app, &ctx, &document);
        assert!(publish(&mut app, &ctx, &document, &input, output, 3).unwrap());
        assert!(!app
            .accept_reviewed_proposal(&ctx, (&document).into(), &id)
            .unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.edit_worker.pending() {
            app.poll_edit_job();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(app.proposal_review.is_none());
        assert!(app.native_canvas.current().is_none());
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.history_lengths(), [1, 0]);
        assert_eq!(engine.history_entries().next().unwrap().actor(), "agent");
        assert_eq!(engine.proposals_state()[0]["status"], "accepted");
        drop(engine);
        native::finish_workspace(&app.workspace);
    }
}

#[test]
fn native_proposal_review_renders_the_expanded_canvas_at_full_detail() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let (mut app, ctx, document, original, _) = app(&runtime, 16);
    let proposal = native::create_stored(
        &app.shared,
        "agent",
        &[json!({"op":"canvas.resize", "width":16000, "height":13, "anchor":"top_right"})],
        &original.id,
        0,
        "Expand canvas",
        None,
    )
    .unwrap();
    app.proposal_review = Some(proposal["id"].as_str().unwrap().into());
    app.set_preview_quality(true).unwrap();
    assert_eq!(
        app.native_proposal_size((&document).into()),
        Some([16000, 13])
    );
    let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16000., 13.));
    app.view_rect = Some(canvas);
    app.canvas_viewport = Some(egui::Rect::from_min_max(egui::pos2(15980., 0.), canvas.max));
    let (input, output) = prepare(&mut app, &ctx, &document);
    assert_eq!(input.stamp.edge, 16000);
    assert_eq!(input.stamp.rect, Some([15978, 0, 16000, 13]));
    assert_eq!(output.uploads.tag().key().rect, [15978, 0, 16000, 13]);
    assert!(publish(&mut app, &ctx, &document, &input, output, 1).unwrap());
    let texture = app
        .native_canvas
        .current()
        .unwrap()
        .texture_ids()
        .next()
        .unwrap();
    let delta = ctx.tex_manager().write().take_delta();
    let image = &delta
        .set
        .iter()
        .find(|(id, _)| *id == texture)
        .unwrap()
        .1
        .image;
    let egui::ImageData::Color(image) = image else {
        panic!("Canvas color upload required")
    };
    let (_, _, original_rgba, _) = original.preview(None, 8192, None, false).unwrap();
    let at = (3 * 19 + 2) * 4;
    assert_eq!(image.size, [22, 13]);
    assert_eq!(
        image.pixels[3 * 22 + 5],
        egui::Color32::from_rgba_unmultiplied(
            original_rgba[at],
            original_rgba[at + 1],
            original_rgba[at + 2],
            original_rgba[at + 3],
        ),
    );
    assert!(app.native_proposal_canvas_current((&document).into()));
    app.proposal_original = true;
    assert!(app.native_proposal_size((&document).into()).is_none());
    assert!(!app.native_proposal_canvas_current((&document).into()));
    app.view_rect = Some(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(19., 13.),
    ));
    app.canvas_viewport = app.view_rect;
    let (input, original_output) = prepare(&mut app, &ctx, &document);
    assert_eq!(original_output.uploads.tag().key().rect, [0, 0, 19, 13]);
    assert!(publish(&mut app, &ctx, &document, &input, original_output, 2).unwrap());
    assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
    app.close_proposal();
    native::finish_workspace(&app.workspace);
}

#[test]
fn ended_native_proposals_invalidate_painted_and_queued_frames_without_a_base_revision_change() {
    for terminal in ["reject", "expire", "remove"] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (mut app, ctx, document, _original, id) = app(&runtime, 16);
        let (input, output) = prepare(&mut app, &ctx, &document);
        assert!(publish(&mut app, &ctx, &document, &input, output, 1).unwrap());
        let project = app.project_id.clone();
        assert!(app
            .capture_native_canvas(&ctx, &project, &document)
            .unwrap()
            .is_none());
        app.preview_epoch.store(2, Ordering::Relaxed);
        let (queued, output) = prepare(&mut app, &ctx, &document);
        app.preview_epoch.store(1, Ordering::Relaxed);
        assert!(app.native_proposal_canvas_current((&document).into()));
        {
            let mut engine = app.shared.lock().unwrap();
            match terminal {
                "reject" => {
                    engine.reject_proposal("human", &id).unwrap();
                }
                "expire" => engine.proposals[0].expires = 0,
                "remove" => engine.proposals.clear(),
                _ => unreachable!(),
            }
        }
        assert!(
            !app.native_proposal_canvas_current((&document).into()),
            "{terminal}"
        );
        assert!(publish(&mut app, &ctx, &document, &queued, output, 2).is_err());
        assert!(app
            .accept_reviewed_proposal(&ctx, (&document).into(), &id)
            .is_err());
        assert_eq!(
            app.shared.lock().unwrap().native_document().unwrap().key(),
            document.key()
        );
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
        app.close_proposal();
        assert!(app.native_canvas.current().is_none());
        native::finish_workspace(&app.workspace);
    }
}

#[test]
fn refused_ui_acceptance_preserves_the_candidate_then_renders_it_for_a_human_retry() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let (mut app, ctx, document, original, id) = app(&runtime, 16);
    let (input, output) = prepare(&mut app, &ctx, &document);
    assert!(publish(&mut app, &ctx, &document, &input, output, 1).unwrap());
    let task = app
        .shared
        .lock()
        .unwrap()
        .reserve(
            "human",
            "Human work",
            vec![crate::engine::Scope::layer(&original.layers[0].id)],
        )
        .unwrap()
        .id;
    assert!(!app
        .accept_reviewed_proposal(&ctx, (&document).into(), &id)
        .unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.edit_worker.pending() {
        app.poll_edit_job();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(app.proposal_error.is_some());
    assert!(app.proposal_rendered.is_none());
    assert!(!app.native_proposal_canvas_current((&document).into()));
    {
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.history_lengths(), [0, 0]);
        assert_eq!(engine.native_document().unwrap().key(), document.key());
        assert_eq!(engine.proposals_state()[0]["status"], "pending");
        engine.finish_task(&task, "ended");
    }
    let (input, output) = prepare(&mut app, &ctx, &document);
    let job = app.preview_epoch.load(Ordering::Relaxed) + 1;
    assert!(publish(&mut app, &ctx, &document, &input, output, job).unwrap());
    assert!(app.proposal_error.is_none());
    assert!(!app
        .accept_reviewed_proposal(&ctx, (&document).into(), &id)
        .unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.edit_worker.pending() {
        app.poll_edit_job();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(app.shared.lock().unwrap().history_lengths(), [1, 0]);
    assert!(app.proposal_review.is_none());
    native::finish_workspace(&app.workspace);
}
