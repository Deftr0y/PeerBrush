use super::*;
use crate::engine::{
    view::{DocumentView, LayerView},
    Layer,
};
const TREE_INDENT: f32 = 22.0;

fn sweep_hits_eye(from: Pos2, to: Pos2, eye: Rect) -> bool {
    if !eye.is_positive() {
        return false;
    }
    let mut enter: f32 = 0.;
    let mut exit: f32 = 1.;
    for (start, end, min, max) in [
        (from.x, to.x, eye.left(), eye.right()),
        (from.y, to.y, eye.top(), eye.bottom()),
    ] {
        let delta = end - start;
        if delta == 0. {
            if start < min || start > max {
                return false;
            }
        } else {
            let a = (min - start) / delta;
            let b = (max - start) / delta;
            enter = enter.max(a.min(b));
            exit = exit.min(a.max(b));
        }
    }
    enter <= exit
}

pub(super) fn label_color(label: &str) -> Option<Color32> {
    let [r, g, b] = match label {
        "red" => [232, 104, 105],
        "orange" => [237, 143, 85],
        "yellow" => [238, 196, 99],
        "green" => [136, 192, 101],
        "blue" => [107, 165, 222],
        "violet" => [174, 136, 218],
        "gray" => [164, 161, 172],
        "seafoam" => [112, 201, 173],
        "indigo" => [129, 139, 227],
        "magenta" => [221, 116, 192],
        "fuchsia" => [238, 123, 159],
        _ => return None,
    };
    Some(Color32::from_rgb(r, g, b))
}

pub(super) const BLEND_ENDPOINTS: [&str; 4] = ["Black low", "Black high", "White low", "White high"];

pub(super) fn blend_range(
    ui: &mut egui::Ui,
    id: egui::Id,
    channel: usize,
    values: &mut [u8; 4],
    mut displayed: [f32; 4],
) -> Option<egui::Response> {
    let (outer, _) = ui.allocate_exact_size(Vec2::new(224., 88.), egui::Sense::hover());
    let ramp = Rect::from_center_size(outer.center(), Vec2::new(outer.width() - 18., 12.));
    let x = |value: f32| ramp.left() + value / 255. * ramp.width();
    gradient_stops::paint_ramp(ui.painter(), ramp, |fraction| {
        let n = (fraction * 255.).round() as u8;
        match channel {
            1 => Color32::from_rgb(n, 0, 0),
            2 => Color32::from_rgb(0, n, 0),
            3 => Color32::from_rgb(0, 0, n),
            _ => Color32::from_gray(n),
        }
    });
    let mut changed = None;
    // Separate handle rows also work when multiple endpoints have the same value.
    let rows = [ramp.top() - 10., ramp.bottom() + 10., ramp.top() - 28., ramp.bottom() + 28.];
    for (index, (name, y)) in BLEND_ENDPOINTS.into_iter().zip(rows).enumerate() {
        let label = format!("{name} · {}", displayed[index].round() as u8);
        let center = egui::pos2(x(displayed[index]), y);
        let (mut response, point) = controls::point_handle(ui, id.with(index), center, &label);
        if let Some(point) = point.filter(|point| (point.x - center.x).abs() > 0.001) {
            let low = index.checked_sub(1).map_or(0, |previous| values[previous]);
            let high = values.get(index + 1).copied().unwrap_or(255);
            let value = (((point.x - ramp.left()) / ramp.width() * 255.).round() as u8).clamp(low, high);
            if value != values[index] {
                values[index] = value;
                displayed[index] = f32::from(value);
                response.mark_changed();
            }
        }
        let x = x(displayed[index]);
        let center = egui::pos2(x, y);
        let edge = if y < ramp.top() { ramp.top() } else { ramp.bottom() };
        let stroke = if response.hovered() || response.dragged() { ACCENT } else { MUTED };
        ui.painter().line_segment([center, egui::pos2(x, edge)], Stroke::new(1., stroke));
        let color = if index < 2 { Color32::from_gray(24) } else { Color32::from_gray(238) };
        ui.painter().circle(center, 4., color, Stroke::new(1.2, stroke));
        if response.changed() {
            changed = Some(response);
        }
    }
    changed
}

#[derive(Default)]
struct FolderSummary {
    direct: usize,
    descendants: usize,
    selected: usize,
}

