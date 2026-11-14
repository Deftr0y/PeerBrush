//! Small monochrome glyphs. Color belongs to state and artwork, not tool identity.
use eframe::egui::{self, Color32, Rect, Stroke, Vec2};
#[derive(Clone, Copy)]
pub enum Icon {
    Cursor,
    Text,
    AlignLeft,
    AlignCenter,
    AlignRight,
    WrapText,
    VectorPath,
    ClosedPath,
    CornerAnchor,
    SmoothAnchor,
    SelectionAdd,
    SelectionSubtract,
    SelectionIntersect,
    SelectionExpand,
    SelectionContract,
    ConnectedMatches,
    AllMatches,
    MirrorHorizontal,
    MirrorVertical,
    QuarterTurnLeft,
    QuarterTurnRight,
    LiquifyPush,
    LiquifyExpand,
    LiquifyPinch,
    Cutout,
    SelectionOverlay,
    Move,
    Rotate,
    Scale,
    Skew,
    Distort,
    Perspective,
    GridWarp,
    Snap,
    Brush,
    StrokeWidth,
    StrokeOpacity,
    Smudge,
    Clone,
    Heal,
    RetouchAligned,
    RetouchRestart,
    Liquify,
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
    CanvasBounds,
    TrimAlpha,
    SingleLayer,
    LayerStack,
    Check,
    Cross,
    Stop,
    Skip,
    NumberedList,
    Mask,
    Solo,
    Trash,
    Down,
    Left,
    Right,
    Up,
    Lock,
    Connect,
    Swap,
    Wand,
    Lasso,
    PolygonLasso,
    MagneticLasso,
    QuickSelection,
    RowSelection,
    ColumnSelection,
    ObjectSelection,
    EllipticalSelection,
    Levels,
    Curves,
    Blur,
    ColorBalance,
    Hue,
    Bloom,
    Invert,
    Grayscale,
    ToneSteps,
    ToneRamp,
    PreserveSamples,
    QuantizeSamples,
    FinePixels,
    CoarsePixels,
}
fn tool_cell(icon: Icon) -> Option<usize> {
    match icon {
        Icon::Cursor => Some(0),
        Icon::Move => Some(1),
        Icon::Rotate => Some(2),
        Icon::Scale => Some(3),
        Icon::Brush => Some(4),
        Icon::Smudge => Some(5),
        Icon::Clone => Some(6),
        Icon::Heal => Some(7),
        Icon::Liquify => Some(8),
        Icon::Eraser => Some(9),
        Icon::Fill => Some(10),
        Icon::Gradient => Some(11),
        Icon::Rectangle => Some(12),
        Icon::Ellipse => Some(13),
        Icon::Selection => Some(14),
        Icon::Picker => Some(15),
        Icon::Pan => Some(16),
        Icon::Wand => Some(17),
        _ => None,
    }
}
fn selection_cell(icon: Icon) -> Option<usize> {
    match icon {
        Icon::Lasso => Some(0),
        Icon::PolygonLasso => Some(1),
        Icon::MagneticLasso => Some(2),
        Icon::QuickSelection => Some(3),
        Icon::RowSelection => Some(4),
        Icon::ColumnSelection => Some(5),
        Icon::ObjectSelection => Some(6),
        Icon::EllipticalSelection => Some(7),
        _ => None,
    }
}
pub fn paint_tinted(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    if matches!(icon, Icon::Text) {
        let scale = rect.width().min(rect.height()) / 32.;
        for [a, b] in [
            [[-9., -10.], [9., -10.]],
            [[0., -10.], [0., 10.]],
            [[-4., 10.], [4., 10.]],
        ] {
            painter.line_segment(
                [a, b].map(|[x, y]| rect.center() + Vec2::new(x, y) * scale),
                Stroke::new(1.7 * scale, color),
            );
        }
    } else if let Some(cell) = tool_cell(icon) {
        paint_atlas(painter, rect, cell, false, color);
    } else if let Some(cell) = selection_cell(icon) {
        paint_atlas(painter, rect, cell, true, color);
    } else {
        paint(painter, rect, icon, true);
    }
}
pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, enabled: bool) {
    if tool_cell(icon).is_some() || selection_cell(icon).is_some() {
        paint_tinted(
            painter,
            rect,
            icon,
            Color32::WHITE.gamma_multiply(if enabled { 1.0 } else { 0.35 }),
        );
        return;
    }
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
        Icon::Text => paint_tinted(painter, rect, icon, color),
        Icon::WrapText => {
            line(&[(-11., -8.), (11., -8.)]);
            line(&[(-11., -2.), (7., -2.), (11., 1.), (11., 6.), (8., 9.), (-11., 9.)]);
            line(&[(-7., 5.), (-11., 9.), (-7., 13.)]);
        }
        Icon::Snap => {
            line(&[(-11., -10.), (-11., 2.), (-8., 8.), (-3., 11.), (3., 11.), (8., 8.), (11., 2.), (11., -10.)]);
            line(&[(-5., -10.), (-5., 1.), (-3., 4.), (3., 4.), (5., 1.), (5., -10.)]);
            line(&[(-11., -10.), (-5., -10.)]);
            line(&[(5., -10.), (11., -10.)]);
        }
        Icon::StrokeWidth => polygon(&[(-12., -1.), (12., -6.), (12., 6.), (-12., 1.)]),
        Icon::StrokeOpacity => {
            for (left, opacity) in [(-12., 0.25), (-6., 0.5), (0., 0.75), (6., 1.)] {
                painter.rect_filled(
                    Rect::from_min_max(point(left, -4.), point(left + 6., 4.)),
                    0.,
                    color.gamma_multiply(opacity),
                );
            }
        }
        Icon::RetouchAligned | Icon::RetouchRestart => {
            line(&[(-13., -7.), (-3., -7.)]);
            line(&[(-8., -12.), (-8., -2.)]);
            if matches!(icon, Icon::RetouchAligned) {
                line(&[(-3., -2.), (4., 5.)]);
                painter.circle_stroke(point(8., 9.), 4. * f, stroke);
            } else {
                line(&[(9., 10.), (13., 6.), (13., 2.), (10., -2.), (5., -4.), (-1., -4.)]);
                line(&[(3., -8.), (-1., -4.), (3., 0.)]);
            }
        }
        Icon::MirrorHorizontal | Icon::MirrorVertical => {
            let axis = |x, y| {
                if matches!(icon, Icon::MirrorHorizontal) { (x, y) } else { (y, x) }
            };
            line(&[axis(0., -13.), axis(0., 13.)]);
            polygon(&[axis(-4., -9.), axis(-4., 9.), axis(-12., 9.)]);
            line(&[axis(4., -9.), axis(12., 9.), axis(4., 9.), axis(4., -9.)]);
        }
        Icon::QuarterTurnLeft | Icon::QuarterTurnRight => {
            let direction = if matches!(icon, Icon::QuarterTurnLeft) { -1_f32 } else { 1_f32 };
            let reflect = |(x, y): (f32, f32)| (direction * x, y);
            line(&[(7., 7.), (3., 10.), (-3., 10.), (-8., 7.), (-11., 2.), (-10., -4.),
                (-6., -9.), (0., -11.), (6., -9.), (10., -5.)].map(reflect));
            line(&[(4., -5.), (10., -5.), (10., -11.)].map(reflect));
        }
        Icon::Skew | Icon::Distort | Icon::Perspective | Icon::GridWarp => {
            let corners = match icon {
                Icon::Skew => [(-6., -10.), (12., -10.), (6., 10.), (-12., 10.)],
                Icon::Distort => [(-9., -6.), (11., -12.), (8., 9.), (-12., 11.)],
                Icon::Perspective => [(-6., -10.), (6., -10.), (12., 10.), (-12., 10.)],
                _ => [(-11., -9.), (11., -9.), (11., 9.), (-11., 9.)],
            };
            let [a, b, c, d] = corners;
            if matches!(icon, Icon::GridWarp) {
                line(&[a, (0., -13.), b, (8., 0.), c, (0., 5.), d, (-14., 0.), a]);
                line(&[(-14., 0.), (0., -4.), (8., 0.)]);
                line(&[(0., -13.), (0., 5.)]);
            } else {
                line(&[a, b, c, d, a]);
                for (x, y) in corners {
                    painter.rect_filled(Rect::from_center_size(point(x, y), Vec2::splat(3.5 * f)), 0., color);
                }
            }
        }
        Icon::LiquifyPush => {
            painter.circle_filled(point(-7., 0.), 4. * f, color);
            line(&[(0., 0.), (12., 0.)]);
            line(&[(7., -5.), (12., 0.), (7., 5.)]);
        }
        Icon::LiquifyExpand | Icon::LiquifyPinch | Icon::SelectionExpand | Icon::SelectionContract => {
            let outward = matches!(icon, Icon::LiquifyExpand | Icon::SelectionExpand);
            if matches!(icon, Icon::SelectionExpand | Icon::SelectionContract) {
                outline((-7., -7.), (7., 7.));
            }
            for (x, y) in [(-1., 0.), (1., 0.), (0., -1.), (0., 1.)] {
                let (from, to) = if outward { (4., 12.) } else { (12., 4.) };
                let tail = (from * x, from * y);
                let head = (to * x, to * y);
                let base = to + if outward { -4. } else { 4. };
                line(&[tail, head]);
                line(&[(base * x - 3. * y, base * y + 3. * x), head,
                    (base * x + 3. * y, base * y - 3. * x)]);
            }
        }
        Icon::Cutout | Icon::SelectionOverlay => {
            if matches!(icon, Icon::SelectionOverlay) {
                painter.rect_filled(
                    Rect::from_min_max(point(-12., -12.), point(12., 12.)), 2., color.gamma_multiply(0.32),
                );
            } else {
                for (x, y) in [(-12., -12.), (6., -12.), (-12., 6.), (6., 6.)] {
                    painter.rect_filled(Rect::from_min_max(point(x, y), point(x + 6., y + 6.)), 0., color.gamma_multiply(0.5));
                }
            }
            painter.circle_filled(rect.center(), 7. * f, color);
        }
        Icon::VectorPath | Icon::ClosedPath => {
            let anchors = [(-10., 7.), (-2., -8.), (10., 3.)];
            line(&anchors);
            if matches!(icon, Icon::ClosedPath) {
                line(&[anchors[2], anchors[0]]);
            }
            for (x, y) in anchors {
                painter.rect_filled(Rect::from_center_size(point(x, y), Vec2::splat(4. * f)), 0., color);
            }
        }
        Icon::CornerAnchor | Icon::SmoothAnchor => {
            if matches!(icon, Icon::CornerAnchor) {
                line(&[(-12., 8.), (0., 0.), (10., 9.)]);
            } else {
                line(&[(-12., 8.), (-8., 6.), (-4., 2.), (0., 0.), (4., -2.), (8., -2.), (12., 0.)]);
                line(&[(-10., 5.), (10., -5.)]);
                for (x, y) in [(-10., 5.), (10., -5.)] {
                    painter.circle_filled(point(x, y), 1.7 * f, color);
                }
            }
            painter.rect_filled(Rect::from_center_size(point(0., 0.), Vec2::splat(5. * f)), 0., color);
        }
        Icon::SelectionAdd | Icon::SelectionSubtract | Icon::SelectionIntersect => {
            let fill = |(left, top), (right, bottom)| {
                painter.rect_filled(
                    Rect::from_min_max(point(left, top), point(right, bottom)),
                    0.,
                    color.gamma_multiply(0.5),
                );
            };
            match icon {
                Icon::SelectionAdd => {
                    fill((-12., -10.), (4., 5.));
                    fill((-4., 5.), (12., 10.));
                    fill((4., -5.), (12., 5.));
                }
                Icon::SelectionSubtract => {
                    fill((-12., -10.), (4., -5.));
                    fill((-12., -5.), (-4., 5.));
                }
                _ => {
                    fill((-4., -5.), (4., 5.));
                }
            }
            outline((-12., -10.), (4., 5.));
            outline((-4., -5.), (12., 10.));
        }
        Icon::ConnectedMatches | Icon::AllMatches => {
            let centers = if matches!(icon, Icon::ConnectedMatches) {
                [(-7., -4.), (0., -4.), (0., 3.), (7., 3.)]
            } else {
                [(-8., -8.), (8., -8.), (-8., 8.), (8., 8.)]
            };
            for (x, y) in centers {
                painter.rect_filled(
                    Rect::from_center_size(point(x, y), Vec2::splat(7. * f)),
                    0.,
                    color,
                );
            }
        }
        Icon::AlignLeft | Icon::AlignCenter | Icon::AlignRight => {
            for (y, width) in [(-8., 22.), (-2., 13.), (4., 19.), (10., 11.)] {
                let left = match icon {
                    Icon::AlignLeft => -11.,
                    Icon::AlignRight => 11. - width,
                    _ => -width * 0.5,
                };
                painter.line_segment([point(left, y), point(left + width, y)], stroke);
            }
        }
        Icon::Cursor => polygon(&[
            (-8., -11.),
            (-8., 8.),
            (-3., 4.),
            (1., 11.),
            (4., 9.),
            (0., 2.),
            (8., 1.),
        ]),
        Icon::Smudge => {
            line(&[
                (-10., 9.),
                (-5., 4.),
                (-4., -7.),
                (-1., -9.),
                (1., -7.),
                (1., 0.),
                (4., -5.),
                (7., -4.),
                (7., 3.),
                (3., 10.),
            ]);
            line(&[(-10., -1.), (-7., -3.)]);
            line(&[(9., 7.), (11., 9.)]);
        }
        Icon::Clone => {
            line(&[(-9., 9.), (-9., 4.), (9., 4.), (9., 9.), (-9., 9.)]);
            line(&[
                (-5., 4.),
                (-4., -1.),
                (-4., -8.),
                (4., -8.),
                (4., -1.),
                (5., 4.),
            ]);
            line(&[(-11., 12.), (11., 12.)]);
        }
        Icon::Heal => {
            line(&[(-10., -5.), (-5., -10.), (10., 5.), (5., 10.), (-10., -5.)]);
            line(&[(-3., -6.), (6., 3.)]);
            line(&[(-6., -3.), (3., 6.)]);
            painter.circle_filled(point(0., 0.), f, color);
        }
        Icon::Liquify => {
            for y in [-8., 0., 8.] {
                line(&[
                    (-11., y),
                    (-5., y - 3.),
                    (2., y + 3.),
                    (7., y + 1.),
                    (11., y - 2.),
                ]);
            }
        }
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
        Icon::Levels => {
            line(&[(-11., 10.), (11., 10.)]);
            for (x, h) in [(-8., 6.), (-4., 14.), (0., 20.), (4., 16.), (8., 8.)] {
                line(&[(x, 8.), (x, 8. - h)]);
            }
        }
        Icon::Curves => {
            line(&[(-11., -11.), (-11., 11.), (11., 11.)]);
            line(&[
                (-9., 8.),
                (-5., 7.),
                (-1., 3.),
                (2., -4.),
                (6., -8.),
                (10., -9.),
            ]);
        }
        Icon::Blur => {
            for (x, y, a) in [(-5., -3., 0.45), (5., -3., 0.6), (0., 5., 0.9)] {
                painter.circle_stroke(
                    point(x, y),
                    6.0 * f,
                    Stroke::new(1.7 * f, color.gamma_multiply(a)),
                );
            }
        }
        Icon::ColorBalance => {
            line(&[(0., -10.), (0., 11.)]);
            line(&[(-11., -5.), (11., -5.)]);
            line(&[(-5., 11.), (5., 11.)]);
            for x in [-8., 8.] {
                line(&[(x, -5.), (x - 4., 4.), (x + 4., 4.), (x, -5.)]);
            }
        }
        Icon::Hue => {
            painter.circle_stroke(rect.center(), 10. * f, stroke);
            for a in [0.0f32, 2.0944, 4.1888] {
                line(&[(0., 0.), (a.cos() * 10., a.sin() * 10.)]);
            }
        }
        Icon::Bloom => {
            painter.circle_stroke(rect.center(), 5. * f, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.;
                line(&[(a.cos() * 8., a.sin() * 8.), (a.cos() * 12., a.sin() * 12.)]);
            }
        }
        Icon::Invert => {
            painter.circle_stroke(rect.center(), 9. * f, stroke);
            polygon(&[(0., -9.), (0., 9.), (6., 6.), (9., 0.), (6., -6.)]);
            line(&[(-12., -10.), (-7., -13.), (-7., -8.)]);
            line(&[(12., 10.), (7., 13.), (7., 8.)]);
        }
        Icon::Grayscale => {
            for (x, c) in [(-7., 100), (0., 175), (7., 240)] {
                painter.rect_filled(
                    egui::Rect::from_min_max(point(x - 3., -10.), point(x + 3., 10.)),
                    0,
                    Color32::from_gray(c),
                );
            }
        }
        Icon::ToneSteps | Icon::ToneRamp => {
            let steps = if matches!(icon, Icon::ToneSteps) { 4 } else { 24 };
            for step in 0..steps {
                let left = -12. + 24. * step as f32 / steps as f32;
                let right = -12. + 24. * (step + 1) as f32 / steps as f32;
                let tone = 45 + 195 * step / (steps - 1);
                painter.rect_filled(
                    Rect::from_min_max(point(left, -6.), point(right, 6.)),
                    0.,
                    Color32::from_gray(tone as u8).gamma_multiply(if enabled { 1. } else { 0.4 }),
                );
            }
        }
        Icon::Frame => {
            for (x, y) in [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)] {
                line(&[(x * 5., y * 10.), (x * 10., y * 10.), (x * 10., y * 5.)]);
            }
        }
        Icon::FinePixels | Icon::CoarsePixels => {
            let cells = if matches!(icon, Icon::FinePixels) { 5 } else { 2 };
            let step = 24. / cells as f32;
            for y in 0..cells {
                for x in 0..cells {
                    let start = point(-12. + x as f32 * step, -12. + y as f32 * step);
                    let shade = if x + y < cells - 1 { 100 } else { 225 };
                    let fill = Color32::from_gray(shade).gamma_multiply(if enabled { 1. } else { 0.4 });
                    painter.rect_filled(Rect::from_min_size(start, Vec2::splat((step - 1.5) * f)), 0., fill);
                }
            }
        }
        Icon::PreserveSamples | Icon::QuantizeSamples => {
            let exact = matches!(icon, Icon::PreserveSamples);
            for (i, tone) in [45, 83, 120, 159, 198, 238].into_iter().enumerate() {
                let x = i as f32 * 4. - 12.;
                let gray = if exact { tone } else if i < 3 { 83 } else { 198 };
                painter.rect_filled(
                    Rect::from_min_max(point(x, -8.), point(x + 4., 8.)),
                    0.,
                    Color32::from_gray(gray).gamma_multiply(if enabled { 1. } else { 0.4 }),
                );
            }
        }
        Icon::CanvasBounds | Icon::TrimAlpha => {
            painter.circle_filled(rect.center(), 5. * f, color);
            let extent = if matches!(icon, Icon::TrimAlpha) { 6. } else { 12. };
            outline((-extent, -extent), (extent, extent));
        }
        Icon::SingleLayer | Icon::LayerStack => {
            let y = if matches!(icon, Icon::SingleLayer) { 0. } else { -5. };
            line(&[(-11., y), (0., y - 6.), (11., y), (0., y + 6.), (-11., y)]);
            if matches!(icon, Icon::LayerStack) {
                for y in [0., 5.] {
                    line(&[(-11., y), (0., y + 6.), (11., y)]);
                }
            }
        }
        Icon::Check => line(&[(-10., 0.), (-3., 7.), (10., -8.)]),
        Icon::Cross => {
            line(&[(-8., -8.), (8., 8.)]);
            line(&[(-8., 8.), (8., -8.)]);
        }
        Icon::Stop => {
            line(&[(-5., -11.), (5., -11.), (11., -5.), (11., 5.), (5., 11.),
                (-5., 11.), (-11., 5.), (-11., -5.), (-5., -11.)]);
            line(&[(-5., 0.), (5., 0.)]);
        }
        Icon::Skip => {
            polygon(&[(-9., -8.), (3., 0.), (-9., 8.)]);
            line(&[(8., -8.), (8., 8.)]);
        }
        Icon::NumberedList => {
            line(&[(-11., -8.), (-8., -11.), (-8., -2.)]);
            line(&[(-11., 4.), (-9., 2.), (-6., 3.), (-6., 5.), (-11., 10.), (-6., 10.)]);
            for y in [-6., 6.] {
                line(&[(0., y), (11., y)]);
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
        Icon::Left => line(&[(3., -6.), (-3., 0.), (3., 6.)]),
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
        Icon::Lasso
        | Icon::PolygonLasso
        | Icon::MagneticLasso
        | Icon::QuickSelection
        | Icon::RowSelection
        | Icon::ColumnSelection
        | Icon::ObjectSelection
        | Icon::EllipticalSelection => unreachable!("Selection glyphs use the bundled atlas"),
    }
}
const ATLAS_CELL: usize = 128;
const TOOL_ATLAS: &[u8; 6 * 3 * ATLAS_CELL * ATLAS_CELL] =
    include_bytes!(concat!(env!("OUT_DIR"), "/tool-icons.alpha"));
