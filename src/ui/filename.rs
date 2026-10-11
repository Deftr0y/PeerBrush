//! Shaped filename presentation only. Original paths and document names stay logical UTF-8.
use super::*;
use resvg::{tiny_skia, usvg};
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

const FONTS: &[(&str, &[u8])] = &[
    (
        "Noto Sans Hebrew",
        include_bytes!("../../assets/fonts/NotoSansHebrew-Regular.ttf"),
    ),
    (
        "Noto Sans Arabic",
        include_bytes!("../../assets/fonts/NotoSansArabic-Regular.ttf"),
    ),
    (
        "Noto Sans Devanagari",
        include_bytes!("../../assets/fonts/NotoSansDevanagari-Regular.ttf"),
    ),
    (
        "Noto Sans CJK SC",
        include_bytes!("../../assets/fonts/NotoSansCJKsc-Regular.otf"),
    ),
];
const CACHE_BYTES: usize = 8 * 1024 * 1024;
const CACHE_ENTRIES: usize = 64;

pub(super) fn install_fallbacks(fonts: &mut egui::FontDefinitions) {
    for (name, bytes) in FONTS {
        fonts
            .font_data
            .insert((*name).into(), egui::FontData::from_static(bytes).into());
    }
    for family in [
        egui::FontFamily::Proportional,
        egui::FontFamily::Name("medium".into()),
        egui::FontFamily::Name("semibold".into()),
    ] {
        fonts
            .families
            .entry(family)
            .or_default()
            .extend(FONTS.iter().map(|(name, _)| (*name).into()));
    }
}

fn fontdb() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        for bytes in
            std::iter::once(include_bytes!("../../assets/fonts/UbuntuSans-Regular.ttf").as_slice())
                .chain(FONTS.iter().map(|(_, bytes)| *bytes))
        {
            db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(bytes)));
        }
        db.set_sans_serif_family("Ubuntu Sans");
        Arc::new(db)
    })
    .clone()
}

fn script_font(c: char) -> &'static str {
    match c as u32 {
        0x0590..=0x05ff | 0xfb1d..=0xfb4f => "Noto Sans Hebrew",
        0x0600..=0x06ff | 0x0750..=0x077f | 0x08a0..=0x08ff | 0xfb50..=0xfdff | 0xfe70..=0xfeff => {
            "Noto Sans Arabic"
        }
        0x0900..=0x097f | 0xa8e0..=0xa8ff => "Noto Sans Devanagari",
        0x1100..=0x11ff
        | 0x2e80..=0x9fff
        | 0xac00..=0xd7ff
        | 0xf900..=0xfaff
        | 0xff00..=0xffef
        | 0x20000..=0x3134f => "Noto Sans CJK SC",
        _ => "Ubuntu Sans",
    }
}