struct TreeInfo<'a> {
    doc: DocumentView<'a>,
    order: &'a crate::tree::TreeOrder,
    visibility: Option<(bool, HashSet<String>)>,
    ordinals: HashMap<&'a str, usize>,
    folders: HashMap<&'a str, FolderSummary>,
}
impl<'a> TreeInfo<'a> {
    fn ordered(
        doc: DocumentView<'a>,
        order: &'a crate::tree::TreeOrder,
        selected: &HashSet<String>,
        visibility: Option<(bool, HashSet<String>)>,
    ) -> Self {
        let mut result = Self {
            doc,
            order,
            visibility,
            ordinals: doc
                .layers()
                .enumerate()
                .map(|(index, layer)| (layer.id(), index))
                .collect(),
            folders: HashMap::new(),
        };
        for (index, layer) in doc.layers().enumerate() {
            let mut parent = result.order.parent(index, layer);
            let selected = usize::from(selected.contains(layer.id()));
            for depth in 0..16 {
                let Some(&ordinal) = parent.and_then(|id| result.ordinals.get(id)) else {
                    break;
                };
                let ancestor = doc.layer(ordinal).unwrap();
                let summary = result.folders.entry(ancestor.id()).or_default();
                summary.direct += usize::from(depth == 0);
                summary.descendants += 1;
                summary.selected += selected;
                parent = result.order.parent(ordinal, ancestor);
            }
        }
        result
    }
    fn parent(&self, layer: LayerView<'a>) -> Option<&str> {
        self.order.parent(*self.ordinals.get(layer.id())?, layer)
    }
    fn visible(&self, layer: LayerView<'a>) -> bool {
        self.visibility
            .as_ref()
            .filter(|(_, ids)| ids.contains(layer.id()))
            .map(|(visible, _)| *visible)
            .unwrap_or(layer.visible())
    }
    fn path(&self, id: &str) -> String {
        folder_path(id, |id| {
            let &index = self.ordinals.get(id)?;
            let ancestor = self.doc.layer(index).unwrap();
            Some((ancestor.name(), self.order.parent(index, ancestor)))
        })
    }
    fn tooltip(&self, layer: LayerView<'a>) -> String {
        let mut text = self.path(layer.id());
        if layer.kind() == "group" {
            let empty = FolderSummary::default();
            let summary = self.folders.get(layer.id()).unwrap_or(&empty);
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

fn folder_path<'a>(id: &str, lookup: impl Fn(&str) -> Option<(&'a str, Option<&'a str>)>) -> String {
    let mut names = Vec::new();
    let mut current = Some(id);
    for _ in 0..=16 {
        let Some((name, parent)) = current.and_then(&lookup) else { break };
        names.push(name);
        current = parent;
    }
    names.reverse();
    names.join(" / ")
}
fn layer_hover(response: egui::Response, text: impl FnOnce() -> String) -> egui::Response {
    response.on_hover_ui(|ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.add(egui::Label::new(text()));
    })
}

struct TreeAnchor {
    disclosure: Rect,
    ai: bool,
    opacity: f32,
}
fn paint_tree_guides(
    painter: &egui::Painter,
    tree: &TreeInfo<'_>,
    anchors: &HashMap<&str, TreeAnchor>,
) {
    let mut visible_children = HashMap::<&str, Vec<_>>::new();
    for (index, layer) in tree.doc.layers().enumerate() {
        if let Some((parent, anchor)) = tree.order.parent(index, layer).zip(anchors.get(layer.id())) {
            if anchors.contains_key(parent) {
                visible_children.entry(parent).or_default().push((layer, anchor));
            }
        }
    }
    for parent in tree.doc.layers().filter(|l| l.kind() == "group") {
        let Some((root, children)) = anchors.get(parent.id()).zip(visible_children.get(parent.id()))
        else {
            continue;
        };
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
                Stroke::new(1., color.gamma_multiply(root.opacity)),
            );
        }
        for &(layer, child) in children {
            let y = child.disclosure.center().y;
            let end = if layer.kind() == "group" {
                child.disclosure.left() - 2.
            } else {
                child.disclosure.right() - 4.
            };
            let color = if root.ai || child.ai {
                AI_BLUE
            } else {
                MUTED.gamma_multiply(0.8)
            };
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2(end, y)],
                Stroke::new(1., color.gamma_multiply(root.opacity.min(child.opacity))),
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
    doc: DocumentView<'_>,
    order: &crate::tree::TreeOrder,
    collapsed: &HashSet<String>,
) -> Vec<(usize, usize)> {
    let mut children = HashMap::<Option<&str>, Vec<usize>>::new();
    for &index in order.indices() {
        let layer = doc.layer(index).unwrap();
        children.entry(order.parent(index, layer)).or_default().push(index);
    }
    let mut visible = Vec::with_capacity(order.indices().len());
    append_rows(doc, &children, None, 0, collapsed, &mut visible);
    visible
}

fn append_rows(
    doc: DocumentView<'_>,
    children: &HashMap<Option<&str>, Vec<usize>>,
    parent: Option<&str>,
    depth: usize,
    collapsed: &HashSet<String>,
    out: &mut Vec<(usize, usize)>,
) {
    for &index in children.get(&parent).into_iter().flatten() {
        let layer = doc.layer(index).unwrap();
        out.push((index, depth));
        if depth < 16 && layer.kind() == "group" && !collapsed.contains(layer.id()) {
            append_rows(doc, children, Some(layer.id()), depth + 1, collapsed, out);
        }
    }
}

pub(super) struct LayerDropPlan {
    parent: Option<String>,
    before: Option<String>,
    order: crate::tree::TreeOrder,
    hint: String,
}

impl LayerDrag {
    fn plan_drop(
        &mut self,
        doc: DocumentView<'_>,
        parent: Option<&str>,
        before: Option<&str>,
    ) -> Result<Arc<LayerDropPlan>, String> {
        if let Some(plan) = self.drop_plan.as_ref().filter(|plan| {
            plan.parent.as_deref() == parent && plan.before.as_deref() == before
        }) {
            return Ok(plan.clone());
        }
        self.drop_plan = None;
        self.commands.clear();
        let (order, commands) = crate::tree::reparent_view(doc, &self.ids, parent, before)?;
        let destination = parent.map_or_else(
            || "Top level".to_owned(),
            |id| folder_path(id, |id| doc.find_layer(id).map(|layer| (layer.name(), layer.parent()))),
        );
        let hint = if let Some(id) = before {
            let name = doc.find_layer(id).map_or("", |layer| layer.name());
            format!("{destination} · before {name}")
        } else {
            format!("{destination} · at end")
        };
        self.target = order
            .indices()
            .iter()
            .position(|&index| doc.layer(index).unwrap().id() == self.id)
            .unwrap_or(0);
        self.commands = commands;
        let plan = Arc::new(LayerDropPlan {
            parent: parent.map(str::to_owned),
            before: before.map(str::to_owned),
            order,
            hint,
        });
        self.drop_plan = Some(plan.clone());
        Ok(plan)
    }
}

pub(super) fn layer_gesture_interrupted(ctx: &egui::Context) -> bool {
    PeerBrush::pointer_contact_cancelled(ctx) || ctx.input(|input| {
        !input.focused || input.viewport().close_requested() || input.key_pressed(egui::Key::Escape)
            || input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
    })
}

