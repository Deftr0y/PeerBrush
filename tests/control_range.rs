use eframe::egui::{self, Pos2, Rect, Vec2};
use peerbrush::controls;
use std::ops::RangeInclusive;

struct Fixture {
    ctx: egui::Context,
    value: f32,
    rect: Rect,
    outside: Pos2,
    editor: egui::Id,
    slider: egui::Id,
    limits: RangeInclusive<f32>,
    width: f32,
    logarithmic: bool,
    native_handle: Option<egui::style::HandleShape>,
    enabled: bool,
    reported_value: Option<f64>,
    time: f64,
}
impl Fixture {
    fn new(value: f32) -> Self {
        Self::configured(value, 0.0..=100.0, 200.0, false, None, 1.0)
    }
    fn configured(
        value: f32,
        limits: RangeInclusive<f32>,
        width: f32,
        logarithmic: bool,
        native_handle: Option<egui::style::HandleShape>,
        scale: f32,
    ) -> Self {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(scale);
        let mut fixture = Self {
            ctx,
            value,
            rect: Rect::NOTHING,
            outside: Pos2::ZERO,
            editor: egui::Id::NULL,
            slider: egui::Id::NULL,
            limits,
            width,
            logarithmic,
            native_handle,
            enabled: true,
            reported_value: None,
            time: 0.0,
        };
        fixture.draw(vec![]);
        fixture
    }
    fn draw(&mut self, events: Vec<egui::Event>) -> bool {
        self.time += 0.05;
        let mut changed = false;
        let output = self.ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 200.))),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = ui
                        .add_enabled_ui(self.enabled, |ui| {
                            if let Some(handle) = self.native_handle {
                                ui.spacing_mut().slider_width = self.width.max(50.0);
                                ui.spacing_mut().interact_size.y = 22.0;
                                ui.add(
                                    egui::Slider::new(&mut self.value, self.limits.clone())
                                        .clamping(egui::SliderClamping::Edits)
                                        .show_value(false)
                                        .handle_shape(handle)
                                        .logarithmic(self.logarithmic)
                                        .max_decimals(2),
                                )
                            } else {
                                controls::range(
                                    ui,
                                    "number",
                                    &mut self.value,
                                    self.limits.clone(),
                                    self.width,
                                    "%",
                                    2,
                                    self.logarithmic,
                                )
                            }
                        })
                        .inner;
                    self.rect = response.rect;
                    self.slider = response.id;
                    self.editor = response.id.with("number");
                    changed = response.changed();
                    self.outside = ui.button("Outside").rect.center();
                });
            },
        );
        self.reported_value = output
            .platform_output
            .events
            .iter()
            .map(|event| event.widget_info())
            .filter(|info| info.typ == egui::WidgetType::Slider)
            .filter_map(|info| info.value)
            .next_back();
        changed
    }
    fn click(&mut self, pos: Pos2) -> bool {
        let pressed = self.draw(vec![egui::Event::PointerMoved(pos), pointer(pos, true)]);
        self.draw(vec![pointer(pos, false)]) || pressed
    }
    fn double_click(&mut self) {
        // Separate this gesture from the previous click sequence.
        self.time += 1.0;
        let pos = self.rect.center();
        assert!(!self.click(pos));
        assert!(!self.click(pos));
        assert!(self
            .ctx
            .data(|data| data.get_temp::<String>(self.editor))
            .is_some());
    }
    fn press(&mut self, fraction: f32) -> Pos2 {
        self.time += 1.0;
        let pos = self.rect.left_center() + Vec2::new(self.rect.width() * fraction, 0.0);
        self.draw(vec![egui::Event::PointerMoved(pos), pointer(pos, true)]);
        pos
    }
}
fn pointer(pos: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}
fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    }
}

