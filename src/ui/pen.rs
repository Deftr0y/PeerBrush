use super::*;
use crate::{
    engine::Layer,
    source::{Content, Source},
};
#[derive(Default)]
pub(super) struct State {
    active: Option<(String, String)>,
    fresh: bool,
    drawing: bool,
    selected: Option<usize>,
    gesture: Option<Gesture>,
}
struct Gesture {
    document: String,
    revision: u64,
    command: Value,
    kind: Kind,
    index: usize,
    start: [f32; 2],
    anchor: [f32; 2],
}
#[derive(Clone, Copy)]
enum Kind {
    Add,
    Move,
    Handle(&'static str),
}
fn nodes(l: &Layer) -> Option<(&[crate::path::Anchor], bool)> {
    match &l.source {
        Some(Source {
            content: Content::Bezier { nodes, closed, .. },
            ..
        }) => Some((nodes, *closed)),
        _ => None,
    }
}
impl PeerBrush {
    pub(super) fn capture_pen(&mut self, doc: &Document) {
        self.select_tool(Tool::Pen);
        if let Some(l) = doc.layers.iter().find(|l| nodes(l).is_some()) {
            self.select_content(&l.id);
            self.pen.active = Some((doc.id.clone(), l.id.clone()));
            self.pen.fresh = false;
            self.pen.selected = (!nodes(l).unwrap().0.is_empty()).then_some(0);
        }
    }
    pub(super) fn cancel_pen(&mut self) {
        self.pen.gesture = None;
        self.transient.clear();
        self.live_gesture = None;
    }
    fn pen_target(&mut self, doc: &Document) -> Option<Layer> {
        if !self.pen.fresh {
            if let Some(l) = doc
                .layers
                .iter()
                .find(|l| l.id == self.selected && nodes(l).is_some())
            {
                if self.pen.active.as_ref() != Some(&(doc.id.clone(), l.id.clone())) {
                    self.pen.active = Some((doc.id.clone(), l.id.clone()));
                    self.pen.selected = None;
                    self.pen.drawing = false;
                }
            }
        }
        self.pen
            .active
            .as_ref()
            .filter(|(d, _)| *d == doc.id)
            .and_then(|(_, id)| {
                doc.layers
                    .iter()
                    .find(|l| l.id == *id && nodes(l).is_some())
            })
            .cloned()
    }
    pub(super) fn pen_toolbar(&mut self, ui: &mut egui::Ui, doc: &Document) {
        if self.tool != Tool::Pen {
            return;
        }
        let target = self.pen_target(doc);
        if ui.button("New path").clicked() {
            self.cancel_pen();
            self.pen.active = None;
            self.pen.fresh = true;
            self.pen.drawing = true;
            self.pen.selected = None;
        }
        if let Some(l) = target {
            let (nodes, closed) = nodes(&l).unwrap();
            if ui
                .add_enabled(
                    nodes.len() >= 2,
                    egui::Button::new(if closed { "Reopen" } else { "Close path" }),
                )
                .clicked()
            {
                self.cancel_pen();
                self.edit(vec![json!({"op":"path.update","layer":l.id,"closed":!closed,"source_revision":doc.revision})],"Change path closure");
                self.pen.drawing = closed;
            }
            if !closed
                && ui
                    .button(if self.pen.drawing {
                        "Finish"
                    } else {
                        "Continue"
                    })
                    .clicked()
            {
                self.cancel_pen();
                self.pen.drawing = !self.pen.drawing;
            }
            if ui
                .add_enabled(closed, egui::Button::new("Selection"))
                .clicked()
            {
                self.cancel_pen();
                self.edit(vec![json!({"op":"path.to_selection","path":l.id,"mode":"replace","feather":self.selection_feather,"source_revision":doc.revision})],"Selection from path");
            }
            if ui
                .add_enabled(closed, egui::Button::new("Mask"))
                .on_hover_text(
                    "Create a mask on the selected layer from this path; Color stays unchanged",
                )
                .clicked()
            {
                self.cancel_pen();
                self.edit(vec![json!({"op":"path.to_mask","path":l.id,"layer":self.selected,"source_revision":doc.revision})],"Mask from path");
            }
            if ui.button("Properties").clicked() {
                self.cancel_pen();
                let selected = self.selected.clone();
                self.selected = l.id.clone();
                self.open_source(doc, "edit");
                self.selected = selected;
            }
            if let Some(index) = self.pen.selected.filter(|i| *i < nodes.len()) {
                for (label, smooth) in [("Corner", false), ("Smooth", true)] {
                    if ui.button(label).clicked() {
                        self.cancel_pen();
                        self.edit(vec![json!({"op":"path.node.convert","layer":l.id,"index":index,"smooth":smooth,"reset_handles":!smooth,"source_revision":doc.revision})],"Convert path point");
                    }
                }
                if ui.button("− Point").clicked() {
                    self.cancel_pen();
                    self.edit(vec![json!({"op":"path.node.delete","layer":l.id,"index":index,"source_revision":doc.revision})],"Delete path point");
                    self.pen.selected = None;
                }
            }
        }
    }
    pub(super) fn pen_canvas(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Document,
        response: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
        scale: f32,
    ) {
        let target = self.pen_target(doc);
        if self
            .pen
            .gesture
            .as_ref()
            .is_some_and(|g| g.document != doc.id || g.revision != doc.revision)
        {
            self.cancel_pen();
            self.message = "Image changed during path edit · preview cancelled".into();
        }
        let text = ui.ctx().wants_keyboard_input();
        if !text
            && ui.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter))
        {
            self.cancel_pen();
            self.pen.drawing = false;
        }
        if !text
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            if let (Some(l), Some(index)) = (&target, self.pen.selected) {
                self.cancel_pen();
                self.edit(vec![json!({"op":"path.node.delete","layer":l.id,"index":index,"source_revision":doc.revision})],"Delete path point");
                self.pen.selected = None;
                return;
            }
        }
        let screen = |p: [f32; 2]| rect.min + Vec2::new(p[0], p[1]) * scale;
        let to_doc = |p: Pos2| [(p.x - rect.min.x) / scale, (p.y - rect.min.y) / scale];
        let cursor = ui.input(|i| i.pointer.interact_pos());
        let mut hit = None;
        if let (Some(l), Some(p)) = (&target, cursor) {
            let (nodes, _) = nodes(l).unwrap();
            for (i, n) in nodes.iter().enumerate() {
                if screen(crate::path::document_point(l, n.point)).distance(p) <= 7. {
                    hit = Some((i, None));
                }
                if self.pen.selected == Some(i) {
                    for (name, h) in [("in", n.in_handle), ("out", n.out_handle)] {
                        if h.is_some_and(|h| {
                            screen(crate::path::document_point(l, h)).distance(p) <= 7.
                        }) {
                            hit = Some((i, Some(name)));
                        }
                    }
                }
            }
        }
        if response.hovered() && ui.input(|i| i.pointer.primary_pressed()) {
            if let Some(p) = cursor.filter(|p| rect.contains(*p)) {
                self.cancel_source();
                let point = to_doc(p);
                if hit.is_none() {
                    if let Some(l) = target.as_ref().filter(|l| {
                        nodes(l).unwrap().1 || ui.input(|i| i.modifiers.ctrl || i.modifiers.command)
                    }) {
                        if let Some((index, t)) = crate::path::nearest_segment(l, point, 7. / scale)
                        {
                            self.cancel_pen();
                            self.edit(vec![json!({"op":"path.node.split","layer":l.id,"index":index,"t":t,"source_revision":doc.revision})],"Insert path point");
                            self.pen.selected = Some(index + 1);
                            return;
                        }
                    }
                }
                let (command, kind, index, anchor) = if let (Some(l), Some((index, handle))) =
                    (&target, hit)
                {
                    let (nodes, closed) = nodes(l).unwrap();
                    let n = &nodes[index];
                    self.pen.selected = Some(index);
                    if ui.input(|i| i.modifiers.alt) && handle.is_none() {
                        self.edit(vec![json!({"op":"path.node.convert","layer":l.id,"index":index,"smooth":!n.smooth,"reset_handles":n.smooth,"source_revision":doc.revision})],"Convert path point");
                        return;
                    }
                    if self.pen.drawing
                        && !closed
                        && index == 0
                        && nodes.len() >= 2
                        && handle.is_none()
                    {
                        self.edit(vec![json!({"op":"path.update","layer":l.id,"closed":true,"source_revision":doc.revision})],"Close path");
                        self.pen.drawing = false;
                        return;
                    }
                    if let Some(handle) = handle {
                        let h = if handle == "in" {
                            n.in_handle
                        } else {
                            n.out_handle
                        }
                        .unwrap();
                        (
                            json!({"op":"path.handle.move","layer":l.id,"index":index,"handle":handle,"point":crate::path::document_point(l,h)}),
                            Kind::Handle(handle),
                            index,
                            crate::path::document_point(l, h),
                        )
                    } else {
                        (
                            json!({"op":"path.node.move","layer":l.id,"index":index,"point":crate::path::document_point(l,n.point)}),
                            Kind::Move,
                            index,
                            crate::path::document_point(l, n.point),
                        )
                    }
                } else if let Some(l) = &target {
                    let (nodes, closed) = nodes(l).unwrap();
                    if closed {
                        self.pen.selected = None;
                        return;
                    }
                    self.pen.drawing = true;
                    (
                        json!({"op":"path.node.add","layer":l.id,"point":point}),
                        Kind::Add,
                        nodes.len(),
                        point,
                    )
                } else {
                    let parent = doc
                        .layers
                        .iter()
                        .find(|l| l.id == self.selected)
                        .and_then(|l| {
                            if l.kind == "group" {
                                Some(l.id.clone())
                            } else {
                                l.parent.clone()
                            }
                        });
                    self.pen.drawing = true;
                    (
                        json!({"op":"path.create","nodes":[{"point":point}],"stroke":self.color.map(|v|u16::from(v)*257),"stroke_width":2.,"parent":parent}),
                        Kind::Add,
                        0,
                        point,
                    )
                };
                self.pen.gesture = Some(Gesture {
                    document: doc.id.clone(),
                    revision: doc.revision,
                    command,
                    kind,
                    index,
                    start: point,
                    anchor,
                });
                self.live_gesture = Some(crate::engine::id());
            }
        }
        if let Some(g) = &mut self.pen.gesture {
            if let Some(p) = cursor {
                let p = to_doc(p);
                let point = [
                    g.anchor[0] + p[0] - g.start[0],
                    g.anchor[1] + p[1] - g.start[1],
                ];
                match g.kind {
                    Kind::Add => {
                        let mut n = json!({"point":g.anchor});
                        if screen(g.anchor).distance(screen(point)) > 3. {
                            n["in_handle"] =
                                json!([2. * g.anchor[0] - point[0], 2. * g.anchor[1] - point[1]]);
                            n["out_handle"] = json!(point);
                            n["smooth"] = json!(true);
                        }
                        if g.command["op"] == "path.create" {
                            g.command["nodes"] = json!([n]);
                        } else {
                            for key in ["point", "in_handle", "out_handle", "smooth"] {
                                if let Some(v) = n.get(key) {
                                    g.command[key] = v.clone();
                                } else {
                                    g.command.as_object_mut().unwrap().remove(key);
                                }
                            }
                        }
                    }
                    Kind::Move => g.command["point"] = json!(point),
                    Kind::Handle(handle) => {
                        g.command["handle"] = json!(handle);
                        g.command["point"] = json!(point);
                    }
                }
            }
            g.command["source_revision"] = json!(g.revision);
            g.command["document_id"] = json!(g.document);
            self.transient.push(g.command.clone());
            if ui.input(|i| i.pointer.primary_released()) {
                let g = self.pen.gesture.take().unwrap();
                self.pen.selected = Some(g.index);
                if !matches!(g.kind, Kind::Add)
                    && g.command["point"].as_array().is_some_and(|p| {
                        p.iter().zip(g.anchor).all(|(p, a)| {
                            (p.as_f64().unwrap_or(f64::INFINITY) - f64::from(a)).abs() < 0.0001
                        })
                    })
                {
                    self.transient.clear();
                    self.live_gesture = None;
                    self.last_preview = None;
                    return;
                }
                self.edit(vec![g.command], "Edit Pen path");
                self.transient.clear();
                self.live_gesture = None;
                if let Some(l) = self
                    .shared
                    .lock()
                    .unwrap()
                    .doc
                    .layers
                    .iter()
                    .find(|l| l.id == self.selected && nodes(l).is_some())
                {
                    self.pen.active = Some((doc.id.clone(), l.id.clone()));
                    self.pen.fresh = false;
                }
                return;
            }
        } else if self.pen.drawing && response.hovered() {
            if let (Some(l), Some(p)) = (&target, cursor.filter(|p| rect.contains(*p))) {
                if !nodes(l).unwrap().1 && hit.is_none() {
                    self.transient.push(json!({"op":"path.node.add","layer":l.id,"point":to_doc(p),"source_revision":doc.revision}));
                }
            }
        }
        let shown = if !self.transient.is_empty() {
            crate::engine::Engine::preview_edits(doc.clone(), &self.transient).ok()
        } else {
            None
        };
        let shown = shown.as_ref().unwrap_or(doc);
        let l = target
            .as_ref()
            .and_then(|l| shown.layers.iter().find(|s| s.id == l.id))
            .or_else(|| {
                shown
                    .layers
                    .iter()
                    .find(|l| nodes(l).is_some() && !doc.layers.iter().any(|old| old.id == l.id))
            });
        if let Some(l) = l {
            let (nodes, _) = nodes(l).unwrap();
            for (i, n) in nodes.iter().enumerate() {
                let at = screen(crate::path::document_point(l, n.point));
                let color = if self.animation.active(ui.input(|i| i.time)) {
                    AI_BLUE
                } else {
                    ACCENT
                };
                painter.rect_filled(
                    Rect::from_center_size(
                        at,
                        Vec2::splat(if self.pen.selected == Some(i) { 7. } else { 5. }),
                    ),
                    0.,
                    color,
                );
                if self.pen.selected == Some(i)
                    || self.pen.gesture.as_ref().is_some_and(|g| g.index == i)
                {
                    for h in [n.in_handle, n.out_handle].into_iter().flatten() {
                        let end = screen(crate::path::document_point(l, h));
                        painter.line_segment([at, end], Stroke::new(1., color));
                        painter.circle_stroke(end, 3., Stroke::new(1., color));
                    }
                }
            }
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(if hit.is_some() {
                egui::CursorIcon::Grab
            } else {
                egui::CursorIcon::Crosshair
            });
        }
    }
}
