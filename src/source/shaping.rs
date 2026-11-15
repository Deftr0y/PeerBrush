//! Opt-in horizontal shaping. Historical character layout remains a separate path.
use super::{outline_work, spacing, text_position::{Cluster, TextCarets}, Content, Source, TextLayout};
use ab_glyph::{point, Font, FontArc, Glyph, GlyphId, PxScale, ScaleFont};
use harfrust::{BufferFlags, Direction, ShapeOptions, ShaperData, UnicodeBuffer};
use std::ops::Range;
use unicode_bidi::Level;
use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

const GLYPHS: usize = 16384;
const COORDINATE: f64 = 100000.;
const WRAP_PROBE_BYTES: usize = 256 * 1024;
pub(super) const MAX_ROWS: usize = 96;

#[derive(Debug, PartialEq)]
struct Run {
    range: Range<usize>,
    script: Script,
    rtl: bool,
}

fn runs(text: &str, level: Option<Level>) -> Vec<Run> {
    let bidi = unicode_bidi::BidiInfo::new(text, level);
    let Some(paragraph) = bidi.paragraphs.first() else {
        return vec![];
    };
    let (levels, visual) = bidi.visual_runs(paragraph, paragraph.range.clone());
    let mut result = Vec::new();
    for range in visual {
        let rtl = levels[range.start].is_rtl();
        let chars: Vec<_> = text[range.clone()]
            .char_indices()
            .map(|(index, ch)| (range.start + index, ch.script()))
            .collect();
        let first = chars
            .iter()
            .find_map(|(_, script)| {
                (!matches!(script, Script::Common | Script::Inherited)).then_some(*script)
            })
            .unwrap_or(Script::Common);
        let mut script = first;
        let mut start = range.start;
        let mut items = Vec::new();
        for (index, next) in chars {
            if !matches!(next, Script::Common | Script::Inherited) && next != script {
                if start != index {
                    items.push(Run {
                        range: start..index,
                        script,
                        rtl,
                    });
                }
                start = index;
                script = next;
            }
        }
        items.push(Run {
            range: start..range.end,
            script,
            rtl,
        });
        if rtl {
            items.reverse();
        }
        result.extend(items);
    }
    result
}

fn tracking_supported(script: Script) -> bool {
    matches!(
        script,
        Script::Latin
            | Script::Greek
            | Script::Cyrillic
            | Script::Han
            | Script::Hangul
            | Script::Hiragana
            | Script::Katakana
            | Script::Bopomofo
            | Script::Common
    )
}

fn coordinate(value: f64) -> Result<f32, String> {
    if !value.is_finite() || value.abs() > COORDINATE {
        return Err(
            "Shaped text exceeds the coordinate limit; use a smaller size or spacing".into(),
        );
    }
    Ok(value as f32)
}

fn checked_glyph(font: &impl Font, value: u32) -> Result<GlyphId, String> {
    if value == 0 || value > u32::from(u16::MAX) || value as usize >= font.glyph_count() {
        return Err("The selected font needs an unsupported glyph".into());
    }
    Ok(GlyphId(value as u16))
}
fn variation_selector(ch: char) -> bool {
    matches!(ch, '\u{fe00}'..='\u{fe0f}' | '\u{e0100}'..='\u{e01ef}')
}
fn variation_sequences(text: &str) -> Result<(), String> {
    let mut previous = None;
    for ch in text.chars() {
        if variation_selector(ch)
            && !previous.is_some_and(|ch| {
                !variation_selector(ch) && !matches!(ch, '\n' | '\u{200c}' | '\u{200d}')
            })
        {
            return Err("A variation selector must follow one base character".into());
        }
        previous = Some(ch);
    }
    Ok(())
}

struct Positioned {
    id: GlyphId,
    x: f64,
    y: f64,
    blank: bool,
}


#[derive(Default)]
struct ShapeWork {
    glyphs: usize,
    runs: usize,
}

struct ShapedLine {
    advance: f64,
    positioned: Vec<Positioned>,
    clusters: Vec<Cluster>,
}

struct LineShaper<'a, F> {
    font: &'a F,
    shaper: harfrust::Shaper<'a>,
    scale: f64,
    letter_spacing: f32,
}