const SELECTION_ATLAS: &[u8; 4 * 2 * ATLAS_CELL * ATLAS_CELL] =
    include_bytes!(concat!(env!("OUT_DIR"), "/selection-icons.alpha"));

fn atlas_image(coverage: &[u8], columns: usize, rows: usize) -> egui::ColorImage {
    let palette: [Color32; 256] = std::array::from_fn(|alpha| Color32::from_white_alpha(alpha as u8));
    egui::ColorImage {
        size: [columns * ATLAS_CELL, rows * ATLAS_CELL],
        pixels: coverage.iter().map(|&alpha| palette[usize::from(alpha)]).collect(),
    }
}
fn paint_atlas(painter: &egui::Painter, rect: Rect, cell: usize, selection: bool, color: Color32) {
    let (name, bytes, columns, rows) = if selection {
        ("PeerBrush bold selections", SELECTION_ATLAS.as_slice(), 4, 2)
    } else {
        ("PeerBrush bold tools", TOOL_ATLAS.as_slice(), 6, 3)
    };
    let id = egui::Id::new(name);
    let texture = if let Some(t) = painter
        .ctx()
        .data(|d| d.get_temp::<egui::TextureHandle>(id))
    {
        t
    } else {
        let t = painter
            .ctx()
            .load_texture(name, atlas_image(bytes, columns, rows), egui::TextureOptions::LINEAR);
        painter.ctx().data_mut(|d| d.insert_temp(id, t.clone()));
        t
    };
    let col = (cell % columns) as f32;
    let row = (cell / columns) as f32;
    let (columns, rows) = (columns as f32, rows as f32);
    // Keep equal square cells and original padding: wide buttons must not stretch glyphs.
    let uv = Rect::from_min_max(
        egui::pos2(col / columns, row / rows),
        egui::pos2((col + 1.0) / columns, (row + 1.0) / rows),
    );
    painter.image(
        texture.id(),
        Rect::from_center_size(rect.center(), Vec2::splat(rect.width().min(rect.height()))),
        uv,
        color,
    );
}
pub fn button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    button_size(ui, icon, tip, [30., 30.])
}
pub fn small_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    button_size(ui, icon, tip, [20., 24.])
}
pub(crate) fn menu_choice(ui: &mut egui::Ui, icon: Icon, label: &str, selection: Option<bool>) -> egui::Response {
    labeled_choice(ui, icon, label, selection, ui.available_width())
}

