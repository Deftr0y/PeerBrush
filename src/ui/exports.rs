//! Production settings and preview remain outside document state and history.
use super::*;
use crate::production_export::{
    self as export, jobs::Existing, Control, Crop, DuplicateNames, Format, Settings,
    Variant,
};
use std::time::Instant;

const PREVIEW_DELAY: Duration = Duration::from_millis(150);
const BATCH_REFRESH: Duration = Duration::from_millis(40);

fn filename_template(ui: &mut egui::Ui, template: &mut String) {
    let label = ui.label("Filename template");
    let mut input = egui::TextEdit::singleline(template)
        .desired_width(ui.available_width())
        .show(ui);
    input.response.labelled_by(label.id);
    let mut insert = None;
    ui.horizontal_wrapped(|ui| {
        for (icon, label, token, help) in [
            (Icon::CanvasBounds, "Document", "{document}", "Insert the document name."),
            (Icon::SingleLayer, "Layer", "{layer}", "Insert the output layer or folder name."),
            (Icon::Scale, "Size variant", "{variant}", "Insert the name of the output size variant."),
        ] {
            if icons::labeled_choice(ui, icon, label, None, 0.)
                .on_hover_text(help)
                .clicked()
            {
                insert = Some(token);
            }
        }
    });
    if let Some(token) = insert {
        let count = template.chars().count();
        let [start, end] = input.cursor_range.map_or([count; 2], |range| {
            let a = range.primary.index.min(count);
            let b = range.secondary.index.min(count);
            [a.min(b), a.max(b)]
        });
        let offset = |index| template.char_indices().nth(index).map_or(template.len(), |(i, _)| i);
        let bytes = offset(start)..offset(end);
        template.replace_range(bytes, token);
        use egui::text::{CCursor, CCursorRange};
        input.state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(start + token.len()))));
        input.state.store(ui.ctx(), input.response.id);
        input.response.request_focus();
        ui.ctx().request_repaint();
    }
}