fn tree(text: &str, font: &str, size: f32, rtl: bool) -> Option<usvg::Tree> {
    let mut escaped = String::new();
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            c => escaped.push(c),
        }
    }
    let embedding = if rtl { '\u{202b}' } else { '\u{202a}' };
    let end = '\u{202c}';
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16384" height="128"><text id="filename" x="0" y="80" font-family="{font}" font-size="{size}" fill="white" xml:space="preserve">{embedding}{escaped}{end}</text></svg>"#
    );
    usvg::Tree::from_str(
        &svg,
        &usvg::Options {
            font_family: font.into(),
            fontdb: fontdb(),
            // Each run has an explicit font. Cross-font fallback can split a shaped
            // cluster; unsupported characters retain the font's missing-glyph sign.
            font_resolver: usvg::FontResolver {
                select_font: usvg::FontResolver::default_font_selector(),
                select_fallback: Box::new(|_, _, _| None),
            },
            ..Default::default()
        },
    )
    .ok()
}
struct Piece {
    tree: usvg::Tree,
    x: f32,
}
struct Layout {
    pieces: Vec<Piece>,
    width: f32,
    top: f32,
    bottom: f32,
    rtl: bool,
}
fn layout(text: &str, size: f32) -> Option<Layout> {
    if text.chars().count() > 512 || !size.is_finite() || !(1. ..=64.).contains(&size) {
        return None;
    }
    let presentation: String = text
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let bidi = unicode_bidi::BidiInfo::new(&presentation, None);
    let paragraph = bidi.paragraphs.first()?;
    let (levels, visual_runs) = bidi.visual_runs(paragraph, paragraph.range.clone());
    let mut result = Layout {
        pieces: Vec::new(),
        width: 0.,
        top: f32::INFINITY,
        bottom: f32::NEG_INFINITY,
        rtl: paragraph.level.is_rtl(),
    };
    for run in visual_runs {
        let rtl = levels[run.start].is_rtl();
        let mut fonts: Vec<(&str, String)> = Vec::new();
        for c in presentation[run].chars() {
            // Formatting has already been resolved by the paragraph bidi pass.
            if matches!(c as u32, 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069) {
                continue;
            }
            let font = if c.is_whitespace()
                || c.is_ascii_punctuation()
                || matches!(c as u32, 0x0300..=0x036f | 0x200c..=0x200d | 0xfe00..=0xfe0f | 0xe0100..=0xe01ef)
            {
                fonts.last().map_or("Ubuntu Sans", |(font, _)| *font)
            } else {
                script_font(c)
            };
            if fonts.last().is_none_or(|(last, _)| *last != font) {
                fonts.push((font, String::new()));
            }
            fonts.last_mut()?.1.push(c);
        }
        if rtl {
            fonts.reverse();
        }
        for (font, text) in fonts {
            let tree = tree(&text, font, size, rtl)?;
            let bounds = tree.root().abs_bounding_box();
            result.top = result.top.min(bounds.top());
            result.bottom = result.bottom.max(bounds.bottom());
            result.pieces.push(Piece {
                tree,
                x: result.width,
            });
            result.width += bounds.width();
        }
    }
    (!result.pieces.is_empty() && result.width > 0. && result.top < result.bottom).then_some(result)
}
struct Raster {
    pixels: egui::ColorImage,
    size: Vec2,
    truncated: bool,
    rtl: bool,
}
fn raster(text: &str, font_size: f32, max_width: f32, scale: f32) -> Option<Raster> {
    let layout = layout(text, font_size)?;
    let natural = layout.width + 2.;
    let width = natural.min(max_width.clamp(1., 2048.));
    let glyph_height = layout.bottom - layout.top;
    let height = (glyph_height + 2.).max(font_size * 1.4);
    let scale = scale.clamp(1., 4.);
    let w = (width * scale).ceil() as u32;
    let h = (height * scale).ceil() as u32;
    if w > 8192 || h > 512 || w as usize * h as usize * 4 > CACHE_BYTES {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    let x = if layout.rtl && natural > width {
        natural - width - 1.
    } else {
        -1.
    };
    let y = layout.top - (height - glyph_height) * 0.5;
    for piece in &layout.pieces {
        let bounds = piece.tree.root().abs_bounding_box();
        resvg::render(
            &piece.tree,
            tiny_skia::Transform::from_row(
                scale,
                0.,
                0.,
                scale,
                (piece.x - bounds.left() - x) * scale,
                -y * scale,
            ),
            &mut pixmap.as_mut(),
        );
    }
    Some(Raster {
        pixels: egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()),
        size: Vec2::new(width, height),
        truncated: natural > width,
        rtl: layout.rtl,
    })
}