impl<F: Font> LineShaper<'_, F> {
    fn line(
        &self,
        line: &str,
        level: Option<Level>,
        baseline: f64,
        work: &mut ShapeWork,
        collect_glyphs: bool,
        collect_carets: bool,
    ) -> Result<ShapedLine, String> {
        let mut positioned = Vec::new();
        let mut caret_clusters: Vec<Cluster> = Vec::new();
        let mut advance = 0f64;
        let mut have_cluster = false;
        for run in runs(line, level) {
            work.runs += 1;
            if work.runs > 4096 {
                return Err("Shaped text exceeds the run budget".into());
            }
            if self.letter_spacing != 0. && !tracking_supported(run.script) {
                return Err("Set letter spacing to zero for joining or complex scripts".into());
            }
            let part = &line[run.range.clone()];
            let mut buffer = UnicodeBuffer::new();
            if !buffer.reserve(part.chars().count()) {
                return Err("Text shaping exceeds the allocation budget".into());
            }
            buffer.push_str(part);
            buffer.set_direction(if run.rtl {
                Direction::RightToLeft
            } else {
                Direction::LeftToRight
            });
            buffer.set_script(
                run.script
                    .short_name()
                    .parse()
                    .map_err(|_| "Unsupported text script")?,
            );
            buffer.set_not_found_variation_selector_glyph(0);
            buffer.set_pre_context(&line[..run.range.start]);
            buffer.set_post_context(&line[run.range.end..]);
            let mut flags = BufferFlags::REMOVE_DEFAULT_IGNORABLES;
            if run.range.start == 0 {
                flags |= BufferFlags::BEGINNING_OF_TEXT;
            }
            if run.range.end == line.len() {
                flags |= BufferFlags::END_OF_TEXT;
            }
            buffer.set_flags(flags);
            let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.shaper.shape(buffer, ShapeOptions::new())
            }))
            .map_err(|_| "The selected font could not shape this text")?;
            work.glyphs = work.glyphs
                .checked_add(output.len())
                .filter(|count| *count <= GLYPHS)
                .ok_or("Shaped text exceeds the glyph budget")?;
            let infos = output.glyph_infos();
            let positions = output.glyph_positions();
            if infos.len() != positions.len() {
                return Err("Invalid shaped text output".into());
            }
            let mut clusters: Vec<_> = infos.iter().map(|info| info.cluster as usize).collect();
            if clusters
                .iter()
                .any(|index| *index >= part.len() || !part.is_char_boundary(*index))
            {
                return Err("Invalid shaped text cluster".into());
            }
            clusters.sort_unstable();
            clusters.dedup();
            let mut previous = None;
            let mut vertical = 0f64;
            for (info, position) in infos.iter().zip(positions) {
                let id = checked_glyph(self.font, info.glyph_id)?;
                if previous.is_some_and(|last| {
                    if run.rtl {
                        info.cluster > last
                    } else {
                        info.cluster < last
                    }
                }) {
                    return Err("Invalid shaped text cluster order".into());
                }
                if previous != Some(info.cluster) {
                    if have_cluster {
                        advance += f64::from(self.letter_spacing);
                    }
                    have_cluster = true;
                }
                let cluster = info.cluster as usize;
                let at = clusters
                    .binary_search(&cluster)
                    .map_err(|_| "Invalid shaped text cluster")?;
                let end = clusters.get(at + 1).copied().unwrap_or(part.len());
                if collect_carets && previous != Some(info.cluster) {
                    caret_clusters.push(Cluster {
                        bytes: (run.range.start + cluster)..(run.range.start + end),
                        advance: [advance; 2],
                        rtl: run.rtl,
                    });
                }
                if collect_glyphs {
                    positioned.push(Positioned {
                        id,
                        x: advance + f64::from(position.x_offset) * self.scale,
                        y: baseline - vertical - f64::from(position.y_offset) * self.scale,
                        blank: part[cluster..end]
                            .chars()
                            .all(|ch| spacing(ch) || matches!(ch, '\u{200c}' | '\u{200d}'))
                            || part[cluster..end]
                                .chars()
                                .any(|ch| spacing(ch) && self.font.glyph_id(ch) == id),
                    });
                }
                advance += f64::from(position.x_advance) * self.scale;
                if let Some(cluster) = caret_clusters.last_mut() {
                    cluster.advance[1] = advance;
                }
                vertical += f64::from(position.y_advance) * self.scale;
                coordinate(advance)?;
                coordinate(vertical)?;
                previous = Some(info.cluster);
            }
        }
        Ok(ShapedLine { advance, positioned, clusters: caret_clusters })
    }
}

struct TextRow<'a> {
    text: &'a str,
    level: Option<Level>,
    soft_end: bool,
}

fn paragraph_rows(
    text: &str,
    width: f64,
    mut measure: impl FnMut(&str, Option<Level>) -> Result<f64, String>,
) -> Result<Vec<TextRow<'_>>, String> {
    let mut rows = Vec::new();
    let mut probed = 0usize;
    for paragraph in text.split('\n') {
        let level = unicode_bidi::BidiInfo::new(paragraph, None)
            .paragraphs.first().map(|paragraph| paragraph.level);
        let boundaries: Vec<_> = paragraph.grapheme_indices(true).map(|(offset, grapheme)| {
            let preferred = grapheme.chars().last().is_some_and(|ch| {
                (spacing(ch) && !matches!(ch, '\u{00a0}' | '\u{2007}' | '\u{202f}'))
                    || matches!(ch, '-' | '\u{2010}' | '\u{200b}')
            });
            (offset + grapheme.len(), preferred)
        }).collect();
        let mut first = 0;
        let mut start = 0;
        loop {
            if rows.len() == MAX_ROWS {
                return Err("Wrapped text exceeds the 96-row layout limit".into());
            }
            let remaining = &boundaries[first..];
            let mut fits = |count: usize| {
                let end = remaining[count - 1].0;
                probed += end - start;
                if probed > WRAP_PROBE_BYTES {
                    return Err("Wrapped text exceeds the line measurement budget".into());
                }
                measure(&paragraph[start..end], level).map(|advance| advance <= width)
            };
            let count = if remaining.is_empty() {
                0
            } else {
                // Bracket the first overflow before narrowing the measured prefix.
                let mut fitting = 0;
                let mut probe = 1;
                while fits(probe)? {
                    fitting = probe;
                    if fitting == remaining.len() { break; }
                    probe = (probe * 2).min(remaining.len());
                }
                if fitting == remaining.len() {
                    fitting
                } else if fitting == 0 {
                    1 // A single grapheme may be wider than the frame.
                } else {
                    while probe - fitting > 1 {
                        let mid = fitting + (probe - fitting) / 2;
                        if fits(mid)? { fitting = mid; } else { probe = mid; }
                    }
                    remaining[..fitting].iter().rposition(|(_, preferred)| *preferred)
                        .map_or(fitting, |index| index + 1)
                }
            };
            let end = if count == 0 { start } else { remaining[count - 1].0 };
            let soft_end = end < paragraph.len();
            rows.push(TextRow { text: &paragraph[start..end], level, soft_end });
            if !soft_end { break; }
            first += count;
            start = end;
        }
    }
    Ok(rows)
}

