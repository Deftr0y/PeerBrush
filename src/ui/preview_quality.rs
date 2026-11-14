//! Display preferences never change document samples or edit commands.
use super::*;

pub(super) const SETTLE_SECONDS: f64 = 0.15;

pub(super) fn display_edge(rect: Option<Rect>, pixels_per_point: f32) -> u32 {
    rect.map_or(1536, |rect| {
        (rect.width().max(rect.height()) * pixels_per_point).ceil() as u32
    })
}

pub(super) fn region_display_edge(
    canvas: Option<Rect>,
    document_width: u32,
    region_edge: u32,
    pixels_per_point: f32,
) -> u32 {
    let Some(canvas) = canvas else { return display_edge(None, pixels_per_point) };
    let pixels_per_sample = f64::from(canvas.width()) * f64::from(pixels_per_point)
        / f64::from(document_width);
    if pixels_per_sample >= 1. - f64::from(f32::EPSILON) {
        return region_edge;
    }
    (f64::from(region_edge) * pixels_per_sample).ceil() as u32
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    version: u32,
    reduced_resolution: bool,
}

fn decode(bytes: &[u8]) -> Result<Preference, String> {
    let value: Preference = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if value.version != 1 {
        return Err("Unsupported preview preferences".into());
    }
    Ok(value)
}

pub(crate) fn validate_file(path: &Path) -> Result<(), String> {
    if let Some(bytes) = crate::tool_settings::read_owned(path)? {
        decode(&bytes)?;
    }
    Ok(())
}

pub(super) struct Quality {
    path: Option<PathBuf>,
    original: Option<Vec<u8>>,
    protected: bool,
    reduced: bool,
    pub open: bool,
    error: Option<String>,
    changing: Option<(Vec<u8>, f64)>,
}

impl Quality {
    pub fn reduced(&self) -> bool {
        self.reduced
    }
    pub fn load(preferences: &Path) -> Self {
        let mut this = Self {
            path: (!preferences.as_os_str().is_empty()).then(|| preferences.join("preview.json")),
            original: None,
            protected: false,
            reduced: false,
            open: false,
            error: None,
            changing: None,
        };
        if let Some(path) = &this.path {
            let loaded = (|| {
                this.original = crate::tool_settings::read_owned(path)?;
                let reduced = this
                    .original
                    .as_deref()
                    .map(decode)
                    .transpose()?
                    .is_some_and(|value| value.reduced_resolution);
                if crate::tool_settings::read_owned(path)? != this.original {
                    return Err("Preview preferences changed while loading".into());
                }
                Ok::<_, String>(reduced)
            })();
            match loaded {
                Ok(reduced) => this.reduced = reduced,
                Err(error) => {
                    this.protected = true;
                    this.error = Some(error);
                }
            }
        }
        this
    }

    fn set(&mut self, reduced: bool) -> Result<(), String> {
        if self.protected {
            return Err(
                "Preview preferences remain protected; repair or restore the file before saving"
                    .into(),
            );
        }
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec_pretty(&Preference {
                version: 1,
                reduced_resolution: reduced,
            })
            .map_err(|e| e.to_string())?;
            crate::tool_settings::write_owned(
                path,
                &bytes,
                self.original.as_deref(),
                "Preview preferences changed on disk; restart before saving",
            )?;
            self.original = Some(bytes);
        }
        self.reduced = reduced;
        self.changing = None;
        Ok(())
    }

    pub fn reduced_edge(
        &self,
        document_edge: u32,
        display_edge: u32,
        editing: bool,
    ) -> Option<u32> {
        (self.reduced && editing && document_edge > display_edge.max(512))
            .then(|| display_edge.clamp(512, 1536))
    }

    pub fn edge(&mut self, key: &[u8], time: f64, reduced_edge: Option<u32>, full_edge: u32) -> u32 {
        let Some(edge) = reduced_edge else {
            self.changing = None;
            return full_edge;
        };
        if self.changing.as_ref().is_none_or(|(old, _)| old.as_slice() != key) {
            self.changing = Some((key.into(), time));
        }
        if time - self.changing.as_ref().unwrap().1 < SETTLE_SECONDS {
            edge
        } else {
            full_edge
        }
    }

    pub fn repaint_when_settled(&self, ctx: &egui::Context, time: f64, is_reduced: bool) {
        if let Some((_, started)) = self.changing.as_ref().filter(|_| is_reduced) {
            let remaining = (started + SETTLE_SECONDS - time).clamp(0., SETTLE_SECONDS);
            ctx.request_repaint_after(Duration::from_secs_f64(remaining));
        }
    }
}

