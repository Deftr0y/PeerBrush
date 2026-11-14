use super::*;
use crate::{engine::view::LayerView, source::Content};
#[derive(Default)]
pub(super) struct State {
    active: Option<(String, String)>,
    fresh: bool,
    drawing: bool,
    selected: Option<usize>,
    gesture: Option<Gesture>,
}
struct Gesture {
    source: selection::GestureSource,
    command: Value,
    kind: Kind,
    index: usize,
    start: [f32; 2],
    anchor: [f32; 2],
}
impl State {
    pub(super) fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Add,
    Move,
    Handle(&'static str),
}
fn nodes<'a>(layer: &LayerView<'a>) -> Option<(&'a [crate::path::Anchor], bool)> {
    layer.source()?.path_nodes()
}
impl PeerBrush {
    pub(super) fn pen_preview_key<'a>(
        &'a self,
        doc: DocumentView<'_>,
        command: &'a Value,
    ) -> Option<impl serde::Serialize + 'a> {
        if self.tool != Tool::Pen
            || command["document_id"].as_str() != Some(doc.id())
            || command["source_revision"].as_u64() != Some(doc.revision())
        {
            return None;
        }
        let op = command["op"].as_str()?;
        let layer = command["layer"].as_str();
        if let Some(gesture) = &self.pen.gesture {
            let id = self.live_gesture.as_deref()?;
            if gesture.source.observes(self, doc) && &gesture.command == command {
                return Some((Some(id), op, layer, self.selected.as_str()));
            }
        } else if self.pen.drawing
            && op == "path.node.add"
            && self.pen.active.as_ref().is_some_and(|(document, target)| {
                document == doc.id() && Some(target.as_str()) == layer
            })
        {
            return Some((None, op, layer, self.selected.as_str()));
        }
        None
    }

    pub(super) fn capture_pen<'a>(&mut self, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        self.select_tool(Tool::Pen);
        if let Some(layer) = doc.layers().find(|layer| nodes(layer).is_some()) {
            self.select_content(layer.id());
            self.pen.active = Some((doc.id().into(), layer.id().into()));
            self.pen.fresh = false;
            self.pen.selected = (!nodes(&layer).unwrap().0.is_empty()).then_some(0);
        }
    }
    pub(super) fn cancel_pen(&mut self) {
        self.pen.gesture = None;
        self.transient.clear();
        self.live_gesture = None;
    }
    pub(super) fn pen_source_stale(&self, doc: DocumentView<'_>) -> bool {
        self.pen.gesture.as_ref().is_some_and(|gesture| !gesture.source.observes(self, doc))
    }
    fn pen_target<'a>(&mut self, doc: DocumentView<'a>) -> Option<LayerView<'a>> {
        if !self.pen.fresh {
            if let Some(layer) = doc
                .layers()
                .find(|layer| layer.id() == self.selected && nodes(layer).is_some())
            {
                if self
                    .pen
                    .active
                    .as_ref()
                    .is_none_or(|(document, id)| document != doc.id() || id != layer.id())
                {
                    self.pen.active = Some((doc.id().into(), layer.id().into()));
                    self.pen.selected = None;
                    self.pen.drawing = false;
                }
            }
        }
        self.pen
            .active
            .as_ref()
            .filter(|(document, _)| document == doc.id())
            .and_then(|(_, id)| {
                doc.layers()
                    .find(|layer| layer.id() == id && nodes(layer).is_some())
            })
    }
    pub(super) fn pen_edit_finished(&mut self, index: usize) {
        let engine = self.shared.lock().unwrap();
        let doc = crate::engine::view::current(&engine);
        if let Some(layer) = doc
            .layers()
            .find(|layer| layer.id() == self.selected && nodes(layer).is_some())
        {
            self.pen.active = Some((doc.id().into(), layer.id().into()));
            self.pen.fresh = false;
            self.pen.selected = (index < nodes(&layer).unwrap().0.len()).then_some(index);
        }
    }
    pub(super) fn pen_toolbar<'a>(&mut self, ui: &mut egui::Ui, doc: impl Into<DocumentView<'a>>) {
        let doc = doc.into();
        if self.edit_worker.pending() {
            return;
        }
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
            let mut next_closed = closed;
            if super::source::path_closure(ui, &mut next_closed, nodes.len() >= 2) {
                self.cancel_pen();
                self.edit(vec![json!({"op":"path.update","layer":l.id(),"closed":next_closed,"document_id":doc.id(),"source_revision":doc.revision()})],"Change path closure");
                self.pen.drawing = !next_closed;
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
                self.edit(vec![json!({"op":"path.to_selection","path":l.id(),"mode":"replace","feather":self.selection_feather,"document_id":doc.id(),"source_revision":doc.revision()})],"Selection from path");
            }
            if ui
                .add_enabled(closed, egui::Button::new("Mask"))
                .on_hover_text(
                    "Create a mask on the selected layer from this path; Color stays unchanged",
                )
                .clicked()
            {
                self.cancel_pen();
                self.edit(vec![json!({"op":"path.to_mask","path":l.id(),"layer":self.selected,"document_id":doc.id(),"source_revision":doc.revision()})],"Mask from path");
            }
            if ui.button("Properties").clicked() {
                self.cancel_pen();
                let selected = self.selected.clone();
                self.selected = l.id().to_owned();
                self.open_source(doc, "edit");
                self.selected = selected;
            }
            if let Some(index) = self.pen.selected.filter(|i| *i < nodes.len()) {
                for (icon, smooth, help) in [
                    (Icon::CornerAnchor, false, "Corner · Reset the anchor's handles"),
                    (Icon::SmoothAnchor, true, "Smooth · Align handles using the neighboring points"),
                ] {
                    if icons::selectable(ui, icon, nodes[index].smooth == smooth, help).clicked() {
                        self.cancel_pen();
                        self.edit(vec![json!({"op":"path.node.convert","layer":l.id(),"index":index,"smooth":smooth,"reset_handles":!smooth,"document_id":doc.id(),"source_revision":doc.revision()})],"Convert path point");
                    }
                }
                if ui.button("− Point").clicked() {
                    self.cancel_pen();
                    self.edit(vec![json!({"op":"path.node.delete","layer":l.id(),"index":index,"document_id":doc.id(),"source_revision":doc.revision()})],"Delete path point");
                    self.pen.selected = None;
                }
            }
        }
    }
    pub(super) fn pen_canvas<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        response: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
        scale: f32,
    ) {
        let doc = doc.into();
        if self.edit_worker.pending() {
            return;
        }
        if Self::pointer_contact_cancelled(ui.ctx())
            || ui.input(|input| {
                !input.focused
                    || input.key_down(egui::Key::Space)
                    || input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
            })
        {
            self.cancel_pen();
            return;
        }
        let target = self.pen_target(doc);
        if self.pen_source_stale(doc) {
            self.cancel_pen();
            self.message = "Image changed during path edit · preview cancelled".into();
            return;
        }
        let text = ui.ctx().wants_keyboard_input();
        if !text
            && ui.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter))
        {
            self.cancel_pen();
            self.pen.drawing = false;
            return;
        }
        if !text
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            if let (Some(l), Some(index)) = (&target, self.pen.selected) {
                self.cancel_pen();
                self.edit(vec![json!({"op":"path.node.delete","layer":l.id(),"index":index,"document_id":doc.id(),"source_revision":doc.revision()})],"Delete path point");
                self.pen.selected = None;
                return;
            }
        }
        let view = self.view;
        let screen = |p: [f32; 2]| view.point(p, rect, scale);
        let to_doc = |p: Pos2| view.document(p, rect, scale);
        let cursor = ui.input(|i| i.pointer.interact_pos());
        let (press, release) = ui.input(|input| canvas_button_samples(input, self.pen.in_gesture()));
        let press = press.filter(|&pos| canvas_press_hits(ui, response, pos));
        let mut hit = None;
        if let (Some(l), Some(p)) = (&target, press.or(cursor)) {
            let (nodes, _) = nodes(l).unwrap();
            for (i, n) in nodes.iter().enumerate() {
                if screen(crate::path::document_point_view(*l, n.point)).distance(p) <= 7. {
                    hit = Some((i, None));
                }
                if self.pen.selected == Some(i) {
                    for (name, h) in [("in", n.in_handle), ("out", n.out_handle)] {
                        if h.is_some_and(|h| {
                            screen(crate::path::document_point_view(*l, h)).distance(p) <= 7.
                        }) {
                            hit = Some((i, Some(name)));
                        }
                    }
                }
            }
        }
        if let Some(p) = press {
            self.cancel_source();
            let point = to_doc(p);
            let modifiers = ui.input(|input| {
                primary_button_samples(input).find(|(pressed, _, _)| *pressed)
                    .map_or(input.modifiers, |(_, _, modifiers)| modifiers)
            });
            if hit.is_none() {
                if let Some(l) = target.as_ref().filter(|l| {
                    nodes(l).unwrap().1 || modifiers.ctrl || modifiers.command
                }) {
                    if let Some((index, t)) =
                        crate::path::nearest_segment_view(*l, point, 7. / scale)
                    {
                        self.cancel_pen();
                        self.edit(vec![json!({"op":"path.node.split","layer":l.id(),"index":index,"t":t,"document_id":doc.id(),"source_revision":doc.revision()})],"Insert path point");
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
                if modifiers.alt && handle.is_none() {
                    self.edit(vec![json!({"op":"path.node.convert","layer":l.id(),"index":index,"smooth":!n.smooth,"reset_handles":n.smooth,"document_id":doc.id(),"source_revision":doc.revision()})],"Convert path point");
                    return;
                }
                if self.pen.drawing
                    && !closed
                    && index == 0
                    && nodes.len() >= 2
                    && handle.is_none()
                {
                    self.edit(vec![json!({"op":"path.update","layer":l.id(),"closed":true,"document_id":doc.id(),"source_revision":doc.revision()})],"Close path");
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
                        json!({"op":"path.handle.move","layer":l.id(),"index":index,"handle":handle,"point":crate::path::document_point_view(*l,h)}),
                        Kind::Handle(handle),
                        index,
                        crate::path::document_point_view(*l, h),
                    )
                } else {
                    (
                        json!({"op":"path.node.move","layer":l.id(),"index":index,"point":crate::path::document_point_view(*l,n.point)}),
                        Kind::Move,
                        index,
                        crate::path::document_point_view(*l, n.point),
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
                    json!({"op":"path.node.add","layer":l.id(),"point":point}),
                    Kind::Add,
                    nodes.len(),
                    point,
                )
            } else {
                let parent = doc
                    .layers()
                    .find(|l| l.id() == self.selected)
                    .and_then(|l| {
                        if l.kind() == "group" {
                            Some(l.id().to_owned())
                        } else {
                            l.parent().map(str::to_owned)
                        }
                    });
                self.pen.drawing = true;
                (
                    json!({"op":"path.create","nodes":[{"point":point}],"stroke":self.palette_value(&super::color::Target::Foreground, doc.depth() == 16),"stroke_width":2.,"parent":parent}),
                    Kind::Add,
                    0,
                    point,
                )
            };
            let source = match self.canvas_gesture_source(doc) {
                Ok(source) => source,
                Err(error) => {
                    self.message = error.into();
                    return;
                }
            };
            self.pen.gesture = Some(Gesture {
                source,
                command,
                kind,
                index,
                start: point,
                anchor,
            });
            self.live_gesture = Some(crate::engine::id());
        }
        if self.pen.in_gesture() && release.is_none() && !ui.input(|input| input.pointer.primary_down()) {
            self.cancel_pen();
            return;
        }
        if let Some(g) = &mut self.pen.gesture {
            if let Some(p) = release.map(|(pos, _)| pos).or(cursor) {
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
            g.command["source_revision"] = json!(doc.revision());
            g.command["document_id"] = json!(doc.id());
            self.transient.push(g.command.clone());
            if release.is_some() {
                let g = self.pen.gesture.take().unwrap();
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
                self.apply_action(
                    vec![g.command],
                    "Edit Pen path",
                    Some((doc.id(), doc.revision())),
                    edit_job::Origin::Pen(g.index, doc.native().map(|source| source.key())),
                );
                self.transient.clear();
                self.live_gesture = None;
                return;
            }
        } else if self.pen.drawing && response.hovered() {
            if let (Some(l), Some(p)) = (&target, cursor)
            {
                if !nodes(l).unwrap().1 && hit.is_none() {
                    self.transient.push(json!({"op":"path.node.add","layer":l.id(),"point":to_doc(p),"document_id":doc.id(),"source_revision":doc.revision()}));
                }
            }
        }
        let controls = crate::path::preview_controls(doc, target, &self.transient)
            .or_else(|_| crate::path::preview_controls(doc, target, &[]))
            .ok()
            .flatten();
        if let Some((origin, source)) = controls {
            let Content::Bezier { nodes, .. } = &source.content else {
                return;
            };
            let document_point =
                |point| crate::path::point_in_document(origin, source.matrix, point);
            for (i, n) in nodes.iter().enumerate() {
                let at = screen(document_point(n.point));
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
                        let end = screen(document_point(h));
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

#[cfg(test)]
mod tests {
    use super::super::{text_tests, workspace_tests as workspace};
    use super::*;

    #[test]
    fn path_creation_selects_the_new_anchor_only_after_successful_publication() {
        for outcome in ["immediate", "commit", "cancel", "reserved", "replacement"] {
            let (mut app, ctx) = workspace::fixture(16);
            let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            app.tool = Tool::Pen;
            app.pen.fresh = true;
            app.pen.drawing = true;
            let command = json!({"op":"path.create","nodes":[{"point":[5,7],"out_handle":[9,8]}],
                "stroke":[12347,23459,34571,65535],"document_id":doc.id,"source_revision":doc.revision});
            let shared = app.shared.clone();
            if outcome == "reserved" {
                shared
                    .lock()
                    .unwrap()
                    .reserve(
                        "agent",
                        "Protected document",
                        vec![crate::engine::Scope {
                            target: None,
                            rect: None,
                        }],
                    )
                    .unwrap();
            }
            let mut guard = shared.lock().unwrap();
            if outcome == "replacement" {
                guard.resident_state_mut().expect("resident fixture").document.id = crate::engine::id();
            }
            let before = serde_json::to_value(&guard.resident_state().expect("resident fixture").document).unwrap();
            let history = guard.history_lengths();
            if outcome == "immediate" {
                drop(guard);
                app.apply_action(
                    vec![command],
                    "Edit Pen path",
                    Some((&doc.id, doc.revision)),
                    edit_job::Origin::Pen(0, None),
                );
            } else {
                app.submit_edit_job(
                    edit_job::Request {
                        project: app.project_id.clone(),
                        document: doc.id.clone(),
                        revision: doc.revision,
                        label: "Edit Pen path".into(),
                        selection_edit: false,
                        origin: edit_job::Origin::Pen(0, None),
                    },
                    edit_job::Commands::resident(vec![command]),
                    &ctx,
                )
                .unwrap();
                assert!(app.pen.active.is_none());
                assert!(app.pen.fresh);
                assert!(app.pen.selected.is_none());
                if outcome == "cancel" {
                    app.cancel_pending_edit();
                }
                drop(guard);
                text_tests::wait_edit(&mut app);
            }
            let engine = shared.lock().unwrap();
            let original = engine
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .find(|layer| layer.id == doc.layers[0].id)
                .unwrap();
            assert_eq!(original.pixels, doc.layers[0].pixels);
            if matches!(outcome, "immediate" | "commit") {
                assert_eq!(engine.history_lengths(), [history[0] + 1, 0]);
                assert_eq!(app.pen.active, Some((doc.id.clone(), app.selected.clone())));
                assert!(!app.pen.fresh);
                assert_eq!(app.pen.selected, Some(0));
                let selected = engine
                    .resident_state().expect("resident fixture").document
                    .layers
                    .iter()
                    .find(|layer| layer.id == app.selected)
                    .unwrap();
                assert_eq!(selected.pixels.depth, 16);
                assert!(matches!(
                    selected.source.as_ref().unwrap().content,
                    Content::Bezier { .. }
                ));
                let target = app.pen_target((&engine.resident_state().expect("resident fixture").document).into()).unwrap();
                let LayerView::Resident(borrowed) = target else {
                    unreachable!()
                };
                assert!(std::ptr::eq(borrowed, selected));
            } else {
                assert_eq!(engine.history_lengths(), history);
                assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
                assert!(app.pen.active.is_none());
                assert!(app.pen.fresh);
                assert!(app.pen.selected.is_none());
            }
        }
    }
}
