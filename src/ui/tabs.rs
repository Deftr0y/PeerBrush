//! Compact project navigation; presentation never changes project sources.
use super::*;

pub(super) struct Label {
    name: String,
    dirty: bool,
    ai: bool,
}

pub(super) struct TabDrag {
    project: String,
    source_order: Vec<String>,
    order: Vec<String>,
}

struct TabResponse {
    body: egui::Response,
    close: egui::Response,
    rect: Rect,
}

fn tab(ui: &mut egui::Ui, id: &str, label: &Label, active: bool, width: f32) -> TabResponse {
    let (_, rect) = ui.allocate_space(Vec2::new(width, 30.));
    let close_rect = Rect::from_min_max(
        Pos2::new(rect.right() - 28., rect.top() + 1.),
        rect.right_bottom() - Vec2::new(1., 1.),
    );
    let body_rect = Rect::from_min_max(rect.min, Pos2::new(close_rect.left(), rect.bottom()));
    let body = ui.interact(
        body_rect,
        ui.id().with(("project", id)),
        egui::Sense::click_and_drag(),
    );
    body.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            &label.name,
        )
    });
    let close = ui.interact(
        close_rect,
        ui.id().with(("close project", id)),
        egui::Sense::click(),
    );
    close.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("Close {}", label.name),
        )
    });
    let hovered = ui.is_enabled() && ui.rect_contains_pointer(rect);
    let hover = ui
        .ctx()
        .animate_bool_with_time(ui.id().with(("project hover", id)), hovered, 0.12);
    let color = if label.ai {
        AI_BLUE
    } else if active {
        Color32::from_rgb(245, 237, 239)
    } else {
        Color32::from_rgb(202, 195, 201)
    };
    let presence =
        ui.ctx()
            .animate_bool_with_time(ui.id().with(("project active", id)), active, 0.14);
    if presence > 0. {
        let mut mesh = egui::epaint::Mesh::default();
        let fill = |y: f32| {
            let t = ((y - rect.top()) / rect.height()).clamp(0., 1.);
            Color32::from_rgb(
                (75. - 32. * t) as u8,
                (48. - 13. * t) as u8,
                (47. - 7. * t) as u8,
            )
            .gamma_multiply(presence)
        };
        mesh.colored_vertex(rect.center(), fill(rect.center().y));
        let radius = 7.;
        for (center, start) in [
            (
                rect.right_top() + Vec2::new(-radius, radius),
                -std::f32::consts::FRAC_PI_2,
            ),
            (rect.right_bottom() - Vec2::splat(radius), 0.),
            (
                rect.left_bottom() + Vec2::new(radius, -radius),
                std::f32::consts::FRAC_PI_2,
            ),
            (rect.left_top() + Vec2::splat(radius), std::f32::consts::PI),
        ] {
            for step in 0..=8 {
                let angle = start + step as f32 * std::f32::consts::FRAC_PI_2 / 8.;
                let point = center + Vec2::new(angle.cos(), angle.sin()) * radius;
                mesh.colored_vertex(point, fill(point.y));
            }
        }
        let count = mesh.vertices.len() as u32 - 1;
        for i in 1..=count {
            mesh.add_triangle(0, i, if i == count { 1 } else { i + 1 });
        }
        ui.painter().add(egui::Shape::mesh(mesh));
        ui.painter().line_segment(
            [
                rect.left_bottom() + Vec2::new(9., -1.5),
                rect.right_bottom() - Vec2::new(9., 1.5),
            ],
            Stroke::new(
                1.5,
                (if label.ai { AI_BLUE } else { ACCENT }).gamma_multiply(presence),
            ),
        );
    } else if hover > 0. {
        ui.painter().rect_filled(
            rect,
            7.,
            Color32::from_rgb(52, 44, 51).gamma_multiply(hover),
        );
    }
    let marker = rect.left_center() + Vec2::new(11., 0.);
    if label.dirty {
        ui.painter().circle_filled(marker, 2.5, ACCENT);
    } else if label.ai {
        ui.painter().circle_filled(marker, 2.5, AI_BLUE);
    } else {
        ui.painter().line_segment(
            [marker - Vec2::new(1.5, 4.), marker + Vec2::new(1.5, 4.)],
            Stroke::new(1.2, color.gamma_multiply(0.65)),
        );
    }
    let text_rect = Rect::from_min_max(
        rect.min + Vec2::new(21., 0.),
        body_rect.max - Vec2::new(3., 0.),
    );
    if !filename::paint(ui, &label.name, text_rect, 12., color) {
        let mut job = egui::text::LayoutJob::simple(
            label.name.clone(),
            egui::FontId::proportional(12.),
            color,
            text_rect.width().max(0.),
        );
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = ui.fonts(|fonts| fonts.layout_job(job));
        ui.painter()
            .with_clip_rect(text_rect.intersect(ui.clip_rect()))
            .galley(
                Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.),
                galley,
                color,
            );
    }
    if active || hovered || close.has_focus() || body.has_focus() {
        if close.hovered() || close.has_focus() {
            ui.painter()
                .rect_filled(close_rect.shrink(2.), 5., Color32::from_rgb(90, 57, 55));
        }
        let c = close_rect.center();
        let p = ui.painter();
        p.line_segment(
            [c - Vec2::splat(3.), c + Vec2::splat(3.)],
            Stroke::new(1.3, color),
        );
        p.line_segment(
            [c + Vec2::new(-3., 3.), c + Vec2::new(3., -3.)],
            Stroke::new(1.3, color),
        );
    }
    for response in [&body, &close] {
        if response.has_focus() {
            ui.painter().rect_stroke(
                response.rect.shrink(1.5),
                5.,
                Stroke::new(1., Color32::from_rgb(255, 180, 143)),
                egui::StrokeKind::Inside,
            );
        }
    }
    body.clone().on_hover_ui(|ui| {
        filename::label(ui, &label.name, 12., color, 900.);
        if label.dirty {
            ui.label("Unsaved changes");
        }
        if label.ai {
            ui.label(RichText::new("AI activity").color(AI_BLUE));
        }
    });
    close.clone().on_hover_ui(|ui| {
        ui.label("Close project");
        filename::label(ui, &label.name, 12., color, 900.);
    });
    TabResponse { body, close, rect }
}

