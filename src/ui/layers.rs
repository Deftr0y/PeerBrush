use super::*;
use crate::engine::Layer;
const TREE_INDENT: f32 = 22.0;
#[derive(Default)]
struct FolderSummary {
    direct: usize,
    descendants: usize,
    selected: usize,
}

struct TreeInfo<'a> {
    doc: &'a Document,
    folders: HashMap<String, FolderSummary>,
    paths: HashMap<String, String>,
}
impl<'a> TreeInfo<'a> {
    fn new(doc: &'a Document, selected: &HashSet<String>) -> Self {
        let layers: HashMap<_, _> = doc.layers.iter().map(|l| (l.id.as_str(), l)).collect();
        let mut folders: HashMap<String, FolderSummary> = HashMap::new();
        let mut paths = HashMap::new();
        for layer in &doc.layers {
            let mut names = vec![layer.name.as_str()];
            let mut parent = layer.parent.as_deref();
            for depth in 0..16 {
                let Some(ancestor) = parent.and_then(|id| layers.get(id)) else {
                    break;
                };
                let summary = folders.entry(ancestor.id.clone()).or_default();
                summary.direct += usize::from(depth == 0);
                summary.descendants += 1;
                summary.selected += usize::from(selected.contains(&layer.id));
                names.push(&ancestor.name);
                parent = ancestor.parent.as_deref();
            }
            names.reverse();
            paths.insert(layer.id.clone(), names.join(" / "));
        }
        Self {
            doc,
            folders,
            paths,
        }
    }
    fn path(&self, id: &str) -> &str {
        self.paths.get(id).map(String::as_str).unwrap_or("")
    }
    fn tooltip(&self, layer: &Layer) -> String {
        let mut text = self.path(&layer.id).to_owned();
        if layer.kind == "group" {
            let empty = FolderSummary::default();
            let summary = self.folders.get(&layer.id).unwrap_or(&empty);
            text.push_str(&format!(
                "\n{} direct children · {} total descendants",
                summary.direct, summary.descendants
            ));
            if summary.selected > 0 {
                text.push_str(&format!("\n{} selected inside", summary.selected));
            }
        }
        text
    }
}
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
    pub(super) fn set_solo(&mut self, active: bool) {
        self.isolate = active;
        self.solo_target = active.then(|| (self.selected.clone(), self.mask));
        self.last_preview = None;
    }
    pub(super) fn sync_solo(&mut self, doc: &Document) {
        if !self.isolate {
            self.solo_target = None;
            return;
        }
        let target = (self.selected.clone(), self.mask);
        let valid = doc
            .layers
            .iter()
            .any(|l| l.id == target.0 && (!target.1 || l.mask.is_some()));
        if !valid || self.solo_target.as_ref().is_some_and(|old| old != &target) {
            self.set_solo(false);
        } else {
            self.solo_target = Some(target);
        }
    }
    pub(super) fn select_channel(&mut self, mask: bool) {
        if self.mask != mask {
            self.set_solo(false);
            self.mask_step = None;
            self.last_preview = None;
        }
        self.mask = mask;
    }
    pub(super) fn solo_caption(&self, doc: &Document) -> Option<String> {
        self.isolate
            .then(|| doc.layers.iter().find(|l| l.id == self.selected))
            .flatten()
            .map(|l| {
                format!(
                    "Solo {} · {}",
                    if self.mask { "Mask" } else { "Color" },
                    l.name
                )
            })
    }
    fn solo_row_opacity(&self, id: &str) -> f32 {
        if self.isolate && id != self.selected {
            0.55
        } else {
            1.
        }
    }
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
        self.select_target(id, false);
    }
    fn select_target(&mut self, id: &str, mask: bool) {
        if self.selected != id || self.mask != mask {
            self.set_solo(false);
        }
        self.liquify_effect = None;
        self.selected = id.into();
        self.selection_layers = [id.to_string()].into_iter().collect();
        self.selection_anchor = id.into();
        self.mask = mask;
        self.mask_step = None;
        self.last_preview = None;
    }
    fn select_row(&mut self, ui: &egui::Ui, doc: &Document, id: &str) {
        let previous = (self.selected.clone(), self.mask);
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
        if previous != (self.selected.clone(), false) {
            self.set_solo(false);
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
        l: &Layer,
        indent: f32,
        thumb: Option<&TextureHandle>,
        mask_thumb: Option<&TextureHandle>,
        tree: &TreeInfo<'_>,
    ) -> Rect {
        let doc = tree.doc;
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
                    if self.collapsed.contains(&l.id) {
                        "Expand folder"
                    } else {
                        "Collapse folder"
                    },
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
            let summary = if l.kind == "group" {
                let empty = FolderSummary::default();
                let counts = tree.folders.get(&l.id).unwrap_or(&empty);
                Some(if self.collapsed.contains(&l.id) && counts.selected > 0 {
                    (
                        format!("{} · {} selected", counts.descendants, counts.selected),
                        true,
                    )
                } else {
                    (counts.descendants.to_string(), false)
                })
            } else {
                None
            };
            let summary_width = summary
                .as_ref()
                .map(|(text, _)| {
                    ui.painter()
                        .layout_no_wrap(text.clone(), egui::FontId::proportional(10.), MUTED)
                        .size()
                        .x
                        + 5.
                })
                .unwrap_or(0.);
            let name_width = (ui.available_width()
                - mask_width
                - summary_width
                - if l.locked { 21.0 } else { 0.0 })
            .max(25.0);
            if self
                .rename_edit
                .as_ref()
                .is_some_and(|edit| edit.layer == l.id)
            {
                self.inline_rename(ui, name_width);
            } else {
                let name = filename::click_label(
                    ui,
                    &l.name,
                    name_width,
                    egui::FontId::new(
                        12.,
                        if l.kind == "group" {
                            egui::FontFamily::Name("semibold".into())
                        } else {
                            egui::FontFamily::Proportional
                        },
                    ),
                    if reserved {
                        AI_BLUE
                    } else if l.visible {
                        Color32::WHITE
                    } else {
                        MUTED
                    },
                )
                .on_hover_text(tree.tooltip(l));
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
            if let Some((text, hidden_selection)) = summary {
                let color = if reserved {
                    AI_BLUE
                } else if hidden_selection {
                    ACCENT
                } else {
                    MUTED
                };
                let response = ui
                    .add(
                        egui::Label::new(RichText::new(text).size(10.).color(color)).sense(
                            if hidden_selection {
                                egui::Sense::click()
                            } else {
                                egui::Sense::hover()
                            },
                        ),
                    )
                    .on_hover_text(if hidden_selection {
                        format!(
                            "{}\nClick to reveal selected layers without changing selection",
                            tree.tooltip(l)
                        )
                    } else {
                        tree.tooltip(l)
                    });
                if response.clicked() {
                    self.collapsed.remove(&l.id);
                    for child in doc
                        .layers
                        .iter()
                        .filter(|child| self.selection_layers.contains(&child.id))
                    {
                        let mut chain = vec![];
                        let mut parent = child.parent.as_deref();
                        for _ in 0..16 {
                            let Some(ancestor) = parent.and_then(|id| {
                                doc.layers.iter().find(|candidate| candidate.id == id)
                            }) else {
                                break;
                            };
                            chain.push(ancestor.id.clone());
                            if ancestor.id == l.id {
                                for id in chain {
                                    self.collapsed.remove(&id);
                                }
                                break;
                            }
                            parent = ancestor.parent.as_deref();
                        }
                    }
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
                    let was_solo_mask = self.isolate && self.selected == l.id && self.mask;
                    self.layer_clipboard = false;
                    self.mask_step = None;
                    if ui.input(|i| i.modifiers.shift) {
                        self.select_content(&l.id);
                        self.layer_cmd("mask.toggle", json!({}), "Toggle mask");
                    } else {
                        self.select_target(&l.id, true);
                        if ui.input(|i| i.modifiers.alt) {
                            self.set_solo(!was_solo_mask);
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
        let mut context_menu = self
            .layer_context
            .take()
            .filter(|(document, _)| document == &doc.id)
            .map(|(_, menu)| menu)
            .unwrap_or_default();
        if let Some(root) = context_menu.as_ref() {
            let close = ui.input(|input| {
                input.key_pressed(egui::Key::Escape)
                    || input.pointer.any_pressed()
                        && input
                            .pointer
                            .interact_pos()
                            .is_some_and(|position| !root.menu_state.read().area_contains(position))
            });
            if close {
                *context_menu = None;
            }
        }
        let mut context_visible = false;
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
            let mut drop_hint=None;
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
                    Ok((p,commands))=>{
                        let info=TreeInfo::new(doc,&self.selection_layers);
                        let destination=parent.map(|id|info.path(id)).unwrap_or("Top level");
                        drop_hint=Some(if let Some(id)=before {
                            let name=doc.layers.iter().find(|l|l.id==id).map(|l|l.name.as_str()).unwrap_or("");
                            format!("{} · before {}",destination,name)
                        } else {format!("{} · at end",destination)});
                        drag.target=p.layers.iter().position(|l|l.id==drag.id).unwrap_or(0);drag.commands=commands;preview=p;
                    }
                    Err(e)=>{drag.commands.clear();self.message=e;}
                }
            }
            if let Some(sweep)=&self.eye_sweep {for l in &mut preview.layers {if sweep.ids.contains(&l.id){l.visible=sweep.visible;}}}
            let mut ordered=vec![];rows(&preview,None,0,&open,&mut ordered);
            let tree=TreeInfo::new(&preview,&self.selection_layers);
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
                let solo_opacity = self.solo_row_opacity(&l.id);
                let row_opacity = arrival * solo_opacity;
                let rect=Rect::from_min_size(base+Vec2::new((1.0-arrival)*14.0,offset),Vec2::new(width,42.0));
                self.layer_rects.insert(l.id.clone(),rect);
                let held=self.layer_drag.as_ref().is_some_and(|d|d.ids.contains(&l.id));
                let response=ui.interact(rect,ui.id().with((&doc.id,&l.id,"row")),egui::Sense::click_and_drag());
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
                    if selected {ui.painter().rect_filled(rect,5,Color32::from_rgb(74,48,71).gamma_multiply(solo_opacity));
                        ui.painter().rect_filled(Rect::from_min_size(rect.min+Vec2::new(0.0,5.0),Vec2::new(2.0,32.0)),1,ACCENT.gamma_multiply(solo_opacity));}
                    else if hover>0.0 {ui.painter().rect_filled(rect,5,Color32::WHITE.gamma_multiply(0.035*hover*solo_opacity));}
                    if ai {
                        let pulse=0.065+0.02*(time as f32*4.0).sin();
                        ui.painter().rect_filled(rect,5,AI_BLUE.gamma_multiply(pulse*solo_opacity));
                        ui.painter().rect_filled(Rect::from_min_size(rect.min+Vec2::new(0.0,4.0),Vec2::new(2.0,34.0)),1,AI_BLUE.gamma_multiply(solo_opacity));
                    }
                    if drop_folder.as_deref()==Some(&l.id){ui.painter().rect_stroke(rect,4,Stroke::new(1.0_f32,ACCENT),egui::StrokeKind::Inside);}
                    let disclosure=ui.scope_builder(egui::UiBuilder::new().id_salt(&l.id).max_rect(rect.shrink2(Vec2::new(4.0,4.0))),|ui| {
                        ui.set_opacity(row_opacity);
                        self.layer_contents(ui,l,indent,thumb.as_ref(),mask_thumb.as_ref(),&tree)
                    }).inner;
                    anchors.insert(l.id.clone(),TreeAnchor {disclosure,ai,opacity:row_opacity});
                }
                // Child controls own their primary interactions. Test the clipped row
                // directly for secondary clicks, including over those controls.
                if !held && ui.rect_contains_pointer(rect)
                    && ui.input(|i|i.pointer.button_clicked(egui::PointerButton::Secondary) && !i.pointer.primary_down())
                {
                    if let Some(position)=ui.input(|i|i.pointer.interact_pos()) {
                        *context_menu=Some(egui::menu::MenuRoot::new(position,response.id));
                    }
                }
                context_visible |= context_menu.as_ref().is_some_and(|root|root.id==response.id);
                let response=response.on_hover_text(format!("{}\nClick to select · Ctrl/Cmd or Shift for multiple · Drag into folders · Drag left to move out · Right-click anywhere for options",tree.tooltip(l)));
                context_menu.show(&response,|ui|self.layer_menu(ui,doc,l));
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
                if let Some(hint)=drop_hint {
                    let galley=painter.layout_no_wrap(hint,egui::FontId::proportional(11.0),ACCENT);
                    let at=egui::pos2(rect.left()+8.,rect.bottom()+4.);
                    painter.with_clip_rect(Rect::from_min_size(at,Vec2::new(width-16.,20.))).galley(at,galley,ACCENT);
                }
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
        if context_visible && context_menu.is_some() {
            self.layer_context = Some((doc.id.clone(), context_menu));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(depth: u16) -> (PeerBrush, egui::Context, Vec<String>, Document) {
        let ctx = egui::Context::default();
        let mut engine = crate::engine::Engine::new();
        engine.doc = Document::new_depth(8, 8, depth).unwrap();
        let mut outer = Layer::new("Outer", "group", 8, 8);
        outer.opacity = 0.6;
        let mut inner = Layer::new("Inner", "group", 8, 8);
        inner.parent = Some(outer.id.clone());
        let mut leaf = Layer::new("Nested paint", "paint", 8, 8);
        leaf.parent = Some(inner.id.clone());
        let mut sibling = Layer::new("Sibling", "paint", 8, 8);
        sibling.parent = Some(outer.id.clone());
        let loose = Layer::new("Loose paint", "paint", 8, 8);
        let unrelated = Layer::new("Other folder", "group", 8, 8);
        let mut other = Layer::new("Other paint", "paint", 8, 8);
        other.parent = Some(unrelated.id.clone());
        engine.doc.layers = vec![outer, inner, leaf, sibling, loose, unrelated, other];
        engine.doc.ensure_depth();
        for (index, color) in [
            (2, [12345, 23457, 34569, 65535]),
            (4, [52345, 11357, 4569, 65535]),
        ] {
            for y in 0..8 {
                for x in 0..8 {
                    engine.doc.layers[index].pixels.set16(x, y, color);
                }
            }
        }
        let ids: Vec<_> = engine.doc.layers.iter().map(|l| l.id.clone()).collect();
        engine
            .edit(
                "human",
                &[json!({"op":"mask.add","layer":ids[2],"value":183})],
                None,
                None,
                "Native mask",
            )
            .unwrap();
        engine.undo.clear();
        let before = engine.doc.clone();
        let mut app = PeerBrush::init(
            &ctx,
            std::sync::Arc::new(std::sync::Mutex::new(engine)),
            crate::server::Connection {
                port: 0,
                token: String::new(),
                state_dir: Default::default(),
                instance_lock: None,
                availability: Default::default(),
            },
        );
        app.selected = ids[2].clone();
        app.selection_layers = [ids[2].clone()].into_iter().collect();
        (app, ctx, ids, before)
    }
    fn frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let time = ctx.input(|i| i.time) + 0.1;
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 1100.))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        )
    }
    fn button(at: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    fn text(output: &egui::FullOutput, value: &str) -> Pos2 {
        output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == value => {
                    Some(t.visual_bounding_rect().center())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing visible {value}"))
    }
    #[test]
    fn shaped_layer_names_keep_inline_rename_selection_and_native_work() {
        for depth in [8, 16] {
            let (mut app, ctx, ids, _) = fixture(depth);
            let logical = "שלום مرحبا हिन्दी 漢字";
            let (before, undo_count) = {
                let mut engine = app.shared.lock().unwrap();
                engine.doc.layers[2].name = logical.into();
                engine.doc.selection = Some([1, 1, 6, 6]);
                (
                    serde_json::to_value(&engine.doc).unwrap(),
                    engine.undo.len(),
                )
            };
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == logical)));
            let row = app.layer_rects[&ids[2]];
            let at = egui::pos2(row.right() - 80., row.center().y);
            for _ in 0..2 {
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(at), button(at, true)],
                );
                frame(&mut app, &ctx, vec![button(at, false)]);
            }
            let edit = app
                .rename_edit
                .as_ref()
                .expect("Double-click shaped name must rename inline");
            assert_eq!(edit.layer, ids[2]);
            assert_eq!(edit.original, logical);
            assert_eq!(edit.text, logical);
            assert_eq!(app.selected, ids[2]);
            let engine = app.shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
            assert_eq!(engine.undo.len(), undo_count);
            assert_eq!(engine.doc.bit_depth, depth);
        }
    }
    #[test]
    fn collapsed_folder_selection_reveal_keeps_native_mask_target_and_other_folders() {
        for depth in [8, 16] {
            let (mut app, ctx, ids, before) = fixture(depth);
            app.selection_layers.insert(ids[6].clone());
            app.mask = true;
            app.collapsed = [ids[0].clone(), ids[1].clone(), ids[5].clone()]
                .into_iter()
                .collect();
            frame(&mut app, &ctx, vec![]);
            let output = frame(&mut app, &ctx, vec![]);
            let at = text(&output, "3 · 1 selected");
            assert!(!app.layer_rects.contains_key(&ids[2]));
            let selected = app.selection_layers.clone();
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at), button(at, true)],
            );
            frame(&mut app, &ctx, vec![button(at, false)]);
            frame(&mut app, &ctx, vec![]);
            assert!(
                app.layer_rects.contains_key(&ids[2]),
                "at={at:?} root={:?} collapsed={:?} selected={:?}",
                app.layer_rects.get(&ids[0]),
                app.collapsed,
                app.selection_layers
            );
            assert!(!app.layer_rects.contains_key(&ids[6]));
            assert!(app.collapsed.contains(&ids[5]));
            assert_eq!(app.selected, ids[2]);
            assert_eq!(app.selection_layers, selected);
            assert!(app.mask);
            let e = app.shared.lock().unwrap();
            assert!(e.undo.is_empty());
            assert_eq!(
                serde_json::to_value(&e.doc).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        }
    }
    #[test]
    fn nested_drag_destination_matches_live_native_pixels_and_shared_reservation_guards() {
        for depth in [8, 16] {
            for reserved in [false, true] {
                let (mut app, ctx, ids, before) = fixture(depth);
                app.select_content(&ids[4]);
                app.collapsed.insert(ids[1].clone());
                if reserved {
                    app.shared
                        .lock()
                        .unwrap()
                        .reserve(
                            "agent",
                            "Paint work",
                            vec![crate::engine::Scope::layer(&ids[4])],
                        )
                        .unwrap();
                }
                for _ in 0..3 {
                    frame(&mut app, &ctx, vec![]);
                }
                let at = app.layer_rects[&ids[4]].left_top() + Vec2::new(20., 2.);
                let destination = app.layer_rects[&ids[1]].center();
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(at), button(at, true)],
                );
                frame(&mut app, &ctx, vec![egui::Event::PointerMoved(destination)]);
                let output = frame(&mut app, &ctx, vec![]);
                text(&output, "Outer / Inner · before Nested paint");
                assert!(
                    app.layer_rects.contains_key(&ids[2]),
                    "Destination opens in presentation only"
                );
                let commands = app.layer_drag.as_ref().unwrap().commands.clone();
                assert!(commands
                    .iter()
                    .any(|c| c["op"] == "layer.parent" && c["parent"] == ids[1]));
                let mut preview = crate::engine::Engine::new();
                preview.doc = before.clone();
                preview
                    .edit("human", &commands, None, None, "Preview")
                    .unwrap();
                let expected = preview.doc.export_png().unwrap();
                let rendered = preview.doc.preview(None, 1400, None, false).unwrap().2;
                assert_ne!(expected, before.export_png().unwrap());
                for _ in 0..50 {
                    if app
                        .canvas_pixels
                        .as_ref()
                        .is_some_and(|(_, _, pixels)| pixels == &rendered)
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    frame(&mut app, &ctx, vec![]);
                }
                assert_eq!(
                    app.canvas_pixels.as_ref().map(|(_, _, pixels)| pixels),
                    Some(&rendered),
                    "Actual canvas receives the shared-engine hierarchy preview"
                );
                assert!(app.shared.lock().unwrap().undo.is_empty());
                assert_eq!(app.shared.lock().unwrap().doc.revision, before.revision);
                frame(&mut app, &ctx, vec![button(destination, false)]);
                let mut e = app.shared.lock().unwrap();
                if reserved {
                    assert!(e.undo.is_empty());
                    assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                    assert_eq!(e.doc.layers[4].parent, None);
                } else {
                    assert_eq!(e.undo.len(), 1);
                    assert_eq!(e.doc.export_png().unwrap(), expected);
                    for original in &before.layers {
                        let current = e.doc.layers.iter().find(|l| l.id == original.id).unwrap();
                        assert_eq!(
                            serde_json::to_value(&current.pixels).unwrap(),
                            serde_json::to_value(&original.pixels).unwrap()
                        );
                        assert_eq!(
                            serde_json::to_value(&current.mask).unwrap(),
                            serde_json::to_value(&original.mask).unwrap()
                        );
                    }
                    e.undo("human").unwrap();
                    assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                }
            }
        }
    }
}