pub(crate) fn labeled_choice(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selection: Option<bool>,
    min_width: f32,
) -> egui::Response {
    let selected = selection.unwrap_or(false);
    let text = ui.painter().layout_no_wrap(
        label.into(),
        egui::TextStyle::Button.resolve(ui.style()),
        Color32::PLACEHOLDER,
    );
    let response = ui.add_sized(
        [min_width.max(text.size().x + 36.), 26.],
        egui::Button::new("").selected(selected).frame(false),
    ).on_hover_text(label);
    response.widget_info(|| match selection {
        Some(selected) => egui::WidgetInfo::selected(
            egui::WidgetType::RadioButton, response.enabled(), selected, label,
        ),
        None => egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), label),
    });
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() {
        ui.painter().rect_filled(response.rect, 3., visuals.weak_bg_fill);
    }
    let start = response.rect.left_center();
    let glyph = Rect::from_center_size(start + Vec2::new(15., 0.), Vec2::splat(22.));
    paint(ui.painter(), glyph, icon, response.enabled());
    let text_pos = start + Vec2::new(30., -text.size().y / 2.);
    ui.painter().galley(text_pos, text, visuals.text_color());
    paint_focus(ui, &response);
    response
}
pub fn selectable(ui: &mut egui::Ui, icon: Icon, selected: bool, label: &str) -> egui::Response {
    let response = choice(ui, icon, selected, label);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, response.enabled(), selected, label)
    });
    response
}
pub fn toggle(ui: &mut egui::Ui, icon: Icon, value: &mut bool, label: &str) -> egui::Response {
    let mut response = choice(ui, icon, *value, label);
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, response.enabled(), *value, label)
    });
    response
}
fn choice(ui: &mut egui::Ui, icon: Icon, selected: bool, label: &str) -> egui::Response {
    let response = ui
        .add_sized([26., 24.], egui::Button::new("").selected(selected))
        .on_hover_text(label);
    paint(ui.painter(), response.rect.shrink(3.), icon, response.enabled());
    paint_focus(ui, &response);
    response
}