#[derive(Clone, PartialEq)]
struct Key {
    text: String,
    size: u32,
    width: u32,
    scale: u32,
}
#[derive(Clone)]
struct Entry {
    key: Key,
    texture: TextureHandle,
    size: Vec2,
    truncated: bool,
    rtl: bool,
    bytes: usize,
}
#[derive(Default)]
struct Cache {
    entries: VecDeque<Entry>,
    bytes: usize,
}
impl Cache {
    fn get(&mut self, key: &Key) -> Option<Entry> {
        let index = self.entries.iter().position(|e| &e.key == key)?;
        let entry = self.entries.remove(index)?;
        self.entries.push_back(entry.clone());
        Some(entry)
    }
    fn insert(&mut self, entry: Entry) {
        while !self.entries.is_empty()
            && (self.bytes + entry.bytes > CACHE_BYTES || self.entries.len() >= CACHE_ENTRIES)
        {
            self.bytes -= self.entries.pop_front().unwrap().bytes;
        }
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
    }
}
fn entry(ctx: &egui::Context, text: &str, size: f32, width: f32) -> Option<Entry> {
    // Latin-1 does not need complex shaping and is already covered by Ubuntu.
    if text.chars().all(|c| (c as u32) < 0x300) || width < 1. {
        return None;
    }
    let width = width.floor().clamp(1., 2048.);
    let scale = (ctx.pixels_per_point() * 1.5).clamp(1., 4.);
    let key = Key {
        text: text.into(),
        size: size.to_bits(),
        width: width.to_bits(),
        scale: scale.to_bits(),
    };
    let cache = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Arc<Mutex<Cache>>>(egui::Id::new("shaped filename cache"))
            .clone()
    });
    if let Some(entry) = cache.lock().ok()?.get(&key) {
        return Some(entry);
    }
    let image = raster(text, size, width, scale)?;
    let bytes = image.pixels.pixels.len() * 4;
    let texture = ctx.load_texture(
        "shaped filename",
        image.pixels,
        egui::TextureOptions::LINEAR,
    );
    let entry = Entry {
        key,
        texture,
        size: image.size,
        truncated: image.truncated,
        rtl: image.rtl,
        bytes,
    };
    cache.lock().ok()?.insert(entry.clone());
    Some(entry)
}
fn paint_entry(ui: &egui::Ui, entry: &Entry, rect: Rect, size: f32, color: Color32) {
    let image_rect = Rect::from_min_size(
        Pos2::new(
            if entry.rtl {
                rect.right() - entry.size.x
            } else {
                rect.left()
            },
            rect.center().y - entry.size.y * 0.5,
        ),
        entry.size,
    );
    let mut clip = rect.intersect(ui.clip_rect());
    if entry.truncated {
        if entry.rtl {
            clip.min.x += size * 1.2;
        } else {
            clip.max.x -= size * 1.2;
        }
    }
    ui.painter().with_clip_rect(clip).image(
        entry.texture.id(),
        image_rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
        color,
    );
    if entry.truncated {
        ui.painter()
            .with_clip_rect(rect.intersect(ui.clip_rect()))
            .text(
                Pos2::new(
                    if entry.rtl { rect.left() } else { rect.right() },
                    rect.center().y,
                ),
                if entry.rtl {
                    egui::Align2::LEFT_CENTER
                } else {
                    egui::Align2::RIGHT_CENTER
                },
                "…",
                egui::FontId::proportional(size),
                color,
            );
    }
}
pub(super) fn paint(ui: &egui::Ui, text: &str, rect: Rect, size: f32, color: Color32) -> bool {
    let Some(entry) = entry(ui.ctx(), text, size, rect.width()) else {
        return false;
    };
    paint_entry(ui, &entry, rect, size, color);
    true
}
pub(super) fn label(
    ui: &mut egui::Ui,
    text: &str,
    size: f32,
    color: Color32,
    max_width: f32,
) -> egui::Response {
    let Some(entry) = entry(ui.ctx(), text, size, max_width.min(ui.available_width())) else {
        return ui.label(RichText::new(text).size(size).color(color));
    };
    let (rect, response) = ui.allocate_exact_size(entry.size, egui::Sense::hover());
    paint_entry(ui, &entry, rect, size, color);
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), text));
    response
}
pub(super) fn project_button(
    ui: &mut egui::Ui,
    text: &str,
    selected: bool,
    color: Color32,
) -> egui::Response {
    let Some(entry) = entry(ui.ctx(), text, 12., 310.) else {
        return ui.add(
            egui::Button::new(RichText::new(text).color(color))
                .selected(selected)
                .wrap(),
        );
    };
    let padding = ui.spacing().button_padding;
    let (rect, response) = ui.allocate_exact_size(
        (entry.size + padding * 2.).max(ui.spacing().interact_size),
        egui::Sense::click(),
    );
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
    }
    paint_entry(ui, &entry, rect.shrink2(padding), 12., color);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected, text)
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shaped_filenames_have_glyphs_direction_and_bounded_density() {
        use ab_glyph::Font;
        for ((_, bytes), sample) in FONTS.iter().zip(['ש', 'ع', 'ह', '漢']) {
            let font = ab_glyph::FontRef::try_from_slice(bytes).unwrap();
            assert_ne!(font.glyph_id(sample).0, 0);
        }
        let cjk = ab_glyph::FontRef::try_from_slice(FONTS[3].1).unwrap();
        for c in ['あ', '한'] {
            assert_ne!(cjk.glyph_id(c).0, 0);
        }
        for name in [
            "café Ω Ж.png",
            "שלום עולם.psd",
            "مرحبا بالعالم.png",
            "हिन्दी.psd",
            "漢字 あいう 한글.png",
            "e\u{301} & <稿>.psd",
        ] {
            let shaped = layout(name, 12.).unwrap();
            let glyphs = glyphs(&shaped);
            assert!(!glyphs.is_empty());
            assert!(
                glyphs
                    .iter()
                    .filter(|(text, _, _)| text.chars().any(char::is_alphanumeric))
                    .all(|(_, id, _)| *id != 0),
                "Missing glyph in {name}"
            );
            let rendered = raster(name, 12., 320., 2.).unwrap();
            assert!(
                rendered.pixels.pixels.iter().filter(|p| p.a() > 0).count() > 30,
                "{name}"
            );
            assert_eq!(rendered.rtl, name.starts_with('ש') || name.starts_with('م'));
            assert!(!rendered.truncated);
            let tiny = raster(name, 12., 10., 4.).unwrap();
            assert!(tiny.truncated);
            assert_eq!(tiny.pixels.size[0], 40);
        }
        assert!(raster(&"漢".repeat(513), 12., 320., 1.).is_none());
        assert!(raster("שלום", f32::NAN, 320., 1.).is_none());
    }
    fn glyphs(layout: &Layout) -> Vec<(String, u32, f32)> {
        let mut result = Vec::new();
        for piece in &layout.pieces {
            let Some(usvg::Node::Text(text)) = piece.tree.node_by_id("filename") else {
                panic!("Missing filename text");
            };
            let left = piece.tree.root().abs_bounding_box().left();
            for glyph in text.layouted().iter().flat_map(|s| &s.positioned_glyphs) {
                result.push((
                    glyph.text.clone(),
                    glyph.id.0,
                    piece.x + glyph.transform().tx - left,
                ));
            }
        }
        result
    }
    #[test]
    fn arabic_joining_and_rtl_extensions_are_shaped_without_changing_logical_name() {
        let joined = glyphs(&layout("لا.png", 12.).unwrap());
        let arabic: Vec<_> = joined
            .iter()
            .filter(|(text, _, _)| text.contains('ل') || text.contains('ا'))
            .collect();
        let isolated = glyphs(&layout("ل ا.png", 12.).unwrap());
        let isolated_ids: Vec<_> = isolated
            .iter()
            .filter(|(text, _, _)| text.contains('ل') || text.contains('ا'))
            .map(|(_, id, _)| *id)
            .collect();
        assert_ne!(
            arabic.iter().map(|(_, id, _)| *id).collect::<Vec<_>>(),
            isolated_ids,
            "Joined Arabic must use contextual forms"
        );
        let latin = joined.iter().find(|(text, _, _)| text == "p").unwrap();
        assert!(
            arabic[0].2 > latin.2,
            "An RTL name starts to the right of its LTR extension"
        );
        let mixed = glyphs(&layout("שלום 12.png", 12.).unwrap());
        let one = mixed.iter().find(|(text, _, _)| text == "1").unwrap();
        let two = mixed.iter().find(|(text, _, _)| text == "2").unwrap();
        assert!(one.2 < two.2, "Numbers retain their reading order");
    }
    #[test]
    fn filename_cache_is_bounded_and_color_is_presentation_only() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        for i in 0..100 {
            entry(&ctx, &format!("שלום {i}.psd"), 12., 200.).unwrap();
        }
        let cache = ctx.data_mut(|d| {
            d.get_temp::<Arc<Mutex<Cache>>>(egui::Id::new("shaped filename cache"))
                .unwrap()
        });
        let cache = cache.lock().unwrap();
        assert_eq!(cache.entries.len(), CACHE_ENTRIES);
        assert!(cache.bytes <= CACHE_BYTES);
        drop(cache);
        let a = entry(&ctx, "שלום 99.psd", 12., 200.).unwrap();
        let b = entry(&ctx, "שלום 99.psd", 12., 200.).unwrap();
        assert_eq!(a.texture.id(), b.texture.id());
    }
}
