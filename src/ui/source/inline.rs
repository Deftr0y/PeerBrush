use super::*;
use crate::source::text_position::CaretMove;
use egui::text::{CCursor, CCursorRange};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy)]
enum Click {
    Caret,
    Word,
    Line,
}

pub(super) struct ClickSequence {
    first: Pos2,
    time: f64,
    click: Click,
}

pub(super) struct Point {
    document: [f32; 2],
    click: Click,
    end: [f32; 2],
    anchor: Option<usize>,
    held: bool,
}

impl Point {
    fn press(document: [f32; 2], anchor: Option<usize>) -> Self {
        Self { document, click: Click::Caret, end: document, anchor, held: true }
    }

    fn click(document: [f32; 2], click: Click) -> Self {
        Self { click, held: false, ..Self::press(document, None) }
    }

    fn resolve(
        &self,
        text: &str,
        carets: &TextCarets,
        local: impl Fn([f32; 2]) -> [f32; 2],
    ) -> (CCursorRange, Caret) {
        let caret = carets.nearest(local(self.end));
        let selection = match self.click {
            Click::Caret => {
                let anchor = self.anchor.unwrap_or_else(|| carets.nearest(local(self.document)).index);
                return (CCursorRange::two(CCursor::new(anchor), CCursor::new(caret.index)), caret);
            }
            Click::Word => word_at(text, caret),
            Click::Line => carets.visual_line(caret),
        };
        if let Some(selection) = selection.filter(|selection| !selection.is_empty()) {
            let end = Caret { index: selection.end, trailing: true };
            (CCursorRange::two(CCursor::new(selection.start), CCursor::new(end.index)), end)
        } else {
            (CCursorRange::one(CCursor::new(caret.index)), caret)
        }
    }
}

fn word_at(text: &str, caret: Caret) -> Option<Range<usize>> {
    let count = text.chars().count();
    let index = caret.index
        .saturating_sub(usize::from(caret.trailing))
        .min(count.checked_sub(1)?);
    let mut start = 0;
    for segment in text.split_word_bounds() {
        let end = start + segment.chars().count();
        if index < end {
            return Some(start..end);
        }
        start = end;
    }
    None
}

pub(super) enum Layout {
    Native(NativeCarets),
    Resident(TextCarets),
}

impl Layout {
    fn map(&self) -> &TextCarets {
        match self {
            Self::Native(layout) => &layout.map,
            Self::Resident(layout) => layout,
        }
    }
}