impl PeerBrush {
    pub(super) fn set_preview_quality(&mut self, reduced: bool) -> Result<(), String> {
        self.preview_quality.set(reduced)?;
        // Reject queued lower-quality frames, even within the same gesture.
        self.preview_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pending = false;
        self.last_preview = None;
        if let Some(editor) = &mut self.free_transform {
            editor.rendered = None;
        }
        self.clear_tab_transfer_preview();
        self.interrupt_animation();
        self.native_dispatch.clear_frames();
        Ok(())
    }

    pub(super) fn preview_quality_window(&mut self, ctx: &egui::Context) {
        if !self.preview_quality.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings").open(&mut open).resizable(false).default_width(420.).show(ctx, |ui| {
            ui.set_max_width(420.);
            controls::label(ui, "Canvas preview quality");
            let reduced = self.preview_quality.reduced();
            ui.add_enabled_ui(!self.preview_quality.protected, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (value, icon, label, help) in [
                        (false, Icon::FinePixels, "Full resolution", "Render at full detail during editing."),
                        (true, Icon::CoarsePixels, "Adaptive resolution", "Use fewer pixels during changes when the canvas is zoomed out."),
                    ] {
                        if icons::labeled_choice(ui, icon, label, Some(reduced == value), 0.)
                            .on_hover_text(help).clicked() && reduced != value
                        {
                            let error = self.set_preview_quality(value).err();
                            self.preview_quality.error = error;
                        }
                    }
                });
            });
            if self.preview_quality.reduced() {
                ui.label("Full detail returns when the edit settles.");
            }
            if let Some(error) = &self.preview_quality.error {
                ui.colored_label(ACCENT, error);
            }
        });
        self.preview_quality.open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    #[test]
    fn preview_opt_in_persists_migrates_and_protects_external_preferences() {
        let root = std::env::temp_dir().join(format!("peerbrush-preview-{}", crate::engine::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("preview.json");
        let mut quality = Quality::load(&root);
        assert_eq!(quality.reduced_edge(4096, 1024, true), None);
        assert!(!path.exists());
        quality.set(true).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            Quality::load(&root).reduced_edge(4096, 1024, true),
            Some(1024)
        );
        assert_eq!(quality.reduced_edge(4096, 1024, false), None);
        assert_eq!(quality.reduced_edge(2048, 2048, true), None);
        assert_eq!(quality.reduced_edge(4096, 2048, true), Some(1536));
        let locations = crate::settings::Locations {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
        };
        let profile = crate::settings::Profile::migrate(locations, &root).unwrap();
        assert!(Quality::load(&profile.locations.profile()).reduced);
        assert_eq!(
            std::fs::read(
                profile
                    .locations
                    .profile()
                    .join("migration-backup/preview.json")
            )
            .unwrap(),
            bytes
        );
        drop(profile);
        quality.set(false).unwrap();
        assert!(!Quality::load(&root).reduced);
        let external = br#"{"version":2,"reduced_resolution":true}"#;
        std::fs::write(&path, external).unwrap();
        assert!(quality.set(true).is_err());
        let mut protected = Quality::load(&root);
        assert!(protected.protected);
        assert!(!protected.reduced);
        assert!(protected.set(true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), external);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resize_preview_uses_the_draft_extent_and_settles_without_changing_the_source() {
        for depth in [8, 16] {
            for (start, finish, reduced, first_width, first_edge) in [
                (1024, 2048, false, 2048, 8192),
                (1024, 2048, true, 1536, 1536),
                (2048, 1024, true, 1024, 8192),
            ] {
                let (mut app, ctx) = super::super::tests::fixture();
                app.preview_quality = Quality::load(Path::new(""));
                app.set_preview_quality(reduced).unwrap();
                let before = {
                    let mut engine = app.shared.lock().unwrap();
                    let resident = engine.resident_state_mut().unwrap();
                    resident.document = Document::new_depth(start, 8, depth).unwrap();
                    resident.document.layers[0].pixels.set16(start as i32 / 2, 4, [12347, 33559, 51237, 65535]);
                    resident.document.clone()
                };
                app.open_geometry(&before, "canvas.resize");
                let editor = app.geometry.as_mut().unwrap();
                editor.width = finish;
                app.transient = vec![editor.command()];
                let expected = Engine::preview_edits(before.clone(), &app.transient).unwrap();
                app.request_preview(&ctx, &before);
                let first = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(first.error.is_none(), "{:?}", first.error);
                assert_eq!(first.w, first_width);
                assert_eq!(first.target.rsplit(';').next().and_then(|edge| edge.parse().ok()), Some(first_edge));
                let settled = if first_edge != 8192 {
                    app.preview_tx.send(first).unwrap();
                    let time = ctx.input(|input| input.time) + 0.2;
                    let _ = ctx.run(egui::RawInput { time: Some(time), ..Default::default() },
                        |ctx| app.request_preview(ctx, &before));
                    app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap()
                } else {
                    first
                };
                assert!(settled.error.is_none(), "{:?}", settled.error);
                assert_eq!((settled.w, settled.h), (finish, 8));
                assert_eq!(settled.bytes, expected.preview(None, 8192, None, false).unwrap().2);
                let engine = app.shared.lock().unwrap();
                let resident = engine.resident_state().unwrap();
                assert_eq!(resident.document.layers[0].pixels, before.layers[0].pixels);
                assert_eq!((resident.document.width, resident.document.height), (start, 8));
                assert!(resident.undo.is_empty());
            }
        }
    }

    #[test]
    fn resident_stroke_quality_settles_from_the_exact_latest_samples() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.preview_quality = Quality::load(Path::new(""));
            app.set_preview_quality(true).unwrap();
            let before = Document::new_depth(2048, 32, depth).unwrap();
            app.shared.lock().unwrap().resident_state_mut().unwrap().document = before.clone();
            app.selected.clone_from(&before.layers[0].id);
            app.tool = Tool::Brush;
            app.radius = 2.;
            app.live_gesture = Some("quality stroke".into());
            app.points = vec![[30., 14.]; 1024];
            app.points[1023] = [90., 16.];
            app.stroke_pressures = vec![0.65; app.points.len()];
            app.stroke_has_pressure = true;
            let request = |app: &mut PeerBrush, time| {
                let _ = ctx.run(egui::RawInput { time: Some(time), ..Default::default() },
                    |ctx| app.request_preview(ctx, &before));
            };

            request(&mut app, 1.);
            let first = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(first.error.is_none());
            assert_eq!(first.w, 1536);
            let original = app.resident_preview_stroke.clone().unwrap();
            assert_eq!(Some(original.command()), app.preview_stroke());
            request(&mut app, 1.08);
            assert!(Arc::ptr_eq(&original, app.resident_preview_stroke.as_ref().unwrap()));
            assert_eq!(app.preview_job, first.job);

            app.points[420] = [48., 9.];
            request(&mut app, 1.14);
            let moved = app.resident_preview_stroke.clone().unwrap();
            assert!(!Arc::ptr_eq(&original, &moved));
            assert_eq!(Some(moved.command()), app.preview_stroke());
            app.stroke_pressures[420] = 0.9;
            request(&mut app, 1.20);
            let pressure = app.resident_preview_stroke.clone().unwrap();
            assert!(!Arc::ptr_eq(&moved, &pressure));
            assert_eq!(Some(pressure.command()), app.preview_stroke());
            assert_eq!(app.preview_job, first.job);

            app.radius = 3.;
            app.preview_tx.send(first).unwrap();
            request(&mut app, 1.24);
            let reduced = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(reduced.error.is_none());
            assert_eq!(reduced.w, 1536);
            let latest = app.resident_preview_stroke.as_ref().unwrap();
            assert!(!Arc::ptr_eq(&pressure, latest));
            let command = latest.command();
            assert_eq!(Some(command.clone()), app.preview_stroke());
            let expected = Engine::preview_edits(before.clone(), &[command]).unwrap();

            app.preview_tx.send(reduced).unwrap();
            request(&mut app, 1.44);
            let settled = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(settled.error.is_none());
            assert!(settled.patch.is_none());
            assert_eq!((settled.w, settled.h), (2048, 32));
            assert_eq!(settled.bytes, expected.preview(None, 8192, None, false).unwrap().2);
            let engine = app.shared.lock().unwrap();
            let resident = engine.resident_state().unwrap();
            assert_eq!(resident.document.layers[0].pixels, before.layers[0].pixels);
            assert_eq!(resident.document.revision, before.revision);
            assert!(resident.undo.is_empty());
        }
    }

    #[test]
    fn resident_mask_refinement_previews_the_requested_mask_at_full_resolution() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.preview_quality = Quality::load(Path::new(""));
            let mut original = Document::new_depth(768, 8, depth).unwrap();
            original.layers[0].pixels.set16(700, 3, [12347, 33559, 51237, 65535]);
            let layer = original.layers[0].id.clone();
            let before = Engine::preview_edits(original, &[json!({"op":"mask.add","layer":layer,"value":128})]).unwrap();
            app.shared.lock().unwrap().resident_state_mut().unwrap().document = before.clone();
            app.open_refinement(&before, Some(layer.clone()));
            let editor = app.refinement.as_mut().unwrap();
            editor.preview = "mask".into();
            editor.values[1] = 1.;
            app.transient = vec![editor.command()];
            let draft = Engine::preview_edits(before.clone(), &app.transient).unwrap();
            let (_, _, mask, _) = draft.preview(None, 8192, Some(&layer), true).unwrap();
            assert_ne!(mask, draft.preview(None, 8192, None, false).unwrap().2);
            app.request_preview(&ctx, &before);
            let frame = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(frame.error.is_none(), "{:?}", frame.error);
            assert_eq!((frame.w, frame.h), (768, 8));
            assert!(frame.patch.is_none());
            assert_eq!(frame.bytes, mask);
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.resident_state().unwrap().document.revision, before.revision);
            assert!(engine.resident_state().unwrap().undo.is_empty());
        }
    }

    #[test]
    fn ai_fades_yield_to_refinement_and_human_interruptions() {
        for depth in [8, 16] {
            for reduced in [false, true] {
                let (mut app, ctx) = super::super::tests::fixture();
                app.preview_quality = Quality::load(Path::new(""));
                app.set_preview_quality(reduced).unwrap();
                let (before, after, change) = {
                    let mut engine = app.shared.lock().unwrap();
                    let resident = engine.resident_state_mut().unwrap();
                    resident.document = Document::new_depth(2048, 8, depth).unwrap();
                    resident.document.layers[0].pixels.set16(2047, 4, [65535, 0, 0, 65535]);
                    let before = resident.document.clone();
                    let layer = before.layers[0].id.clone();
                    engine.edit("agent", &[json!({
                        "op":"paint.fill","layer":layer,"rect":[2047,4,2048,5],"color":[0,0,255,255]
                    })], None, None, "Paint preview test").unwrap();
                    (before, engine.resident_state().unwrap().document.clone(), engine.ai_change.clone())
                };
                assert!(change.is_some());
                let edge = if reduced { 1536 } else { crate::raster::MAX_EDGE };
                let (width, height, bytes, _) = before.preview(None, edge, None, false).unwrap();
                assert!(app.set_canvas(&ctx, width, height, bytes));
                app.animation.observe(&before, None, 0.);
                assert!(app.animation.observe(&after, change, 1.));
                let draw = |app: &mut PeerBrush, time| {
                    let _ = ctx.run(egui::RawInput { time: Some(time), ..Default::default() },
                        |ctx| app.request_preview(ctx, &after));
                };
                draw(&mut app, 1.);
                let first = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(first.error.is_none(), "{:?}", first.error);
                assert_eq!(first.w, width);
                app.preview_tx.send(first).unwrap();
                draw(&mut app, 1.03);
                assert!(app.animation.fading());
                assert!(!app.canvas_capture_ready((&after).into()));
                if !reduced {
                    app.interrupt_animation();
                    assert!(!app.canvas_capture_ready((&after).into()));
                }
                draw(&mut app, 1.2);
                let full = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(full.error.is_none(), "{:?}", full.error);
                assert_eq!((full.w, full.h), (2048, 8));
                app.preview_tx.send(full).unwrap();
                let expected = after.preview(None, crate::raster::MAX_EDGE, None, false).unwrap().2;
                for time in [1.23, 1.3, 1.5] {
                    draw(&mut app, time);
                    assert!(!app.animation.fading());
                    assert!(app.canvas_capture_ready((&after).into()));
                    assert_eq!(app.canvas_pixels.as_ref().unwrap().2, expected);
                }
                let engine = app.shared.lock().unwrap();
                assert_eq!(engine.resident_state().unwrap().document.revision, after.revision);
                assert_eq!(engine.resident_state().unwrap().undo.len(), 1);
            }
        }
    }

    #[test]
    fn native_depth_edit_previews_finish_full_size_without_mutating_source_or_history() {
        for depth in [8, 16] {
            for (reduced, switch_off) in [(false, false), (true, false), (true, true)] {
                let (mut app, ctx) = super::super::tests::fixture();
                app.preview_quality = Quality::load(Path::new(""));
                app.preview_quality.set(reduced).unwrap();
                let before = {
                    let mut engine = app.shared.lock().unwrap();
                    engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(2048, 8, depth).unwrap();
                    engine.resident_state_mut().expect("resident fixture").document.layers[0]
                        .pixels
                        .set16(2047, 4, [12347, 33559, 51237, 65535]);
                    engine.resident_state().expect("resident fixture").document.clone()
                };
                app.selected = before.layers[0].id.clone();
                app.transient =
                    vec![json!({"op":"layer.update","layer":app.selected,"opacity":0.5})];
                let expected = Engine::preview_edits(before.clone(), &app.transient).unwrap();
                app.request_preview(&ctx, &before);
                let first = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                let final_frame = if reduced {
                    assert_eq!(first.w, 1536);
                    if switch_off {
                        app.set_canvas(&ctx, 1, 1, vec![1, 2, 3, 255]);
                        app.set_preview_quality(false).unwrap();
                        app.preview_tx.send(first).unwrap();
                        app.request_preview(&ctx, &before);
                        assert_eq!(app.canvas_pixels.as_ref().unwrap().2, vec![1, 2, 3, 255]);
                    } else {
                        app.preview_tx.send(first).unwrap();
                        let start = ctx.input(|input| input.time);
                        let _ = ctx.run(
                            egui::RawInput {
                                time: Some(start + 0.05),
                                ..Default::default()
                            },
                            |ctx| app.request_preview(ctx, &before),
                        );
                        assert_eq!(app.canvas_pixels.as_ref().unwrap().0, 1536);
                        assert!(!app.pending);
                        assert!(!app.canvas_capture_ready((&before).into()));
                        let _ = ctx.run(
                            egui::RawInput {
                                time: Some(start + 0.2),
                                ..Default::default()
                            },
                            |ctx| {
                                app.request_preview(ctx, &before);
                            },
                        );
                    }
                    app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap()
                } else {
                    first
                };
                assert!(!final_frame.coarse);
                assert!(final_frame.error.is_none(), "{:?}", final_frame.error);
                assert_eq!((final_frame.w, final_frame.h), (2048, 8));
                assert_eq!(
                    final_frame.bytes,
                    expected.preview(None, 8192, None, false).unwrap().2
                );
                app.preview_tx.send(final_frame).unwrap();
                let time = ctx.input(|input| input.time) + 0.2;
                let _ = ctx.run(
                    egui::RawInput {
                        max_texture_side: Some(1024),
                        time: Some(time),
                        ..Default::default()
                    },
                    |ctx| app.request_preview(ctx, &before),
                );
                assert_eq!(app.texture.as_ref().unwrap().size(), [2048, 8]);
                assert!(app.canvas_capture_ready((&before).into()));
                let engine = app.shared.lock().unwrap();
                assert_eq!(
                    serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
                assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
            }
        }
    }
}