impl PeerBrush {
    pub(super) fn draw_project_tabs(
        &mut self,
        ctx: &egui::Context,
    ) -> (
        Option<String>,
        Option<(String, Shared)>,
        Option<TabTransfer>,
    ) {
        let active = crate::workspace::active_id_in(&self.workspace);
        self.switch_project(&active);
        let mut entries = crate::workspace::entries_in(&self.workspace);
        let source_order: Vec<_> = entries.iter().map(|(id, _, _)| id.clone()).collect();
        let escape = self.tab_drag.is_some()
            && ctx.input_mut(|i| i.consume_key(i.modifiers, egui::Key::Escape));
        if escape
            || self.lifecycle.is_some()
            || self.lifecycle_frame
            || self.layer_drag.is_some()
            || self
                .tab_drag
                .as_ref()
                .is_some_and(|d| d.source_order != source_order)
        {
            self.tab_drag = None;
        }
        if let Some(drag) = &self.tab_drag {
            entries.sort_by_key(|(id, _, _)| drag.order.iter().position(|p| p == id).unwrap());
        }
        let live: HashSet<_> = entries.iter().map(|(id, _, _)| id.clone()).collect();
        self.project_views.retain(|id, _| live.contains(id));
        self.project_labels.retain(|id, _| live.contains(id));
        self.project_rects.clear();
        let changed = self.tab_active != active;
        self.tab_active = active;
        for (id, name, shared) in &entries {
            if let Ok(e) = shared.try_lock() {
                self.project_labels.insert(
                    id.clone(),
                    Label {
                        name: e.doc.name.clone(),
                        dirty: e.doc.revision != e.saved_revision,
                        ai: !e.leases.is_empty()
                            || e.ai_change.as_ref().is_some_and(|change| {
                                crate::engine::now().saturating_sub(change.at) < 3
                            }),
                    },
                );
            } else {
                self.project_labels
                    .entry(id.clone())
                    .or_insert_with(|| Label {
                        name: name.clone(),
                        dirty: false,
                        ai: false,
                    });
            }
        }
        let mut select = None;
        let mut close = None;
        let mut hovered = None;
        let mut started = None;
        let mut strip = Rect::NOTHING;
        let panel = egui::TopBottomPanel::top("project tabs")
            .exact_height(38.)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(27, 24, 29))
                    .inner_margin(egui::Margin::symmetric(6, 4)),
            )
            .show(ctx, |ui| {
                if self.lifecycle.is_some() || self.lifecycle_frame {
                    ui.disable();
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.;
                    let available = (ui.available_width() - 68.).max(72.);
                    let width = ((available - 4. * entries.len().saturating_sub(1) as f32)
                        / entries.len().max(1) as f32)
                        .clamp(132., 216.);
                    let scroll = egui::ScrollArea::horizontal()
                        .id_salt("project tab scroll")
                        .drag_to_scroll(false)
                        .auto_shrink([false, false])
                        .max_width(available)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.;
                                for (id, _, shared) in &entries {
                                    let response = tab(
                                        ui,
                                        id,
                                        &self.project_labels[id],
                                        id == &self.project_id,
                                        width,
                                    );
                                    self.project_rects.insert(id.clone(), response.rect);
                                    if response.body.drag_started() && self.layer_drag.is_none() {
                                        started = Some(id.clone());
                                    }
                                    if self.tab_drag.as_ref().is_some_and(|d| d.project == *id) {
                                        ui.painter().line_segment(
                                            [response.rect.left_top(), response.rect.left_bottom()],
                                            Stroke::new(2., ACCENT),
                                        );
                                    }
                                    if changed && id == &self.project_id {
                                        ui.scroll_to_rect(response.rect, Some(egui::Align::Center));
                                    }
                                    if id != &self.project_id
                                        && ui.is_enabled()
                                        && response.rect.intersect(ui.clip_rect()).contains(
                                            ctx.input(|i| i.pointer.interact_pos())
                                                .unwrap_or(Pos2::new(-1., -1.)),
                                        )
                                    {
                                        if let Some(drag) = &self.layer_drag {
                                            if let Ok(e) = shared.try_lock() {
                                                let target = self
                                                    .project_views
                                                    .get(id)
                                                    .map(|v| v.selected.clone())
                                                    .filter(|target| {
                                                        e.doc.layers.iter().any(|l| l.id == *target)
                                                    })
                                                    .unwrap_or_else(|| {
                                                        e.doc
                                                            .layers
                                                            .first()
                                                            .map(|l| l.id.clone())
                                                            .unwrap_or_default()
                                                    });
                                                hovered = Some(TabTransfer {
                                                    source: self.project_id.clone(),
                                                    destination: id.clone(),
                                                    source_document: self.doc_snapshot.id.clone(),
                                                    destination_document: e.doc.id.clone(),
                                                    source_revision: drag.revision,
                                                    destination_revision: e.doc.revision,
                                                    layers: drag.ids.clone(),
                                                    target,
                                                    move_layers: ctx.input(|i| i.modifiers.shift),
                                                });
                                                ui.painter().line_segment(
                                                    [
                                                        response.rect.left_bottom(),
                                                        response.rect.right_bottom(),
                                                    ],
                                                    Stroke::new(2., ACCENT),
                                                );
                                            }
                                        }
                                    }
                                    if self.tab_drag.is_none() && response.body.clicked() {
                                        select = Some(id.clone());
                                    }
                                    if self.tab_drag.is_none() && response.close.clicked() {
                                        close = Some((id.clone(), shared.clone()));
                                    }
                                }
                            });
                            if self.tab_drag.is_some() {
                                if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                                    let viewport = ui.clip_rect();
                                    if viewport.contains(pos) {
                                        let speed = if pos.x < viewport.left() + 24. {
                                            180.
                                        } else if pos.x > viewport.right() - 24. {
                                            -180.
                                        } else {
                                            0.
                                        };
                                        if speed != 0. {
                                            ui.scroll_with_delta_animation(
                                                Vec2::new(
                                                    speed * ctx.input(|i| i.stable_dt).min(0.05),
                                                    0.,
                                                ),
                                                egui::style::ScrollAnimation::none(),
                                            );
                                            ctx.request_repaint();
                                        }
                                    }
                                }
                            }
                        });
                    strip = scroll.inner_rect;
                    let new = ui
                        .add_sized([28., 28.], egui::Button::new("+").frame(false))
                        .on_hover_text("New project");
                    if new.clicked() {
                        self.show_new = true;
                    }
                    let picker = ui
                        .menu_button(" ", |ui| {
                            ui.set_max_width(330.);
                            egui::ScrollArea::vertical()
                                .max_height(360.)
                                .show(ui, |ui| {
                                    for (id, _, _) in &entries {
                                        let label = &self.project_labels[id];
                                        if filename::project_button(
                                            ui,
                                            &format!(
                                                "{}{}",
                                                label.name,
                                                if label.dirty { " ·" } else { "" }
                                            ),
                                            id == &self.project_id,
                                            if label.ai { AI_BLUE } else { MUTED },
                                        )
                                        .clicked()
                                        {
                                            select = Some(id.clone());
                                            ui.close_menu();
                                        }
                                    }
                                });
                        })
                        .response
                        .on_hover_text("All open projects");
                    self.project_picker_rect = Some(picker.rect);
                    picker.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            ui.is_enabled(),
                            "All open projects",
                        )
                    });
                    icons::paint(
                        ui.painter(),
                        picker.rect.shrink(5.),
                        Icon::Down,
                        ui.is_enabled(),
                    );
                });
            });
        self.project_drop_rect = Some(panel.response.rect);
        if let Some(project) = started {
            if !escape {
                self.tab_drag = Some(TabDrag {
                    project,
                    source_order: source_order.clone(),
                    order: source_order,
                });
            }
        }
        if let Some(drag) = &mut self.tab_drag {
            let pointer = ctx.input(|i| i.pointer.interact_pos());
            let inside = pointer.is_some_and(|pos| strip.contains(pos));
            if inside {
                // Fixed positions in the strip determine the slot, independent of the moving IDs.
                let mut slots: Vec<_> = self.project_rects.values().collect();
                slots.sort_by(|a, b| a.center().x.total_cmp(&b.center().x));
                let x = pointer.unwrap().x;
                let index = slots
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        (a.center().x - x)
                            .abs()
                            .total_cmp(&(b.center().x - x).abs())
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                drag.order = drag.source_order.clone();
                drag.order.retain(|id| id != &drag.project);
                drag.order.insert(index, drag.project.clone());
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                ctx.request_repaint();
            }
            if ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
                let drag = self.tab_drag.take().unwrap();
                if inside {
                    if let Err(error) = crate::workspace::reorder_in(
                        &self.workspace,
                        &drag.source_order,
                        &drag.order,
                    ) {
                        self.message = error;
                    }
                }
            } else if !ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
                self.tab_drag = None;
            }
        }
        (select, close, hovered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::sync::Mutex;

    fn fixture(depth: u16, count: usize) -> (PeerBrush, egui::Context, Vec<Shared>) {
        let ctx = egui::Context::default();
        let mut e = Engine::new();
        e.doc = Document::new_depth(32, 24, depth).unwrap();
        let root = Arc::new(Mutex::new(e));
        crate::workspace::attach(&root);
        let mut projects = vec![root.clone()];
        for _ in 1..count {
            projects.push(crate::workspace::new_project(&root, 32, 24, depth, None).unwrap());
        }
        for (i, shared) in projects.iter().enumerate() {
            shared.lock().unwrap().doc.name =
                format!("Project {} · an unusually long artwork name", i + 1);
        }
        let app = PeerBrush::init(
            &ctx,
            root,
            Connection {
                port: 0,
                token: String::new(),
                state_dir: std::env::temp_dir(),
                instance_lock: None,
                availability: Default::default(),
            },
        );
        (app, ctx, projects)
    }
    fn frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        width: f32,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 640.))),
                events,
                ..Default::default()
            },
            |ctx| app.project_tabs(ctx),
        )
    }
    fn click(app: &mut PeerBrush, ctx: &egui::Context, width: f32, pos: Pos2) {
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                width,
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
    fn ids(app: &PeerBrush) -> Vec<String> {
        crate::workspace::entries_in(&app.workspace)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect()
    }
    #[test]
    fn unicode_project_tabs_switch_without_mutating_names_native_pixels_or_history() {
        let names = [
            "café Ω Ж.psd",
            "שלום עולם.psd",
            "مرحبا بالعالم.psd",
            "हिन्दी.psd",
            "漢字 あいう 한글.psd",
        ];
        for depth in [8, 16] {
            let (mut app, ctx, projects) = fixture(depth, names.len());
            for (shared, name) in projects.iter().zip(names) {
                shared.lock().unwrap().doc.name = name.into();
            }
            let before: Vec<_> = projects
                .iter()
                .map(|s| {
                    let e = s.lock().unwrap();
                    (e.doc.clone(), e.undo.len(), e.path.clone())
                })
                .collect();
            frame(&mut app, &ctx, 1600., Vec::new());
            for id in ids(&app) {
                let rect = app.project_rects[&id];
                click(
                    &mut app,
                    &ctx,
                    1600.,
                    rect.left_center() + Vec2::new(30., 0.),
                );
                assert_eq!(app.project_id, id);
            }
            for (shared, (doc, history, path)) in projects.iter().zip(before) {
                let e = shared.lock().unwrap();
                assert_eq!(
                    serde_json::to_value(&e.doc).unwrap(),
                    serde_json::to_value(doc).unwrap()
                );
                assert_eq!(e.undo.len(), history);
                assert_eq!(e.path, path);
            }
        }
    }
    fn press(app: &mut PeerBrush, ctx: &egui::Context, width: f32, pos: Pos2, pressed: bool) {
        frame(
            app,
            ctx,
            width,
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
    fn begin_drag(app: &mut PeerBrush, ctx: &egui::Context, width: f32, source: &str, to: Pos2) {
        let from = app.project_rects[source].center() - Vec2::new(20., 0.);
        press(app, ctx, width, from, true);
        frame(
            app,
            ctx,
            width,
            vec![egui::Event::PointerMoved(from + Vec2::new(12., 0.))],
        );
        assert!(
            app.tab_drag.is_some(),
            "Tab body must start a reorder gesture"
        );
        frame(app, ctx, width, vec![egui::Event::PointerMoved(to)]);
        frame(app, ctx, width, vec![]);
    }
    #[test]
    fn tab_drags_show_live_order_then_commit_without_switching_or_touching_native_work() {
        for depth in [8, 16] {
            let (mut app, ctx, projects) = fixture(depth, 4);
            let original = ids(&app);
            crate::workspace::select_in(&app.workspace, &original[0]).unwrap();
            for (i, project) in projects.iter().enumerate() {
                let mut e = project.lock().unwrap();
                let layer = e.doc.layers[0].id.clone();
                e.doc.layers[0]
                    .pixels
                    .set16(2, 3, [10001 + i as u16, 20003, 40007, 65535]);
                e.edit(
                    "human",
                    &[
                        json!({"op":"selection","kind":"ellipse","rect":[1,2,19,20],"feather":2}),
                        json!({"op":"mask.add","layer":layer}),
                    ],
                    None,
                    None,
                    "Human mask",
                )
                .unwrap();
                e.path = Some(std::path::PathBuf::from(format!("artwork-{i}.psd")));
            }
            for _ in 0..3 {
                frame(&mut app, &ctx, 980., vec![]);
            }
            app.zoom = 2.5;
            app.pan = Vec2::new(7., -9.);
            let selected = app.selected.clone();
            let before: Vec<_> = projects
                .iter()
                .map(|p| {
                    let e = p.lock().unwrap();
                    (
                        crate::psd::encode(&e.doc).unwrap(),
                        e.undo.len(),
                        e.redo.len(),
                        e.path.clone(),
                        e.saved_revision,
                    )
                })
                .collect();
            let end = app.project_rects[&original[3]].center();
            begin_drag(&mut app, &ctx, 980., &original[1], end);
            assert_eq!(ids(&app), original, "Only presentation changes while held");
            assert_eq!(
                app.tab_drag.as_ref().unwrap().order,
                vec![
                    original[0].clone(),
                    original[2].clone(),
                    original[3].clone(),
                    original[1].clone()
                ]
            );
            assert!(
                app.project_rects[&original[1]].left() > app.project_rects[&original[3]].left()
            );
            let output = frame(&mut app, &ctx, 980., vec![]);
            assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::LineSegment{points,stroke} if stroke.width==2. && stroke.color==ACCENT && points[0].x==app.project_rects[&original[1]].left())));
            assert_eq!(app.project_id, original[0]);
            press(&mut app, &ctx, 980., end, false);
            assert_eq!(
                ids(&app),
                vec![
                    original[0].clone(),
                    original[2].clone(),
                    original[3].clone(),
                    original[1].clone()
                ]
            );
            let first = app.project_rects[&original[0]].center();
            begin_drag(&mut app, &ctx, 980., &original[1], first);
            press(&mut app, &ctx, 980., first, false);
            assert_eq!(
                ids(&app),
                vec![
                    original[1].clone(),
                    original[0].clone(),
                    original[2].clone(),
                    original[3].clone()
                ]
            );
            assert_eq!(app.project_id, original[0]);
            assert_eq!(crate::workspace::active_id_in(&app.workspace), original[0]);
            assert_eq!(app.selected, selected);
            assert_eq!(app.zoom, 2.5);
            assert_eq!(app.pan, Vec2::new(7., -9.));
            for (project, before) in projects.iter().zip(before) {
                let e = project.lock().unwrap();
                assert_eq!(
                    (
                        crate::psd::encode(&e.doc).unwrap(),
                        e.undo.len(),
                        e.redo.len(),
                        e.path.clone(),
                        e.saved_revision
                    ),
                    before
                );
                assert_eq!(e.doc.bit_depth, depth);
            }
        }
    }
    #[test]
    fn tab_drags_cancel_on_escape_outside_drop_and_concurrent_project_membership_or_order() {
        let (mut app, ctx, projects) = fixture(16, 3);
        let original = ids(&app);
        for _ in 0..3 {
            frame(&mut app, &ctx, 980., vec![]);
        }
        for scenario in 0..4 {
            let expected = ids(&app);
            let end = app.project_rects[expected.last().unwrap()].center();
            begin_drag(&mut app, &ctx, 980., &original[0], end);
            match scenario {
                0 => {
                    frame(
                        &mut app,
                        &ctx,
                        980.,
                        vec![egui::Event::Key {
                            key: egui::Key::Escape,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        }],
                    );
                    press(&mut app, &ctx, 980., end, false);
                }
                1 => press(&mut app, &ctx, 980., Pos2::new(end.x, 100.), false),
                2 => {
                    crate::workspace::new_project(&app.workspace_root, 32, 24, 16, None).unwrap();
                    frame(&mut app, &ctx, 980., vec![]);
                    press(&mut app, &ctx, 980., end, false);
                }
                _ => {
                    let mut order = expected.clone();
                    order.reverse();
                    crate::workspace::reorder_in(&app.workspace, &expected, &order).unwrap();
                    frame(&mut app, &ctx, 980., vec![]);
                    press(&mut app, &ctx, 980., end, false);
                    assert_eq!(ids(&app), order);
                }
            }
            assert!(app.tab_drag.is_none());
            if scenario < 2 {
                assert_eq!(ids(&app), expected);
            }
            if scenario == 2 {
                assert_eq!(&ids(&app)[..expected.len()], expected);
            }
            frame(&mut app, &ctx, 980., vec![]);
        }
        // A closed target must not be restored by a pending gesture.
        let order = ids(&app);
        let end = app.project_rects[order.last().unwrap()].center();
        begin_drag(&mut app, &ctx, 980., &original[0], end);
        let e = projects[1].lock().unwrap();
        let doc = e.doc.id.clone();
        drop(e);
        crate::workspace::close_in(&app.workspace, &original[1], &doc, 0, false, "human").unwrap();
        frame(&mut app, &ctx, 980., vec![]);
        press(&mut app, &ctx, 980., end, false);
        assert!(app.tab_drag.is_none());
        assert!(!ids(&app).contains(&original[1]));
        assert!(projects[0].lock().unwrap().undo.is_empty());
    }
    #[test]
    fn tab_drag_edge_scroll_reaches_hidden_projects_without_changing_active_identity() {
        let (mut app, ctx, _) = fixture(16, 16);
        let original = ids(&app);
        crate::workspace::select_in(&app.workspace, &original[0]).unwrap();
        for _ in 0..20 {
            frame(&mut app, &ctx, 980., vec![]);
        }
        let edge = Pos2::new(900., 19.);
        begin_drag(&mut app, &ctx, 980., &original[0], edge);
        let initial = app.project_rects[&original[15]].left();
        for _ in 0..420 {
            frame(&mut app, &ctx, 980., vec![]);
        }
        assert!(app.project_rects[&original[15]].left() < initial - 500.);
        assert_eq!(
            app.tab_drag.as_ref().unwrap().order.last(),
            Some(&original[0])
        );
        press(&mut app, &ctx, 980., edge, false);
        assert_eq!(ids(&app).last(), Some(&original[0]));
        assert_eq!(app.project_id, original[0]);
        assert_eq!(crate::workspace::active_id_in(&app.workspace), original[0]);
    }
    #[test]
    fn project_bodies_and_close_targets_preserve_native_sources_and_review_the_right_tab() {
        for depth in [8, 16] {
            let (mut app, ctx, projects) = fixture(depth, 3);
            let first = projects[0].lock().unwrap().project_id.clone();
            let second = projects[1].lock().unwrap().project_id.clone();
            crate::workspace::select_in(&app.workspace, &first).unwrap();
            {
                let mut e = projects[1].lock().unwrap();
                let layer = e.doc.layers[0].id.clone();
                e.edit("human",&[json!({"op":"paint.fill","layer":layer,"rect":[0,0,2,2],"color":[233,84,32,255]})],None,None,"Human work").unwrap();
            }
            for _ in 0..3 {
                frame(&mut app, &ctx, 980., vec![]);
            }
            assert!(app.project_labels[&second].dirty);
            let before = projects[1]
                .lock()
                .unwrap()
                .doc
                .preview(None, 32, None, false)
                .unwrap()
                .2;
            let pos = app.project_rects[&second].center() - Vec2::new(20., 0.);
            click(&mut app, &ctx, 980., pos);
            assert_eq!(app.project_id, second);
            let pos = app.project_rects[&first].center() - Vec2::new(20., 0.);
            click(&mut app, &ctx, 980., pos);
            assert_eq!(app.project_id, first);
            let close = app.project_rects[&second].right_center() - Vec2::new(14., 0.);
            click(&mut app, &ctx, 980., close);
            assert!(
                app.lifecycle.is_some(),
                "Dirty close must open native review"
            );
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(980., 640.))),
                        ..Default::default()
                    },
                    |ctx| app.lifecycle_dialog(ctx),
                );
            }
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(980., 640.))),
                    ..Default::default()
                },
                |ctx| app.lifecycle_dialog(ctx),
            );
            let name = projects[1].lock().unwrap().doc.name.clone();
            assert!(
                output.shapes.iter().any(
                    |s| matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.job.text==name)
                ),
                "Close review must name the requested project"
            );
            assert_eq!(
                projects[1]
                    .lock()
                    .unwrap()
                    .doc
                    .preview(None, 32, None, false)
                    .unwrap()
                    .2,
                before
            );
            assert_eq!(projects[1].lock().unwrap().undo.len(), 1);
            assert!(projects[0].lock().unwrap().undo.is_empty());
        }
    }
    #[test]
    fn overflow_picker_selects_hidden_projects_and_scrolls_active_into_view_at_native_widths() {
        for width in [980., 1360., 2000.] {
            let (mut app, ctx, projects) = fixture(16, 16);
            let first = projects[0].lock().unwrap().project_id.clone();
            let second = projects[1].lock().unwrap().project_id.clone();
            let last = projects[15].lock().unwrap().project_id.clone();
            crate::workspace::select_in(&app.workspace, &first).unwrap();
            for _ in 0..8 {
                frame(&mut app, &ctx, width, vec![]);
            }
            for _ in 0..3 {
                frame(&mut app, &ctx, width, vec![]);
            }
            frame(&mut app, &ctx, width, vec![]);
            let picker = app
                .project_picker_rect
                .expect("All projects picker must be rendered")
                .center();
            assert!(
                picker.x < width && picker.y < 38.,
                "Picker clipped at {width}: {picker:?}"
            );
            // The rightmost picker stays outside the scrollable strip.
            click(&mut app, &ctx, width, picker);
            for _ in 0..3 {
                frame(&mut app, &ctx, width, vec![]);
            }
            let output = frame(&mut app, &ctx, width, vec![]);
            let target = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(t)
                    if t.galley.job.text.contains("Project 2 ·")
                        && t.pos.y > 38.
                        && shape.clip_rect.contains(t.pos) =>
                {
                    Some(t.pos + t.galley.size() / 2.)
                }
                _ => None,
            });
            click(
                &mut app,
                &ctx,
                width,
                target.expect("Picker must show the second project"),
            );
            assert_eq!(app.project_id, second);
            // Background switches must reveal a previously hidden active tab.
            crate::workspace::select_in(&app.workspace, &last).unwrap();
            for _ in 0..16 {
                frame(&mut app, &ctx, width, vec![]);
            }
            let rect = app.project_rects[&last];
            assert!(
                rect.left() >= 5. && rect.right() <= width - 65.,
                "{width}: {rect:?}"
            );
            assert!(rect.width() >= 132. && rect.width() <= 216.);
            assert!(projects.iter().all(|p| p.lock().unwrap().undo.is_empty()));
        }
    }
    #[test]
    fn keyboard_focus_activates_the_tab_and_close_control_with_distinct_accessible_targets() {
        let ctx = egui::Context::default();
        let label = Label {
            name: "Keyboard artwork".into(),
            dirty: true,
            ai: true,
        };
        let mut ids = None;
        let mut clicked = (false, false);
        let mut render = |events| {
            ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let r = tab(ui, "keyboard", &label, true, 180.);
                        ids = Some((r.body.id, r.close.id));
                        clicked = (r.body.clicked(), r.close.clicked());
                    });
                },
            )
        };
        render(vec![]);
        drop(render);
        let ids = ids.unwrap();
        assert_ne!(ids.0, ids.1);
        for (id, expected) in [(ids.0, (true, false)), (ids.1, (false, true))] {
            ctx.memory_mut(|m| m.request_focus(id));
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let r = tab(ui, "keyboard", &label, true, 180.);
                        clicked = (r.body.clicked(), r.close.clicked());
                    });
                },
            );
            assert_eq!(clicked, expected);
        }
    }
}
