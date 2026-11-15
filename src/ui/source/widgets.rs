//! Source-only numeric text input. Stock DragValue still performs pointer
//! dragging/shift precision; its unbounded focused String branch is excluded.
use super::{buffer::Fixed, native::Native, text_state::TextState};
use crate::large_document::{Reservation, ResidentBudget};
use eframe::egui::{self, emath::Numeric};
use std::ops::{Range, RangeInclusive};
use unicode_segmentation::UnicodeSegmentation;

struct CanvasText<'a>(&'a mut dyn egui::TextBuffer);

impl CanvasText<'_> {
    fn delete_grapheme(
        &mut self, cursor: egui::text::CCursor, backward: bool,
    ) -> egui::text::CCursor {
        let mut start = 0;
        for grapheme in self.0.as_str().graphemes(true) {
            let end = start + grapheme.chars().count();
            if (backward && start < cursor.index && cursor.index <= end)
                || (!backward && start <= cursor.index && cursor.index < end)
            {
                self.0.delete_char_range(start..end);
                return egui::text::CCursor { index: start, prefer_next_row: true };
            }
            start = end;
        }
        cursor
    }
}

impl egui::TextBuffer for CanvasText<'_> {
    fn is_mutable(&self) -> bool {
        self.0.is_mutable()
    }
    fn as_str(&self) -> &str {
        self.0.as_str()
    }
    fn insert_text(&mut self, text: &str, index: usize) -> usize {
        self.0.insert_text(text, index)
    }
    fn delete_char_range(&mut self, range: Range<usize>) {
        self.0.delete_char_range(range);
    }
    fn replace_with(&mut self, text: &str) {
        self.0.replace_with(text);
    }

    fn delete_previous_char(&mut self, cursor: egui::text::CCursor) -> egui::text::CCursor {
        self.delete_grapheme(cursor, true)
    }
    fn delete_next_char(&mut self, cursor: egui::text::CCursor) -> egui::text::CCursor {
        self.delete_grapheme(cursor, false)
    }
}
struct Number {
    id: egui::Id,
    text: String,
    editing: bool,
}
pub(super) struct Widgets {
    pub states: TextState,
    numbers: Vec<Number>,
}
impl Widgets {
    pub fn new(context: &egui::Context, pool: &ResidentBudget) -> Result<Self, String> {
        let states = TextState::new(context, pool)?;
        let mut numbers = Vec::new();
        numbers
            .try_reserve_exact(512)
            .map_err(|_| "Source numeric field allocation refused")?;
        if numbers.capacity() > 512 {
            return Err("Source numeric metadata capacity exceeded admission".into());
        }
        Ok(Self { states, numbers })
    }
    fn number<N: Numeric>(
        &mut self,
        ui: &mut egui::Ui,
        value: &mut N,
        limits: RangeInclusive<N>,
        pool: &ResidentBudget,
        owner: &Reservation,
    ) -> Result<egui::Response, String> {
        let id = ui.next_auto_id();
        self.states.prepare(id, 64)?;
        let index = if let Some(index) = self.numbers.iter().position(|field| field.id == id) {
            index
        } else {
            if self.numbers.len() == 512 {
                return Err("Source numeric field count exceeded admission".into());
            }
            let mut text = String::new();
            text.try_reserve_exact(64)
                .map_err(|_| "Source numeric text allocation refused")?;
            if text.capacity() > 64 {
                return Err("Source numeric text capacity exceeded admission".into());
            }
            self.numbers.push(Number {
                id,
                text,
                editing: false,
            });
            self.numbers.len() - 1
        };
        let focused = ui.memory_mut(|memory| {
            memory.interested_in_focus(id, ui.layer_id());
            memory.has_focus(id)
        });
        let field = &mut self.numbers[index];
        if !focused {
            field.editing = false;
            // This is precisely the nonfocused branch, including pointer drag,
            // smart aim, Shift and keyboard focus registration.
            let response = ui.add(crate::controls::drag_value(value, ui.is_enabled()).range(limits));
            self.states.observe(id)?;
            return Ok(response);
        }
        let old = value.to_f64();
        let speed: f64 = if N::INTEGRAL { 0.25 } else { 1. };
        guard_events(ui, 64)?;
        let decimals = if N::INTEGRAL {
            0
        } else {
            ui.input(|input| {
                (f64::from(input.aim_radius()) / speed)
                    .log10()
                    .ceil()
                    .clamp(0., 15.) as usize
            })
        };
        let max = if N::INTEGRAL { 0 } else { decimals + 2 };
        let change = ui.input_mut(|input| {
            input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) as f64
                - input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) as f64
        });
        let range = limits.start().to_f64()..=limits.end().to_f64();
        let mut current = clamp(old, range.clone());
        if change != 0. {
            current = egui::emath::round_to_decimals(current + speed * change, decimals);
        }
        if !field.editing || current != old {
            let formatted = ui.style().number_formatter.format(current, decimals..=max);
            if formatted.len() > 64 {
                return Err("Source numeric formatter exceeded its bounded entry".into());
            }
            field.text.clear();
            field.text.push_str(&formatted);
            field.editing = true;
        }
        *value = N::from_f64(current);
        let before = field.text.clone();
        let mut refused = false;
        let mut buffer = Fixed::new(&mut field.text, 64, pool, owner, &mut refused)?;
        let mut response = ui.add(
            egui::TextEdit::singleline(&mut buffer)
                .clip_text(false)
                .horizontal_align(ui.layout().horizontal_align())
                .vertical_align(ui.layout().vertical_align())
                .margin(ui.spacing().button_padding)
                .min_size(ui.spacing().interact_size)
                .id(id)
                .desired_width(ui.spacing().interact_size.x)
                .font(ui.style().drag_value_text_style.clone()),
        );
        if refused {
            field.text.clear();
            field.text.push_str(&before);
            *value = N::from_f64(old);
        } else if response.changed() {
            if let Some(parsed) = parse(&field.text) {
                *value = N::from_f64(clamp(parsed, range));
            }
        }
        if value.to_f64() != old {
            response.mark_changed();
        }
        response.widget_info(|| egui::WidgetInfo::drag_value(ui.is_enabled(), value.to_f64()));
        self.states.observe(id)?;
        if refused {
            response = response.on_hover_text("Numeric entry is limited to64 UTF-8 bytes");
        }
        Ok(response)
    }
}
fn clamp(value: f64, range: RangeInclusive<f64>) -> f64 {
    let (mut low, mut high) = (*range.start(), *range.end());
    if low.total_cmp(&high).is_gt() {
        std::mem::swap(&mut low, &mut high);
    }
    if value.total_cmp(&low).is_lt() {
        low
    } else if value.total_cmp(&high).is_gt() {
        high
    } else {
        value
    }
}
fn parse(text: &str) -> Option<f64> {
    // Match pinned egui's parser, including whitespace and the Unicode minus.
    let mut bytes = [0u8; 64];
    let mut count = 0;
    for character in text.chars().filter(|c| !c.is_whitespace()) {
        let character = if character == '\u{2212}' {
            '-'
        } else {
            character
        };
        let mut encoded = [0u8; 4];
        let value = character.encode_utf8(&mut encoded).as_bytes();
        if count + value.len() > 64 {
            return None;
        }
        bytes[count..count + value.len()].copy_from_slice(value);
        count += value.len();
    }
    std::str::from_utf8(&bytes[..count]).ok()?.parse().ok()
}
pub(super) fn numeric<N: Numeric>(
    ui: &mut egui::Ui,
    value: &mut N,
    limits: RangeInclusive<N>,
    native: Option<&mut Native>,
) -> egui::Response {
    let Some(native) = native else {
        return crate::controls::numeric(ui, value, limits);
    };
    let result = ui
        .scope(|ui| {
            let visuals = ui.visuals_mut();
            for widget in [
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
            ] {
                widget.bg_fill = egui::Color32::TRANSPARENT;
                widget.weak_bg_fill = egui::Color32::TRANSPARENT;
                widget.bg_stroke = egui::Stroke::NONE;
            }
            let widgets = native
                .widgets
                .as_mut()
                .ok_or("Source editor text state was not admitted")?;
            widgets.number(ui, value, limits, &native.pool, &native.draft)
        })
        .inner;
    match result {
        Ok(response) => response,
        Err(error) => {
            native.error = Some(error);
            ui.add_enabled(false, egui::Label::new("Unavailable"))
        }
    }
}
pub(super) fn text(
    ui: &mut egui::Ui,
    text: &mut String,
    limit: usize,
    code: bool,
    single: bool,
    width: f32,
    native: Option<&mut Native>,
) -> egui::Response {
    text_widget(ui, text, limit, single, native, |ui, text, id| {
        visible_text(ui, text, limit, code, single, width, id)
    })
}