pub(super) fn layout(source: &Source, font: &FontArc) -> Result<Vec<Glyph>, String> {
    layout_with_carets(source, font, None)
}

pub(super) fn layout_with_carets(
    source: &Source,
    font: &FontArc,
    carets: Option<&mut TextCarets>,
) -> Result<Vec<Glyph>, String> {
    let Content::Text {
        text,
        layout,
        font: reference,
        size,
        line_height,
        letter_spacing,
        align,
        ..
    } = &source.content
    else {
        return Err("Shaped layout requires a text source".into());
    };
    layout_parts(
        *layout,
        source.width,
        text,
        reference,
        *size,
        *line_height,
        *letter_spacing,
        align,
        font,
        &mut |_| Ok(()),
        carets,
    )
}

pub(super) fn layout_parts(
    layout: TextLayout,
    width: u32,
    text: &str,
    reference: &crate::fonts::TextFont,
    size: f32,
    line_height: f32,
    letter_spacing: f32,
    align: &str,
    font: &impl Font,
    check: &mut dyn FnMut(Option<GlyphId>) -> Result<(), String>,
    mut carets: Option<&mut TextCarets>,
) -> Result<Vec<Glyph>, String> {
    variation_sequences(text)?;
    let index = match reference {
        crate::fonts::TextFont::Bundled(_) => 0,
        crate::fonts::TextFont::Installed(font) => font.index,
    };
    let face = harfrust::FontRef::from_index(font.font_data(), index)
        .map_err(|_| "The selected font cannot be shaped")?;
    let height = f64::from(font.height_unscaled());
    if !height.is_finite()
        || height <= 0.
        || !font.ascent_unscaled().is_finite()
        || !font.descent_unscaled().is_finite()
        || font
            .units_per_em()
            .is_none_or(|units| !units.is_finite() || units <= 0.)
    {
        return Err("The selected font has unsupported text metrics".into());
    }
    let scale = f64::from(size) / height;
    let scaled = font.as_scaled(PxScale::from(size));
    let data = ShaperData::new(&face);
    let shaper = LineShaper { font, shaper: data.shaper(&face).build(), scale, letter_spacing };
    let rows = if layout == TextLayout::Paragraph {
        paragraph_rows(text, f64::from(width), |line, level| {
            check(None)?;
            Ok(shaper.line(line, level, 0., &mut ShapeWork::default(), false, false)?.advance)
        })?
    } else {
        text.split('\n').map(|text| TextRow { text, level: None, soft_end: false }).collect()
    };
    let mut glyph_positions = Vec::new();
    let mut shape_work = ShapeWork::default();
    let mut work = 0u64;
    for (row, TextRow { text: line, level, soft_end }) in rows.into_iter().enumerate() {
        check(None)?;
        let baseline = f64::from(scaled.ascent())
            + row as f64 * f64::from(scaled.height()) * f64::from(line_height);
        coordinate(baseline)?;
        let ShapedLine { advance, positioned, mut clusters } = shaper.line(
            line, level, baseline, &mut shape_work, true, carets.is_some(),
        )?;
        let start = match align {
            "center" => (f64::from(width) - advance) * 0.5,
            "right" => f64::from(width) - advance,
            _ => 0.,
        };
        coordinate(start)?;
        if let Some(carets) = carets.as_deref_mut() {
            let vertical = [
                (baseline - f64::from(scaled.ascent())) as f32,
                (baseline - f64::from(scaled.descent())) as f32,
            ];
            carets.shaped_row(line, start, vertical, &mut clusters, soft_end);
        }
        for glyph in positioned {
            let glyph_position = point(coordinate(start + glyph.x)?, coordinate(glyph.y)?);
            let positioned = glyph.id.with_scale_and_position(size, glyph_position);
            check(Some(positioned.id))?;
            match font.outline_glyph(positioned.clone()) {
                Some(outline) => {
                    let bounds = outline.px_bounds();
                    work = outline_work(bounds, work)?;
                    for value in [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y] {
                        coordinate(f64::from(value))?;
                    }
                    glyph_positions.push(positioned);
                }
                None if glyph.blank => (),
                None => {
                    return Err(
                        "The selected font needs a glyph without a supported outline".into(),
                    )
                }
            }
        }
    }
    Ok(glyph_positions)
}