pub(super) fn edit(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    editor: &mut Editor,
    projection: &view::Projection,
    bounds: Rect,
    resize: &[Rect],
    id: egui::Id,
) -> bool {
    let worker_layout = editor.needs_preview_carets();
    let mut received = if worker_layout {
        if editor.error.is_some() {
            return false;
        }
        match editor.layout.as_ref().filter(|(key, _)| editor.layout_key() == Some(*key)) {
            Some((_, Some(layout))) => Some(layout.clone()),
            Some((_, None)) => return false,
            None => None,
        }
    } else {
        None
    };
    let Ok(_admission) = editor.native.as_ref().filter(|_| !worker_layout).map(|native| {
        let bytes = TextCarets::ADMISSION_BYTES;
        native.pool().reserve_bytes(bytes, bytes)
    }).transpose() else {
        return false;
    };
    let mut static_layout = if worker_layout {
        None
    } else {
        let Ok(Some(map)) = TextCarets::bundled_legacy(&editor.source) else {
            return false;
        };
        Some(map)
    };
    let carets = static_layout.as_ref().or_else(|| received.as_deref().map(Layout::map));
    let [a, b, c, d, tx, ty] = editor.source.matrix;
    let [x, y] = [editor.x as f32 + tx, editor.y as f32 + ty];
    let determinant = a * d - b * c;
    if determinant.abs() < 1e-6 {
        return false;
    }
    let field = [editor.source.width as f32, editor.source.height as f32];
    let local = |[px, py]: [f32; 2]| {
        let (px, py) = (px - x, py - y);
        [(d * px - c * py) / determinant, (a * py - b * px) / determinant]
    };
    let screen = |[u, v]: [f32; 2]| projection.point([x + a * u + c * v, y + b * u + d * v]);
    let ctx = ui.ctx().clone();
    let hit = |pos: Pos2| {
        let [u, v] = local(projection.document(pos));
        painter.clip_rect().contains(pos)
            && !resize.iter().any(|handle| handle.contains(pos))
            && (0. ..=field[0]).contains(&u)
            && (0. ..=field[1]).contains(&v)
            && ctx.layer_id_at(pos) == Some(ui.layer_id())
    };
    let prior_range = egui::TextEdit::load_state(&ctx, id)
        .and_then(|state| state.cursor.char_range());
    let focus = editor.canvas_focus;
    if focus == Some(CanvasFocus::ResumeEditing) && prior_range.is_some() {
        ctx.memory_mut(|memory| memory.request_focus(id));
    }
    let Content::Text { text, .. } = &editor.source.content else { return false };
    let mut range = None;
    if focus.is_none() || (focus == Some(CanvasFocus::StartEditing) && editor.layer.is_some()) {
        let (click_distance, click_delay) = ctx.options(|options| {
            (options.input_options.max_click_dist, options.input_options.max_double_click_delay)
        });
        let (events, clicked, down, time) = ui.input(|input| {
            let events: Vec<_> = input.events.iter().filter(|event| matches!(event,
                egui::Event::PointerMoved(_) | egui::Event::PointerGone
                | egui::Event::PointerButton { button: egui::PointerButton::Primary, .. }
            )).cloned().collect();
            (events, input.pointer.button_clicked(egui::PointerButton::Primary), input.pointer.primary_down(), input.time)
        });
        // Query layers after releasing egui's input lock.
        let last_release = events.iter().rposition(|event| matches!(event,
            egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: false, .. }));
        for (event_index, event) in events.iter().enumerate() {
            match event {
                egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed: true, modifiers,
                } if !modifiers.alt && hit(*pos) => {
                    let point = projection.document(*pos);
                    let anchor = prior_range.filter(|_| modifiers.shift).map(|range| range.secondary.index);
                    let Some(carets) = carets else {
                        editor.inline_point = Some(Point::press(point, anchor));
                        continue;
                    };
                    editor.inline_point = None;
                    let caret = carets.nearest(local(point));
                    let index = caret.index;
                    editor.inline_affinity = Some(caret);
                    let anchor = anchor.unwrap_or(index);
                    editor.inline_anchor = Some(anchor);
                    range = Some(CCursorRange::two(CCursor::new(anchor), CCursor::new(index)));
                }
                egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed: false, modifiers,
                } if Some(event_index) == last_release
                    && clicked
                    && (editor.inline_anchor.is_some() || editor.inline_point.as_ref().is_some_and(|point| point.held))
                    && !modifiers.shift && !modifiers.alt && hit(*pos) =>
                {
                    let mut sequence = editor.inline_click.take().filter(|prior| {
                        time - prior.time <= click_delay
                            && prior.first.distance(*pos) <= click_distance
                            && !matches!(prior.click, Click::Line)
                    }).map_or(ClickSequence { first: *pos, time, click: Click::Caret }, |prior| {
                        ClickSequence {
                            click: match prior.click { Click::Caret => Click::Word, _ => Click::Line },
                            ..prior
                        }
                    });
                    sequence.time = time;
                    let click = sequence.click;
                    editor.inline_click = Some(sequence);
                    if !matches!(click, Click::Caret) {
                        editor.inline_point = Some(Point::click(projection.document(*pos), click));
                    } else if let Some(point) = &mut editor.inline_point {
                        point.end = point.document;
                        point.held = false;
                    }
                    editor.inline_anchor = None;
                }
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary, pressed: true, ..
                } => {
                    editor.inline_anchor = None;
                    editor.inline_point = None;
                    editor.inline_click = None;
                }
                egui::Event::PointerMoved(pos)
                | egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed: false, ..
                } => {
                    let released = matches!(event, egui::Event::PointerButton { .. });
                    if let Some(point) = editor.inline_point.as_mut().filter(|point| point.held) {
                        point.end = projection.document(*pos);
                        point.held = !released;
                    }
                    if let Some(anchor) = editor.inline_anchor {
                        let position = projection.document(*pos);
                        if let Some(carets) = carets {
                            let caret = carets.nearest(local(position));
                            editor.inline_affinity = Some(caret);
                            range = Some(CCursorRange::two(CCursor::new(anchor), CCursor::new(caret.index)));
                        } else if editor.inline_point.is_none() {
                            editor.inline_point = Some(Point { held: !released, ..Point::press(position, Some(anchor)) });
                        }
                    }
                    if released {
                        editor.inline_anchor = None;
                        editor.inline_click = None;
                    }
                }
                egui::Event::PointerGone => {
                    if editor.inline_point.as_ref().is_some_and(|point| point.held) {
                        editor.inline_point = None;
                    }
                    editor.inline_anchor = None;
                    editor.inline_click = None;
                }
                _ => {}
            }
        }
        if !down {
            editor.inline_anchor = None;
        }
    }
    if let Some(carets) = carets {
        if let Some(point) = editor.inline_point.take() {
            let (selection, caret) = point.resolve(text, carets, local);
            editor.inline_affinity = Some(caret);
            if point.held && ui.input(|input| input.pointer.primary_down()) {
                editor.inline_anchor = Some(selection.secondary.index);
            }
            range = Some(selection);
        }
    }
    if ctx.pointer_interact_pos().is_some_and(hit) {
        ctx.set_cursor_icon(egui::CursorIcon::Text);
        ctx.output_mut(|out| out.mutable_text_under_cursor = true);
    }
    let keyboard_range = prior_range.filter(|_| range.is_none() && ctx.memory(|memory| memory.has_focus(id)));
    if let (Some(carets), Some(prior_range)) = (carets, keyboard_range) {
        if let Some((range, caret)) = keyboard(
            ui, carets, prior_range, editor.inline_affinity, &mut editor.inline_column,
        ) {
            let mut state = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
            state.cursor.set_char_range(Some(range));
            egui::TextEdit::store_state(&ctx, id, state);
            editor.inline_affinity = Some(caret);
        }
    } else {
        editor.inline_column = None;
    }
    let Content::Text { text, .. } = &mut editor.source.content else { return false };
    if focus == Some(CanvasFocus::StartEditing) && editor.layer.is_none() {
        range = Some(CCursorRange::two(CCursor::new(0), CCursor::new(text.chars().count())));
    }
    let mut input_ui = ui.new_child(egui::UiBuilder::new()
        .id_salt(("inline text input", editor.token.nonce()))
        .max_rect(Rect::from_min_size(bounds.min, Vec2::splat(24.))));
    input_ui.set_clip_rect(Rect::NOTHING);
    let input = widgets::canvas_body(&mut input_ui, text, editor.native.as_mut(), id, None, range);
    editor.canvas_input = Some(input.id);
    input.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Canvas text"));
    if let Some(state) = egui::TextEdit::load_state(&ctx, id).filter(|_| input.id == id) {
        if focus.is_some() {
            editor.canvas_focus = None;
            input.request_focus();
        }
        if focus.is_some() || range.is_some() {
            ctx.request_repaint();
        }
        if input.changed() {
            editor.text_changed(&ctx);
            if worker_layout {
                received = None;
            } else {
                drop(static_layout.take());
                static_layout = TextCarets::bundled_legacy(&editor.source).ok().flatten();
            }
        }
        if ctx.memory(|memory| memory.has_focus(id)) && ui.input(|input| input.focused) {
            if let (Some(range), Some(carets)) = (
                state.cursor.char_range(),
                static_layout.as_ref().or_else(|| received.as_deref().map(Layout::map)),
            ) {
                let index = range.primary.index;
                let caret = editor.inline_affinity.filter(|caret| caret.index == index)
                    .unwrap_or(Caret { index, trailing: false });
                draw(painter, &ctx, ui.layer_id(), carets, range, caret, field, bounds, screen);
            } else {
                // IME still needs a visible anchor during the next preview.
                let to_global = ctx.layer_transform_to_global(ui.layer_id()).unwrap_or_default();
                let point = to_global * bounds.min;
                ctx.output_mut(|out| {
                    out.ime = Some(egui::output::IMEOutput {
                        rect: to_global * bounds,
                        cursor_rect: Rect::from_min_size(point, Vec2::new(1., 12.)),
                    });
                });
            }
        }
    }
    true
}