struct PreviewSource {
    document: export::ObservedDocument,
    name: String,
    settings: Settings,
    selected_only: bool,
    targets: Vec<String>,
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
    preview_source: Option<PreviewSource>,
    changed_at: Instant,
    pending: Option<Control>,
    preview: Option<TextureHandle>,
    preview_output: Option<export::Output>,
    preview_plan: Option<Arc<export::DesktopPlan>>,
    preview_index: usize,
    preview_choice: Option<(Option<String>, String)>,
    preview_encoded_bytes: Option<usize>,
    tx: mpsc::Sender<Result<export::DesktopPreview, String>>,
    rx: mpsc::Receiver<Result<export::DesktopPreview, String>>,
    folder_tx: mpsc::Sender<Result<Option<PathBuf>, String>>,
    folder_rx: mpsc::Receiver<Result<Option<PathBuf>, String>>,
    folder_pending: bool,
    folder_error: Option<String>,
    pub(super) start_rect: Option<Rect>,
    pub(super) save_rect: Option<Rect>,
}
impl Drop for Editor {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.cancel();
        }
    }
}
impl Editor {
    fn targets(&self) -> &[String] {
        if self.selected_only {
            &self.targets
        } else {
            &[]
        }
    }
    #[cfg(test)]
    fn source<'a>(&self, doc: impl Into<DocumentView<'a>>) -> Value {
        let doc = doc.into();
        json!({"action":"preview","project_id":self.project,"document_id":doc.id(),"expected_revision":doc.revision(),"settings":self.settings,"targets":self.targets(),"output_index":self.preview_index,"max_edge":512})
    }
    fn sync_preview(&mut self, doc: DocumentView<'_>) {
        if self.preview_source.as_ref().is_some_and(|source| {
            source.document.observes(doc)
                && source.name == doc.name()
                && source.settings == self.settings
                && source.selected_only == self.selected_only
                && source.targets.as_slice() == self.targets()
        }) {
            return;
        }
        self.preview_source = Some(PreviewSource {
            document: export::ObservedDocument::new(doc),
            name: doc.name().into(),
            settings: self.settings.clone(),
            selected_only: self.selected_only,
            targets: self.targets().to_vec(),
        });
        self.preview_plan = None;
        self.preview_index = 0;
        self.clear_preview();
    }
    fn clear_preview(&mut self) {
        self.changed_at = Instant::now();
        self.preview = None;
        self.preview_output = None;
        self.preview_encoded_bytes = None;
        self.error = None;
        if let Some(pending) = &self.pending {
            pending.cancel();
        }
    }
}
impl PeerBrush {
    pub(super) fn open_exports<'a>(&mut self, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        if self.export_editor.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (folder_tx, folder_rx) = mpsc::channel();
        self.export_editor = Some(Editor {
            project: self.project_id.clone(),
            document: doc.id().to_owned(),
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
            preview_source: None,
            changed_at: Instant::now(),
            pending: None,
            preview: None,
            preview_output: None,
            preview_plan: None,
            preview_index: 0,
            preview_choice: None,
            preview_encoded_bytes: None,
            tx,
            rx,
            folder_tx,
            folder_rx,
            folder_pending: false,
            folder_error: None,
            start_rect: None,
            save_rect: None,
        });
    }
    pub(super) fn exports_capture_ready(&self) -> bool {
        self.export_editor.as_ref().is_none_or(|editor| {
            editor.project != self.project_id
                || editor.document != DocumentView::from(&self.doc_snapshot).id()
                || (editor.pending.is_none()
                    && (editor.preview.is_some() || editor.error.is_some()))
        })
    }
    pub(super) fn exports_window<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
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
            match folder {
                Ok(Some(folder)) => editor.directory = folder.to_string_lossy().into(),
                Ok(None) => {}
                Err(error) => editor.folder_error = Some(error),
            }
        }
        while let Ok(result) = editor.rx.try_recv() {
            // Each panel has its own channel and only starts a worker after the previous reply.
            let Some(pending) = editor.pending.take() else {
                continue;
            };
            // Returning to the same settings does not revive a canceled request.
            if pending.check().is_err() {
                continue;
            }
            let result = result.and_then(|reply| {
                editor.preview_index = reply.index;
                editor.preview_choice = reply.plan.outputs().get(reply.index)
                    .map(|output| (output.target.clone(), output.variant.clone()));
                editor.preview_plan = Some(reply.plan);
                reply.pixels
            });
            match result {
                Ok(pixels) => {
                    editor.preview_encoded_bytes = pixels.encoded_bytes;
                    editor.preview = Some(ctx.load_texture(
                        "production export preview",
                        egui::ColorImage::from_rgba_unmultiplied(
                            [pixels.width as usize, pixels.height as usize],
                            &pixels.rgba,
                        ),
                        egui::TextureOptions::LINEAR,
                    ));
                    editor.preview_output = editor.preview_plan.as_ref()
                        .and_then(|plan| plan.outputs().get(editor.preview_index))
                        .cloned();
                    editor.error = None;
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
        let valid = editor.document == doc.id();
        let mut open = true;
        let mut save = false;
        let mut delete = false;
        let mut start = false;
        let mut cancel = false;
        egui::Window::new("Production export").id(egui::Id::new("production export")).open(&mut open).collapsible(false).default_width(800.).show(ctx,|ui| {
            ui.label(RichText::new(doc.name()).strong());
            ui.horizontal_wrapped(|ui| {
                controls::label(ui, "Format");
                for (format, label, help) in [
                    (Format::Png, "PNG", "Lossless PNG at the project’s channel depth."),
                    (Format::Jxl, "JPEG XL", "Lossless or lossy JPEG XL at the project’s channel depth."),
                    (Format::Webp, "WebP · lossless 8-bit", "Lossless WebP with 8-bit output channels."),
                ] {
                    ui.selectable_value(&mut editor.settings.format, format, label).on_hover_text(help);
                }
            });
            if editor.settings.format == Format::Jxl {
                ui.horizontal_wrapped(|ui| {
                    controls::label(ui, "Compression");
                    for (lossless, icon, label, help) in [
                        (true, Icon::PreserveSamples, "Lossless", "Preserve every output sample."),
                        (false, Icon::QuantizeSamples, "Lossy", "Allow changes to color samples for a smaller file. Alpha stays lossless."),
                    ] {
                        if icons::labeled_choice(ui, icon, label, Some(editor.settings.jxl.lossless == lossless), 0.)
                            .on_hover_text(help).clicked()
                        {
                            editor.settings.jxl.lossless = lossless;
                        }
                    }
                    if !editor.settings.jxl.lossless {
                        controls::label(ui, "Distance");
                        controls::range(ui, "JPEG XL distance", &mut editor.settings.jxl.distance,
                            0.1..=15., 110., "", 1, true)
                            .on_hover_text("Lower distance preserves more detail. Double-click to enter an exact value.");
                    }
                    controls::label(ui, "Effort");
                    let mut effort = editor.settings.jxl.effort as f32;
                    if controls::range(ui, "JPEG XL effort", &mut effort, 1.0..=9.,
                        95., "", 0, false)
                        .on_hover_text("Higher effort takes longer and can reduce file size. Double-click to enter an exact value.")
                        .changed() {
                        editor.settings.jxl.effort = effort.round() as u32;
                    }
                });
            }
            let output_depth = if editor.settings.format == Format::Webp { 8 } else { doc.depth() };
            let tagged = doc.profile().is_some() || doc.srgb_tagged();
            let profile = match editor.settings.format {
                Format::Jxl if !tagged => "Untagged RGB encoded as sRGB",
                Format::Jxl if !editor.settings.jxl.lossless => "Decoded output may convert to sRGB",
                _ if tagged => "ICC retained",
                _ => "Untagged working RGB",
            };
            ui.label(format!("{output_depth}-bit output · {profile}"));
            if !valid {
                ui.label(RichText::new("The document source changed. Settings are retained; choose the current document before exporting.").color(ACCENT));
                if ui.button("Use current document").clicked(){editor.document=doc.id().to_owned();editor.targets=self.selected_layer_ids(doc);editor.selected_only=false;}
            }
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("export preset").selected_text(editor.preset.as_ref().and_then(|id|snapshot.presets.iter().find(|p|&p.id==id)).map(|p|p.name.as_str()).unwrap_or("Unsaved settings")).show_ui(ui,|ui|{
                    if ui.selectable_label(editor.preset.is_none(),"Unsaved settings").clicked(){editor.preset=None;}
                    for preset in &snapshot.presets {if ui.selectable_label(editor.preset.as_ref()==Some(&preset.id),&preset.name).clicked(){editor.preset=Some(preset.id.clone());editor.name=preset.name.clone();editor.settings=preset.settings.clone();}}
                });
                if !snapshot.actor.is_empty() && snapshot.actor!="human" {ui.label(RichText::new("AI updated presets").color(AI_BLUE));}
                let name = ui.label("Preset name");
                ui.add(egui::TextEdit::singleline(&mut editor.name).desired_width(140.))
                    .labelled_by(name.id);
                let response=ui.add_enabled(snapshot.error.is_none(),egui::Button::new("Save preset"));editor.save_rect=Some(response.rect);save=response.clicked();
                delete=ui.add_enabled(editor.preset.is_some()&&snapshot.error.is_none(),egui::Button::new("Delete preset")).clicked();
            });
            if let Some(error)=&snapshot.error {ui.label(RichText::new(error).color(ACCENT));}
            ui.columns(2,|columns|{
                let ui=&mut columns[0];
                ui.horizontal_wrapped(|ui| {
                    if icons::labeled_choice(ui, Icon::CanvasBounds, "Document composite", Some(!editor.selected_only), 0.).clicked() {
                        editor.selected_only = false;
                    }
                    if ui.add_enabled_ui(!doc.read_only(), |ui| {
                        icons::labeled_choice(ui, Icon::LayerStack, "Selected layers/folders", Some(editor.selected_only), 0.)
                    }).inner.clicked() {
                        editor.selected_only = true;
                        editor.targets = self.selected_layer_ids(doc);
                    }
                });
                if editor.selected_only {
                    if ui.button("Use current layer selection").clicked(){editor.targets=self.selected_layer_ids(doc);}
                    let roots=crate::tree::roots_view(doc,&editor.targets.iter().cloned().collect());
                    for id in roots {if let Some(layer)=doc.find_layer(&id){ui.label(layer.name());}}
                    ui.label(RichText::new("Isolated on transparency. Ancestor folders and document filters are excluded; a clipped layer needs its containing folder.").small().color(MUTED));
                } else {ui.label(RichText::new("Current composite, including document filters.").small().color(MUTED));}
                filename_template(ui, &mut editor.settings.template);
                ui.horizontal_wrapped(|ui| {
                    controls::label(ui, "Bounds");
                    for (crop, icon, label, help) in [
                        (Crop::Canvas, Icon::CanvasBounds, "Canvas bounds", "Keep the entire canvas."),
                        (Crop::Alpha, Icon::TrimAlpha, "Trim transparency", "Crop to the visible pixels."),
                    ] {
                        let selected = Some(editor.settings.crop == crop);
                        if icons::labeled_choice(ui, icon, label, selected, 0.).on_hover_text(help).clicked() {
                            editor.settings.crop = crop;
                        }
                    }
                });
                ui.horizontal(|ui|{
                    ui.label("Padding");let mut padding=editor.settings.padding as f32;
                    if controls::range(ui,"export padding",&mut padding,0.0..=4096.,130.," px",0,false).changed(){editor.settings.padding=padding.round() as u32;}
                });
                ui.horizontal_wrapped(|ui| {
                    controls::label(ui, "Sampling");
                    if let Some((method, _)) = controls::sampling_choices(ui, editor.settings.resampling) {
                        editor.settings.resampling = method;
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    controls::label(ui, "Duplicate names");
                    for (policy, icon, label, help) in [
                        (DuplicateNames::Error, Icon::Stop, "Stop on duplicates", "Stop if two planned outputs resolve to the same filename."),
                        (DuplicateNames::Number, Icon::NumberedList, "Number duplicates", "Give each planned output a unique filename by adding a number."),
                    ] {
                        let selected = Some(editor.settings.duplicate_names == policy);
                        if icons::labeled_choice(ui, icon, label, selected, 0.).on_hover_text(help).clicked() {
                            editor.settings.duplicate_names = policy;
                        }
                    }
                });
                ui.label("Size variants");
                let mut remove = None;
                let can_remove_variant = editor.settings.variants.len() > 1;
                for (index, variant) in editor.settings.variants.iter_mut().enumerate() {
                    ui.push_id(index, |ui| ui.horizontal_wrapped(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut variant.name).desired_width(130.));
                        let mut scale = variant.scale as f32;
                        let scale_control = controls::range(ui, "export scale", &mut scale, 0.01..=8., 120., "×", 3, true)
                            .on_hover_text("Drag to resize the output. Double-click to enter an exact scale.");
                        let label = format!("Scale for size variant {}", index + 1);
                        if scale_control.changed() {
                            variant.scale = f64::from(scale);
                        }
                        scale_control.widget_info(|| egui::WidgetInfo::slider(scale_control.enabled(), variant.scale, &label));
                        if ui.add_enabled_ui(can_remove_variant, |ui| {
                            icons::small_button(ui, Icon::Trash, &format!("Remove size variant {}", index + 1))
                        }).inner.clicked() {
                            remove = Some(index);
                        }
                    }));
                }
                if let Some(index) = remove { editor.settings.variants.remove(index); }
                if ui.add_enabled(editor.settings.variants.len()<12,egui::Button::new("Add size variant")).clicked(){let count=editor.settings.variants.len()+1;editor.settings.variants.push(Variant {name:format!("size-{count}"),scale:1.});}
                editor.sync_preview(doc);
                let ui=&mut columns[1];
                ui.label("Actual output preview");
                if let Some(plan) = &editor.preview_plan {
                    let outputs = plan.outputs();
                    if outputs.len() > 1 {
                        let mut index = editor.preview_index;
                        let label = |i: usize| format!("{}. {}", i + 1, outputs[i].filename);
                        ui.horizontal(|ui| {
                            let previous = ui.add_enabled_ui(index > 0, |ui| {
                                icons::small_button(ui, Icon::Left, "Previous output")
                            }).inner;
                            if previous.clicked() {
                                index -= 1;
                            }
                            let width = (ui.available_width() - ui.spacing().item_spacing.x - 20.)
                                .clamp(60., 320.);
                            let menu = ui.allocate_ui_with_layout(
                                Vec2::new(width, ui.spacing().interact_size.y),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| egui::ComboBox::from_id_salt("export preview output")
                                    .width(width)
                                    .truncate()
                                    .selected_text(label(index))
                                    .show_ui(ui, |ui| {
                                        for (i, output) in outputs.iter().enumerate() {
                                            ui.selectable_value(&mut index, i, label(i)).on_hover_text(
                                                format!("{} · {} × {} px", output.target_name, output.width, output.height),
                                            );
                                        }
                                    }),
                            ).inner;
                            menu.response.widget_info(|| {
                                let mut info = egui::WidgetInfo::labeled(
                                    egui::WidgetType::ComboBox, menu.response.enabled(), "Preview output",
                                );
                                info.current_text_value = Some(label(index));
                                info
                            });
                            menu.response.on_hover_text(label(index));
                            let next = ui.add_enabled_ui(index + 1 < outputs.len(), |ui| {
                                icons::small_button(ui, Icon::Right, "Next output")
                            }).inner;
                            if next.clicked() {
                                index += 1;
                            }
                        });
                        if index != editor.preview_index {
                            editor.preview_index = index;
                            editor.preview_choice = Some((outputs[index].target.clone(), outputs[index].variant.clone()));
                            editor.clear_preview();
                        }
                    }
                }
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
                    ui.label(&output.filename);
                    let mut details = format!("{} × {} · {} bit", output.width, output.height, output.bit_depth);
                    if let Some(bytes) = editor.preview_encoded_bytes {
                        details.push_str(&format!(" · {bytes} bytes"));
                    }
                    ui.label(details);
                    ui.label(format!("Document bounds: {}, {} → {}, {}",output.document_rect[0],output.document_rect[1],output.document_rect[2],output.document_rect[3]));
                }
                if let Some(state)=&run {
                    if state.document_id!=doc.id() {ui.label(RichText::new("Batch from an earlier document source").small().color(MUTED));}
                    let color=if state.actor=="human" {ACCENT} else {AI_BLUE};ui.label(RichText::new(&state.stage).color(color));
                    if let Some(error)=&state.error {ui.label(RichText::new(error).color(ACCENT));}
                    egui::ScrollArea::vertical().id_salt("export receipts").max_height(160.).show(ui,|ui|{for receipt in &state.outputs {ui.label(format!("{} · {}",receipt.output.filename,receipt.status));if let Some(message)=&receipt.message {ui.label(RichText::new(message).small().color(MUTED));}}});
                }
            });
            ui.horizontal_wrapped(|ui| {
                let label = ui.label("Output folder");
                let path = ui.add(egui::TextEdit::singleline(&mut editor.directory).interactive(!editor.folder_pending))
                    .labelled_by(label.id);
                if path.changed() {
                    editor.folder_error = None;
                }
                if ui.add_enabled_ui(!editor.folder_pending, |ui| {
                    icons::labeled_choice(ui, Icon::Folder, "Choose folder…", None, 0.)
                }).inner.clicked() {
                    editor.folder_pending = true;
                    editor.folder_error = None;
                    let tx = editor.folder_tx.clone();
                    let ctx = ctx.clone();
                    let picker = std::thread::Builder::new().name("peerbrush-export-folder".into()).spawn(move || {
                        let folder = std::panic::catch_unwind(|| rfd::FileDialog::new().pick_folder())
                            .map_err(|_| "The folder picker could not complete".to_owned());
                        if tx.send(folder).is_ok() {
                            ctx.request_repaint();
                        }
                    });
                    if let Err(error) = picker {
                        editor.folder_pending = false;
                        editor.folder_error = Some(format!("Could not open the folder picker: {error}"));
                    }
                }
            });
            if let Some(error) = &editor.folder_error {
                ui.colored_label(ACCENT, error);
            }
            ui.horizontal_wrapped(|ui| {
                controls::label(ui, "Existing files");
                for (choice, icon, label, help) in [
                    (Existing::Error, Icon::Stop, "Stop", "Stop the batch when an output file already exists."),
                    (Existing::Skip, Icon::Skip, "Skip", "Keep the existing file and continue with the next output."),
                    (Existing::Replace, Icon::Swap, "Replace", "Replace the existing file and keep a recovery copy."),
                ] {
                    let selected = Some(editor.existing == choice);
                    if icons::labeled_choice(ui, icon, label, selected, 0.).on_hover_text(help).clicked() {
                        editor.existing = choice;
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                let button=ui.add_enabled(valid&&!running&&!self.busy&&!editor.folder_pending&&!editor.directory.trim().is_empty(),egui::Button::new("Export batch"));editor.start_rect=Some(button.rect);start=button.clicked();
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
        let missing_targets = editor.selected_only && editor.targets().is_empty();
        if missing_targets {
            editor.error = Some("Select layers or folders before exporting".into());
        }
        if start && !missing_targets {
            match export::desktop_start(
                &self.shared,
                &editor.project,
                &export::ObservedDocument::new(doc),
                editor.settings.clone(),
                editor.targets().to_vec(),
                PathBuf::from(&editor.directory),
                editor.existing,
            ) {
                Ok(state) => {
                    editor.run = Some(state.run);
                    editor.error = None;
                    ctx.request_repaint_after(BATCH_REFRESH);
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
        let needs_preview = open
            && valid
            && editor.pending.is_none()
            && editor.preview.is_none()
            && editor.error.is_none();
        let preview_wait = PREVIEW_DELAY.saturating_sub(editor.changed_at.elapsed());
        if needs_preview && preview_wait.is_zero() {
            let control = Control::default();
            let worker_control = control.clone();
            let project = editor.project.clone();
            let document = export::ObservedDocument::new(doc);
            let output = match &editor.preview_plan {
                Some(plan) => export::PreviewOutput::Planned(plan.clone(), editor.preview_index),
                None => export::PreviewOutput::Plan(
                    editor.targets().to_vec(), editor.settings.clone(), editor.preview_choice.clone(),
                ),
            };
            let shared = self.shared.clone();
            let tx = editor.tx.clone();
            let ctx = ctx.clone();
            let worker = move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    export::desktop_preview(
                        &shared, &project, &document, output, &worker_control,
                    )
                }))
                .unwrap_or_else(|_| {
                    Err("Export preview could not complete; source remains unchanged".into())
                });
                if tx.send(result).is_ok() {
                    ctx.request_repaint();
                }
            };
            match std::thread::Builder::new()
                .name("peerbrush-export-preview".into())
                .spawn(worker)
            {
                Ok(_) => editor.pending = Some(control),
                Err(error) => editor.error = Some(format!("Could not start export preview: {error}")),
            }
        }
        if open {
            if needs_preview && !preview_wait.is_zero() {
                ctx.request_repaint_after(preview_wait);
            }
            if running {
                ctx.request_repaint_after(BATCH_REFRESH);
            }
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
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
        engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(16, 12, depth).unwrap();
        engine.resident_state_mut().expect("resident fixture").document.name = "Production.psd".into();
        engine.resident_state_mut().expect("resident fixture").document.layers[0].name = "Art".into();
        engine.resident_state_mut().expect("resident fixture").document.layers[0]
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
        let doc = shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.open_exports(&doc);
        (app, ctx, shared, root)
    }
    fn preview(app: &mut PeerBrush, ctx: &egui::Context) -> egui::FullOutput {
        frame(app, ctx, vec![]);
        app.export_editor.as_mut().unwrap().changed_at = Instant::now() - Duration::from_secs(1);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let frame = frame(app, ctx, vec![]);
            let e = app.export_editor.as_ref().unwrap();
            if e.preview.is_some() {
                return frame;
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
            let doc = shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
            let preview_frame = preview(&mut app, &ctx);
            let editor = app.export_editor.as_ref().unwrap();
            let reply = export::request(&shared, &editor.source(&doc)).unwrap();
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(reply["images"][0]["data"].as_str().unwrap())
                .unwrap();
            let rgba = image::load_from_memory(&bytes).unwrap().into_rgba8();
            let expected = egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            );
            let texture = editor.preview.as_ref().unwrap().id();
            let (_, delta) = preview_frame.textures_delta.set.iter().find(|(id, _)| *id == texture).unwrap();
            let egui::ImageData::Color(image) = &delta.image else {
                panic!("Export preview must upload an RGBA texture");
            };
            assert_eq!(image.as_ref(), &expected);
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
            let plan = editor.preview_plan.as_ref().unwrap().clone();
            let outputs = plan.outputs();
            assert_eq!(outputs.len(), 2);
            click_text(&mut app, &ctx, &format!("1. {}", outputs[0].filename));
            click_text(&mut app, &ctx, &format!("2. {}", outputs[1].filename));
            let preview_frame = preview(&mut app, &ctx);
            let editor = app.export_editor.as_ref().unwrap();
            assert_eq!(editor.preview_index, 1);
            assert!(Arc::ptr_eq(editor.preview_plan.as_ref().unwrap(), &plan));
            let output = editor.preview_output.as_ref().unwrap();
            assert_eq!(output.variant, "double");
            let reply = export::request(&shared, &editor.source(&doc)).unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(reply["images"][0]["data"].as_str().unwrap())
                .unwrap();
            let rgba = image::load_from_memory(&bytes).unwrap().into_rgba8();
            let expected = egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            );
            let texture = editor.preview.as_ref().unwrap().id();
            let (_, delta) = preview_frame.textures_delta.set.iter().find(|(id, _)| *id == texture).unwrap();
            let egui::ImageData::Color(image) = &delta.image else {
                panic!("Export preview must upload an RGBA texture");
            };
            assert_eq!(image.as_ref(), &expected);
            app.export_editor.as_mut().unwrap().settings.resampling = export::Resampling::Nearest;
            preview(&mut app, &ctx);
            let editor = app.export_editor.as_ref().unwrap();
            assert!(!Arc::ptr_eq(editor.preview_plan.as_ref().unwrap(), &plan));
            assert_eq!(editor.preview_output.as_ref().unwrap().variant, "double");
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
                serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                before
            );
            assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            drop(app);
            drop(shared);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn native_export_discards_stale_results_on_names_settings_and_source_changes() {
        let (mut app, ctx, shared, root) = fixture(16);
        preview(&mut app, &ctx);
        // Save As can rename a document without changing its pixel revision.
        shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.name = "Renamed.psd".into();
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.export_editor.as_ref().unwrap().preview_source.as_ref().unwrap().name, "Renamed.psd");
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
        let original_padding = editor.settings.padding;
        let control = Control::default();
        editor.pending = Some(control.clone());
        editor.settings.padding = 3;
        frame(&mut app, &ctx, vec![]);
        assert!(control.check().is_err());
        assert!(app.export_editor.as_ref().unwrap().preview.is_none());
        app.export_editor.as_mut().unwrap().settings.padding = original_padding;
        frame(&mut app, &ctx, vec![]);
        let editor = app.export_editor.as_ref().unwrap();
        assert_eq!(editor.preview_source.as_ref().unwrap().settings.padding, original_padding);
        editor
            .tx
            .send(Err(control.check().unwrap_err()))
            .unwrap();
        preview(&mut app, &ctx);
        let output = app.export_editor.as_ref().unwrap().preview_output.as_ref().unwrap();
        assert_eq!(output.filename, "Renamed_composite_native.png");

        app.export_editor.as_mut().unwrap().settings.padding = 3;
        let source = Document::new_depth(16, 12, 16).unwrap();
        shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = source;
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
                let before = serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
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
                    click_text(&mut app, &ctx, "Lossy");
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
                assert!(app.export_editor.as_ref().unwrap().preview_encoded_bytes.is_some_and(|size| size > 0));
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
                        image::load_from_memory(&shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap())
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
                    serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                    before
                );
                assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                drop(app);
                drop(shared);
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    #[test]
    fn native_export_settings_bind_only_shared_document_metadata_and_keep_preview_requests_exact() {
        for depth in [8, 16] {
            let ctx = egui::Context::default();
            let mut document = Document::new_depth(9, 7, depth).unwrap();
            document.name = "Native export".into();
            let mut engine = crate::engine::Engine::new();
            engine.resident_state_mut().expect("resident fixture").document = document.clone();
            let mut app = PeerBrush::init(
                &ctx,
                Arc::new(std::sync::Mutex::new(engine)),
                Connection {
                    port: 0,
                    token: String::new(),
                    state_dir: Default::default(),
                    instance_lock: None,
                    availability: Default::default(),
                },
            );
            let pool = crate::large_document::ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let owner = crate::engine::view::DocumentSnapshot::from(
                crate::tree::presentation_tests::native(&document, &pool),
            );
            let admitted = pool.used();
            app.open_exports(owner.view());
            let editor = app.export_editor.as_mut().unwrap();
            let request = editor.source(owner.view());
            assert_eq!(request, editor.source(&document));
            assert_eq!(request["document_id"], document.id);
            assert_eq!(request["expected_revision"], 0);
            assert_eq!(request["max_edge"], 512);
            editor.sync_preview(owner.view());
            let settled = Instant::now() - Duration::from_secs(1);
            editor.changed_at = settled;
            let control = Control::default();
            editor.pending = Some(control.clone());
            editor.sync_preview(owner.view());
            assert_eq!(editor.changed_at, settled);
            assert!(control.check().is_ok());
            editor.selected_only = true;
            editor.targets.clear();
            editor.sync_preview(owner.view());
            assert!(control.check().is_err());
            editor.error = Some("Select layers or folders before exporting".into());
            editor.selected_only = false;
            editor.sync_preview(owner.view());
            assert!(editor.error.is_none());
            assert_eq!(pool.used(), admitted);

            let replacement = crate::tree::presentation_tests::native(&document, &pool);
            let replacement_view = DocumentView::Native(&replacement);
            assert_eq!(editor.source(owner.view()), editor.source(replacement_view));
            let control = Control::default();
            editor.pending = Some(control.clone());
            editor.sync_preview(replacement_view);
            assert!(control.check().is_err());
            assert!(editor.preview_source.as_ref().unwrap().document.observes(replacement_view));
            assert!(!editor.preview_source.as_ref().unwrap().document.observes(owner.view()));

            app.shared.lock().unwrap().authority =
                crate::engine::authority::DocumentAuthority::from_native(replacement, 8).unwrap();
            let stale = export::ObservedDocument::new(owner.view());
            let result = export::desktop_preview(
                &app.shared,
                &app.project_id,
                &stale,
                export::PreviewOutput::Plan(vec![], Settings::default(), None),
                &Control::default(),
            );
            assert!(matches!(result, Err(error) if error == "Export source changed before capture"));
            let directory = crate::engine::native::worker::tests::Fixture::new();
            let result = export::desktop_start(
                &app.shared,
                &app.project_id,
                &stale,
                Settings::default(),
                vec![],
                directory.path().to_owned(),
                Existing::Error,
            );
            assert!(matches!(result, Err(error) if error == "Export source changed before capture"));
            let library = app.shared.lock().unwrap().export_library.clone();
            assert!(export::jobs::states(&library).is_empty());
            assert!(std::fs::read_dir(directory.path()).unwrap().next().is_none());
        }
    }
}
