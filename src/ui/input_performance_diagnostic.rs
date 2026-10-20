//! Opt-in CPU-side workspace timings; no native window or timing pass/fail threshold.
use super::*;
use crate::engine::Engine;
use std::time::Instant;

const SAMPLES: usize = 8;
const SIZE: Vec2 = Vec2::new(1360., 900.);

struct State(PathBuf);
impl State {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("peerbrush-ui-diagnostic-{}", crate::engine::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for State {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn button(at: Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button,
        pressed,
        modifiers: Default::default(),
    }
}
fn frame(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>) -> f64 {
    let started = Instant::now();
    let output = workspace_tests::draw(app, ctx, SIZE, events);
    std::hint::black_box(output.shapes.len());
    started.elapsed().as_secs_f64() * 1000.
}
fn timings(mut f: impl FnMut() -> f64) -> Value {
    let samples: Vec<_> = (0..SAMPLES).map(|_| f()).collect();
    assert!(samples.iter().all(|n| n.is_finite() && *n >= 0.));
    json!(samples)
}
fn canvas_ready(app: &mut PeerBrush, ctx: &egui::Context) -> f64 {
    let started = Instant::now();
    loop {
        frame(app, ctx, vec![]);
        if !app.pending && app.canvas_preview.is_some() && app.canvas_preview == app.last_preview {
            return started.elapsed().as_secs_f64() * 1000.;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "Actual canvas worker did not settle"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn fixture(depth: u16, filled: bool) -> (State, PeerBrush, egui::Context, f64) {
    let state = State::new();
    let ctx = egui::Context::default();
    let shared = Arc::new(std::sync::Mutex::new(Engine::new()));
    {
        let mut e = shared.lock().unwrap();
        e.doc = Document::new_depth(512, 512, depth).unwrap();
        if filled {
            for y in 0..512 {
                for x in 0..512 {
                    e.doc.layers[0].pixels.set16(
                        x,
                        y,
                        if x < 256 {
                            [10001, 20003, 30007, 65535]
                        } else {
                            [16001, 26003, 36007, 65535]
                        },
                    );
                }
            }
        }
    }
    let started = Instant::now();
    let mut app = PeerBrush::init(
        &ctx,
        shared,
        Connection {
            port: 0,
            token: String::new(),
            state_dir: state.0.clone(),
            instance_lock: None,
            availability: Default::default(),
        },
    );
    let init_ms = started.elapsed().as_secs_f64() * 1000.;
    app.recovery.open = false;
    ctx.style_mut(|style| style.animation_time = 0.);
    canvas_ready(&mut app, &ctx);
    (state, app, ctx, init_ms)
}

fn begin_parameter(app: &mut PeerBrush, ctx: &egui::Context, wand: bool) -> Value {
    let mut rect = Rect::NOTHING;
    let mut control_frame = |app: &mut PeerBrush, events| {
        let doc = app.shared.lock().unwrap().doc.clone();
        let started = Instant::now();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200., 160.))),
                time: Some(ctx.input(|i| i.time) + 0.05),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if wand {
                        rect = app.selection_toolbar(ui, &doc)[0];
                    } else {
                        let mut opacity = 75.;
                        let response = controls::range(
                            ui,
                            "diagnostic opacity",
                            &mut opacity,
                            0.0..=100.0,
                            200.,
                            "%",
                            0,
                            false,
                        );
                        rect = response.rect;
                        if response.changed() {
                            app.layer_parameter(
                                json!({"opacity":opacity/100.}),
                                "Opacity",
                                &response,
                            );
                        }
                    }
                });
            },
        );
        std::hint::black_box(output.shapes.len());
        (rect, started.elapsed().as_secs_f64() * 1000.)
    };
    let (rect, _) = control_frame(app, vec![]);
    let at = rect.center();
    let (_, press_ms) = control_frame(
        app,
        vec![
            egui::Event::PointerMoved(at),
            button(at, egui::PointerButton::Primary, true),
        ],
    );
    let (_, changed_ms) =
        control_frame(app, vec![egui::Event::PointerMoved(at + Vec2::new(8., 0.))]);
    let edit = app
        .parameter_gesture
        .as_ref()
        .expect("Real range input must create the pending edit");
    assert_eq!(
        edit.commands[0]["op"],
        if wand {
            "selection.wand_update"
        } else {
            "layer.update"
        }
    );
    json!({"press_ms":press_ms,"changed_input_ms":changed_ms})
}

