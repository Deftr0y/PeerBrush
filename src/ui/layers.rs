use super::*;
use crate::engine::Layer;
const TREE_INDENT: f32 = 22.0;
struct TreeAnchor {
    disclosure: Rect,
    ai: bool,
    opacity: f32,
}
fn paint_tree_guides(
    painter: &egui::Painter,
    doc: &Document,
    anchors: &HashMap<String, TreeAnchor>,
) {
    for parent in doc.layers.iter().filter(|layer| layer.kind == "group") {
        let Some(root) = anchors.get(&parent.id) else {
            continue;
        };
        let children: Vec<_> = doc
            .layers
            .iter()
            .filter(|layer| layer.parent.as_deref() == Some(&parent.id))
            .filter_map(|layer| anchors.get(&layer.id).map(|anchor| (layer, anchor)))
            .collect();
        let x = root.disclosure.center().x;
        let start = root.disclosure.bottom();
        let end = children
            .iter()
            .map(|(_, child)| child.disclosure.center().y)
            .max_by(f32::total_cmp);
        if let Some(end) = end {
            let color = if root.ai {
                AI_BLUE
            } else {
                MUTED.gamma_multiply(0.8)
            };
            painter.line_segment(
                [egui::pos2(x, start), egui::pos2(x, end)],
                Stroke::new(1.0, color.gamma_multiply(root.opacity)),
            );
        }
        for (layer, child) in children {
            let y = child.disclosure.center().y;
            let end = if layer.kind == "group" {
                child.disclosure.left() - 2.0
            } else {
                child.disclosure.right() - 4.0
            };
            let color = if root.ai || child.ai {
                AI_BLUE
            } else {
                MUTED.gamma_multiply(0.8)
            };
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2(end, y)],
                Stroke::new(1.0, color.gamma_multiply(root.opacity.min(child.opacity))),
            );
        }
    }
}
fn click_modifiers(ui: &egui::Ui) -> egui::Modifiers {
    ui.input(|i| {
        i.events
            .iter()
            .rev()
            .find_map(|event| match event {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(i.modifiers)
    })
}
fn rows(
    doc: &Document,
    parent: Option<&str>,
    depth: usize,
    collapsed: &HashSet<String>,
    out: &mut Vec<(usize, usize)>,
) {
    if depth > 16 {
        return;
    }
    for (index, l) in doc
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.parent.as_deref() == parent)
    {
        out.push((index, depth));
        if l.kind == "group" && !collapsed.contains(&l.id) {
            rows(doc, Some(&l.id), depth + 1, collapsed, out);
        }
    }
}
impl PeerBrush {
    pub(super) fn begin_rename(&mut self, _doc: &Document, id: &str) {
        if self.rename_edit.is_some() {
            self.finish_rename();
        }
        let (document, revision, name) = {
            let e = self.shared.lock().unwrap();
            let Some(layer) = e.doc.layers.iter().find(|l| l.id == id) else {
                return;
            };
            (e.doc.id.clone(), e.doc.revision, layer.name.clone())
        };
        self.select_content(id);
        self.layer_clipboard = true;
        self.layer_drag = None;
        self.rename_edit = Some(RenameEdit {
            document,
            layer: id.into(),
            original: name.clone(),
            text: name,
            revision,
            focus: true,
        });
    }
    pub(super) fn finish_rename(&mut self) {
        let Some(edit) = self.rename_edit.take() else {
            return;
        };
        if edit.text == edit.original {
            return;
        }
        if edit.text.trim().is_empty() {
            self.message = "Layer names need text".into();
            return;
        }
        let result = {
            let mut e = self.shared.lock().unwrap();
            if e.doc.id != edit.document {
                Err("The document changed while renaming".into())
            } else {
                e.edit(
                    "human",
                    &[json!({"op":"layer.update","layer":edit.layer,"name":edit.text})],
                    Some(edit.revision),
                    None,
                    "Rename layer",
                )
            }
        };
        self.message = result.map(|_| "Renamed layer".into()).unwrap_or_else(|e| e);
        self.last_preview = None;
    }
    fn inline_rename(&mut self, ui: &mut egui::Ui, width: f32) {
        let mut commit = false;
        let mut cancel = false;
        if let Some(edit) = &mut self.rename_edit {
            let id = egui::Id::new(("layer rename", &edit.document, &edit.layer));
            let response = ui.add_sized(
                [width, 30.0],
                egui::TextEdit::singleline(&mut edit.text)
                    .id(id)
                    .frame(false)
                    .font(egui::FontId::proportional(12.0))
                    .char_limit(256),
            );
            let initial = edit.focus;
            if initial {
                edit.focus = false;
                response.request_focus();
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(edit.text.chars().count()),
                        )));
                    state.store(ui.ctx(), id);
                }
            }
            cancel = (response.has_focus() || response.lost_focus())
                && ui.input(|i| i.key_pressed(egui::Key::Escape));
            commit = !initial
                && (response.lost_focus()
                    || response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if cancel || commit {
                response.surrender_focus();
            }
        }
        if cancel {
            self.rename_edit = None;
        } else if commit {
            self.finish_rename();
        }
    }
    pub(super) fn select_content(&mut self, id: &str) {
        self.liquify_effect = None;
        self.selected = id.into();
        self.selection_layers = [id.to_string()].into_iter().collect();
        self.selection_anchor = id.into();
        self.mask = false;
        self.mask_step = None;
        self.last_preview = None;
    }
    fn select_row(&mut self, ui: &egui::Ui, doc: &Document, id: &str) {
        self.layer_clipboard = true;
        let mods = click_modifiers(ui);
        let command = mods.command || mods.ctrl;
        if mods.shift {
            let mut visible = vec![];
            rows(doc, None, 0, &self.collapsed, &mut visible);
            let a = visible
                .iter()
                .position(|&(i, _)| doc.layers[i].id == self.selection_anchor);
            let b = visible.iter().position(|&(i, _)| doc.layers[i].id == id);
            if let (Some(a), Some(b)) = (a, b) {
                if !command {
                    self.selection_layers.clear();
                }
                for &(i, _) in &visible[a.min(b)..=a.max(b)] {
                    self.selection_layers.insert(doc.layers[i].id.clone());
                }
                self.selected = id.into();
            } else {
                self.select_content(id);
            }
        } else if command {
            if self.selection_layers.contains(id) && self.selection_layers.len() > 1 {
                self.selection_layers.remove(id);
                if self.selected == id {
                    self.selected = doc
                        .layers
                        .iter()
                        .find(|l| self.selection_layers.contains(&l.id))
                        .unwrap()
                        .id
                        .clone();
                }
            } else {
                self.selection_layers.insert(id.into());
                self.selected = id.into();
            }
            self.selection_anchor = id.into();
        } else {
            self.select_content(id);
        }
        self.mask = false;
        self.mask_step = None;
        self.last_preview = None;
    }
    fn thumb_button(
        &mut self,
        ui: &mut egui::Ui,
        texture: Option<&TextureHandle>,
        selected: bool,
        tip: &str,
    ) -> egui::Response {
        let (r, response) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::click());
        if let Some(t) = texture {
            ui.painter().image(
                t.id(),
                r,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            ui.painter()
                .rect_filled(r, 2, Color32::from_rgb(54, 51, 58));
        }
        if selected {
            ui.painter().rect_stroke(
                r,
                2,
                Stroke::new(1.0_f32, ACCENT),
                egui::StrokeKind::Outside,
            );
        }
        response.on_hover_text(tip)
    }
    fn layer_contents(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Document,
        l: &Layer,
        indent: f32,
        thumb: Option<&TextureHandle>,
        mask_thumb: Option<&TextureHandle>,
    ) -> Rect {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let (grip, _) = ui.allocate_exact_size(Vec2::new(6.0, 28.0), egui::Sense::hover());
            for x in 0..2 {
                for y in 0..3 {
                    ui.painter().circle_filled(
                        grip.center() + Vec2::new((x as f32 - 0.5) * 3.0, (y as f32 - 1.0) * 4.0),
                        0.8,
                        MUTED.gamma_multiply(0.5),
                    );
                }
            }
            let eye = eye_button(ui, l.visible);
            self.eye_rects.insert(l.id.clone(), eye.rect);
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if eye.rect.contains(pos) && ui.clip_rect().contains(pos) {
                    if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
                        if !self.selection_layers.contains(&l.id) {
                            self.select_content(&l.id);
                        }
                        self.eye_sweep = Some(EyeSweep {
                            gesture: crate::engine::id(),
                            previous: pos,
                            revision: doc.revision,
                            visible: !l.visible,
                            ids: [l.id.clone()].into_iter().collect(),
                        });
                        self.layer_drag = None;
                    } else if ui.input(|i| i.pointer.primary_down()) {
                        if let Some(sweep) = &mut self.eye_sweep {
                            sweep.ids.insert(l.id.clone());
                        }
                    }
                }
            }
            ui.add_space(indent);
            let disclosure = if l.kind == "group" {
                let response = icons::small_button(
                    ui,
                    if self.collapsed.contains(&l.id) {
                        Icon::Right
                    } else {
                        Icon::Down
                    },
                    "Expand / collapse group",
                );
                if response.clicked() {
                    if !self.collapsed.remove(&l.id) {
                        self.collapsed.insert(l.id.clone());
                    }
                }
                response.rect
            } else {
                ui.allocate_exact_size(Vec2::new(20.0, 24.0), egui::Sense::hover())
                    .0
            };
            if l.kind == "group" {
                let (r, response) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::click());
                icons::paint(ui.painter(), r, Icon::Folder, true);
                if response.clicked() {
                    self.select_row(ui, doc, &l.id);
                }
            } else if l.kind == "adjustment" {
                let (r, response) = ui.allocate_exact_size(Vec2::splat(30.), egui::Sense::click());
                icons::paint(ui.painter(), r, Icon::Adjust, true);
                if response.clicked() {
                    self.select_row(ui, doc, &l.id);
                }
            } else if self
                .thumb_button(
                    ui,
                    thumb,
                    self.selection_layers.contains(&l.id) && !self.mask,
                    "Edit layer content",
                )
                .clicked()
            {
                self.select_row(ui, doc, &l.id);
            }
            if l.clip_to.is_some() {
                ui.label(RichText::new("↳").color(MUTED))
                    .on_hover_text("Clipped to the base below");
            }
            let reserved = self.ai_reserved(&l.id);
            if reserved {
                ui.label(RichText::new("AI").size(10.).color(AI_BLUE))
                    .on_hover_text(
                        "AI has reserved this layer or a region; visibility stays available",
                    );
            }
            let mask_width = if l.mask.is_some() { 35.0 } else { 0.0 };
            let name_width =
                (ui.available_width() - mask_width - if l.locked { 21.0 } else { 0.0 }).max(25.0);
            if self
                .rename_edit
                .as_ref()
                .is_some_and(|edit| edit.layer == l.id)
            {
                self.inline_rename(ui, name_width);
            } else {
                let name = ui.add_sized(
                    [name_width, 30.0],
                    egui::Label::new(
                        RichText::new(&l.name)
                            .size(12.0)
                            .font(egui::FontId::new(
                                12.0,
                                if l.kind == "group" {
                                    egui::FontFamily::Name("semibold".into())
                                } else {
                                    egui::FontFamily::Proportional
                                },
                            ))
                            .color(if reserved {
                                AI_BLUE
                            } else if l.visible {
                                Color32::WHITE
                            } else {
                                MUTED
                            }),
                    )
                    .truncate()
                    .sense(egui::Sense::click()),
                );
                if name.clicked() {
                    self.select_row(ui, doc, &l.id);
                    let time = ui.input(|i| i.time);
                    let modifiers = click_modifiers(ui);
                    let pointer = name.interact_pointer_pos().unwrap_or(name.rect.center());
                    let options = ui.ctx().options(|o| o.input_options.clone());
                    // egui counts rapid clicks globally, including clicks on different rows.
                    // Rename requires two unmodified clicks on this same name at the same spot.
                    let same_name = self.name_click.as_ref().is_some_and(|(id, point, at)| {
                        id == &l.id
                            && time - at < options.max_double_click_delay
                            && point.distance(pointer) <= options.max_click_dist
                    });
                    if name.double_clicked()
                        && same_name
                        && !modifiers.command
                        && !modifiers.ctrl
                        && !modifiers.shift
                        && !modifiers.alt
                    {
                        self.begin_rename(doc, &l.id);
                    }
                    self.name_click = Some((l.id.clone(), pointer, time));
                }
            }
            if l.mask.is_some() {
                if self
                    .thumb_button(
                        ui,
                        mask_thumb,
                        self.selected == l.id && self.mask,
                        "Paint mask · Alt isolate · Shift toggle",
                    )
                    .clicked()
                {
                    self.select_content(&l.id);
                    self.layer_clipboard = false;
                    self.mask_step = None;
                    if ui.input(|i| i.modifiers.shift) {
                        self.layer_cmd("mask.toggle", json!({}), "Toggle mask");
                    } else {
                        self.mask = true;
                        if ui.input(|i| i.modifiers.alt) {
                            self.isolate = !self.isolate;
                        }
                        self.last_preview = None;
                    }
                }
            }
            if l.locked {
                let (_, r) = ui.allocate_space(Vec2::splat(16.0));
                icons::paint(ui.painter(), r, Icon::Lock, true);
            }
            disclosure
        })
        .inner
    }
    pub(super) fn layer_list(&mut self, ui: &mut egui::Ui, doc: &Document) {
        if self
            .layer_drag
            .as_ref()
            .is_some_and(|d| d.revision != doc.revision)
            || self
                .eye_sweep
                .as_ref()
                .is_some_and(|s| s.revision != doc.revision)
        {
            self.layer_drag = None;
            self.eye_sweep = None;
            self.message = "Layer gesture cancelled: document changed".into();
        }
        if let (Some(sweep), Some(pointer)) =
            (&mut self.eye_sweep, ui.input(|i| i.pointer.interact_pos()))
        {
            if ui.input(|i| i.pointer.primary_down()) {
                let segment = Rect::from_two_pos(sweep.previous, pointer).expand(1.0);
                for (id, rect) in &self.eye_rects {
                    if rect.intersects(segment) {
                        sweep.ids.insert(id.clone());
                    }
                }
                sweep.previous = pointer;
            }
        }
        self.layer_rects.clear();
        self.eye_rects.clear();
        let reserve = (ui.available_height() * 0.58).clamp(280.0, 480.0);
        let height = (ui.available_height() - reserve).max(46.0);
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false,true]).show(ui,|ui| {
            let mut original=vec![];rows(doc,None,0,&self.collapsed,&mut original);
            let base=ui.cursor().min;let width=ui.available_width();
            let pointer=ui.input(|i|i.pointer.interact_pos());
            let mut preview=doc.clone();let mut open=self.collapsed.clone();let mut drop_folder=None;
            if let (Some(drag),Some(pointer))=(&mut self.layer_drag,pointer) {
                let slot=((pointer.y-base.y)/46.0).floor().max(0.0) as usize;
                let target=original.get(slot).map(|&(i,depth)|(&doc.layers[i],depth));
                let (parent,before)=if let Some((target,depth))=target {
                    let y=pointer.y-(base.y+slot as f32*46.0);
                    if target.kind=="group" && (10.0..32.0).contains(&y) && !drag.ids.contains(&target.id) {
                        drop_folder=Some(target.id.clone());open.remove(&target.id);
                        (Some(target.id.as_str()),doc.layers.iter().find(|l|l.parent.as_deref()==Some(&target.id)&&!drag.ids.contains(&l.id)).map(|l|l.id.as_str()))
                    } else {
                        let parent=if depth>0 && pointer.x<base.x+45.0 {None}else{target.parent.as_deref()};
                        let before=if y<21.0 && !drag.ids.contains(&target.id) {Some(target.id.as_str())}else{
                            original.iter().skip(slot+1).map(|&(i,_)|&doc.layers[i]).find(|l|l.parent.as_deref()==parent&&!drag.ids.contains(&l.id)).map(|l|l.id.as_str())
                        };
                        (parent,before)
                    }
                } else {(None,None)};
                match crate::tree::reparent(doc,&drag.ids,parent,before) {
                    Ok((p,commands))=>{drag.target=p.layers.iter().position(|l|l.id==drag.id).unwrap_or(0);drag.commands=commands;preview=p;}
                    Err(e)=>{drag.commands.clear();self.message=e;}
                }
            }
            if let Some(sweep)=&self.eye_sweep {for l in &mut preview.layers {if sweep.ids.contains(&l.id){l.visible=sweep.visible;}}}
            let mut ordered=vec![];rows(&preview,None,0,&open,&mut ordered);
            let mut floating=None;
            let mut anchors=HashMap::new();
            for (slot,(index,depth)) in ordered.iter().copied().enumerate() {
                let l=&preview.layers[index];let target=slot as f32*46.0;
                let time=ui.input(|i| i.time);
                let ai=self.ai_layer(&l.id,time);
                let duration=if ai {0.36}else{0.12};
                let offset=ui.ctx().animate_value_with_time(ui.id().with((&doc.id,&l.id,"position")),target,duration);
                let indent=ui.ctx().animate_value_with_time(ui.id().with((&doc.id,&l.id,"indent")),depth as f32*TREE_INDENT,duration);
                let arrival=self.animation.arrival(&l.id,time);
                let rect=Rect::from_min_size(base+Vec2::new((1.0-arrival)*14.0,offset),Vec2::new(width,42.0));
                self.layer_rects.insert(l.id.clone(),rect);
                let held=self.layer_drag.as_ref().is_some_and(|d|d.ids.contains(&l.id));
                let response=ui.interact(rect,ui.id().with((&l.id,"row")),egui::Sense::click_and_drag());
                if response.drag_started() && self.eye_sweep.is_none() && !self.rename_edit.as_ref().is_some_and(|edit| edit.layer == l.id) {
                    if !self.selection_layers.contains(&l.id) {self.select_content(&l.id);}
                    let start=ui.input(|i|i.pointer.press_origin()).unwrap_or(rect.center());
                    self.layer_drag=Some(LayerDrag {gesture:crate::engine::id(),id:l.id.clone(),revision:doc.revision,offset:start.y-rect.top(),target:index,ids:crate::tree::roots(doc,&self.selection_layers),commands:vec![]});
                }
                if response.clicked(){self.select_row(ui,doc,&l.id);}
                let thumb=if l.kind=="group"{None}else{self.thumbnail(ui,doc,&l.id,false)};
                let mask_thumb=if l.mask.is_some(){self.thumbnail(ui,doc,&l.id,true)}else{None};
                if held {
                    let gap=Rect::from_min_size(base+Vec2::new(0.0,target),Vec2::new(width,42.0));
                    ui.painter().line_segment([gap.left_top(),gap.right_top()],Stroke::new(2.0_f32,ACCENT));
                    if self.layer_drag.as_ref().is_some_and(|d|d.id==l.id){floating=Some((l.clone(),thumb,depth));}
                } else {
                    let selected=self.selection_layers.contains(&l.id);
                    let hover=ui.ctx().animate_bool_with_time(response.id.with("hover"),response.hovered(),0.08);
                    if selected {ui.painter().rect_filled(rect,5,Color32::from_rgb(74,48,71));
                        ui.painter().rect_filled(Rect::from_min_size(rect.min+Vec2::new(0.0,5.0),Vec2::new(2.0,32.0)),1,ACCENT);}
                    else if hover>0.0 {ui.painter().rect_filled(rect,5,Color32::WHITE.gamma_multiply(0.035*hover));}
                    if ai {
                        let pulse=0.065+0.02*(time as f32*4.0).sin();
                        ui.painter().rect_filled(rect,5,AI_BLUE.gamma_multiply(pulse));
                        ui.painter().rect_filled(Rect::from_min_size(rect.min+Vec2::new(0.0,4.0),Vec2::new(2.0,34.0)),1,AI_BLUE);
                    }
                    if drop_folder.as_deref()==Some(&l.id){ui.painter().rect_stroke(rect,4,Stroke::new(1.0_f32,ACCENT),egui::StrokeKind::Inside);}
                    let disclosure=ui.scope_builder(egui::UiBuilder::new().id_salt(&l.id).max_rect(rect.shrink2(Vec2::new(4.0,4.0))),|ui| {
                        ui.set_opacity(arrival);
                        self.layer_contents(ui,doc,l,indent,thumb.as_ref(),mask_thumb.as_ref())
                    }).inner;
                    anchors.insert(l.id.clone(),TreeAnchor {disclosure,ai,opacity:arrival});
                }
                response.on_hover_text("Click to select · Ctrl/Cmd or Shift for multiple · Drag into folders · Drag left to move out").context_menu(|ui|self.layer_menu(ui,doc,l));
            }
            paint_tree_guides(ui.painter(),&preview,&anchors);
            if let (Some((layer,thumb,depth)),Some(pointer),Some(drag))=(floating,pointer,&self.layer_drag) {
                let painter=ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip,ui.id().with("held layer")));
                let rect=Rect::from_min_size(egui::pos2(base.x,pointer.y-drag.offset),Vec2::new(width,42.0));
                painter.rect_filled(rect.translate(Vec2::new(0.0,3.0)),5,Color32::BLACK.gamma_multiply(0.35));
                painter.rect_filled(rect,5,Color32::from_rgb(74,48,71));
                painter.line_segment([rect.left_top(),rect.left_bottom()],Stroke::new(2.0_f32,ACCENT));
                let x=rect.left()+75.0+depth as f32*TREE_INDENT;
                let image_rect=Rect::from_min_size(egui::pos2(x,rect.top()+6.0),Vec2::splat(30.0));
                if layer.kind=="group"{icons::paint(&painter,image_rect,Icon::Folder,true);}
                else if let Some(t)=thumb {painter.image(t.id(),image_rect,Rect::from_min_max(Pos2::ZERO,Pos2::new(1.0,1.0)),Color32::WHITE);}
                let label=if drag.ids.len()>1 {format!("{}  +{}",layer.name,drag.ids.len()-1)}else{layer.name};
                painter.text(egui::pos2(x+38.0,rect.center().y),egui::Align2::LEFT_CENTER,label,egui::FontId::proportional(12.0),Color32::WHITE);
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            if ui.input(|i|i.pointer.button_released(egui::PointerButton::Primary)) {
                if let Some(drag)=self.layer_drag.take() {
                    if pointer.is_some_and(|p|ui.clip_rect().contains(p)) && !drag.commands.is_empty() {
                        let result=self.shared.lock().unwrap().edit("human",&drag.commands,Some(drag.revision),None,"Move layers");
                        self.message=result.map(|_|"Move layers".into()).unwrap_or_else(|e|e);self.last_preview=None;
                        if let Some(id)=drop_folder {self.collapsed.remove(&id);}
                    }
                }
                if let Some(sweep)=self.eye_sweep.take() {
                    let commands=sweep.ids.iter().map(|id|json!({"op":"layer.update","layer":id,"visible":sweep.visible})).collect::<Vec<_>>();
                    let result=self.shared.lock().unwrap().edit("human",&commands,Some(sweep.revision),None,"Layer visibility sweep");
                    self.message=result.map(|_|"Layer visibility".into()).unwrap_or_else(|e|e);self.last_preview=None;
                }
            }
            ui.advance_cursor_after_rect(Rect::from_min_size(base,Vec2::new(width,ordered.len() as f32*46.0)));
        });
    }
}