impl PeerBrush {
    pub(super) fn set_solo(&mut self, active: bool) {
        self.isolate = active;
        self.solo_target = active.then(|| (self.selected.clone(), self.mask));
        self.last_preview = None;
    }
    pub(super) fn sync_solo<'a>(&mut self, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        if !self.isolate {
            self.solo_target = None;
            return;
        }
        let target = (self.selected.clone(), self.mask);
        let valid = doc
            .layers()
            .any(|l| l.id() == target.0 && (!target.1 || l.mask().is_some()));
        if !valid || self.solo_target.as_ref().is_some_and(|old| old != &target) {
            self.set_solo(false);
        } else {
            self.solo_target = Some(target);
        }
    }
    pub(super) fn select_channel(&mut self, mask: bool) {
        if self.mask != mask {
            self.duplicate_repeat = None;
            self.set_solo(false);
            self.mask_step = None;
            self.last_preview = None;
        }
        self.mask = mask;
    }
    pub(super) fn solo_caption<'a>(&self, doc: impl Into<DocumentView<'a>>) -> Option<String> {
        let doc = doc.into();
        self.isolate
            .then(|| doc.find_layer(&self.selected))
            .flatten()
            .map(|l| {
                format!(
                    "Solo {} · {}",
                    if self.mask { "Mask" } else { "Color" },
                    l.name()
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
    pub(super) fn begin_rename<'a>(&mut self, _doc: impl Into<DocumentView<'a>>, id: &str) {
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; wait for its result".into();
            return;
        }
        if self.rename_edit.is_some() {
            self.finish_rename();
            if self.rename_edit.is_some() || self.edit_worker.pending() {
                return;
            }
        }
        let (document, revision, name) = {
            let e = self.shared.lock().unwrap();
            let view = crate::engine::view::current(&e);
            let Some(layer) = view.find_layer(id) else {
                return;
            };
            (
                view.id().to_owned(),
                view.revision(),
                layer.name().to_owned(),
            )
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
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; rename draft preserved".into();
            return;
        }
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
        self.apply_action(
            vec![json!({"op":"layer.update","layer":edit.layer,"name":edit.text})],
            "Rename layer",
            Some((&edit.document, edit.revision)),
            edit_job::Origin::Action,
        );
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
            self.duplicate_repeat = None;
            self.set_solo(false);
        }
        self.liquify_effect = None;
        self.selected = id.into();
        self.selection_layers = [id.to_string()].into_iter().collect();
        self.selection_anchor = id.into();
        self.mask = mask;
        self.clear_duplicate_if_selection_changed();
        self.mask_step = None;
        self.last_preview = None;
    }
    fn select_row(
        &mut self,
        ui: &egui::Ui,
        doc: DocumentView<'_>,
        order: &crate::tree::TreeOrder,
        id: &str,
    ) {
        let previous = (self.selected.clone(), self.mask);
        self.layer_clipboard = true;
        let mods = click_modifiers(ui);
        let command = mods.command || mods.ctrl;
        if mods.shift {
            let visible = rows(doc, order, &self.collapsed);
            let a = visible
                .iter()
                .position(|&(i, _)| doc.layer(i).unwrap().id() == self.selection_anchor);
            let b = visible
                .iter()
                .position(|&(i, _)| doc.layer(i).unwrap().id() == id);
            if let (Some(a), Some(b)) = (a, b) {
                if !command {
                    self.selection_layers.clear();
                }
                for &(i, _) in &visible[a.min(b)..=a.max(b)] {
                    self.selection_layers
                        .insert(doc.layer(i).unwrap().id().to_owned());
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
                        .layers()
                        .find(|l| self.selection_layers.contains(l.id()))
                        .unwrap()
                        .id()
                        .to_owned();
                }
            } else {
                self.selection_layers.insert(id.into());
                self.selected = id.into();
            }
            self.selection_anchor = id.into();
        } else {
            self.select_content(id);
        }
        if previous.0 != self.selected || previous.1 {
            self.set_solo(false);
        }
        self.mask = false;
        self.clear_duplicate_if_selection_changed();
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
        l: LayerView<'_>,
        indent: f32,
        show_details: bool,
        thumb: Option<&TextureHandle>,
        mask_thumb: Option<&TextureHandle>,
        tree: &TreeInfo<'_>,
        reserved: bool,
    ) -> Rect {
        let doc = tree.doc;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let (grip, grip_response) = ui.allocate_exact_size(Vec2::new(6.0, 28.0), egui::Sense::hover());
            if show_details {
                let label = l.color_label();
                if let Some(color) = label_color(label) {
                    ui.painter().circle_filled(grip.center() - Vec2::new(0., 10.), 2.5, color);
                    grip_response.on_hover_text(format!("Layer color label · {label}"));
                }
                for x in 0..2 {
                    for y in 0..3 {
                        ui.painter().circle_filled(
                            grip.center() + Vec2::new((x as f32 - 0.5) * 3.0, (y as f32 - 1.0) * 4.0),
                            0.8,
                            MUTED.gamma_multiply(0.5),
                        );
                    }
                }
            }
            let visible = tree.visible(l);
            let eye = eye_button(ui, visible);
            if !show_details && eye.gained_focus() {
                eye.scroll_to_me(None);
            }
            self.eye_rects.insert(l.id().to_owned(), eye.rect);
            if eye.clicked() && !eye.clicked_by(egui::PointerButton::Primary)
                && self.eye_sweep.is_none() && self.layer_drag.is_none()
            {
                if !self.selection_layers.contains(l.id()) {
                    self.select_content(l.id());
                }
                self.apply_action(
                    vec![json!({"op":"layer.update","layer":l.id(),"visible":!visible})],
                    "Toggle layer visibility",
                    Some((doc.id(), doc.revision())),
                    edit_job::Origin::Action,
                );
            }
            if self.eye_sweep.is_none() && self.layer_drag.is_none() && eye.enabled()
                && ui.input(|i| i.pointer.primary_pressed()) && !layer_gesture_interrupted(ui.ctx())
            {
                let (press, _) = ui.input(|input| canvas_contact(input, false));
                if let Some((pos, _)) = press.filter(|&(pos, _)| canvas_press_hits(ui, &eye, pos)) {
                    if !self.selection_layers.contains(l.id()) {
                        self.select_content(l.id());
                    }
                    self.eye_sweep = Some(EyeSweep {
                        gesture: crate::engine::id(),
                        document: doc.id().to_owned(),
                        previous: pos,
                        native: doc.native().map(|doc| doc.key()),
                        revision: doc.revision(),
                        visible: !visible,
                        ids: [l.id().to_owned()].into_iter().collect(),
                    });
                }
            }
            ui.add_space(indent);
            let disclosure = if l.kind() == "group" {
                let response = icons::small_button(
                    ui,
                    if self.collapsed.contains(l.id()) {
                        Icon::Right
                    } else {
                        Icon::Down
                    },
                    if self.collapsed.contains(l.id()) {
                        "Expand folder"
                    } else {
                        "Collapse folder"
                    },
                );
                if !show_details && response.gained_focus() {
                    response.scroll_to_me(None);
                }
                if response.clicked() {
                    if !self.collapsed.remove(l.id()) {
                        self.collapsed.insert(l.id().to_owned());
                    }
                }
                response.rect
            } else {
                ui.allocate_exact_size(Vec2::new(20.0, 24.0), egui::Sense::hover())
                    .0
            };
            // Keep row navigation, folder guides and visibility geometry during scroll.
            // Names, thumbnails and mask controls are needed only in the viewport.
            if !show_details {
                return disclosure;
            }
            if l.kind() == "group" {
                let (r, response) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::click());
                icons::paint(ui.painter(), r, Icon::Folder, true);
                if response.clicked() {
                    self.select_row(ui, doc, tree.order, l.id());
                }
            } else if l.kind() == "adjustment" {
                let (r, response) = ui.allocate_exact_size(Vec2::splat(30.), egui::Sense::click());
                icons::paint(ui.painter(), r, Icon::Adjust, true);
                if response.clicked() {
                    self.select_row(ui, doc, &tree.order, l.id());
                }
            } else if self
                .thumb_button(
                    ui,
                    thumb,
                    self.selection_layers.contains(l.id()) && !self.mask,
                    "Edit layer content",
                )
                .clicked()
            {
                self.select_row(ui, doc, &tree.order, l.id());
            }
            if l.clip_to().is_some() {
                ui.label(RichText::new("↳").color(MUTED))
                    .on_hover_text("Clipped to the base below");
            }
            if reserved {
                ui.label(RichText::new("AI").size(10.).color(AI_BLUE))
                    .on_hover_text(
                        "AI has reserved this layer or a region; visibility stays available",
                    );
            }
            let mask_width = if l.mask().is_some() { 60.0 } else { 0.0 };
            let summary = if l.kind() == "group" {
                let empty = FolderSummary::default();
                let counts = tree.folders.get(l.id()).unwrap_or(&empty);
                Some(if self.collapsed.contains(l.id()) && counts.selected > 0 {
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
                - if l.locked() { 21.0 } else { 0.0 })
            .max(25.0);
            if self
                .rename_edit
                .as_ref()
                .is_some_and(|edit| edit.layer == l.id())
            {
                self.inline_rename(ui, name_width);
            } else {
                let name = filename::click_label(
                    ui,
                    l.name(),
                    name_width,
                    egui::FontId::new(
                        12.,
                        if l.kind() == "group" {
                            egui::FontFamily::Name("semibold".into())
                        } else {
                            egui::FontFamily::Proportional
                        },
                    ),
                    if reserved {
                        AI_BLUE
                    } else if tree.visible(l) {
                        Color32::WHITE
                    } else {
                        MUTED
                    },
                );
                let name = layer_hover(name, || tree.tooltip(l));
                if name.clicked() {
                    self.select_row(ui, doc, &tree.order, l.id());
                    let time = ui.input(|i| i.time);
                    let modifiers = click_modifiers(ui);
                    let pointer = name.interact_pointer_pos().unwrap_or(name.rect.center());
                    let options = ui.ctx().options(|o| o.input_options.clone());
                    // egui counts rapid clicks globally, including clicks on different rows.
                    // Rename requires two unmodified clicks on this same name at the same spot.
                    let same_name = self.name_click.as_ref().is_some_and(|(id, point, at)| {
                        id == l.id()
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
                        self.begin_rename(doc, l.id());
                    }
                    self.name_click = Some((l.id().to_owned(), pointer, time));
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
                    );
                let response = layer_hover(response, || {
                    let mut text = tree.tooltip(l);
                    if hidden_selection {
                        text.push_str("\nClick to reveal selected layers without changing selection");
                    }
                    text
                });
                if response.clicked() {
                    self.collapsed.remove(l.id());
                    for child in doc
                        .layers()
                        .filter(|child| self.selection_layers.contains(child.id()))
                    {
                        let mut chain = vec![];
                        let mut parent = tree.parent(child);
                        for _ in 0..16 {
                            let Some(ancestor) = parent.and_then(|id| doc.find_layer(id)) else {
                                break;
                            };
                            chain.push(ancestor.id().to_owned());
                            if ancestor.id() == l.id() {
                                for id in chain {
                                    self.collapsed.remove(&id);
                                }
                                break;
                            }
                            parent = tree.parent(ancestor);
                        }
                    }
                }
            }
            if l.mask().is_some() {
                self.mask_link_button(ui, doc, l, reserved);
                if self
                    .thumb_button(
                        ui,
                        mask_thumb,
                        self.selected == l.id() && self.mask,
                        "Paint mask · Alt isolate · Shift toggle",
                    )
                    .clicked()
                {
                    let was_solo_mask = self.isolate && self.selected == l.id() && self.mask;
                    self.layer_clipboard = false;
                    self.mask_step = None;
                    if ui.input(|i| i.modifiers.shift) {
                        self.select_content(l.id());
                        self.layer_cmd("mask.toggle", json!({}), "Toggle mask");
                    } else {
                        self.select_target(l.id(), true);
                        if ui.input(|i| i.modifiers.alt) {
                            self.set_solo(!was_solo_mask);
                        }
                        self.last_preview = None;
                    }
                }
            }
            if l.locked() {
                let (_, r) = ui.allocate_space(Vec2::splat(16.0));
                icons::paint(ui.painter(), r, Icon::Lock, true);
            }
            disclosure
        })
        .inner
    }
    pub(super) fn mask_link_button<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        layer: impl Into<LayerView<'a>>,
        reserved: bool,
    ) -> egui::Response {
        let doc = doc.into();
        let layer = layer.into();
        let mut linked = layer.mask().unwrap().linked();
        let tip = if linked {
            "Mask linked · transform Color and Mask together · Click to unlink"
        } else {
            "Mask unlinked · transform only the selected Color or Mask · Click to link"
        };
        let response = ui
            .add_enabled_ui(
                !doc.read_only() && !self.busy && !layer.locked() && !reserved,
                |ui| icons::small_button(ui, Icon::Connect, tip),
            )
            .inner;
        if response.clicked() {
            self.edit(vec![json!({"op":"mask.link","layer":layer.id(),"linked":!linked,"document_id":doc.id(),"source_revision":doc.revision()})], if linked { "Unlink mask" } else { "Link mask" });
            if let Ok(engine) = self.shared.try_lock() {
                let current = crate::engine::view::current(&engine);
                if current.id() == doc.id() {
                    if let Some(mask) = current.find_layer(layer.id()).and_then(|layer| layer.mask()) {
                        linked = mask.linked();
                    }
                }
            }
            ui.ctx().request_repaint();
        }
        if !linked {
            ui.painter().line_segment(
                [
                    response.rect.left_bottom() + Vec2::new(2., -4.),
                    response.rect.right_top() + Vec2::new(-2., 4.),
                ],
                Stroke::new(1.5, MUTED),
            );
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                response.rect,
                3,
                Stroke::new(1., ACCENT),
                egui::StrokeKind::Inside,
            );
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                response.enabled(),
                linked,
                "Link layer mask",
            )
        });
        response
    }
    pub(super) fn layer_list<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        reserved: &HashSet<&str>,
    ) {
        let doc = doc.into();
        let view = doc;
        let mut context_menu = self
            .layer_context
            .take()
            .filter(|(document, _)| document == &doc.id())
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
            .is_some_and(|d| d.document != doc.id() || d.revision != doc.revision() || d.native != doc.native().map(|doc| doc.key()))
            || self
                .eye_sweep
                .as_ref()
                .is_some_and(|s| s.document != doc.id() || s.revision != doc.revision() || s.native != doc.native().map(|doc| doc.key()))
        {
            self.layer_drag = None;
            self.eye_sweep = None;
            self.message = "Layer gesture cancelled: document changed".into();
        }
        let held = self.layer_drag.is_some() || self.eye_sweep.is_some();
        let (release_point, sweep_points) = ui.input(|input| {
            let (start, release) = canvas_contact_range(input, held);
            let release_point = release.and_then(|index| primary_button_sample(&input.events[index]))
                .map(|(_, point, _)| point);
            let end = release.map_or(input.events.len(), |index| index + 1);
            let points = if self.eye_sweep.is_some() || !held && input.pointer.primary_pressed() {
                input.events[start..end].iter().filter_map(|event| match event {
                    egui::Event::PointerMoved(point) => Some(*point),
                    _ => primary_button_sample(event).filter(|(pressed, _, _)| !pressed)
                        .map(|(_, point, _)| point),
                }).collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            (release_point, points)
        });
        self.layer_rects.clear();
        self.eye_rects.clear();
        let reserve = (ui.available_height() * 0.58).clamp(280.0, 480.0);
        let height = (ui.available_height() - reserve).max(46.0);
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false,true]).show(ui,|ui| {
            let base=ui.cursor().min;let width=ui.available_width();
            let pointer=release_point.or_else(|| ui.input(|i|i.pointer.interact_pos()));
            let mut preview=None;let mut open=std::borrow::Cow::Borrowed(&self.collapsed);let mut drop_folder=None;
            if let (Some(drag),Some(pointer))=(&mut self.layer_drag,pointer) {
                let original=rows(view,&crate::tree::TreeOrder::original(view),&self.collapsed);
                let slot=((pointer.y-base.y)/46.0).floor().max(0.0) as usize;
                let target=original.get(slot).map(|&(i,depth)|(view.layer(i).unwrap(),depth));
                let (parent,before)=if let Some((target,depth))=target {
                    let y=pointer.y-(base.y+slot as f32*46.0);
                    if target.kind()=="group" && (10.0..32.0).contains(&y) && !drag.ids.iter().any(|id|id==target.id()) {
                        drop_folder=Some(target.id().to_owned());open.to_mut().remove(target.id());
                        (Some(target.id()),view.layers().find(|l|l.parent()==Some(target.id())&&!drag.ids.iter().any(|id|id==l.id())).map(|l|l.id()))
                    } else {
                        let parent=if depth>0 && pointer.x<base.x+45.0 {None}else{target.parent()};
                        let before=if y<21.0 && !drag.ids.iter().any(|id|id==target.id()) {Some(target.id())}else{
                            original.iter().skip(slot+1).map(|&(i,_)|view.layer(i).unwrap()).find(|l|l.parent()==parent&&!drag.ids.iter().any(|id|id==l.id())).map(|l|l.id())
                        };
                        (parent,before)
                    }
                } else {(None,None)};
                match drag.plan_drop(view,parent,before) {
                    Ok(plan)=>preview=Some(plan),
                    Err(e)=>self.message=e.into(),
                }
            }
            let visibility=self.eye_sweep.as_ref().map(|sweep|(sweep.visible,sweep.ids.clone()));
            let original;
            let order=if let Some(plan)=&preview {&plan.order}else{
                original=crate::tree::TreeOrder::original(view);
                &original
            };
            let tree=TreeInfo::ordered(view,order,&self.selection_layers,visibility);
            let ordered=rows(view,tree.order,&open);
            let mut floating=None;
            let mut anchors=HashMap::new();
            let time=ui.input(|i| i.time);
            for (slot,(index,depth)) in ordered.iter().copied().enumerate() {
                let l=view.layer(index).unwrap();let target=slot as f32*46.0;
                let locked_by_ai=reserved.contains(l.id());
                let ai=locked_by_ai || self.animation.layer(l.id(),time);
                let duration=if ai {0.36}else{0.12};
                let offset=ui.ctx().animate_value_with_time(ui.id().with((&doc.id(),l.id(),"position")),target,duration);
                let indent=ui.ctx().animate_value_with_time(ui.id().with((&doc.id(),l.id(),"indent")),depth as f32*TREE_INDENT,duration);
                let arrival=self.animation.arrival(l.id(),time);
                let solo_opacity = self.solo_row_opacity(l.id());
                let row_opacity = arrival * solo_opacity;
                let rect=Rect::from_min_size(base+Vec2::new((1.0-arrival)*14.0,offset),Vec2::new(width,42.0));
                self.layer_rects.insert(l.id().to_owned(),rect);
                let held=self.layer_drag.as_ref().is_some_and(|d|d.ids.iter().any(|id|id==l.id()));
                let response=ui.interact(rect,ui.id().with((&doc.id(),l.id(),"row")),egui::Sense::click_and_drag());
                let visible = ui.is_rect_visible(rect);
                if !visible && response.gained_focus() {
                    response.scroll_to_me(None);
                }
                if response.drag_started() && self.eye_sweep.is_none()
                    && !layer_gesture_interrupted(ui.ctx())
                    && !self.rename_edit.as_ref().is_some_and(|edit| edit.layer == l.id())
                {
                    if !self.selection_layers.contains(l.id()) {self.select_content(l.id());}
                    let start=ui.input(|i|i.pointer.press_origin()).unwrap_or(rect.center());
                    self.layer_drag=Some(LayerDrag {gesture:crate::engine::id(),document:doc.id().to_owned(),id:l.id().to_owned(),revision:doc.revision(),native:doc.native().map(|doc|doc.key()),offset:start.y-rect.top(),target:index,ids:crate::tree::roots_view(view,&self.selection_layers),commands:vec![],drop_plan:None});
                }
                if response.clicked(){self.select_row(ui,view,&crate::tree::TreeOrder::original(view),l.id());}
                response.widget_info(|| egui::WidgetInfo::selected(
                    egui::WidgetType::Button, response.enabled(), self.selection_layers.contains(l.id()), l.name(),
                ));
                let floating_row = held && self.layer_drag.as_ref().is_some_and(|drag| drag.id == l.id());
                let thumb = if l.kind() != "group" && (visible || floating_row) {
                    self.thumbnail(ui, doc, l.id(), false)
                } else {
                    None
                };
                let mask_thumb = if visible && !held && l.mask().is_some() {
                    self.thumbnail(ui, doc, l.id(), true)
                } else {
                    None
                };
                if held {
                    let gap=Rect::from_min_size(base+Vec2::new(0.0,target),Vec2::new(width,42.0));
                    ui.painter().line_segment([gap.left_top(),gap.right_top()],Stroke::new(2.0_f32,ACCENT));
                    if self.layer_drag.as_ref().is_some_and(|d|d.id==l.id()){floating=Some((l,thumb,depth));}
                } else {
                    if visible {
                        let selected=self.selection_layers.contains(l.id());
                        let hover=ui.ctx().animate_bool_with_time(response.id.with("hover"),response.hovered(),0.08);
                        if selected {ui.painter().rect_filled(rect,5,Color32::from_rgb(74,48,71).gamma_multiply(solo_opacity));
                            ui.painter().rect_filled(Rect::from_min_size(rect.min+Vec2::new(0.0,5.0),Vec2::new(2.0,32.0)),1,ACCENT.gamma_multiply(solo_opacity));}
                        else if hover>0.0 {ui.painter().rect_filled(rect,5,Color32::WHITE.gamma_multiply(0.035*hover*solo_opacity));}
                        if ai {
                            let pulse=0.065+0.02*(time as f32*4.0).sin();
                            ui.painter().rect_filled(rect,5,AI_BLUE.gamma_multiply(pulse*solo_opacity));
                            ui.painter().rect_filled(Rect::from_min_size(rect.min+Vec2::new(0.0,4.0),Vec2::new(2.0,34.0)),1,AI_BLUE.gamma_multiply(solo_opacity));
                        }
                        if drop_folder.as_deref()==Some(l.id()){ui.painter().rect_stroke(rect,4,Stroke::new(1.0_f32,ACCENT),egui::StrokeKind::Inside);}
                        icons::paint_focus(ui, &response);
                    }
                    let editing = self.rename_edit.as_ref().is_some_and(|edit| edit.layer == l.id());
                    let disclosure=ui.scope_builder(egui::UiBuilder::new().id_salt(l.id()).max_rect(rect.shrink2(Vec2::new(4.0,4.0))),|ui| {
                        ui.set_opacity(row_opacity);
                        self.layer_contents(ui,l,indent,visible || editing,thumb.as_ref(),mask_thumb.as_ref(),&tree,locked_by_ai)
                    }).inner;
                    anchors.insert(l.id(),TreeAnchor {disclosure,ai,opacity:row_opacity});
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
                let response=if visible {
                    layer_hover(response,||format!("{}\nClick to select · Ctrl/Cmd or Shift for multiple · Drag into folders · Drag left to move out · Right-click anywhere for options",tree.tooltip(l)))
                } else { response };
                context_menu.show(&response,|ui|self.layer_menu(ui,doc,l,locked_by_ai));
            }
            paint_tree_guides(ui.painter(),&tree,&anchors);
            if let (Some((layer,thumb,depth)),Some(pointer),Some(drag))=(floating,pointer,&self.layer_drag) {
                let painter=ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip,ui.id().with("held layer")));
                let rect=Rect::from_min_size(egui::pos2(base.x,pointer.y-drag.offset),Vec2::new(width,42.0));
                painter.rect_filled(rect.translate(Vec2::new(0.0,3.0)),5,Color32::BLACK.gamma_multiply(0.35));
                painter.rect_filled(rect,5,Color32::from_rgb(74,48,71));
                painter.line_segment([rect.left_top(),rect.left_bottom()],Stroke::new(2.0_f32,ACCENT));
                let x=rect.left()+75.0+depth as f32*TREE_INDENT;
                let image_rect=Rect::from_min_size(egui::pos2(x,rect.top()+6.0),Vec2::splat(30.0));
                if layer.kind()=="group"{icons::paint(&painter,image_rect,Icon::Folder,true);}
                else if let Some(t)=thumb {painter.image(t.id(),image_rect,Rect::from_min_max(Pos2::ZERO,Pos2::new(1.0,1.0)),Color32::WHITE);}
                let label=if drag.ids.len()>1 {format!("{}  +{}",layer.name(),drag.ids.len()-1)}else{layer.name().to_owned()};
                painter.text(egui::pos2(x+38.0,rect.center().y),egui::Align2::LEFT_CENTER,label,egui::FontId::proportional(12.0),Color32::WHITE);
                if let Some(plan)=&preview {
                    let galley=painter.layout_no_wrap(plan.hint.clone(),egui::FontId::proportional(11.0),ACCENT);
                    let at=egui::pos2(rect.left()+8.,rect.bottom()+4.);
                    painter.with_clip_rect(Rect::from_min_size(at,Vec2::new(width-16.,20.))).galley(at,galley,ACCENT);
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            if let Some(sweep) = &mut self.eye_sweep {
                let clip = ui.clip_rect();
                let previous_count = sweep.ids.len();
                for point in sweep_points {
                    for (id, rect) in &self.eye_rects {
                        if !sweep.ids.contains(id)
                            && rect.intersect(clip).is_positive()
                            && sweep_hits_eye(sweep.previous, point, rect.expand(1.).intersect(clip))
                        {
                            sweep.ids.insert(id.clone());
                        }
                    }
                    sweep.previous = point;
                }
                if sweep.ids.len() != previous_count {
                    ui.ctx().request_repaint();
                }
            }
            if release_point.is_some() {
                if let Some(drag)=self.layer_drag.take() {
                    if pointer.is_some_and(|p|ui.clip_rect().contains(p)) && !drag.commands.is_empty() {
                        self.apply_action(drag.commands,"Move layers",Some((&drag.document,drag.revision)),edit_job::Origin::Action);
                        if let Some(id)=drop_folder {self.collapsed.remove(&id);}
                    }
                }
                if let Some(sweep)=self.eye_sweep.take() {
                    let commands=sweep.ids.iter().map(|id|json!({"op":"layer.update","layer":id,"visible":sweep.visible})).collect::<Vec<_>>();
                    self.apply_action(commands,"Layer visibility sweep",Some((&sweep.document,sweep.revision)),edit_job::Origin::Action);
                }
            }
            ui.advance_cursor_after_rect(Rect::from_min_size(base,Vec2::new(width,ordered.len() as f32*46.0)));
        });
        if context_visible && context_menu.is_some() {
            self.layer_context = Some((doc.id().to_owned(), context_menu));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(depth: u16) -> (PeerBrush, egui::Context, Vec<String>, Document) {
        let ctx = egui::Context::default();
        let mut engine = crate::engine::Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
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
        engine.resident_state_mut().expect("resident fixture").document.layers = vec![outer, inner, leaf, sibling, loose, unrelated, other];
        engine.resident_state_mut().expect("resident fixture").document.ensure_depth();
        for (index, color) in [
            (2, [12345, 23457, 34569, 65535]),
            (4, [52345, 11357, 4569, 65535]),
        ] {
            for y in 0..8 {
                for x in 0..8 {
                    engine.resident_state_mut().expect("resident fixture").document.layers[index].pixels.set16(x, y, color);
                }
            }
        }
        let ids: Vec<_> = engine.resident_state().expect("resident fixture").document.layers.iter().map(|l| l.id.clone()).collect();
        engine
            .edit(
                "human",
                &[json!({"op":"mask.add","layer":ids[2],"value":183})],
                None,
                None,
                "Native mask",
            )
            .unwrap();
        engine.resident_state_mut().expect("resident fixture").undo.clear();
        let before = engine.resident_state().expect("resident fixture").document.clone();
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
    fn layer_gestures_cancel_on_same_revision_document_replacement() {
        let (mut app, ctx, ids, before) = fixture(16);
        let pointer = egui::pos2(40., 40.);
        let _ = ctx.run(
            egui::RawInput {
                events: vec![button(pointer, true)],
                ..Default::default()
            },
            |_| {},
        );
        app.layer_drag = Some(LayerDrag {
            gesture: crate::engine::id(),
            document: before.id.clone(),
            native: None,
            id: ids[2].clone(),
            revision: before.revision,
            offset: 0.,
            target: 0,
            ids: vec![ids[2].clone()],
            commands: vec![json!({"op":"layer.update", "layer":ids[2], "visible":false})],
            drop_plan: None,
        });
        app.eye_sweep = Some(EyeSweep {
            gesture: crate::engine::id(),
            document: before.id.clone(),
            native: None,
            previous: pointer,
            revision: before.revision,
            visible: false,
            ids: [ids[2].clone()].into_iter().collect(),
        });
        let replacement = {
            let mut engine = app.shared.lock().unwrap();
            engine.resident_state_mut().expect("resident fixture").document.id = crate::engine::id();
            engine.resident_state().expect("resident fixture").document.clone()
        };
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500., 900.))),
                events: vec![button(pointer, false)],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.layer_list(ui, &replacement, &HashSet::new()));
            },
        );
        assert!(app.layer_drag.is_none());
        assert!(app.eye_sweep.is_none());
        assert!(app
            .message
            .contains("Layer gesture cancelled: document changed"));
        let engine = app.shared.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
            serde_json::to_value(&replacement).unwrap()
        );
        assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
    }

    #[test]
    fn shaped_layer_names_keep_inline_rename_selection_and_native_work() {
        for depth in [8, 16] {
            let (mut app, ctx, ids, _) = fixture(depth);
            let logical = "שלום مرحبا हिन्दी 漢字";
            let (before, undo_count) = {
                let mut engine = app.shared.lock().unwrap();
                engine.resident_state_mut().expect("resident fixture").document.layers[2].name = logical.into();
                engine.resident_state_mut().expect("resident fixture").document.selection = Some([1, 1, 6, 6]);
                (
                    serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    engine.resident_state().expect("resident fixture").undo.len(),
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
            assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), undo_count);
            assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
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
            assert!(e.resident_state().expect("resident fixture").undo.is_empty());
            assert_eq!(
                serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
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
                preview.resident_state_mut().expect("resident fixture").document = before.clone();
                preview
                    .edit("human", &commands, None, None, "Preview")
                    .unwrap();
                let expected = preview.resident_state().expect("resident fixture").document.export_png().unwrap();
                let rendered = preview.resident_state().expect("resident fixture").document.preview(None, 8192, None, false).unwrap().2;
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, before.revision);
                frame(&mut app, &ctx, vec![button(destination, false)]);
                let mut e = app.shared.lock().unwrap();
                if reserved {
                    assert!(e.resident_state().expect("resident fixture").undo.is_empty());
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers[4].parent, None);
                } else {
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), expected);
                    for original in &before.layers {
                        let current = e.resident_state().expect("resident fixture").document.layers.iter().find(|l| l.id == original.id).unwrap();
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
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                }
            }
        }
    }

    #[test]
    fn drop_plan_reuses_the_destination_and_clears_commands_after_rejection() {
        let (_, _, ids, doc) = fixture(16);
        let view = DocumentView::from(&doc);
        let mut drag = LayerDrag {
            gesture: crate::engine::id(),
            document: view.id().into(),
            id: ids[1].clone(),
            revision: view.revision(),
            native: None,
            offset: 0.,
            target: 1,
            ids: vec![ids[1].clone()],
            commands: Vec::new(),
            drop_plan: None,
        };
        let first = drag.plan_drop(view, None, Some(&ids[4])).unwrap();
        let commands = drag.commands.as_ptr();
        let same = drag.plan_drop(view, None, Some(&ids[4])).unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert_eq!(drag.commands.as_ptr(), commands);
        assert_eq!(first.hint, "Top level · before Loose paint");

        let changed = drag.plan_drop(view, Some(&ids[5]), None).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(drag.commands[0]["parent"], ids[5]);
        assert!(drag.plan_drop(view, Some(&ids[1]), None).is_err());
        assert!(drag.drop_plan.is_none() && drag.commands.is_empty());
        let retried = drag.plan_drop(view, Some(&ids[5]), None).unwrap();
        assert!(!Arc::ptr_eq(&changed, &retried));
        assert_eq!(drag.commands[0]["parent"], ids[5]);
    }

    #[test]
    fn folder_guides_follow_preview_parents_and_omit_hidden_rows() {
        let (_, ctx, ids, doc) = fixture(16);
        let view = DocumentView::from(&doc);
        let moved = [ids[4].clone()];
        let (order, _) = crate::tree::reparent_view(view, &moved, Some(&ids[1]), None).unwrap();
        let tree = TreeInfo::ordered(view, &order, &moved.into_iter().collect(), None);
        assert_eq!(
            rows(view, tree.order, &HashSet::new()),
            [(0, 0), (1, 1), (2, 2), (4, 2), (3, 1), (5, 0), (6, 1)],
        );
        assert_eq!(
            rows(view, tree.order, &HashSet::from([ids[1].clone()])),
            [(0, 0), (1, 1), (3, 1), (5, 0), (6, 1)],
        );
        assert_eq!(tree.path(&ids[4]), "Outer / Inner / Loose paint");
        assert_eq!(
            tree.tooltip(view.layer(1).unwrap()),
            "Outer / Inner\n2 direct children · 2 total descendants\n1 selected inside"
        );

        let anchor = |x, y| TreeAnchor {
            disclosure: Rect::from_min_size(Pos2::new(x, y), Vec2::splat(10.)),
            ai: false,
            opacity: 1.,
        };
        let anchors = HashMap::from([
            (ids[1].as_str(), anchor(10., 10.)),
            (ids[4].as_str(), anchor(30., 40.)),
        ]);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            paint_tree_guides(&ctx.layer_painter(egui::LayerId::background()), &tree, &anchors);
        });
        let guides = output.shapes.into_iter().filter_map(|shape| match shape.shape {
            egui::Shape::LineSegment { points, .. } => Some(points),
            _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(guides, [
            [Pos2::new(15., 20.), Pos2::new(15., 45.)],
            [Pos2::new(15., 45.), Pos2::new(36., 45.)],
        ]);
    }
}

