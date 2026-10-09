//! Compact project navigation; presentation never changes project sources.
use super::*;

pub(super) struct Label {
    name: String,
    dirty: bool,
    ai: bool,
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
        egui::Sense::click(),
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
    body.clone().on_hover_text(format!(
        "{}{}{}",
        label.name,
        if label.dirty {
            " · Unsaved changes"
        } else {
            ""
        },
        if label.ai { " · AI activity" } else { "" }
    ));
    close.clone().on_hover_text(format!("Close {}", label.name));
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
        let entries = crate::workspace::entries_in(&self.workspace);
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
        egui::TopBottomPanel::top("project tabs")
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
                    egui::ScrollArea::horizontal()
                        .id_salt("project tab scroll")
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
                                    if response.body.clicked() {
                                        select = Some(id.clone());
                                    }
                                    if response.close.clicked() {
                                        close = Some((id.clone(), shared.clone()));
                                    }
                                }
                            });
                        });
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
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new(format!(
                                                        "{}{}",
                                                        label.name,
                                                        if label.dirty { " ·" } else { "" }
                                                    ))
                                                    .color(if label.ai { AI_BLUE } else { MUTED }),
                                                )
                                                .selected(id == &self.project_id)
                                                .wrap(),
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