#[test]
fn double_click_typing_selects_exact_value_and_accepts_enter_or_blur() {
    let mut fixture = Fixture::new(73.12345);
    fixture.double_click();
    assert_eq!(fixture.value, 73.12345);
    assert_eq!(
        fixture
            .ctx
            .data(|data| data.get_temp::<String>(fixture.editor))
            .unwrap(),
        "73.12345"
    );
    assert!(!fixture.draw(vec![egui::Event::Text("37.125".into())]));
    assert_eq!(
        fixture
            .ctx
            .data(|data| data.get_temp::<String>(fixture.editor))
            .unwrap(),
        "37.125"
    );
    assert!(fixture.draw(vec![key(egui::Key::Enter)]));
    assert_eq!(fixture.value, 37.125);
    assert!(fixture
        .ctx
        .data(|data| data.get_temp::<String>(fixture.editor))
        .is_none());
    assert!(!fixture.draw(vec![]));
    fixture.double_click();
    fixture.draw(vec![egui::Event::Text("200".into())]);
    let outside = fixture.outside;
    assert!(fixture.click(outside));
    assert_eq!(fixture.value, 100.0);
}

#[test]
fn escape_and_invalid_input_preserve_values_and_drag_still_updates() {
    let mut fixture = Fixture::new(50.0);
    fixture.double_click();
    fixture.draw(vec![egui::Event::Text("12".into())]);
    assert!(!fixture.draw(vec![key(egui::Key::Escape)]));
    assert_eq!(fixture.value, 50.0);
    fixture.double_click();
    fixture.draw(vec![egui::Event::Text("NaN".into())]);
    assert!(!fixture.draw(vec![key(egui::Key::Enter)]));
    assert_eq!(fixture.value, 50.0);
    fixture.draw(vec![]);
    let start = fixture.rect.center();
    fixture.time += 1.0;
    assert!(!fixture.draw(vec![egui::Event::PointerMoved(start), pointer(start, true)]));
    let end = start - Vec2::new(45.0, 0.0);
    assert!(fixture.draw(vec![egui::Event::PointerMoved(end)]));
    assert!(fixture.value < 40.0);
    fixture.draw(vec![pointer(end, false)]);
}

#[test]
fn full_bar_mapping_reduces_pointer_gain_and_reports_actual_values() {
    for width in [15.0, 50.0, 130.0, 200.0] {
        for scale in [1.0, 2.0] {
            let mut compact = Fixture::configured(50.0, 0.0..=100.0, width, false, None, scale);
            let mut circle = Fixture::configured(
                50.0,
                0.0..=100.0,
                width,
                false,
                Some(egui::style::HandleShape::Circle),
                scale,
            );
            let mut full = Fixture::configured(
                50.0,
                0.0..=100.0,
                width,
                false,
                Some(egui::style::HandleShape::Rect { aspect_ratio: 0.0 }),
                scale,
            );
            for fixture in [&mut compact, &mut circle, &mut full] {
                let start = fixture.press(0.5);
                let end = start + Vec2::new(fixture.rect.width() * 0.2, 0.0);
                assert!(fixture.draw(vec![egui::Event::PointerMoved(end)]));
                assert_eq!(fixture.reported_value, Some(f64::from(fixture.value)));
            }
            assert_eq!(compact.rect.width(), width.max(50.0));
            assert_eq!(compact.value, full.value);
            assert!(
                compact.value > 50.0 && compact.value < circle.value,
                "width={width}, scale={scale}: full bar={}, circle={}",
                compact.value,
                circle.value
            );
        }
    }
}