pub(crate) fn paint_focus(ui: &egui::Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(response.rect.shrink(1.), 2.,
            Stroke::new(1., ui.visuals().text_color()), egui::StrokeKind::Inside);
    }
}

fn button_size(ui: &mut egui::Ui, icon: Icon, tip: &str, size: [f32; 2]) -> egui::Response {
    let r = ui
        .add_sized(size, egui::Button::new("").frame(false))
        .on_hover_text(tip);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tip));
    let t = ui
        .ctx()
        .animate_bool_with_time(r.id.with("hover"), r.hovered(), 0.1);
    if t > 0.0 {
        ui.painter()
            .rect_filled(r.rect, 4, Color32::WHITE.gamma_multiply(0.07 * t));
    }
    paint(ui.painter(), r.rect.shrink(2.), icon, ui.is_enabled());
    paint_focus(ui, &r);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_coverage_has_clear_background_and_filled_glyphs_in_every_cell() {
        for (bytes, columns, rows) in [(TOOL_ATLAS.as_slice(), 6, 3), (SELECTION_ATLAS.as_slice(), 4, 2)] {
            let image = atlas_image(bytes, columns, rows);
            let [width, height] = image.size;
            assert_eq!(width * rows, height * columns, "Square atlas cells");
            for row in 0..rows {
                for column in 0..columns {
                    let mut filled = 0;
                    let mut clear = 0;
                    for y in row * height / rows..(row + 1) * height / rows {
                        for x in column * width / columns..(column + 1) * width / columns {
                            let alpha = image.pixels[y * width + x].a();
                            filled += usize::from(alpha > 240);
                            clear += usize::from(alpha == 0);
                        }
                    }
                    let area = width * height / (columns * rows);
                    assert!(
                        filled > area / 30,
                        "Missing or faint glyph at {column}, {row}"
                    );
                    assert!(
                        clear > area / 2,
                        "Background must disappear at {column}, {row}"
                    );
                }
            }
        }
    }

    #[test]
    fn state_tint_preserves_square_glyphs_and_reuses_the_neutral_texture() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(egui::pos2(10., 10.), Vec2::new(42., 39.));
        let mut texture = None;
        for (index, color) in [
            Color32::WHITE,
            Color32::from_rgb(233, 84, 32),
            Color32::from_rgb(90, 170, 255),
            Color32::from_gray(100),
        ]
        .into_iter()
        .enumerate()
        {
            ctx.begin_pass(Default::default());
            let painter = ctx.layer_painter(egui::LayerId::background());
            paint_tinted(&painter, rect, Icon::Heal, color);
            let output = ctx.end_pass();
            let egui::Shape::Mesh(mesh) = &output.shapes[0].shape else {
                panic!("Raster glyph mesh");
            };
            assert!(mesh.vertices.iter().all(|v| v.color == color));
            assert_eq!(mesh.calc_bounds().size(), Vec2::splat(39.));
            if let Some(id) = texture {
                assert_eq!(mesh.texture_id, id);
                assert!(
                    output.textures_delta.set.is_empty(),
                    "Tint must not rebuild artwork"
                );
            } else {
                texture = Some(mesh.texture_id);
                assert!(!output.textures_delta.set.is_empty());
            }
            assert_eq!(
                index == 0,
                output
                    .textures_delta
                    .set
                    .iter()
                    .any(|(id, _)| Some(*id) == texture)
            );
        }
    }
}