#[cfg(test)]
mod native_metadata_tests {
    use super::*;
    #[test]
    fn native_owned_tree_rows_and_metadata_controls_use_the_same_borrowed_snapshot() {
        for depth in [8, 16] {
            let ctx = egui::Context::default();
            let mut doc = Document::new_depth(9, 7, depth).unwrap();
            let folder = Layer::new("Native folder", "group", 9, 7);
            doc.layers[0].name = "Native child".into();
            doc.layers[0].parent = Some(folder.id.clone());
            let child = doc.layers[0].id.clone();
            doc.layers.insert(0, folder);
            doc.ensure_depth();
            let shared = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
            shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
            let mut app = PeerBrush::init(
                &ctx,
                shared,
                crate::server::Connection {
                    port: 0,
                    token: String::new(),
                    state_dir: Default::default(),
                    instance_lock: None,
                    availability: Default::default(),
                },
            );
            app.selected = child.clone();
            app.selection_layers = [child.clone()].into_iter().collect();
            let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let snapshot = crate::engine::view::DocumentSnapshot::from(
                crate::tree::presentation_tests::native(&doc, &pool),
            );
            let owner = snapshot.clone();
            let view = owner.view();
            let admitted = pool.used();
            let order = crate::tree::TreeOrder::original(view);
            let tree = TreeInfo::ordered(view, &order, &app.selection_layers, None);
            let visible = rows(view, tree.order, &HashSet::new());
            assert_eq!(visible, vec![(0, 0), (1, 1)]);
            assert_eq!(
                tree.tooltip(view.layer(0).unwrap()),
                "Native folder\n1 direct children · 1 total descendants\n1 selected inside"
            );
            assert_eq!(tree.path(&child), "Native folder / Native child");
            app.set_solo(true);
            app.sync_solo(view);
            assert_eq!(
                app.solo_caption(view).as_deref(),
                Some("Solo Color · Native child")
            );
            app.begin_rename(view, &child);
            assert_eq!(app.rename_edit.as_ref().unwrap().original, "Native child");
            app.rename_edit = None;
            app.selection_gesture_source = Some(app.canvas_gesture_source(view).unwrap());
            assert!(!app.selection_gesture_stale(view));
            let foreign = Document::new_depth(9, 7, depth).unwrap();
            assert!(app.selection_gesture_stale(&foreign));
            app.selection_gesture_source = None;
            app.tool = Tool::Clone;
            app.retouch_source = Some(super::super::retouch::Anchor {
                project: view.id().to_owned(),
                layer: child.clone(),
                mask: false,
                point: [2., 2.],
                destination: None,
            });
            assert!(app.begin_retouch(view, [4., 3.]));
            assert_eq!(app.retouch_revision, Some(view.revision()));
            app.retouch_source.as_mut().unwrap().layer = "foreign layer".into();
            assert!(!app.begin_retouch(view, [4., 3.]));
            assert!(app.retouch_source.is_none());
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    for &(index, depth) in &visible {
                        app.layer_contents(
                            ui,
                            view.layer(index).unwrap(),
                            depth as f32 * TREE_INDENT,
                            true,
                            None,
                            None,
                            &tree,
                            false,
                        );
                    }
                    app.layer_footer(ui, view, view.layer(1).unwrap());
                });
            });
            assert_eq!(pool.used(), admitted);
            assert_eq!(view.depth(), depth);
            assert_eq!(view.revision(), 0);
            let before = serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
            let output = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(420., 1000.))),
                ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.layers(ui, view, &HashSet::new()));
            });
            assert!(app.layer_rects.contains_key(&child));
            assert!(app.opacity_rect.is_some());
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "Native child")));
            assert_eq!(serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(), before);
            app.layer_drag = Some(LayerDrag {
                gesture: crate::engine::id(), document: view.id().into(), id: child.clone(),
                revision: view.revision(), native: view.native().map(|doc| doc.key()),
                offset: 0., target: 1, ids: vec![child.clone()],
                commands: vec![json!({"op":"layer.update","layer":child,"visible":false})],
                drop_plan: None,
            });
            app.eye_sweep = Some(EyeSweep {
                gesture: crate::engine::id(), document: view.id().into(), previous: Pos2::ZERO,
                revision: view.revision(), native: view.native().map(|doc| doc.key()),
                visible: false, ids: [child.clone()].into_iter().collect(),
            });
            let replacement = crate::tree::presentation_tests::native(&doc, &pool);
            assert_eq!(replacement.metadata().revision(), view.revision());
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.layer_list(ui, &replacement, &HashSet::new()));
            });
            assert!(app.layer_drag.is_none() && app.eye_sweep.is_none());
            assert_eq!(serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(), before);
            app.layer_thumbnails.stop();
        }
    }
}
