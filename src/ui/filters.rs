//! Document-bound, non-destructive filter browsing. Preview buffers never enter history.
use super::effects::effect_icon;
use super::*;
use crate::filter_library::Preset;

pub(super) struct Editor {
    pub document: String,
    pub revision: u64,
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
    pub thumbnails: HashMap<String, TextureHandle>,
    pub pending: bool,
    tx: mpsc::Sender<(String, Result<Vec<u8>, String>)>,
    rx: mpsc::Receiver<(String, Result<Vec<u8>, String>)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::sync::Mutex;
    fn fixture() -> (PeerBrush, egui::Context) {
        let ctx = egui::Context::default();
        let mut e = Engine::new();
        e.doc = Document::new_depth(12, 8, 16).unwrap();
        e.doc.layers[0]
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
            },
        );
        (app, ctx)
    }
    fn draw(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        doc: &Document,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
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
    fn browsing_strength_comparison_cancel_and_apply_use_one_native_engine_edit() {
        let (mut app, ctx) = fixture();
        let doc = app.shared.lock().unwrap().doc.clone();
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
        assert!(app.shared.lock().unwrap().undo.is_empty());
        app.filter_editor.as_mut().unwrap().original = true;
        draw(&mut app, &ctx, &doc, vec![]);
        assert!(app.transient.is_empty());
        app.filter_editor.as_mut().unwrap().original = false;
        draw(&mut app, &ctx, &doc, vec![]);
        let e = app.filter_editor.take().unwrap();
        app.apply_filter_editor(&e).unwrap();
        app.cancel_filters();
        let changed = app.shared.lock().unwrap().doc.clone();
        assert_eq!(changed.export_png().unwrap(), preview.export_png().unwrap());
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
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
            app.shared.lock().unwrap().doc.export_png().unwrap(),
            changed.export_png().unwrap()
        );
        app.shared.lock().unwrap().undo("human").unwrap();
        assert_eq!(
            app.shared.lock().unwrap().doc.export_png().unwrap(),
            doc.export_png().unwrap()
        );
    }
    #[test]
    fn stale_preview_tab_switch_locks_reservations_and_native_close_preserve_work() {
        let (mut app, ctx) = fixture();
        let doc = app.shared.lock().unwrap().doc.clone();
        app.open_filters(&doc);
        app.request_close(lifecycle::Intent::Exit);
        assert!(app.lifecycle.is_none());
        assert!(app.filter_editor.is_some());
        let editor = app.filter_editor.take().unwrap();
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
        assert!(app.apply_filter_editor(&editor).is_err());
        app.filter_editor = Some(editor);
        app.pending = true;
        app.request_preview(&ctx, &doc);
        assert!(app.filter_editor.is_none());
        assert_eq!(app.shared.lock().unwrap().doc.layers[0].name, "Human");
        let doc = app.shared.lock().unwrap().doc.clone();
        app.open_filters(&doc);
        let editor = app.filter_editor.take().unwrap();
        app.shared.lock().unwrap().doc.layers[0].locked = true;
        assert!(app.apply_filter_editor(&editor).is_err());
        app.shared.lock().unwrap().doc.layers[0].locked = false;
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
        assert!(app.apply_filter_editor(&editor).is_err());
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        app.filter_editor = Some(editor);
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
        let before = app.shared.lock().unwrap().doc.clone();
        let filter = before.filters[0].id.clone();
        let history = app.shared.lock().unwrap().undo.len();
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
        assert_eq!(app.shared.lock().unwrap().doc.filters[0].weight, 1.);
        assert_eq!(app.shared.lock().unwrap().undo.len(), history);
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
            app.shared.lock().unwrap().doc.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        assert_eq!(app.shared.lock().unwrap().undo.len(), history + 1);
        app.shared.lock().unwrap().undo("human").unwrap();
        assert_eq!(
            app.shared.lock().unwrap().doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
    }
}
impl Editor {
    pub fn new(doc: &Document) -> Self {
        let (tx, rx) = mpsc::channel();
        let p = crate::filter_library::curated().remove(0);
        let mut editor = Self {
            document: doc.id.clone(),
            revision: doc.revision,
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
            thumbnails: HashMap::new(),
            pending: false,
            tx,
            rx,
        };
        if let Some(f) = doc.filters.last() {
            editor.target = Some(f.id.clone());
            editor.kind = f.kind.clone();
            editor.settings = f.settings.clone();
            editor.weight = f.weight;
            editor.preset = None;
            editor.name = effects::effect_name(&f.kind).into();
            editor.category = crate::effects::catalog::get(&f.kind)
                .map_or("Custom", |e| e.category)
                .into();
        }
        editor
    }
    pub fn command(&self) -> Value {
        json!({"op":if self.target.is_some(){"filter.update"}else{"filter.add"},"filter":self.target,"kind":self.kind,"settings":self.settings,"weight":self.weight,"document_id":self.document,"source_revision":self.revision})
    }
    pub fn preview_commands(&self, doc: &Document) -> Vec<Value> {
        if self.unfiltered {
            doc.filters.iter().map(|f|json!({"op":"filter.update","filter":f.id,"enabled":false,"document_id":self.document,"source_revision":self.revision})).collect()
        } else if self.original {
            vec![]
        } else {
            vec![self.command()]
        }
    }
    fn choose(&mut self, p: &Preset) {
        self.kind = p.kind.clone();
        self.settings = p.settings.clone();
        self.weight = p.weight;
        self.preset = Some(p.id.clone());
        self.name = p.name.clone();
        self.category = p.category.clone();
        self.error = None;
    }
    fn thumbnail(&mut self, ctx: &egui::Context, p: &Preset) -> Option<TextureHandle> {
        let key = serde_json::to_string(p).unwrap();
        if let Some(texture) = self.thumbnails.get(&key) {
            return Some(texture.clone());
        }
        if !self.pending {
            self.pending = true;
            let tx = self.tx.clone();
            let p = p.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = crate::filter_library::thumbnail(&p);
                let _ = tx.send((key, result));
                ctx.request_repaint();
            });
        }
        None
    }
}
impl PeerBrush {
    pub(super) fn document_filter_stack(&mut self, ui: &mut egui::Ui, doc: &Document) {
        let mut display = self.animation.document(doc, ui.input(|i| i.time), false);
        if let Some(edit) = &self.parameter_gesture {
            if edit.document == doc.id && edit.revision == doc.revision {
                if let Ok(preview) =
                    crate::engine::Engine::preview_edits(doc.clone(), &edit.commands)
                {
                    display = preview;
                }
            }
        }
        ui.set_min_width(290.);
        ui.add_enabled_ui(!doc.read_only && !self.busy, |ui| {
            for (index, f) in display.filters.iter().enumerate().rev() {
                ui.push_id(&f.id, |ui| {
                    ui.horizontal(|ui| {
                        let mut enabled = f.enabled;
                        if ui
                            .checkbox(&mut enabled, "")
                            .on_hover_text("Bypass filter")
                            .changed()
                        {
                            self.edit(
                                vec![json!({"op":"filter.update","filter":f.id,"enabled":enabled})],
                                "Toggle whole-image filter",
                            );
                        }
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::splat(18.), egui::Sense::hover());
                        icons::paint(ui.painter(), rect, effect_icon(&f.kind), f.enabled);
                        ui.label(effects::effect_name(&f.kind));
                        let mut value = f.weight * 100.;
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
                                json!({"filter":f.id,"weight":value/100.}),
                                "Filter strength",
                                &response,
                            );
                        }
                        if index + 1 < doc.filters.len()
                            && icons::small_button(ui, Icon::Up, "Move filter up").clicked()
                        {
                            self.edit(
                                vec![json!({"op":"filter.reorder","filter":f.id,"index":index+1})],
                                "Reorder filters",
                            );
                        }
                        if index > 0
                            && icons::small_button(ui, Icon::Down, "Move filter down").clicked()
                        {
                            self.edit(
                                vec![json!({"op":"filter.reorder","filter":f.id,"index":index-1})],
                                "Reorder filters",
                            );
                        }
                        if icons::small_button(ui, Icon::Trash, "Remove filter").clicked() {
                            self.edit(
                                vec![json!({"op":"filter.delete","filter":f.id})],
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
    pub(super) fn open_filters(&mut self, doc: &Document) {
        self.cancel_geometry();
        self.cancel_source();
        self.cancel_refinement();
        self.finish_parameter(true);
        self.points.clear();
        self.drag_start = None;
        self.gizmo_handle = 0;
        self.layer_drag = None;
        self.eye_sweep = None;
        self.animation.cancel();
        self.isolate = false;
        self.mask = false;
        self.filter_editor = Some(Editor::new(doc));
        self.last_preview = None;
    }
    pub(super) fn cancel_filters(&mut self) {
        self.filter_editor = None;
        self.transient
            .retain(|c| !c["op"].as_str().is_some_and(|op| op.starts_with("filter.")));
        self.last_preview = None;
    }
    fn apply_filter_editor(&mut self, editor: &Editor) -> Result<(), String> {
        let mut e = self.shared.lock().unwrap();
        e.edit(
            "human",
            &[editor.command()],
            Some(editor.revision),
            None,
            "Whole-image filter",
        )?;
        Ok(())
    }
    pub(super) fn filters_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(mut editor) = self.filter_editor.take() else {
            return;
        };
        if editor.document != doc.id || editor.revision != doc.revision {
            self.cancel_filters();
            self.message = "Filter preview cancelled · project changed".into();
            return;
        }
        while let Ok((key, result)) = editor.rx.try_recv() {
            editor.pending = false;
            if let Ok(rgba) = result {
                if editor.thumbnails.len() >= 64 {
                    editor.thumbnails.clear();
                }
                editor.thumbnails.insert(
                    key,
                    ctx.load_texture(
                        "filter thumbnail",
                        egui::ColorImage::from_rgba_unmultiplied([96, 64], &rgba),
                        egui::TextureOptions::LINEAR,
                    ),
                );
            }
        }
        let library = self.shared.lock().unwrap().filter_library.clone();
        let (presets, library_error) = {
            let l = library.lock().unwrap();
            (l.presets(), l.error.clone())
        };
        let before = editor.command();
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut stack_edit = None;
        egui::Window::new("Whole-image filters")
            .open(&mut open).collapsible(false).resizable(true).default_width(650.0)
            .default_pos(egui::pos2((ctx.screen_rect().right()-660.0).max(8.0),150.0))
            .show(ctx,|ui| {
                ui.horizontal(|ui| {
                    ui.label("Entire composited image");
                    if ui.selectable_label(!editor.original&&!editor.unfiltered,"Filtered").clicked(){editor.original=false;editor.unfiltered=false;}
                    if ui.selectable_label(editor.original&&!editor.unfiltered,"Current image").on_hover_text("Compare with the stack before this preview").clicked(){editor.original=true;editor.unfiltered=false;}
                    if ui.selectable_label(editor.unfiltered,"Original").on_hover_text("Compare with all whole-image filters bypassed").clicked(){editor.unfiltered=true;editor.original=false;}
                });
                if !doc.filters.is_empty() {
                    egui::ScrollArea::vertical().id_salt("document filter stack").max_height(100.).show(ui,|ui| {
                        for (index,f) in doc.filters.iter().enumerate().rev() {
                            ui.push_id(&f.id,|ui|ui.horizontal(|ui| {
                                let mut enabled=f.enabled;
                                if ui.checkbox(&mut enabled,"").on_hover_text("Bypass filter").changed(){stack_edit=Some(json!({"op":"filter.update","filter":f.id,"enabled":enabled}));}
                                if ui.selectable_label(editor.target.as_deref()==Some(&f.id),effects::effect_name(&f.kind)).clicked(){editor.target=Some(f.id.clone());editor.kind=f.kind.clone();editor.settings=f.settings.clone();editor.weight=f.weight;editor.preset=None;editor.name=effects::effect_name(&f.kind).into();editor.category=crate::effects::catalog::get(&f.kind).map_or("Custom",|e|e.category).into();editor.error=None;}
                                ui.label(format!("{:.0}%",f.weight*100.));
                                if index+1<doc.filters.len()&&icons::small_button(ui,Icon::Up,"Move filter up").clicked(){stack_edit=Some(json!({"op":"filter.reorder","filter":f.id,"index":index+1}));}
                                if index>0&&icons::small_button(ui,Icon::Down,"Move filter down").clicked(){stack_edit=Some(json!({"op":"filter.reorder","filter":f.id,"index":index-1}));}
                                if icons::small_button(ui,Icon::Trash,"Remove filter").clicked(){stack_edit=Some(json!({"op":"filter.delete","filter":f.id}));}
                            }));
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut editor.search).hint_text("Search filters…").desired_width(220.).frame(false));
                    let mut categories=presets.iter().map(|p|p.category.clone()).collect::<Vec<_>>();categories.sort();categories.dedup();
                    egui::ComboBox::from_id_salt("filter category").selected_text(if editor.category_filter.is_empty(){"All categories"}else{&editor.category_filter}).show_ui(ui,|ui| {
                        ui.selectable_value(&mut editor.category_filter,String::new(),"All categories");
                        for category in categories {ui.selectable_value(&mut editor.category_filter,category.clone(),category);}
                    });
                    if ui.button("New filter").clicked(){editor.target=None;editor.error=None;}
                });
                ui.columns(2,|columns| {
                    let filtered=presets.iter().filter(|p| {
                        (editor.category_filter.is_empty()||editor.category_filter==p.category)
                        && editor.search.split_whitespace().all(|q|format!("{} {} {}",p.name,p.category,p.kind).to_lowercase().contains(&q.to_lowercase()))
                    }).collect::<Vec<_>>();
                    egui::ScrollArea::vertical().id_salt("filter library").max_height(280.).show_rows(&mut columns[0],72.,filtered.len(),|ui,rows| {
                        for row in rows {
                            let p=filtered[row];
                            ui.push_id(&p.id,|ui| {
                                let response=ui.horizontal(|ui| {
                                    if let Some(texture)=editor.thumbnail(ctx,p) {ui.add(egui::Image::new((texture.id(),Vec2::new(96.,64.))));}
                                    else {ui.allocate_space(Vec2::new(96.,64.));}
                                    ui.vertical(|ui|{ui.label(&p.name);ui.label(RichText::new(&p.category).size(11.).color(MUTED));});
                                }).response;
                                if ui.interact(response.rect,ui.id().with("choose"),egui::Sense::click()).clicked(){editor.choose(p);}
                                if editor.preset.as_deref()==Some(&p.id) {ui.painter().line_segment([response.rect.left_top(),response.rect.left_bottom()],Stroke::new(2.,ACCENT));}
                            });
                        }
                    });
                    if filtered.is_empty(){columns[0].label("No matching filters");}
                    let ui=&mut columns[1];
                    ui.horizontal(|ui| {
                        let (rect,_)=ui.allocate_exact_size(Vec2::splat(18.),egui::Sense::hover());
                        icons::paint(ui.painter(),rect,effect_icon(&editor.kind),true);
                        ui.label(effects::effect_name(&editor.kind));
                        let mut value=editor.weight*100.;
                        if controls::range(ui,"filter strength",&mut value,0.0..=100.0,110.,"%",0,false).changed(){editor.weight=value/100.;}
                    });
                    egui::ComboBox::from_id_salt("filter kind").selected_text("Effect settings").show_ui(ui,|ui| {
                        for entry in crate::effects::catalog::ENTRIES.iter().filter(|e|crate::filters::supports(e.kind)) {
                            if ui.selectable_label(editor.kind==entry.kind,entry.name).clicked(){editor.kind=entry.kind.into();editor.settings=crate::effects::defaults(entry.kind);editor.preset=None;editor.name=entry.name.into();editor.category=entry.category.into();editor.error=None;}
                        }
                    });
                    egui::ScrollArea::vertical().id_salt("filter settings").max_height(160.).show(ui,|ui|{effects::settings(ui,&editor.kind,&mut editor.settings);});
                    ui.add(egui::TextEdit::singleline(&mut editor.name).hint_text("Preset name").desired_width(230.).char_limit(80).frame(false));
                    ui.add(egui::TextEdit::singleline(&mut editor.category).hint_text("Preset category").desired_width(230.).char_limit(40).frame(false));
                    let custom=editor.preset.as_ref().and_then(|id|presets.iter().find(|p|&p.id==id)).is_some_and(|p|p.custom);
                    ui.horizontal(|ui| {
                        if ui.button("Save new preset").clicked(){match library.lock().unwrap().save(None,&editor.name,&editor.category,&editor.kind,editor.settings.clone(),editor.weight){Ok(p)=>editor.choose(&p),Err(e)=>editor.error=Some(e)}}
                        if custom&&ui.button("Update").clicked(){match library.lock().unwrap().save(editor.preset.as_deref(),&editor.name,&editor.category,&editor.kind,editor.settings.clone(),editor.weight){Ok(p)=>editor.choose(&p),Err(e)=>editor.error=Some(e)}}
                    });
                    ui.horizontal(|ui| {
                        if custom&&ui.button("Rename").clicked(){match library.lock().unwrap().rename(editor.preset.as_deref().unwrap(),&editor.name){Ok(p)=>editor.preset=Some(p.id),Err(e)=>editor.error=Some(e)}}
                        if custom&&ui.button("Delete preset").clicked(){match library.lock().unwrap().delete(editor.preset.as_deref().unwrap()){Ok(())=>editor.preset=None,Err(e)=>editor.error=Some(e)}}
                        if editor.preset.is_some()&&ui.button("Export…").clicked(){if let Some(path)=rfd::FileDialog::new().add_filter("PeerBrush filter",&["json"]).set_file_name("filter.json").save_file(){let result=library.lock().unwrap().export(editor.preset.as_deref().unwrap()).and_then(|data|serde_json::to_vec_pretty(&data).map_err(|e|e.to_string())).and_then(|bytes|server::atomic_write(&path,&bytes));if let Err(e)=result{editor.error=Some(e);}}}
                    });
                    if ui.button("Import preset…").clicked(){if let Some(path)=rfd::FileDialog::new().add_filter("PeerBrush filter",&["json"]).pick_file(){let result=(|| {let data=crate::filter_library::read(&path)?;library.lock().unwrap().import(&data)})();match result{Ok(p)=>editor.choose(&p),Err(e)=>editor.error=Some(e)}}}
                });
                if let Some(error)=library_error.as_ref().or(editor.error.as_ref()){ui.colored_label(ACCENT,error);}
                ui.horizontal(|ui| {
                    apply=ui.add_enabled(!self.busy&&!doc.read_only&&editor.error.is_none(),egui::Button::new(if editor.target.is_some(){"Apply changes"}else{"Apply filter"})).clicked();
                    cancel=ui.button("Cancel").clicked();
                    ui.label(RichText::new("Top filters run last · source layers remain editable").size(11.).color(MUTED));
                });
            });
        let command = editor.command();
        if command != before {
            editor.error = None;
        }
        if let Some(mut command) = stack_edit {
            command["document_id"] = json!(editor.document);
            command["source_revision"] = json!(editor.revision);
            let result = self.shared.lock().unwrap().edit(
                "human",
                &[command],
                Some(editor.revision),
                None,
                "Edit whole-image filter stack",
            );
            match result {
                Ok(_) => {
                    let doc = self.shared.lock().unwrap().doc.clone();
                    editor = Editor::new(&doc);
                    self.last_preview = None;
                }
                Err(e) => editor.error = Some(e),
            }
        }
        if apply {
            match self.apply_filter_editor(&editor) {
                Ok(()) => {
                    self.cancel_filters();
                    return;
                }
                Err(e) => editor.error = Some(e),
            }
        }
        if !open || cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_filters();
        } else {
            self.transient = editor.preview_commands(doc);
            self.filter_editor = Some(editor);
        }
    }
}