#[test]
#[ignore = "Opt-in CPU workspace component timings; run alone with --exact --ignored --nocapture"]
fn actual_workspace_control_and_navigation_components() {
    let build: Value = serde_json::from_str(crate::build_info::json()).unwrap();
    assert_eq!(
        build["tree"], "clean",
        "Record timings only from a clean source-bound build"
    );
    assert_eq!(build["commit"].as_str().unwrap().len(), 40);
    let mut cases = Vec::new();
    for depth in [8, 16] {
        for filled in [false, true] {
            let (_state, mut app, ctx, init_ms) = fixture(depth, filled);
            let before = app.shared.lock().unwrap().doc.clone();
            let before_json = serde_json::to_value(&before).unwrap();
            let before_native = before.layers[0].pixels.rgba16();
            let idle = timings(|| frame(&mut app, &ctx, vec![]));
            let clone = timings(|| {
                let started = Instant::now();
                std::hint::black_box(before.clone());
                started.elapsed().as_secs_f64() * 1000.
            });
            let at = app.view_rect.expect("Actual workspace canvas").center();
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(at),
                    button(at, egui::PointerButton::Middle, true),
                ],
            );
            let original_pan = app.pan;
            let mut step = 0.;
            let pan = timings(|| {
                step += 1.;
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(at + Vec2::new(step * 3., 0.))],
                )
            });
            assert_ne!(app.pan, original_pan, "Actual canvas navigation must run");
            frame(
                &mut app,
                &ctx,
                vec![button(
                    at + Vec2::new(step * 3., 0.),
                    egui::PointerButton::Middle,
                    false,
                )],
            );
            assert_eq!(
                serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap(),
                before_json
            );
            assert_eq!(
                app.shared.lock().unwrap().doc.layers[0].pixels.rgba16(),
                before_native
            );
            assert!(app.shared.lock().unwrap().undo.is_empty());
            cases.push(
                json!({"depth":depth,"filled":filled,"width":512,"height":512,
                "layers":before.layers.len(),"tiles8":before.layers[0].pixels.tiles.len(),
                "tiles16":before.layers[0].pixels.samples16.len(),"init_cpu_ms":init_ms,
                "idle_frame_ms":idle,"document_clone_ms":clone,"pan_frame_ms":pan}),
            );
        }
        for wand in [false, true] {
            let (_state, mut app, ctx, _) = fixture(depth, true);
            if wand {
                app.tool = Tool::MagicWand;
                app.selection_kind = "wand".into();
                let mut e = app.shared.lock().unwrap();
                let layer = app.selected.clone();
                e.edit(
                    "human",
                    &[json!({"op":"selection","kind":"wand","layer":layer,
                    "point":[5,5],"tolerance":0,"feather":2})],
                    None,
                    None,
                    "Diagnostic seed",
                )
                .unwrap();
                e.undo.clear();
                drop(e);
                canvas_ready(&mut app, &ctx);
            }
            let before = app.shared.lock().unwrap().doc.clone();
            let before_json = serde_json::to_value(&before).unwrap();
            let before_native = before.layers[0].pixels.rgba16();
            let control = begin_parameter(&mut app, &ctx, wand);
            let commands = app.parameter_gesture.as_ref().unwrap().commands.clone();
            let expected = Engine::preview_edits(before.clone(), &commands).unwrap();
            let direct = timings(|| {
                let input = before.clone();
                let started = Instant::now();
                std::hint::black_box(Engine::preview_edits(input, &commands).unwrap());
                started.elapsed().as_secs_f64() * 1000.
            });
            let canvas_ms = canvas_ready(&mut app, &ctx);
            assert!(
                app.parameter_gesture.is_some(),
                "Held edit must survive full workspace frames"
            );
            let generation = app.canvas_generation;
            let held = timings(|| frame(&mut app, &ctx, vec![]));
            assert_eq!(
                app.canvas_generation, generation,
                "Identical held edit must retain the accepted canvas"
            );
            let (w, h, actual) = app.canvas_pixels.as_ref().unwrap();
            let (_, _, mut bytes, _) = expected.preview(None, (*w).max(*h), None, false).unwrap();
            if wand {
                crate::selection::display::apply(&expected, *w, *h, &mut bytes, "cutout");
            }
            assert_eq!(
                actual, &bytes,
                "Actual canvas worker must display shared-engine pixels"
            );
            assert_eq!(
                serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap(),
                before_json
            );
            assert_eq!(
                app.shared.lock().unwrap().doc.layers[0].pixels.rgba16(),
                before_native
            );
            assert!(app.shared.lock().unwrap().undo.is_empty());
            assert!(app.shared.lock().unwrap().redo.is_empty());
            app.finish_parameter(true);
            cases.push(
                json!({"depth":depth,"control":if wand {"wand_feather"} else {"opacity"},
                "width":512,"height":512,"input":control,"direct_preview_ms":direct,
                "canvas_settle_wall_ms":canvas_ms,"identical_pending_frame_ms":held}),
            );
        }
    }
    println!(
        "\n{}",
        json!({"format":"PeerBrushUiCpuDiagnosticV1","build":build,
        "samples_per_series":SAMPLES,"cases":cases,"native_window":false,
        "limits":"CPU egui only; caches and background scheduling uncontrolled; no first-present or physical input latency"})
    );
}
