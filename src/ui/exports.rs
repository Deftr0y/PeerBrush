//! Production settings and preview remain outside document state and history.
use super::*;
use crate::production_export::{
    self as export, jobs::Existing, Control, Crop, DuplicateNames, Format, Resampling, Settings,
    Variant,
};
use std::time::Instant;
struct Pending {
    key: String,
    control: Control,
}

pub(super) struct Editor {
    pub(super) project: String,
    pub(super) document: String,
    pub(super) settings: Settings,
    pub(super) selected_only: bool,
    pub(super) targets: Vec<String>,
    pub(super) directory: String,
    pub(super) existing: Existing,
    pub(super) run: Option<String>,
    preset: Option<String>,
    name: String,
    error: Option<String>,
    desired: String,
    changed_at: Instant,
    pending: Option<Pending>,
    preview: Option<TextureHandle>,
    preview_output: Option<export::Output>,
    tx: mpsc::Sender<(String, Result<Value, String>)>,
    rx: mpsc::Receiver<(String, Result<Value, String>)>,
    folder_tx: mpsc::Sender<Option<PathBuf>>,
    folder_rx: mpsc::Receiver<Option<PathBuf>>,
    folder_pending: bool,
    pub(super) start_rect: Option<Rect>,
    pub(super) save_rect: Option<Rect>,
}
impl Drop for Editor {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.control.cancel();
        }
    }
}
impl Editor {
    fn source(&self, doc: &Document) -> Value {
        let targets = if self.selected_only {
            self.targets.clone()
        } else {
            vec![]
        };
        json!({"action":"preview","project_id":self.project,"document_id":doc.id,"expected_revision":doc.revision,"settings":self.settings,"targets":targets,"max_edge":512})
    }
    fn sync_preview(&mut self, doc: &Document) -> Value {
        let source = self.source(doc);
        let desired = json!({"request":source,"document_name":doc.name}).to_string();
        if desired != self.desired {
            self.desired = desired;
            self.changed_at = Instant::now();
            self.preview = None;
            self.preview_output = None;
            self.error = None;
            if let Some(pending) = &self.pending {
                pending.control.cancel();
            }
        }
        source
    }
}
impl PeerBrush {
    pub(super) fn open_exports(&mut self, doc: &Document) {
        if self.export_editor.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (folder_tx, folder_rx) = mpsc::channel();
        self.export_editor = Some(Editor {
            project: self.project_id.clone(),
            document: doc.id.clone(),
            settings: Settings::default(),
            selected_only: false,
            targets: self.selected_layer_ids(doc),
            directory: String::new(),
            existing: Existing::Error,
            run: export::jobs::states(&self.shared.lock().unwrap().export_library)
                .into_iter()
                .rev()
                .find(|state| state.project_id == self.project_id)
                .map(|state| state.run),
            preset: None,
            name: "Production".into(),
            error: None,
            desired: String::new(),
            changed_at: Instant::now(),
            pending: None,
            preview: None,
            preview_output: None,
            tx,
            rx,
            folder_tx,
            folder_rx,
            folder_pending: false,
            start_rect: None,
            save_rect: None,
        });
    }
    pub(super) fn exports_capture_ready(&self) -> bool {
        self.export_editor.as_ref().is_none_or(|editor| {
            editor.project != self.project_id
                || editor.document != self.doc_snapshot.id
                || (editor.pending.is_none()
                    && (editor.preview.is_some() || editor.error.is_some()))
        })
    }
    pub(super) fn exports_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(mut editor) = self.export_editor.take() else {
            return;
        };
        if self.lifecycle_frame || editor.project != self.project_id {
            self.export_editor = Some(editor);
            return;
        }
        editor.sync_preview(doc);
        while let Ok(folder) = editor.folder_rx.try_recv() {
            editor.folder_pending = false;
            if let Some(folder) = folder {
                editor.directory = folder.to_string_lossy().into();
            }
        }
        while let Ok((key, result)) = editor.rx.try_recv() {
            if editor
                .pending
                .as_ref()
                .is_some_and(|pending| pending.key == key)
            {
                editor.pending = None;
            }
            if key != editor.desired {
                continue;
            }
            match result {
                Ok(result) => {
                    let loaded = (|| -> Result<(TextureHandle, export::Output), String> {
                        use base64::Engine as _;
                        let image = &result["images"][0];
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(
                                image["data"]
                                    .as_str()
                                    .ok_or("Export preview pixels missing")?,
                            )
                            .map_err(|e| e.to_string())?;
                        let image = image::load_from_memory(&bytes)
                            .map_err(|e| e.to_string())?
                            .into_rgba8();
                        let texture = ctx.load_texture(
                            "production export preview",
                            egui::ColorImage::from_rgba_unmultiplied(
                                [image.width() as usize, image.height() as usize],
                                image.as_raw(),
                            ),
                            egui::TextureOptions::LINEAR,
                        );
                        let output = serde_json::from_value(result["plan"]["outputs"][0].clone())
                            .map_err(|e| e.to_string())?;
                        Ok((texture, output))
                    })();
                    match loaded {
                        Ok((texture, output)) => {
                            editor.preview = Some(texture);
                            editor.preview_output = Some(output);
                            editor.error = None;
                        }
                        Err(error) => editor.error = Some(error),
                    }
                }
                Err(error) => editor.error = Some(error),
            }
        }
        let library = self.shared.lock().unwrap().export_library.clone();
        let snapshot = library.lock().unwrap().snapshot();
        let run = editor
            .run
            .as_deref()
            .and_then(|id| export::jobs::state(&library, id).ok());
        let running = run.as_ref().is_some_and(|state| state.status == "running");
        let valid = editor.document == doc.id;
        let mut open = true;
        let mut save = false;
        let mut delete = false;
        let mut start = false;
        let mut cancel = false;
        egui::Window::new("Production export").id(egui::Id::new("production export")).open(&mut open).collapsible(false).default_width(800.).show(ctx,|ui| {
            ui.label(RichText::new(&doc.name).strong());
            ui.horizontal(|ui| {
                ui.label("Format");
                ui.selectable_value(&mut editor.settings.format,Format::Png,"PNG");
                ui.selectable_value(&mut editor.settings.format,Format::Jxl,"JPEG XL");
                ui.selectable_value(&mut editor.settings.format,Format::Webp,"WebP · lossless 8-bit");
            });
            ui.label(format!("{} · {}",match editor.settings.format {
                Format::Png=>format!("Native {}-bit output",doc.bit_depth),
                Format::Jxl=>format!("Native {}-bit JPEG XL output",doc.bit_depth),
                Format::Webp=>format!("8-bit output · {}-bit project unchanged",doc.bit_depth),
            },if editor.settings.format == Format::Jxl {
                if doc.icc_profile.is_none() && !doc.srgb_tagged { "Untagged RGB encoded as sRGB" }
                else if editor.settings.jxl.lossless { "ICC retained" }
                else { "Decoded output may convert to sRGB" }
            } else if doc.icc_profile.is_some()||doc.srgb_tagged { "ICC retained" } else { "untagged working RGB" }));
            if editor.settings.format == Format::Jxl {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut editor.settings.jxl.lossless, "Lossless");
                    if !editor.settings.jxl.lossless {
                        ui.label("Distance");
                        controls::range(ui, "JPEG XL distance", &mut editor.settings.jxl.distance,
                            0.1..=15., 110., "", 1, false);
                    }
                    ui.label("Effort");
                    let mut effort = editor.settings.jxl.effort as f32;
                    if controls::range(ui, "JPEG XL effort", &mut effort, 1.0..=9.,
                        95., "", 0, false).changed() {
                        editor.settings.jxl.effort = effort.round() as u32;
                    }
                });
                if !editor.settings.jxl.lossless {
                    ui.label(RichText::new("Lower distance preserves more detail. Alpha stays lossless.").small().color(MUTED));
                }
            }
            if !valid {
                ui.label(RichText::new("The document source changed. Settings are retained; choose the current document before exporting.").color(ACCENT));
                if ui.button("Use current document").clicked(){editor.document=doc.id.clone();editor.targets=self.selected_layer_ids(doc);editor.selected_only=false;}
            }
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("export preset").selected_text(editor.preset.as_ref().and_then(|id|snapshot.presets.iter().find(|p|&p.id==id)).map(|p|p.name.as_str()).unwrap_or("Unsaved settings")).show_ui(ui,|ui|{
                    if ui.selectable_label(editor.preset.is_none(),"Unsaved settings").clicked(){editor.preset=None;}
                    for preset in &snapshot.presets {if ui.selectable_label(editor.preset.as_ref()==Some(&preset.id),&preset.name).clicked(){editor.preset=Some(preset.id.clone());editor.name=preset.name.clone();editor.settings=preset.settings.clone();}}
                });
                if !snapshot.actor.is_empty() && snapshot.actor!="human" {ui.label(RichText::new("AI updated presets").color(AI_BLUE));}
                ui.text_edit_singleline(&mut editor.name);
                let response=ui.add_enabled(snapshot.error.is_none(),egui::Button::new("Save preset"));editor.save_rect=Some(response.rect);save=response.clicked();
                delete=ui.add_enabled(editor.preset.is_some()&&snapshot.error.is_none(),egui::Button::new("Delete preset")).clicked();
            });
            if let Some(error)=&snapshot.error {ui.label(RichText::new(error).color(ACCENT));}
            ui.columns(2,|columns|{
                let ui=&mut columns[0];
                ui.horizontal(|ui|{
                    ui.selectable_value(&mut editor.selected_only,false,"Document composite");
                    if ui.add_enabled(!doc.read_only,egui::Button::new("Selected layers/folders").selected(editor.selected_only)).clicked(){editor.selected_only=true;editor.targets=self.selected_layer_ids(doc);}
                });
                if editor.selected_only {
                    if ui.button("Use current layer selection").clicked(){editor.targets=self.selected_layer_ids(doc);}
                    let roots=crate::tree::roots(doc,&editor.targets.iter().cloned().collect());
                    for id in roots {if let Some(layer)=doc.layers.iter().find(|l|l.id==id){ui.label(&layer.name);}}
                    ui.label(RichText::new("Isolated on transparency. Ancestor folders and document filters are excluded; a clipped layer needs its containing folder.").small().color(MUTED));
                } else {ui.label(RichText::new("Current composite, including document filters.").small().color(MUTED));}
                ui.label("Filename · {document} / {layer} / {variant}");ui.text_edit_singleline(&mut editor.settings.template);
                ui.horizontal(|ui|{
                    ui.selectable_value(&mut editor.settings.crop,Crop::Canvas,"Canvas bounds");ui.selectable_value(&mut editor.settings.crop,Crop::Alpha,"Trim transparency");
                });
                ui.horizontal(|ui|{
                    ui.label("Padding");let mut padding=editor.settings.padding as f32;
                    if controls::range(ui,"export padding",&mut padding,0.0..=4096.,130.," px",0,false).changed(){editor.settings.padding=padding.round() as u32;}
                });
                ui.horizontal_wrapped(|ui|{
                    egui::ComboBox::from_id_salt("export sampling").selected_text(match editor.settings.resampling {Resampling::Nearest=>"Nearest sampling",Resampling::Bilinear=>"Bilinear sampling"}).show_ui(ui,|ui|{
                        ui.selectable_value(&mut editor.settings.resampling,Resampling::Nearest,"Nearest sampling");ui.selectable_value(&mut editor.settings.resampling,Resampling::Bilinear,"Bilinear sampling");
                    });
                    egui::ComboBox::from_id_salt("export duplicate names").selected_text(match editor.settings.duplicate_names {DuplicateNames::Error=>"Stop on duplicate names",DuplicateNames::Number=>"Number duplicate names"}).show_ui(ui,|ui|{
                        ui.selectable_value(&mut editor.settings.duplicate_names,DuplicateNames::Error,"Stop on duplicate names");ui.selectable_value(&mut editor.settings.duplicate_names,DuplicateNames::Number,"Number duplicate names");
                    });
                });
                ui.label("Size variants");let mut remove=None;
                for (index,variant) in editor.settings.variants.iter_mut().enumerate() {ui.push_id(index,|ui|{ui.horizontal(|ui|{
                    ui.add(egui::TextEdit::singleline(&mut variant.name).desired_width(130.));let mut scale=variant.scale as f32;
                    if controls::range(ui,"export scale",&mut scale,0.01..=8.,120.,"×",3,true).changed(){variant.scale=f64::from(scale);}
                    if ui.small_button("×").clicked(){remove=Some(index);}
                });});}
                if let Some(index)=remove {if editor.settings.variants.len()>1 {editor.settings.variants.remove(index);}}
                if ui.add_enabled(editor.settings.variants.len()<12,egui::Button::new("Add size variant")).clicked(){let count=editor.settings.variants.len()+1;editor.settings.variants.push(Variant {name:format!("size-{count}"),scale:1.});}
                editor.sync_preview(doc);
                let ui=&mut columns[1];
                ui.label("Actual output preview");
                if let Some(texture)=&editor.preview {
                    let size=texture.size_vec2();let scale=(ui.available_width()/size.x).min(250./size.y).min(1.);
                    let (rect,_)=ui.allocate_exact_size(size*scale,egui::Sense::hover());
                    let cell=12.;for y in 0..(rect.height()/cell).ceil() as i32 {for x in 0..(rect.width()/cell).ceil() as i32 {
                        let tile=Rect::from_min_size(rect.min+Vec2::new(x as f32*cell,y as f32*cell),Vec2::splat(cell)).intersect(rect);
                        ui.painter().rect_filled(tile,0,if (x+y)%2==0 {Color32::from_gray(45)} else {Color32::from_gray(57)});
                    }}
                    ui.painter().image(texture.id(),rect,Rect::from_min_max(Pos2::ZERO,Pos2::new(1.,1.)),Color32::WHITE);
                } else if editor.pending.is_some() {ui.spinner();ui.label("Rendering native source…");}
                if let Some(output)=&editor.preview_output {
                    ui.label(&output.filename);ui.label(format!("{} × {} · {} bit",output.width,output.height,output.bit_depth));
                    ui.label(format!("Document bounds: {}, {} → {}, {}",output.document_rect[0],output.document_rect[1],output.document_rect[2],output.document_rect[3]));
                }
                if let Some(state)=&run {
                    if state.document_id!=doc.id {ui.label(RichText::new("Batch from an earlier document source").small().color(MUTED));}
                    let color=if state.actor=="human" {ACCENT} else {AI_BLUE};ui.label(RichText::new(&state.stage).color(color));
                    if let Some(error)=&state.error {ui.label(RichText::new(error).color(ACCENT));}
                    egui::ScrollArea::vertical().id_salt("export receipts").max_height(160.).show(ui,|ui|{for receipt in &state.outputs {ui.label(format!("{} · {}",receipt.output.filename,receipt.status));if let Some(message)=&receipt.message {ui.label(RichText::new(message).small().color(MUTED));}}});
                }
            });
            ui.horizontal(|ui|{
                ui.label("Output folder");ui.text_edit_singleline(&mut editor.directory);
                if ui.add_enabled(!editor.folder_pending,egui::Button::new("Choose…")).clicked(){editor.folder_pending=true;let tx=editor.folder_tx.clone();let ctx=ctx.clone();std::thread::spawn(move||{let folder=rfd::FileDialog::new().pick_folder();let _=tx.send(folder);ctx.request_repaint();});}
            });
            ui.horizontal(|ui|{
                egui::ComboBox::from_id_salt("existing export files").selected_text(match editor.existing {Existing::Error=>"Stop on existing files",Existing::Skip=>"Skip existing files",Existing::Replace=>"Replace existing files"}).show_ui(ui,|ui|{
                    ui.selectable_value(&mut editor.existing,Existing::Error,"Stop on existing files");ui.selectable_value(&mut editor.existing,Existing::Skip,"Skip existing files");ui.selectable_value(&mut editor.existing,Existing::Replace,"Replace existing files");
                });
                let button=ui.add_enabled(valid&&!running&&!self.busy&&!editor.directory.trim().is_empty(),egui::Button::new("Export batch"));editor.start_rect=Some(button.rect);start=button.clicked();
                cancel=ui.add_enabled(running,egui::Button::new("Cancel batch")).clicked();
                ui.label(RichText::new("Completed exports are retained when a batch stops. Sources stay editable.").small().color(MUTED));
            });
            if let Some(error)=&editor.error {ui.label(RichText::new(error).color(ACCENT));}
        });
        if save {
            let mut request = json!({"action":"save_preset","actor":"human","name":editor.name,"settings":editor.settings,"expected_library_revision":snapshot.revision});
            if let Some(id) = &editor.preset {
                request["preset"] = json!(id);
            }
            match server::dispatch(&self.shared, "exports", &request) {
                Ok(result) => {
                    editor.preset = result["preset"]["id"].as_str().map(String::from);
                    editor.error = None;
                }
                Err(error) => editor.error = Some(error),
            }
        }
        if delete {
            if let Some(id) = &editor.preset {
                match server::dispatch(
                    &self.shared,
                    "exports",
                    &json!({"action":"delete_preset","actor":"human","preset":id,"expected_library_revision":snapshot.revision}),
                ) {
                    Ok(_) => {
                        editor.preset = None;
                        editor.error = None;
                    }
                    Err(error) => editor.error = Some(error),
                }
            }
        }
        let targets = if editor.selected_only {
            editor.targets.clone()
        } else {
            vec![]
        };
        let source = editor.sync_preview(doc);
        let desired = editor.desired.clone();
        if editor.selected_only && targets.is_empty() {
            editor.error = Some("Select layers or folders before exporting".into());
        }
        if start && (!editor.selected_only || !targets.is_empty()) {
            let mut request = source.clone();
            request["action"] = json!("start");
            request["actor"] = json!("human");
            request["directory"] = json!(editor.directory);
            request["existing"] = json!(editor.existing);
            match server::dispatch(&self.shared, "exports", &request) {
                Ok(state) => {
                    editor.run = state["run"].as_str().map(String::from);
                    editor.error = None;
                }
                Err(error) => editor.error = Some(error),
            }
        }
        if cancel {
            if let Some(run) = &editor.run {
                if let Err(error) = server::dispatch(
                    &self.shared,
                    "exports",
                    &json!({"action":"cancel","actor":"human","run":run}),
                ) {
                    editor.error = Some(error);
                }
            }
        }
        if open
            && valid
            && editor.pending.is_none()
            && editor.preview.is_none()
            && editor.error.is_none()
            && editor.changed_at.elapsed().as_secs_f32() > 0.15
        {
            let control = Control::default();
            editor.pending = Some(Pending {
                key: desired.clone(),
                control: control.clone(),
            });
            let shared = self.shared.clone();
            let tx = editor.tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    export::request_with_control(&shared, &source, &control)
                }))
                .unwrap_or_else(|_| {
                    Err("Export preview could not complete; source remains unchanged".into())
                });
                let _ = tx.send((desired, result));
                ctx.request_repaint();
            });
        }
        if open {
            ctx.request_repaint_after(Duration::from_millis(
                if running || editor.pending.is_some() {
                    40
                } else {
                    150
                },
            ));
            self.export_editor = Some(editor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let doc = app.shared.lock().unwrap().doc.clone();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                focused: true,
                events,
                ..Default::default()
            },
            |ctx| app.exports_window(ctx, &doc),
        )
    }
    fn click(app: &mut PeerBrush, ctx: &egui::Context, save: bool) {
        for _ in 0..4 {
            frame(app, ctx, vec![]);
        }
        let e = app.export_editor.as_ref().unwrap();
        let pos = if save { e.save_rect } else { e.start_rect }
            .unwrap()
            .center();
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
    }
    fn fixture(depth: u16) -> (PeerBrush, egui::Context, Shared, PathBuf) {
        let ctx = egui::Context::default();
        let mut engine = crate::engine::Engine::new();
        engine.doc = Document::new_depth(16, 12, depth).unwrap();
        engine.doc.name = "Production.psd".into();
        engine.doc.layers[0].name = "Art".into();
        engine.doc.layers[0]
            .pixels
            .set16(3, 4, [12347, 23459, 34571, 65535]);
        let root =
            std::env::temp_dir().join(format!("peerbrush-export-ui-{}", crate::engine::id()));
        std::fs::create_dir_all(&root).unwrap();
        engine.export_library = Arc::new(std::sync::Mutex::new(export::library::Library::load(
            root.join("presets.json"),
        )));
        let shared = Arc::new(std::sync::Mutex::new(engine));
        let mut app = PeerBrush::init(
            &ctx,
            shared.clone(),
            Connection {
                port: 0,
                token: String::new(),
                state_dir: root.clone(),
                instance_lock: None,
                availability: Default::default(),
            },
        );
        let doc = shared.lock().unwrap().doc.clone();
        app.open_exports(&doc);
        (app, ctx, shared, root)
    }
    fn preview(app: &mut PeerBrush, ctx: &egui::Context) {
        frame(app, ctx, vec![]);
        app.export_editor.as_mut().unwrap().changed_at = Instant::now() - Duration::from_secs(1);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            frame(app, ctx, vec![]);
            let e = app.export_editor.as_ref().unwrap();
            if e.preview.is_some() {
                return;
            }
            assert!(e.error.is_none(), "{:?}", e.error);
            assert!(
                Instant::now() < deadline,
                "Native export preview did not complete"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn click_text(app: &mut PeerBrush, ctx: &egui::Context, text: &str) {
        frame(app, ctx, vec![]);
        let output = frame(app, ctx, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(value) if value.galley.job.text == text => {
                    Some(value.pos + value.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Actual export control missing: {text}"));
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
    }
    #[test]
    fn native_export_preview_preset_and_batch_buttons_preserve_native_sources() {
        for depth in [8, 16] {
            let (mut app, ctx, shared, root) = fixture(depth);
            let doc = shared.lock().unwrap().doc.clone();
            let before = serde_json::to_value(&doc).unwrap();
            let editor = app.export_editor.as_mut().unwrap();
            editor.directory = root.to_string_lossy().into();
            editor.selected_only = true;
            editor.targets = vec![doc.layers[0].id.clone()];
            editor.settings.crop = Crop::Alpha;
            editor.settings.padding = 2;
            editor.settings.variants.push(Variant {
                name: "double".into(),
                scale: 2.,
            });
            preview(&mut app, &ctx);
            assert_eq!(
                app.export_editor
                    .as_ref()
                    .unwrap()
                    .preview_output
                    .as_ref()
                    .unwrap()
                    .document_rect,
                [1, 2, 6, 7]
            );
            click(&mut app, &ctx, true);
            assert!(
                app.export_editor.as_ref().unwrap().preset.is_some(),
                "{:?}",
                app.export_editor.as_ref().unwrap().error
            );
            assert!(root.join("presets.json").is_file());
            click(&mut app, &ctx, false);
            let run = app
                .export_editor
                .as_ref()
                .unwrap()
                .run
                .clone()
                .expect("Export button starts shared batch");
            let library = shared.lock().unwrap().export_library.clone();
            let mut state = export::jobs::state(&library, &run).unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while state.status == "running" {
                assert!(
                    Instant::now() < deadline,
                    "Native export batch did not complete"
                );
                std::thread::sleep(Duration::from_millis(2));
                frame(&mut app, &ctx, vec![]);
                state = export::jobs::state(&library, &run).unwrap();
            }
            assert_eq!(state.status, "completed", "{:?}", state.error);
            assert_eq!(state.outputs.len(), 2);
            for receipt in state.outputs {
                assert_eq!(receipt.status, "written");
                let settings = &app.export_editor.as_ref().unwrap().settings;
                assert_eq!(
                    std::fs::read(receipt.path).unwrap(),
                    export::render(&doc, &receipt.output, settings, &Control::default()).unwrap()
                );
                assert_eq!(receipt.output.bit_depth, depth);
            }
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                before
            );
            assert!(shared.lock().unwrap().undo.is_empty());
            drop(app);
            drop(shared);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn native_export_discards_stale_results_on_names_settings_and_source_changes() {
        let (mut app, ctx, shared, root) = fixture(16);
        preview(&mut app, &ctx);
        let old = app.export_editor.as_ref().unwrap().desired.clone();
        // Save As can rename a document without changing its pixel revision.
        shared.lock().unwrap().doc.name = "Renamed.psd".into();
        frame(&mut app, &ctx, vec![]);
        assert_ne!(app.export_editor.as_ref().unwrap().desired, old);
        assert!(app.export_editor.as_ref().unwrap().preview.is_none());
        preview(&mut app, &ctx);
        assert_eq!(
            app.export_editor
                .as_ref()
                .unwrap()
                .preview_output
                .as_ref()
                .unwrap()
                .filename,
            "Renamed_composite_native.png"
        );
        let editor = app.export_editor.as_mut().unwrap();
        let control = Control::default();
        editor.pending = Some(Pending {
            key: editor.desired.clone(),
            control: control.clone(),
        });
        editor.settings.padding = 3;
        frame(&mut app, &ctx, vec![]);
        assert!(control.check().is_err());
        assert!(app.export_editor.as_ref().unwrap().preview.is_none());
        let source = Document::new_depth(16, 12, 16).unwrap();
        shared.lock().unwrap().doc = source;
        app.export_editor.as_mut().unwrap().directory = root.to_string_lossy().into();
        click(&mut app, &ctx, false);
        assert!(app.export_editor.as_ref().unwrap().run.is_none());
        assert_eq!(app.export_editor.as_ref().unwrap().settings.padding, 3);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        drop(app);
        drop(shared);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn actual_derived_format_controls_preview_preset_and_batch_keep_native_sources() {
        for format in [Format::Webp, Format::Jxl] {
            for depth in [8, 16] {
                let (mut app, ctx, shared, root) = fixture(depth);
                let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
                click_text(
                    &mut app,
                    &ctx,
                    if format == Format::Jxl {
                        "JPEG XL"
                    } else {
                        "WebP · lossless 8-bit"
                    },
                );
                assert_eq!(app.export_editor.as_ref().unwrap().settings.format, format);
                if format == Format::Jxl {
                    assert!(app.export_editor.as_ref().unwrap().settings.jxl.lossless);
                    click_text(&mut app, &ctx, "Lossless");
                    assert!(!app.export_editor.as_ref().unwrap().settings.jxl.lossless);
                    for value in ["3", "1.0"] {
                        click_text(&mut app, &ctx, value);
                        frame(
                            &mut app,
                            &ctx,
                            vec![egui::Event::Key {
                                key: egui::Key::ArrowRight,
                                physical_key: Some(egui::Key::ArrowRight),
                                pressed: true,
                                repeat: false,
                                modifiers: Default::default(),
                            }],
                        );
                    }
                    let options = app.export_editor.as_ref().unwrap().settings.jxl;
                    assert!(options.effort > 3 && options.distance > 1.);
                }
                app.export_editor.as_mut().unwrap().directory = root.to_string_lossy().into();
                preview(&mut app, &ctx);
                let output = app
                    .export_editor
                    .as_ref()
                    .unwrap()
                    .preview_output
                    .as_ref()
                    .unwrap();
                assert_eq!(
                    output.bit_depth,
                    if format == Format::Webp { 8 } else { depth }
                );
                assert_eq!(output.native_source_depth, depth);
                let texture = app
                    .export_editor
                    .as_ref()
                    .unwrap()
                    .preview
                    .as_ref()
                    .unwrap()
                    .id();
                let shown = frame(&mut app, &ctx, vec![]);
                assert!(shown.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Mesh(mesh)if mesh.texture_id==texture)),"Actual encoded-output preview is painted");
                click(&mut app, &ctx, true);
                let library = export::library::Library::load(root.join("presets.json"));
                assert_eq!(
                    library.snapshot().presets[0].settings,
                    app.export_editor.as_ref().unwrap().settings
                );
                click(&mut app, &ctx, false);
                let run = app.export_editor.as_ref().unwrap().run.as_deref().unwrap();
                let library = shared.lock().unwrap().export_library.clone();
                let mut state = export::jobs::state(&library, run).unwrap();
                let deadline = Instant::now() + Duration::from_secs(15);
                while state.status == "running" {
                    assert!(
                        Instant::now() < deadline,
                        "Derived export batch did not complete"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                    state = export::jobs::state(&library, run).unwrap();
                }
                assert_eq!(state.status, "completed", "{:?}", state.error);
                assert_eq!(state.outputs[0].output.format, format);
                let bytes = std::fs::read(&state.outputs[0].path).unwrap();
                if format == Format::Webp {
                    assert_eq!(&bytes[8..12], b"WEBP");
                    assert_eq!(
                        image::load_from_memory(&bytes).unwrap().color(),
                        image::ColorType::Rgba8
                    );
                } else {
                    let decoded = crate::jpeg_xl::decode(&bytes, &|| Ok(())).unwrap();
                    assert_eq!(
                        (
                            decoded.info.width,
                            decoded.info.height,
                            decoded.info.bit_depth
                        ),
                        (16, 12, depth)
                    );
                    let expected =
                        image::load_from_memory(&shared.lock().unwrap().doc.export_png().unwrap())
                            .unwrap()
                            .into_rgba16()
                            .into_raw();
                    let actual = match decoded.pixels {
                        crate::jpeg_xl::Pixels::Eight(bytes) => bytes
                            .into_iter()
                            .map(|v| u16::from(v) * 257)
                            .collect::<Vec<_>>(),
                        crate::jpeg_xl::Pixels::Sixteen(words) => words,
                    };
                    assert!(actual
                        .iter()
                        .skip(3)
                        .step_by(4)
                        .eq(expected.iter().skip(3).step_by(4)));
                    if depth == 16 {
                        assert!(actual
                            .chunks_exact(4)
                            .any(|p| p[3] != 0 && p[..3].iter().any(|v| v % 257 != 0)));
                    }
                }
                assert_eq!(
                    serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                    before
                );
                assert!(shared.lock().unwrap().undo.is_empty());
                drop(app);
                drop(shared);
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }
}