pub(super) fn canvas_id(native: Option<&Native>, fallback: egui::Id) -> egui::Id {
    native.and_then(|native| native.widgets.as_ref())
        .map_or(fallback, |widgets| widgets.states.id("source-body"))
}

pub(super) fn canvas_body(
    ui: &mut egui::Ui,
    text: &mut String,
    native: Option<&mut Native>,
    id: egui::Id,
    width: Option<f32>,
    selection: Option<egui::text::CCursorRange>,
) -> egui::Response {
    text_widget(ui, text, 16384, false, native, |ui, text, _| {
        let mut text = CanvasText(text);
        let ctx = ui.ctx().clone();
        if let Some(selection) = selection {
            let mut state = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
            state.cursor.set_char_range(Some(selection));
            egui::TextEdit::store_state(&ctx, id, state);
            ctx.memory_mut(|memory| memory.request_focus(id));
        }
        // The canvas already handled this pointer input. TextEdit would
        // surrender focus on a press outside its hidden input rectangle.
        let pointer = selection.map(|_| ctx.input_mut(|input| std::mem::take(&mut input.pointer)));
        let response = if let Some(width) = width {
            visible_text(ui, &mut text, 16384, false, false, width, Some(id))
        } else {
            let font = egui::TextStyle::Body.resolve(ui.style());
            let mut layout = |ui: &egui::Ui, text: &str, _: f32| {
                ui.fonts(|fonts| fonts.layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::TRANSPARENT))
            };
            ui.add(egui::TextEdit::multiline(&mut text)
                .id(id)
                .char_limit(4096)
                .desired_rows(1)
                .desired_width(24.)
                .layouter(&mut layout)
                .frame(false))
        };
        if let Some(pointer) = pointer {
            ctx.input_mut(|input| input.pointer = pointer);
        }
        response
    })
}

