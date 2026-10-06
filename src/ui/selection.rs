use super::*;
pub(super) fn outline(ui: &egui::Ui, painter: &egui::Painter, rect: Rect) {
    polygon(
        ui,
        painter,
        &[
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
        ],
    );
}
pub(super) fn polygon(ui: &egui::Ui, painter: &egui::Painter, points: &[Pos2]) {
    if points.len() < 3 {
        return;
    }
    let phase = ui.input(|i| (i.time * 12.0 % 8.0) as f32);
    let clip = painter.clip_rect();
    let mut distance = 0.0;
    for (a, b) in points
        .iter()
        .copied()
        .zip(points.iter().copied().cycle().skip(1))
        .take(points.len())
    {
        let length = a.distance(b);
        if length < 0.1 {
            continue;
        }
        let direction = (b - a) / length;
        let mut lo = 0.0_f32;
        let mut hi = length;
        for (origin, delta, min, max) in [
            (a.x, direction.x, clip.left(), clip.right()),
            (a.y, direction.y, clip.top(), clip.bottom()),
        ] {
            if delta.abs() < 0.001 {
                if origin < min || origin > max {
                    hi = -1.0;
                }
            } else {
                let t0 = (min - origin) / delta;
                let t1 = (max - origin) / delta;
                lo = lo.max(t0.min(t1));
                hi = hi.min(t0.max(t1));
            }
        }
        if hi >= lo {
            painter.line_segment(
                [a + direction * lo, a + direction * hi],
                Stroke::new(1.5_f32, Color32::BLACK),
            );
            let offset = distance + phase;
            let mut t = ((lo + offset) / 8.0).floor() * 8.0 - offset;
            while t < hi {
                let from = t.max(lo);
                let to = (t + 4.0).min(hi);
                if to > from {
                    painter.line_segment(
                        [a + direction * from, a + direction * to],
                        Stroke::new(1.5_f32, Color32::WHITE),
                    );
                }
                t += 8.0;
            }
        }
        distance = (distance + length) % 8.0;
    }
    ui.ctx().request_repaint_after(Duration::from_millis(60));
}