fn keyboard(
    ui: &egui::Ui,
    carets: &TextCarets,
    mut range: CCursorRange,
    affinity: Option<Caret>,
    column: &mut Option<f32>,
) -> Option<(CCursorRange, Caret)> {
    fn navigation(event: &egui::Event) -> Option<(CaretMove, bool)> {
        let egui::Event::Key { key, modifiers, pressed: true, .. } = event else { return None };
        if modifiers.ctrl || modifiers.alt {
            return None;
        }
        let motion = match (key, modifiers.command, modifiers.mac_cmd) {
            (egui::Key::ArrowLeft, _, true) | (egui::Key::Home, false, false) => CaretMove::LineStart,
            (egui::Key::ArrowRight, _, true) | (egui::Key::End, false, false) => CaretMove::LineEnd,
            (egui::Key::ArrowLeft, false, false) => CaretMove::Left,
            (egui::Key::ArrowRight, false, false) => CaretMove::Right,
            (egui::Key::ArrowUp, false, false) => CaretMove::Up,
            (egui::Key::ArrowDown, false, false) => CaretMove::Down,
            _ => return None,
        };
        Some((motion, modifiers.shift))
    }
    ui.input_mut(|input| {
        // Keep stock text/IME handling in order when other editing events are present.
        if input.events.iter().any(|event| matches!(event,
            egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Ime(_)
            | egui::Event::Cut | egui::Event::Copy | egui::Event::PointerButton { .. }
        ) || matches!(event, egui::Event::Key { pressed: true, .. }) && navigation(event).is_none()) {
            *column = None;
            return None;
        }
        let mut current = affinity.filter(|caret| caret.index == range.primary.index)
            .unwrap_or(Caret { index: range.primary.index, trailing: false });
        let mut moved = false;
        input.events.retain(|event| {
            let Some((motion, extend)) = navigation(event) else { return true };
            let collapse = !extend && range.primary.index != range.secondary.index
                && matches!(motion, CaretMove::Left | CaretMove::Right);
            current = if collapse {
                *column = None;
                let other = Caret { index: range.secondary.index, trailing: false };
                let order = |caret| carets.at(caret).map(|(x, [top, _])| (top, x));
                match (order(current), order(other)) {
                    (Some((ay, ax)), Some((by, bx))) => {
                        let before = ay.total_cmp(&by).then_with(|| ax.total_cmp(&bx)).is_lt();
                        if before == matches!(motion, CaretMove::Left) { current } else { other }
                    }
                    _ => current,
                }
            } else {
                carets.step(current, motion, column)
            };
            range.primary = CCursor::new(current.index);
            if !extend {
                range.secondary = range.primary;
            }
            moved = true;
            false
        });
        moved.then_some((range, current))
    })
}