#[test]
fn logarithmic_mapping_batching_and_regrip_keep_native_full_ranges() {
    for (limits, logarithmic) in [
        (0.0..=100.0, false),
        (0.01..=8.0, true),
        (1.0..=1024.0, true),
        (0.0..=1024.0, true),
    ] {
        for width in [50.0, 130.0] {
            for scale in [1.0, 1.5, 2.0] {
                let value = (*limits.start() + *limits.end()) * 0.5;
                let make = |handle| {
                    Fixture::configured(value, limits.clone(), width, logarithmic, handle, scale)
                };
                let mut compact = make(None);
                let mut batched = make(None);
                let mut native = make(Some(egui::style::HandleShape::Rect { aspect_ratio: 0.0 }));
                for fixture in [&mut compact, &mut batched, &mut native] {
                    fixture.press(0.5);
                }
                let middle =
                    compact.rect.left_center() + Vec2::new(compact.rect.width() * 0.7, 0.0);
                let end = compact.rect.left_center() + Vec2::new(compact.rect.width() * 0.85, 0.0);
                compact.draw(vec![egui::Event::PointerMoved(middle)]);
                compact.draw(vec![egui::Event::PointerMoved(end)]);
                batched.draw(vec![
                    egui::Event::PointerMoved(middle),
                    egui::Event::PointerMoved(end),
                ]);
                native.draw(vec![egui::Event::PointerMoved(end)]);
                assert_eq!(compact.value, native.value);
                assert_eq!(batched.value, native.value);
                assert_eq!(compact.reported_value, Some(f64::from(compact.value)));
                compact.draw(vec![pointer(end, false)]);
                for endpoint in [*limits.start(), *limits.end()] {
                    let before = compact.value;
                    compact.press(0.5);
                    assert_eq!(compact.value, before, "regrip must not jump");
                    let end = if endpoint == *limits.start() {
                        compact.rect.left_center() - Vec2::X
                    } else {
                        compact.rect.right_center() + Vec2::X
                    };
                    compact.draw(vec![egui::Event::PointerMoved(end)]);
                    assert_eq!(compact.value, endpoint);
                    compact.draw(vec![pointer(end, false)]);
                }
            }
        }
    }
}

#[test]
fn keyboard_external_values_and_disabled_controls_keep_native_semantics() {
    let mut compact = Fixture::new(50.12345);
    let mut native = Fixture::configured(
        50.12345,
        0.0..=100.0,
        200.0,
        false,
        Some(egui::style::HandleShape::Rect { aspect_ratio: 0.0 }),
        1.0,
    );
    for fixture in [&mut compact, &mut native] {
        fixture.value = 60.12345;
        assert!(!fixture.draw(vec![]));
        assert_eq!(fixture.value, 60.12345);
        fixture
            .ctx
            .memory_mut(|memory| memory.request_focus(fixture.slider));
        assert!(fixture.draw(vec![key(egui::Key::ArrowRight)]));
        assert_eq!(fixture.reported_value, Some(f64::from(fixture.value)));
    }
    assert_eq!(compact.value, native.value);
    assert!(compact.value > 60.12345);
    for fixture in [&mut compact, &mut native] {
        fixture.value = 100.0;
        assert!(!fixture.draw(vec![key(egui::Key::ArrowRight)]));
        assert_eq!(fixture.value, 100.0);
        fixture.enabled = false;
        fixture.draw(vec![]);
        let before = fixture.value;
        let start = fixture.press(0.5);
        let end = start - Vec2::new(40.0, 0.0);
        assert!(!fixture.draw(vec![
            egui::Event::PointerMoved(end),
            key(egui::Key::ArrowLeft)
        ]));
        assert_eq!(fixture.value, before);
        fixture.draw(vec![pointer(end, false)]);
    }
}

#[test]
fn fixed_endpoint_ranges_render_finite_geometry_and_preserve_exact_values() {
    for endpoint in [0.0, 100.0] {
        let ctx = egui::Context::default();
        let mut value = endpoint;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 200.))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    assert!(!controls::range(
                        ui,
                        "fixed",
                        &mut value,
                        endpoint..=endpoint,
                        145.0,
                        "%",
                        2,
                        false
                    )
                    .changed());
                });
            },
        );
        assert_eq!(value, endpoint);
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        for primitive in primitives {
            if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive {
                assert!(mesh
                    .vertices
                    .iter()
                    .all(|vertex| vertex.pos.x.is_finite() && vertex.pos.y.is_finite()));
            }
        }
    }
}
