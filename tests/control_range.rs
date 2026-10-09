use eframe::egui::{self, Pos2, Rect, Vec2};
use peerbrush::controls;

struct Fixture {
    ctx: egui::Context,
    value: f32,
    rect: Rect,
    outside: Pos2,
    editor: egui::Id,
    time: f64,
}
impl Fixture {
    fn new(value: f32) -> Self {
        let mut fixture = Self {
            ctx: egui::Context::default(),
            value,
            rect: Rect::NOTHING,
            outside: Pos2::ZERO,
            editor: egui::Id::NULL,
            time: 0.0,
        };
        fixture.draw(vec![]);
        fixture
    }
    fn draw(&mut self, events: Vec<egui::Event>) -> bool {
        self.time += 0.05;
        let mut changed = false;
        let _ = self.ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 200.))),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = controls::range(
                        ui,
                        "number",
                        &mut self.value,
                        0.0..=100.0,
                        200.0,
                        "%",
                        2,
                        false,
                    );
                    self.rect = response.rect;
                    self.editor = response.id.with("number");
                    changed = response.changed();
                    self.outside = ui.button("Outside").rect.center();
                });
            },
        );
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
    assert!(!fixture.draw(vec![egui::Event::Text("37.25".into())]));
    assert_eq!(
        fixture
            .ctx
            .data(|data| data.get_temp::<String>(fixture.editor))
            .unwrap(),
        "37.25"
    );
    assert!(fixture.draw(vec![key(egui::Key::Enter)]));
    assert_eq!(fixture.value, 37.25);
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