pub(super) fn paint(
    base: &mut crate::raster::Raster,
    color: [u16; 4],
    font: &FontArc,
    glyphs: Vec<Glyph>,
) -> Result<(), String> {
    for glyph in glyphs {
        let outline = font
            .outline_glyph(glyph)
            .ok_or("Validated shaped outline is missing")?;
        let bounds = outline.px_bounds();
        if bounds.max.x < 0.
            || bounds.max.y < 0.
            || bounds.min.x >= base.width as f32
            || bounds.min.y >= base.height as f32
        {
            continue;
        }
        outline.draw(|xx, yy, coverage| {
            let px = bounds.min.x as i32 + xx as i32;
            let py = bounds.min.y as i32 + yy as i32;
            let mut pixel = color;
            pixel[3] = (f32::from(pixel[3]) * coverage).round() as u16;
            if pixel[3] > 0 {
                base.set16(
                    px,
                    py,
                    crate::raster::blend16(base.get16(px, py), pixel, 1., "normal"),
                );
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        fonts::{self, FontError, InstalledFont, TextFont},
        source::{Content, TextLayout},
    };

    fn source(text: &str) -> Source {
        Source {
            width: 256,
            height: 96,
            matrix: super::super::identity(),
            content: Content::Text {
                text: text.into(),
                layout: TextLayout::Shaped,
                font: "regular".into(),
                size: 32.,
                line_height: 1.25,
                letter_spacing: 0.,
                align: "left".into(),
                color: [12501, 23003, 34507, 65535],
            },
        }
    }
    fn reference(index: u32, bytes: &[u8]) -> InstalledFont {
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
        InstalledFont {
            family: "Synthetic shaping fixture".into(),
            postscript: "PeerBrushSynthetic-Regular".into(),
            style: "normal".into(),
            weight: 400,
            stretch: 5,
            index,
            sha256: digest
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }
    #[test]
    fn bidi_script_itemization_keeps_clusters_and_reverses_rtl_script_order() {
        let text = "ab\u{0301} \u{05d0}\u{05d1} cd";
        let actual = runs(text, None);
        assert_eq!(
            actual
                .iter()
                .map(|run| (&text[run.range.clone()], run.script, run.rtl))
                .collect::<Vec<_>>(),
            vec![
                ("ab\u{0301} ", Script::Latin, false),
                ("\u{05d0}\u{05d1}", Script::Hebrew, true),
                (" cd", Script::Latin, false)
            ]
        );
        let same_direction = "A\u{0915}\u{093f}B";
        assert_eq!(
            runs(same_direction, None)
                .iter()
                .map(|run| (&same_direction[run.range.clone()], run.script))
                .collect::<Vec<_>>(),
            vec![
                ("A", Script::Latin),
                ("\u{0915}\u{093f}", Script::Devanagari),
                ("B", Script::Latin)
            ]
        );
        let common = "(\u{05d0}\u{0301})";
        let actual = runs(common, None);
        assert!(actual
            .iter()
            .all(|run| run.script == Script::Hebrew && run.rtl));
        assert_eq!(
            actual.iter().map(|run| run.range.len()).sum::<usize>(),
            common.len()
        );
    }
    #[test]
    fn shaped_carets_share_the_ligature_layout_and_skip_combining_interiors() {
        let font = fonts::snapshot(&"regular".into()).unwrap();
        let text = "ffi a\u{0301}";
        let source = source(text);
        let mut map = TextCarets::new(text, TextLayout::Shaped).unwrap();
        let shaped = layout_with_carets(&source, &font, Some(&mut map)).unwrap();
        assert_eq!(shaped, layout(&source, &font).unwrap());
        let at = |index| map.at(super::super::text_position::Caret { index, trailing: false }).unwrap();
        assert!(at(0).0 < at(1).0 && at(1).0 < at(2).0 && at(2).0 < at(3).0);
        let (x, [top, bottom]) = at(5);
        assert_eq!(map.nearest([x, (top + bottom) * 0.5]).index, 6);
    }
    #[test]
    fn paragraph_breaks_prefer_spaces_and_keep_whole_graphemes() {
        let measure = |line: &str, _| Ok(line.chars().count() as f64);
        let rows = paragraph_rows("ab cd ef", 5., measure).unwrap();
        assert_eq!(rows.iter().map(|row| (row.text, row.soft_end)).collect::<Vec<_>>(),
            [("ab ", true), ("cd ef", false)]);
        let rows = paragraph_rows("a\u{0301}b\n", 1., measure).unwrap();
        assert_eq!(rows.iter().map(|row| (row.text, row.soft_end)).collect::<Vec<_>>(),
            [("a\u{0301}", true), ("b", false), ("", false)]);
    }

    #[test]
    fn paragraph_pixels_and_carets_follow_the_same_wrapped_rows() {
        use super::super::text_position::Caret;
        let font = fonts::snapshot(&"regular".into()).unwrap();
        let text = "aaaa aaaa\nb";
        let mut wrapped = source(text);
        let scaled = font.as_scaled(PxScale::from(32.));
        wrapped.width = (4. * scaled.h_advance(scaled.glyph_id('a'))
            + scaled.h_advance(scaled.glyph_id(' ')) + 3.).ceil() as u32;
        wrapped.height = 160;
        if let Content::Text { layout, .. } = &mut wrapped.content {
            *layout = TextLayout::Paragraph;
        }
        let mut map = TextCarets::new(text, TextLayout::Paragraph).unwrap();
        layout_with_carets(&wrapped, &font, Some(&mut map)).unwrap();
        let (x, first) = map.at(Caret { index: 5, trailing: true }).unwrap();
        let (_, second) = map.at(Caret { index: 5, trailing: false }).unwrap();
        let (_, third) = map.at(Caret { index: 10, trailing: false }).unwrap();
        assert!(first[0] < second[0] && second[0] < third[0]);
        let hit = map.nearest([x, (first[0] + first[1]) * 0.5]);
        assert_eq!((hit.index, hit.trailing), (5, true));
        assert_eq!(map.highlights(4..6).count(), 2);
        let mut hard_breaks = source("aaaa \naaaa\nb");
        hard_breaks.width = wrapped.width;
        hard_breaks.height = wrapped.height;
        for depth in [8, 16] {
            assert_eq!(wrapped.render(wrapped.width, wrapped.height, depth).unwrap(),
                hard_breaks.render(wrapped.width, wrapped.height, depth).unwrap());
        }
    }

    #[test]
    fn combining_ligatures_whitespace_alignment_and_baselines_use_native_font_metrics() {
        let font = fonts::snapshot(&"regular".into()).unwrap();
        assert!(
            layout(&source("ffi"), &font).unwrap().len() < 3,
            "Bundled font must provide an actual default ligature"
        );
        for depth in [8, 16] {
            let composed = source("\u{00e1}").render(256, 96, depth).unwrap();
            let combining = source("a\u{0301}").render(256, 96, depth).unwrap();
            assert_eq!(composed.rgba16(), combining.rgba16());
            let shaped = source("office");
            let mut legacy = shaped.clone();
            if let Content::Text { layout, .. } = &mut legacy.content {
                *layout = TextLayout::Legacy;
            }
            assert_ne!(
                shaped.render(256, 96, depth).unwrap().rgba16(),
                legacy.render(256, 96, depth).unwrap().rgba16()
            );
            let rendered = shaped.render(256, 96, depth).unwrap();
            assert_eq!(rendered.depth, depth);
            assert!(rendered.rgba16().chunks_exact(4).any(|pixel| pixel[3] > 0));
            if depth == 16 {
                assert!(rendered.rgba16().iter().any(|word| word % 257 != 0));
            }
        }
        // The second A starts after its native advance plus the actual space,
        // preserving advance width independently of visible ink bounds.
        let spaced = layout(&source("A A"), &font).unwrap();
        let single = layout(&source("A"), &font).unwrap();
        let scaled = font.as_scaled(32.);
        let expected = scaled.h_advance(font.glyph_id('A')) + scaled.h_advance(font.glyph_id(' '));
        assert_eq!(spaced.len(), 2);
        assert!((spaced[1].position.x - single[0].position.x - expected).abs() < 0.0001);
        let lines = layout(&source("A\nA"), &font).unwrap();
        assert!((lines[1].position.y - lines[0].position.y - 40.).abs() < 0.0001);
        for (name, factor) in [("center", 0.5), ("right", 1.)] {
            let mut aligned = source("A ");
            if let Content::Text { align, .. } = &mut aligned.content {
                *align = name.into();
            }
            let result = layout(&aligned, &font).unwrap();
            assert!((result[0].position.x - (256. - expected) * factor).abs() < 0.0001);
        }
    }
    #[test]
    fn tracking_is_between_clusters_and_complex_tracking_is_explicitly_refused() {
        let font = fonts::snapshot(&"regular".into()).unwrap();
        let plain = layout(&source("a\u{0301}a\u{0301}"), &font).unwrap();
        let mut tracked = source("a\u{0301}a\u{0301}");
        if let Content::Text { letter_spacing, .. } = &mut tracked.content {
            *letter_spacing = 3.25;
        }
        let actual = layout(&tracked, &font).unwrap();
        assert_eq!(plain.len(), 2);
        assert_eq!(actual[0].position, plain[0].position);
        assert_eq!(actual[1].position.x, plain[1].position.x + 3.25);
        assert!(!tracking_supported(Script::Arabic));
        assert!(!tracking_supported(Script::Devanagari));
        assert!(!tracking_supported(Script::Khmer));
        assert!(tracking_supported(Script::Latin));
    }
    #[test]
    fn official_static_arabic_and_indic_fixtures_join_reorder_and_keep_native_engine_pixels() {
        let arabic = include_bytes!("../../tests/fonts/noto/NotoSansArabic-Regular.ttf");
        let indic = include_bytes!("../../tests/fonts/noto/NotoSansDevanagari-Regular.ttf");
        for (bytes, text, script) in [
            (arabic.as_slice(), "\u{0644}\u{0627}", Script::Arabic),
            (indic.as_slice(), "\u{0915}\u{093f}", Script::Devanagari),
        ] {
            let font = FontArc::try_from_vec(bytes.to_vec()).unwrap();
            let mut reference = reference(0, bytes);
            reference.family = if script == Script::Arabic {
                "Noto Sans Arabic"
            } else {
                "Noto Sans Devanagari"
            }
            .into();
            reference.postscript = if script == Script::Arabic {
                "NotoSansArabic-Regular"
            } else {
                "NotoSansDevanagari-Regular"
            }
            .into();
            let mut definition = source(text);
            if let Content::Text { font, .. } = &mut definition.content {
                *font = TextFont::Installed(reference.clone());
            }
            let actual = layout(&definition, &font).unwrap();
            if script == Script::Arabic {
                assert_eq!(
                    actual.len(),
                    1,
                    "Lam-alef forms one actual outline ligature"
                );
                assert_ne!(actual[0].id, font.glyph_id('\u{0644}'));
                assert_ne!(actual[0].id, font.glyph_id('\u{0627}'));
                let mut pair = definition.clone();
                if let Content::Text { text, .. } = &mut pair.content {
                    *text = "\u{0628}\u{0628}".into();
                }
                let joined = layout(&pair, &font).unwrap();
                assert_eq!(joined.len(), 2);
                assert_ne!(
                    joined[0].id, joined[1].id,
                    "Initial/final joining forms differ"
                );
                assert!(joined
                    .iter()
                    .all(|glyph| glyph.id != font.glyph_id('\u{0628}')));
                assert!(joined[0].position.x < joined[1].position.x);
                if let Content::Text { text, .. } = &mut pair.content {
                    *text = "\u{0644}\u{064e}\u{0627}".into();
                }
                let marks = layout(&pair, &font).unwrap();
                assert_eq!(
                    marks.len(),
                    2,
                    "Fatha is positioned with the lam-alef cluster"
                );
                assert!(marks
                    .iter()
                    .any(|glyph| glyph.position.y != actual[0].position.y));
            } else {
                assert!(actual.len() >= 2);
                assert_ne!(
                    actual[0].id,
                    font.glyph_id('\u{0915}'),
                    "I-matra must precede the consonant visually"
                );
                let consonant = actual
                    .iter()
                    .position(|glyph| glyph.id == font.glyph_id('\u{0915}'))
                    .expect("Ka is retained");
                assert!(consonant > 0);
                assert!(actual[0].position.x <= actual[consonant].position.x);
            }
            let mut tracked = definition.clone();
            if let Content::Text { letter_spacing, .. } = &mut tracked.content {
                *letter_spacing = 1.;
            }
            assert!(layout(&tracked, &font)
                .unwrap_err()
                .contains("letter spacing to zero"));
            for depth in [8, 16] {
                let guard = fonts::test_fonts(vec![(reference.clone(), Ok(font.clone()))]);
                let mut engine = crate::engine::Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = crate::engine::Document::new_depth(280, 120, depth).unwrap();
                let before = engine.resident_state().expect("resident fixture").document.export_png().unwrap();
                let command =
                    serde_json::json!({"op":"source.add","source":definition,"x":7,"y":9});
                let preview =
                    crate::engine::Engine::preview_edits(engine.resident_state().expect("resident fixture").document.clone(), &[command.clone()])
                        .unwrap();
                assert_eq!(guard.requests(), 1);
                let shared = std::sync::Arc::new(std::sync::Mutex::new(engine));
                let reply = crate::server::dispatch(&shared, "edit", &serde_json::json!({"actor":"human","commands":[command],"expected_revision":0,"feedback":"always","max_edge":280})).unwrap();
                use base64::Engine as _;
                let png = base64::engine::general_purpose::STANDARD
                    .decode(reply["images"][0]["data"].as_str().unwrap())
                    .unwrap();
                let (width, height, rgba, rect) = preview.preview(None, 280, None, false).unwrap();
                let feedback =
                    image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
                assert_eq!(feedback.color(), image::ColorType::Rgba8);
                assert_eq!((feedback.width(), feedback.height()), (width, height));
                assert_eq!(feedback.as_bytes(), rgba.as_slice());
                assert_eq!(reply["images"][0]["mime_type"], "image/png");
                assert_eq!(reply["images"][0]["width"], serde_json::json!(width));
                assert_eq!(reply["images"][0]["height"], serde_json::json!(height));
                assert_eq!(reply["images"][0]["document_rect"], serde_json::json!(rect));
                assert_eq!(reply["images"][0]["revision"], serde_json::json!(1));
                assert_eq!(reply["revision"], serde_json::json!(1));
                let observed = crate::server::dispatch(
                    &shared,
                    "observe",
                    &serde_json::json!({"image": false}),
                )
                .unwrap();
                assert_eq!(
                    observed["state"]["layers"][0]["source"]["content"]["layout"],
                    "shaped"
                );
                let mut engine = shared.lock().unwrap();
                assert_eq!(guard.requests(), 2, "Apply takes one new exact snapshot");
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].source, Some(definition.clone().into()));
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.depth, depth);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    preview.export_png().unwrap()
                );
                assert!(engine.resident_state().expect("resident fixture").document.layers[0]
                    .pixels
                    .rgba16()
                    .chunks_exact(4)
                    .any(|pixel| pixel[3] > 0));
                if depth == 16 {
                    assert!(engine.resident_state().expect("resident fixture").document.layers[0]
                        .pixels
                        .rgba16()
                        .iter()
                        .any(|word| word % 257 != 0));
                }
                for bytes in [
                    crate::psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    crate::psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                ] {
                    let reopened = crate::psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.bit_depth, depth);
                    assert_eq!(reopened.layers[0].source, Some(definition.clone().into()));
                    assert_eq!(
                        reopened.layers[0].pixels.rgba16(),
                        preview.layers[0].pixels.rgba16()
                    );
                    assert_eq!(
                        reopened.export_png().unwrap(),
                        preview.export_png().unwrap()
                    );
                    let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
                    let offset = 30 + color_len;
                    let resources =
                        u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                    let mut standard = bytes[..offset].to_vec();
                    standard.extend_from_slice(&0u32.to_be_bytes());
                    standard.extend_from_slice(&bytes[offset + 4 + resources..]);
                    let standard = crate::psd::decode(&standard).unwrap();
                    assert_eq!(standard.bit_depth, depth);
                    assert_eq!(
                        standard.layers[0].pixels.rgba16(),
                        preview.layers[0].pixels.rgba16()
                    );
                    assert_eq!(
                        standard.export_png().unwrap(),
                        preview.export_png().unwrap()
                    );
                }
                assert_eq!(
                    guard.requests(),
                    2,
                    "Saving never re-shapes baked native channels"
                );
                let id = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let before_reserved = serde_json::json!({"doc":engine.resident_state().expect("resident fixture").document,"png":engine.resident_state().expect("resident fixture").document.export_png().unwrap(),"undo":engine.resident_state().expect("resident fixture").undo.len(),"redo":engine.resident_state().expect("resident fixture").redo.len()});
                engine
                    .reserve(
                        "other",
                        "Preserve shaped text",
                        vec![crate::engine::Scope::layer(&id)],
                    )
                    .unwrap();
                assert!(engine
                    .edit(
                        "human",
                        &[serde_json::json!({"op":"source.update","layer":id,"source":definition})],
                        Some(1),
                        None,
                        "Reserved shaped text"
                    )
                    .is_err());
                assert_eq!(
                    serde_json::json!({"doc":engine.resident_state().expect("resident fixture").document,"png":engine.resident_state().expect("resident fixture").document.export_png().unwrap(),"undo":engine.resident_state().expect("resident fixture").undo.len(),"redo":engine.resident_state().expect("resident fixture").redo.len()}),
                    before_reserved
                );
                engine.leases.clear();
                // expected_revision permits disjoint edits; sampled source definitions
                // also require the exact source guard used by the native text editor.
                let document_id = engine.resident_state().expect("resident fixture").document.id.clone();
                assert!(engine
                    .edit(
                        "human",
                        &[serde_json::json!({"op":"source.update","layer":id,"source":definition,"document_id":document_id,"source_revision":0})],
                        Some(0),
                        None,
                        "Stale shaped text"
                    )
                    .unwrap_err()
                    .contains("The source project changed."));
                assert_eq!(
                    serde_json::json!({"doc":engine.resident_state().expect("resident fixture").document,"png":engine.resident_state().expect("resident fixture").document.export_png().unwrap(),"undo":engine.resident_state().expect("resident fixture").undo.len(),"redo":engine.resident_state().expect("resident fixture").redo.len()}),
                    before_reserved
                );
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), before);
                engine.redo("human").unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    preview.export_png().unwrap()
                );
                let saved = engine.resident_state().expect("resident fixture").document.clone();
                drop(engine);
                drop(guard);
                let missing = fonts::test_fonts(vec![(reference.clone(), Err(FontError::Missing))]);
                definition.validate_structure().unwrap();
                for bytes in [
                    crate::psd::encode(&saved).unwrap(),
                    crate::psd::encode_psb(&saved).unwrap(),
                ] {
                    let reopened = crate::psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].source, Some(definition.clone().into()));
                    assert_eq!(
                        reopened.layers[0].pixels.rgba16(),
                        saved.layers[0].pixels.rgba16()
                    );
                    assert_eq!(reopened.export_png().unwrap(), saved.export_png().unwrap());
                }
                assert_eq!(
                    missing.requests(),
                    0,
                    "Missing Arabic/Indic font does not discard or re-render stored text"
                );
                assert_eq!(
                    definition.render(256, 96, depth).unwrap_err(),
                    FontError::Missing.to_string()
                );
            }
        }
    }
    #[test]
    fn exact_collection_index_snapshot_and_missing_font_preserve_baked_native_sources() {
        let standalone = include_bytes!("../../tests/fonts/noto/NotoSansDevanagari-Regular.ttf");
        let mut faces = [
            include_bytes!("../../tests/fonts/noto/NotoSansArabic-Regular.ttf").to_vec(),
            standalone.to_vec(),
        ];
        for face in &mut faces {
            while face.len() % 4 != 0 {
                face.push(0);
            }
        }
        let mut bytes = b"ttcf\x00\x01\x00\x00\x00\x00\x00\x02".to_vec();
        let offsets = [20u32, 20 + faces[0].len() as u32];
        for offset in offsets {
            bytes.extend_from_slice(&offset.to_be_bytes());
        }
        for (mut face, offset) in faces.into_iter().zip(offsets) {
            let tables = u16::from_be_bytes(face[4..6].try_into().unwrap()) as usize;
            for index in 0..tables {
                let at = 12 + index * 16 + 8;
                let relative = u32::from_be_bytes(face[at..at + 4].try_into().unwrap());
                face[at..at + 4].copy_from_slice(&(relative + offset).to_be_bytes());
            }
            bytes.extend(face);
        }
        let mut reference = reference(1, &bytes);
        reference.family = "Noto Sans Devanagari".into();
        reference.postscript = "NotoSansDevanagari-Regular".into();
        let mut standalone_reference = reference.clone();
        standalone_reference.index = 0;
        standalone_reference.sha256 = ring::digest::digest(&ring::digest::SHA256, standalone)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let exact: FontArc = ab_glyph::FontVec::try_from_vec_and_index(bytes.clone(), 1)
            .unwrap()
            .into();
        let independent = FontArc::try_from_vec(standalone.to_vec()).unwrap();
        let wrong: FontArc = ab_glyph::FontVec::try_from_vec_and_index(bytes, 0)
            .unwrap()
            .into();
        assert_eq!(
            wrong.glyph_id('\u{0915}').0,
            0,
            "The first collection face cannot shape this Devanagari input"
        );
        for depth in [8, 16] {
            let mut installed = source("\u{0915}\u{093f}");
            if let Content::Text { font, .. } = &mut installed.content {
                *font = TextFont::Installed(reference.clone());
            }
            let expected = {
                let _guard = fonts::test_fonts(vec![(
                    standalone_reference.clone(),
                    Ok(independent.clone()),
                )]);
                let mut definition = installed.clone();
                if let Content::Text { font, .. } = &mut definition.content {
                    *font = TextFont::Installed(standalone_reference.clone());
                }
                definition.render(256, 96, depth).unwrap()
            };
            let baked = {
                let guard = fonts::test_fonts(vec![(reference.clone(), Ok(exact.clone()))]);
                let result = installed.render(256, 96, depth).unwrap();
                assert_eq!(
                    guard.requests(),
                    1,
                    "render resolves one selected collection face"
                );
                assert_eq!(result.rgba16(), expected.rgba16());
                result
            };
            let mut engine = crate::engine::Engine::new();
            engine.resident_state_mut().expect("resident fixture").document = crate::engine::Document::new_depth(256, 96, depth).unwrap();
            engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels = baked;
            engine.resident_state_mut().expect("resident fixture").document.layers[0].source = Some(installed.clone().into());
            for error in [
                FontError::Missing,
                FontError::Changed,
                FontError::Unsupported,
            ] {
                let guard = fonts::test_fonts(vec![(reference.clone(), Err(error))]);
                installed.validate_structure().unwrap();
                assert_eq!(
                    installed.render(256, 96, depth).unwrap_err(),
                    error.to_string()
                );
                assert_eq!(guard.requests(), 1);
                for bytes in [
                    crate::psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    crate::psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                ] {
                    let reopened = crate::psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].source, Some(installed.clone().into()));
                    assert_eq!(reopened.layers[0].pixels.rgba16(), expected.rgba16());
                    assert_eq!(
                        reopened.export_png().unwrap(),
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap()
                    );
                }
                assert_eq!(guard.requests(), 1, "storage never resolves a missing font");
            }
        }
    }
    #[test]
    fn malformed_controls_coverage_and_coordinate_work_fail_before_native_projection() {
        for text in [
            "x\t",
            "x\u{202e}y",
            "x\u{2066}y",
            "x\u{2028}y",
            "x\u{10ffff}",
        ] {
            assert!(source(text).render(256, 96, 16).is_err());
        }
        let mut wide = source(&"W".repeat(4096));
        if let Content::Text {
            size,
            letter_spacing,
            ..
        } = &mut wide.content
        {
            *size = 1024.;
            *letter_spacing = 1024.;
        }
        assert!(wide
            .render(256, 96, 16)
            .unwrap_err()
            .contains("coordinate limit"));
        let mut high = source(&vec!["W"; 96].join("\n"));
        if let Content::Text {
            size, line_height, ..
        } = &mut high.content
        {
            *size = 1024.;
            *line_height = 4.;
        }
        assert!(high.render(256, 96, 16).is_err());
        let mut invalid = serde_json::to_value(source("text")).unwrap();
        invalid["content"]["layout"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<Source>(invalid).is_err());
        for text in ["", "\n", "\n\n", "A\n"] {
            assert!(source(text).render(256, 96, 16).is_ok());
        }
        assert!(source(&"\n".repeat(96)).render(256, 96, 16).is_err());
    }
    #[test]
    fn actual_glyph_count_and_supported_base_variation_sequences_fail_closed() {
        let font = fonts::snapshot(&"regular".into()).unwrap();
        assert!(font.glyph_count() > 1);
        for id in [
            0,
            font.glyph_count() as u32,
            u32::from(u16::MAX) + 1,
            u32::MAX,
        ] {
            assert!(checked_glyph(&font, id).is_err());
        }
        assert_eq!(
            checked_glyph(&font, font.glyph_count() as u32 - 1)
                .unwrap()
                .0 as usize,
            font.glyph_count() - 1
        );
        let space = font.glyph_id(' ');
        assert_eq!(checked_glyph(&font, u32::from(space.0)).unwrap(), space);
        assert!(
            source("A ").render(256, 96, 16).is_ok(),
            "Valid blank glyphs retain advances"
        );
        let space_and_base = layout(&source(" A"), &font).unwrap();
        let space_mark_and_base = layout(&source(" \u{0301}A"), &font).unwrap();
        let a = font.glyph_id('A');
        let expected = space_and_base.iter().find(|glyph| glyph.id == a).unwrap();
        let actual = space_mark_and_base
            .iter()
            .find(|glyph| glyph.id == a)
            .unwrap();
        assert_eq!(
            actual.position, expected.position,
            "Space retains its advance even when sharing a mark cluster"
        );
        assert!(
            space_mark_and_base.len() > space_and_base.len(),
            "The combining mark is retained as an outline"
        );
        for depth in [8, 16] {
            assert!(source(" \u{0301}A")
                .render(256, 96, depth)
                .unwrap()
                .rgba16()
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 0));
        }
        for text in [
            "A\u{fe0f}",
            "A\u{e0100}",
            "\u{fe0f}",
            "\u{e0100}",
            "A\u{fe0f}\u{fe0f}",
            "A\n\u{fe0f}",
        ] {
            for depth in [8, 16] {
                assert!(
                    source(text).render(256, 96, depth).is_err(),
                    "Unsupported, standalone and repeated selectors must not disappear"
                );
            }
        }
    }
}