fn visible_text(
    ui: &mut egui::Ui,
    text: &mut dyn egui::TextBuffer,
    limit: usize,
    code: bool,
    single: bool,
    width: f32,
    id: Option<egui::Id>,
) -> egui::Response {
        let mut edit = if single {
            egui::TextEdit::singleline(text).hint_text("Search installed fonts")
        } else {
            egui::TextEdit::multiline(text)
                .desired_rows(if code { 8 } else { 3 })
                .desired_width(width)
        };
        if let Some(id) = id {
            edit = edit.id(id);
        }
        if code {
            edit = edit.code_editor();
        }
        ui.add(
            edit.char_limit(if single {
                128
            } else if code {
                limit
            } else {
                4096
            })
            .frame(false),
        )
}

fn text_widget(
    ui: &mut egui::Ui,
    text: &mut String,
    limit: usize,
    single: bool,
    native: Option<&mut Native>,
    draw: impl FnOnce(&mut egui::Ui, &mut dyn egui::TextBuffer, Option<egui::Id>) -> egui::Response,
) -> egui::Response {
    let Some(native) = native else {
        return draw(ui, text, None);
    };
    let result = (|| {
        let widgets = native
            .widgets
            .as_ref()
            .ok_or("Source editor text state was not admitted")?;
        let id = widgets
            .states
            .id(if single { "font-search" } else { "source-body" });
        widgets.states.prepare(id, limit)?;
        guard_events(ui, limit)?;
        // Stock TextEdit deletes the selection before asking TextBuffer to
        // insert. Restore the entire frame atomically when that insert refuses.
        let before = text.clone();
        let mut refused = false;
        let mut buffer = Fixed::new(text, limit, &native.pool, &native.draft, &mut refused)?;
        let response = draw(ui, &mut buffer, Some(id));
        widgets.states.observe(id)?;
        if refused {
            text.clear();
            text.push_str(&before);
            return Err("Source text input exceeds its admitted UTF-8 byte limit".into());
        }
        Ok(response)
    })();
    match result {
        Ok(response) => response,
        Err(error) => {
            native.error = Some(error);
            ui.add_enabled(false, egui::Label::new("Input limit reached"))
        }
    }
}
fn guard_events(ui: &egui::Ui, limit: usize) -> Result<(), String> {
    // filtered_events clones events before TextBuffer sees an insertion. Bound
    // all String payloads and the event header vector before calling TextEdit,
    // including a click that grants focus during this same widget invocation.
    ui.input(|input| {
        if input.events.len() > 4096 {
            return Err("Source input event count exceeded admission".into());
        }
        let mut bytes = 0usize;
        for event in &input.events {
            let text = match event {
                egui::Event::Text(text) | egui::Event::Paste(text) => Some(text.as_str()),
                egui::Event::Ime(egui::ImeEvent::Commit(text) | egui::ImeEvent::Preedit(text)) => {
                    Some(text.as_str())
                }
                egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                    data: Some(egui::accesskit::ActionData::Value(text)), ..
                }) => Some(text.as_ref()),
                _ => None,
            };
            if let Some(text) = text {
                bytes = bytes
                    .checked_add(text.len())
                    .filter(|bytes| *bytes <= limit)
                    .ok_or("Source input text exceeded admission")?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{text::CCursor, TextBuffer};

    #[test]
    fn canvas_text_deletes_graphemes_from_plain_and_admitted_buffers() {
        let verify = |text: &mut dyn TextBuffer| {
            let mut text = CanvasText(text);
            let next = text.delete_next_char(CCursor::new(1));
            assert_eq!(text.as_str(), "A👩‍🎨Z");
            assert_eq!(next.index, 1);
            let previous = text.delete_previous_char(CCursor::new(4));
            assert_eq!(previous.index, 1);
            assert_eq!(text.as_str(), "AZ");
            text.delete_previous_char(CCursor::new(0));
            text.delete_next_char(CCursor::new(2));
            assert_eq!(text.as_str(), "AZ");
        };
        let original = "Ae\u{301}👩‍🎨Z";
        verify(&mut original.to_owned());
        let pool = ResidentBudget::new(4096).unwrap();
        let owner = pool.reserve_bytes(64, 64).unwrap();
        let mut native = String::with_capacity(64);
        native.push_str(original);
        let mut refused = false;
        verify(&mut Fixed::new(&mut native, 64, &pool, &owner, &mut refused).unwrap());
        assert_eq!(native.capacity(), 64);
        assert!(!refused);
    }

    #[test]
    fn numeric_parser_and_clamp_preserve_native_words_and_signed_zero() {
        assert_eq!(parse(" 65\u{2009}535 "), Some(65535.));
        assert_eq!(parse("\u{2212}0.125"), Some(-0.125));
        assert_eq!(clamp(-0., 0. ..=1.).to_bits(), 0f64.to_bits());
        assert_eq!(clamp(65535., 65535. ..=0.), 65535.);
        assert!(parse(&"1".repeat(65)).is_none());
    }
}
