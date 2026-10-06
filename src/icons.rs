//! Small monochrome glyphs. Color belongs to state and artwork, not tool identity.
use eframe::egui::{self, Color32, Rect, Stroke, Vec2};
#[derive(Clone, Copy)]
pub enum Icon {
    Cursor,
    Move,
    Rotate,
    Scale,
    Brush,
    Eraser,
    Fill,
    Gradient,
    Rectangle,
    Ellipse,
    Selection,
    Picker,
    Pan,
    Eye,
    EyeOff,
    Folder,
    AddLayer,
    FillLayer,
    Save,
    Undo,
    Redo,
    Adjust,
    Frame,
    Mask,
    Solo,
    Trash,
    Down,
    Right,
    Up,
    Lock,
    Connect,
    Swap,
    Wand,
}
pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, enabled: bool) {
    let f = rect.width().min(rect.height()) / 32.0;
    let color = Color32::from_gray(if enabled { 240 } else { 100 });
    let point = |x: f32, y: f32| rect.center() + Vec2::new(x, y) * f;
    let stroke = Stroke::new(1.7 * f, color);
    let line = |pts: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            pts.iter().map(|&(x, y)| point(x, y)).collect(),
            stroke,
        ));
    };
    let polygon = |pts: &[(f32, f32)]| {
        painter.add(egui::Shape::convex_polygon(
            pts.iter().map(|&(x, y)| point(x, y)).collect(),
            color,
            Stroke::NONE,
        ));
    };
    let outline = |a: (f32, f32), b: (f32, f32)| {
        painter.rect_stroke(
            Rect::from_min_max(point(a.0, a.1), point(b.0, b.1)),
            1,
            stroke,
            egui::StrokeKind::Inside,
        );
    };
    match icon {
        Icon::Cursor => polygon(&[
            (-8., -11.),
            (-8., 8.),
            (-3., 4.),
            (1., 11.),
            (4., 9.),
            (0., 2.),
            (8., 1.),
        ]),
        Icon::Move => {
            line(&[(-11., 0.), (11., 0.)]);
            line(&[(0., -11.), (0., 11.)]);
            for (x, y) in [(-1., 0.), (1., 0.), (0., -1.), (0., 1.)] {
                line(&[
                    (x * 7. - y * 3., y * 7. - x * 3.),
                    (x * 11., y * 11.),
                    (x * 7. + y * 3., y * 7. + x * 3.),
                ]);
            }
        }
        Icon::Rotate => {
            let pts = (0..32)
                .map(|i| {
                    let a = 0.2 + i as f32 * 0.16;
                    point(a.cos() * 9., a.sin() * 9.)
                })
                .collect();
            painter.add(egui::Shape::line(pts, stroke));
            line(&[(1., -12.), (7., -9.), (2., -5.)]);
        }
        Icon::Scale => {
            outline((-10., -2.), (2., 10.));
            line(&[(-4., 4.), (10., -10.)]);
            line(&[(3., -10.), (10., -10.), (10., -3.)]);
        }
        Icon::Brush => {
            polygon(&[(-3., 1.), (7., -11.), (10., -8.), (0., 4.)]);
            line(&[
                (-5., 2.),
                (-1., 6.),
                (-4., 10.),
                (-10., 11.),
                (-8., 7.),
                (-5., 2.),
            ]);
        }
        Icon::Eraser => {
            line(&[
                (-10., 1.),
                (2., -11.),
                (10., -3.),
                (-2., 9.),
                (-5., 9.),
                (-10., 4.),
                (-10., 1.),
            ]);
            line(&[(-5., -4.), (3., 4.)]);
            line(&[(-3., 11.), (11., 11.)]);
        }
        Icon::Fill | Icon::FillLayer => {
            line(&[(-11., -1.), (-2., -10.), (8., -1.), (-1., 8.), (-11., -1.)]);
            line(&[(-8., -2.), (-8., -9.), (-4., -12.)]);
            line(&[(-9., -1.), (6., -1.)]);
            polygon(&[(10., 2.), (7., 8.), (8., 11.), (12., 11.), (13., 8.)]);
        }
        Icon::Gradient => {
            outline((-10., -9.), (10., 9.));
            for i in 0..7 {
                let x = -7. + i as f32 * 2.3;
                painter.line_segment(
                    [point(x, -6.), point(x, 6.)],
                    Stroke::new(1.8 * f, color.gamma_multiply(1. - i as f32 / 7.)),
                );
            }
        }
        Icon::Rectangle => outline((-10., -8.), (10., 8.)),
        Icon::Ellipse => {
            painter.circle_stroke(rect.center(), 10. * f, stroke);
        }
        Icon::Selection => {
            for i in 0..3 {
                let a = -10. + i as f32 * 7.;
                line(&[(a, -10.), (a + 4., -10.)]);
                line(&[(a, 10.), (a + 4., 10.)]);
                line(&[(-10., a), (-10., a + 4.)]);
                line(&[(10., a), (10., a + 4.)]);
            }
        }
        Icon::Picker => {
            line(&[
                (-9., 10.),
                (-7., 5.),
                (4., -6.),
                (8., -2.),
                (-3., 9.),
                (-9., 10.),
            ]);
            line(&[(0., -10.), (4., -14.), (12., -6.), (8., -2.), (0., -10.)]);
            line(&[(-4., 3.), (0., 7.)]);
        }
        Icon::Pan => line(&[
            (-8., 0.),
            (-5., 3.),
            (-5., -7.),
            (-2., -8.),
            (-1., -1.),
            (-1., -11.),
            (2., -11.),
            (3., -1.),
            (3., -8.),
            (6., -7.),
            (6., 1.),
            (8., -3.),
            (11., -1.),
            (8., 7.),
            (4., 11.),
            (-3., 11.),
            (-10., 3.),
            (-10., 1.),
            (-8., 0.),
        ]),
        Icon::Eye | Icon::EyeOff | Icon::Solo => {
            line(&[
                (-11., 0.),
                (-6., -5.),
                (0., -7.),
                (6., -5.),
                (11., 0.),
                (6., 5.),
                (0., 7.),
                (-6., 5.),
                (-11., 0.),
            ]);
            painter.circle_stroke(rect.center(), 3. * f, stroke);
            if matches!(icon, Icon::EyeOff) {
                line(&[(-10., 9.), (10., -9.)]);
            }
        }
        Icon::Folder => line(&[
            (-11., 8.),
            (-11., -8.),
            (-3., -8.),
            (0., -5.),
            (11., -5.),
            (11., 8.),
            (-11., 8.),
        ]),
        Icon::AddLayer => {
            outline((-10., -10.), (6., 9.));
            line(&[(4., 5.), (12., 5.)]);
            line(&[(8., 1.), (8., 9.)]);
        }
        Icon::Save => {
            outline((-10., -10.), (10., 10.));
            outline((-5., 2.), (5., 9.));
            line(&[(-5., -9.), (-5., -3.), (4., -3.), (4., -9.)]);
        }
        Icon::Undo | Icon::Redo => {
            let s = if matches!(icon, Icon::Redo) { -1. } else { 1. };
            line(&[
                (-10. * s, -4.),
                (2. * s, -4.),
                (7. * s, -2.),
                (9. * s, 2.),
                (8. * s, 7.),
                (4. * s, 9.),
            ]);
            line(&[(-5. * s, -10.), (-11. * s, -4.), (-5. * s, 2.)]);
        }
        Icon::Adjust => {
            for (x, y) in [(-8., -4.), (0., 5.), (8., -6.)] {
                line(&[(x, -11.), (x, 11.)]);
                painter.circle_filled(point(x, y), 3. * f, Color32::from_rgb(41, 39, 43));
                painter.circle_stroke(point(x, y), 3. * f, stroke);
            }
        }
        Icon::Frame => {
            for (x, y) in [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)] {
                line(&[(x * 5., y * 10.), (x * 10., y * 10.), (x * 10., y * 5.)]);
            }
        }
        Icon::Mask => {
            painter.circle_stroke(rect.center(), 10. * f, stroke);
            polygon(&[
                (0., -10.),
                (0., 10.),
                (6., 8.),
                (10., 3.),
                (10., -3.),
                (6., -8.),
            ]);
        }
        Icon::Trash => {
            line(&[(-7., -5.), (-6., 10.), (6., 10.), (7., -5.)]);
            line(&[(-10., -7.), (10., -7.)]);
            line(&[(-4., -7.), (-3., -11.), (3., -11.), (4., -7.)]);
            line(&[(-2., -2.), (-2., 6.)]);
            line(&[(2., -2.), (2., 6.)]);
        }
        Icon::Down => line(&[(-6., -3.), (0., 3.), (6., -3.)]),
        Icon::Right => line(&[(-3., -6.), (3., 0.), (-3., 6.)]),
        Icon::Up => line(&[(-6., 3.), (0., -3.), (6., 3.)]),
        Icon::Lock => {
            outline((-8., -2.), (8., 11.));
            line(&[
                (-5., -2.),
                (-5., -8.),
                (-3., -11.),
                (3., -11.),
                (5., -8.),
                (5., -2.),
            ]);
            painter.circle_filled(point(0., 3.), 1.5 * f, color);
        }
        Icon::Connect => {
            line(&[
                (-4., -9.),
                (-8., -9.),
                (-11., -6.),
                (-11., -2.),
                (-8., 1.),
                (-4., 1.),
                (3., -6.),
            ]);
            line(&[
                (4., 9.),
                (8., 9.),
                (11., 6.),
                (11., 2.),
                (8., -1.),
                (4., -1.),
                (-3., 6.),
            ]);
            line(&[(-4., 4.), (4., -4.)]);
        }
        Icon::Swap => {
            line(&[(-10., -4.), (10., -4.), (5., -9.)]);
            line(&[(10., 4.), (-10., 4.), (-5., 9.)]);
        }
        Icon::Wand => {
            line(&[(-10., 11.), (6., -5.)]);
            line(&[(-7., 11.), (8., -3.)]);
            line(&[(5., -11.), (5., -7.)]);
            line(&[(11., -5.), (14., -5.)]);
            line(&[(-2., -6.), (-5., -9.)]);
            line(&[(10., -10.), (13., -13.)]);
        }
    }
}
pub fn button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    button_size(ui, icon, tip, [30., 30.])
}
pub fn small_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    button_size(ui, icon, tip, [20., 24.])
}
fn button_size(ui: &mut egui::Ui, icon: Icon, tip: &str, size: [f32; 2]) -> egui::Response {
    let r = ui
        .add_sized(size, egui::Button::new("").frame(false))
        .on_hover_text(tip);
    let t = ui
        .ctx()
        .animate_bool_with_time(r.id.with("hover"), r.hovered(), 0.1);
    if t > 0.0 {
        ui.painter()
            .rect_filled(r.rect, 4, Color32::WHITE.gamma_multiply(0.07 * t));
    }
    paint(ui.painter(), r.rect.shrink(2.), icon, ui.is_enabled());
    r
}