fn draw(
    painter: &egui::Painter,
    ctx: &egui::Context,
    layer: egui::LayerId,
    carets: &TextCarets,
    range: CCursorRange,
    caret: Caret,
    [width, height]: [f32; 2],
    bounds: Rect,
    screen: impl Fn([f32; 2]) -> Pos2,
) {
    let [first, last] = range.sorted();
    for [left, top, right, bottom] in carets.highlights(first.index..last.index) {
        let (left, right) = (left.max(0.), right.min(width));
        let (top, bottom) = (top.max(0.), bottom.min(height));
        if left < right && top < bottom {
            painter.add(egui::Shape::convex_polygon(
                [[left, top], [right, top], [right, bottom], [left, bottom]].map(&screen).to_vec(),
                ACCENT.gamma_multiply(0.28),
                Stroke::NONE,
            ));
        }
    }
    if let Some((x, [top, bottom])) = carets.at(caret) {
        let x = x.clamp(0., width);
        let top = top.clamp(0., height);
        let bottom = bottom.clamp(0., height);
        let cursor = [[x, top], [x, bottom]].map(screen);
        if top < bottom {
            painter.line_segment(cursor, Stroke::new(3., Color32::BLACK));
            painter.line_segment(cursor, Stroke::new(1.25, Color32::WHITE));
        }
        let to_global = ctx.layer_transform_to_global(layer).unwrap_or_default();
        ctx.output_mut(|out| {
            out.ime = Some(egui::output::IMEOutput {
                rect: to_global * bounds,
                cursor_rect: to_global * Rect::from_two_pos(cursor[0], cursor[1]).expand(1.),
            });
        });
    }
}
