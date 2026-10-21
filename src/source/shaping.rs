//! Opt-in horizontal shaping. Historical character layout remains a separate path.
use super::{outline_work, spacing, Content, Source};
use ab_glyph::{point, Font, FontArc, Glyph, GlyphId, PxScale, ScaleFont};
use harfrust::{BufferFlags, Direction, ShapeOptions, ShaperData, UnicodeBuffer};
use std::ops::Range;
use unicode_script::{Script, UnicodeScript};

const GLYPHS: usize = 16384;
const COORDINATE: f64 = 100000.;

#[derive(Debug, PartialEq)]
struct Run {
    range: Range<usize>,
    script: Script,
    rtl: bool,
}

fn runs(text: &str) -> Vec<Run> {
    let bidi = unicode_bidi::BidiInfo::new(text, None);
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

fn checked_glyph(font: &FontArc, value: u32) -> Result<GlyphId, String> {
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

pub(super) fn layout(source: &Source, font: &FontArc) -> Result<Vec<Glyph>, String> {
    let Content::Text {
        text,
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
    let scale = f64::from(*size) / height;
    let scaled = font.as_scaled(PxScale::from(*size));
    let data = ShaperData::new(&face);
    let shaper = data.shaper(&face).build();
    let mut glyph_positions = Vec::new();
    let mut glyphs = 0usize;
    let mut run_count = 0usize;
    let mut work = 0u64;
    for (row, line) in text.split('\n').enumerate() {
        let baseline = f64::from(scaled.ascent())
            + row as f64 * f64::from(scaled.height()) * f64::from(*line_height);
        coordinate(baseline)?;
        let mut positioned = Vec::new();
        let mut advance = 0f64;
        let mut have_cluster = false;
        for run in runs(line) {
            run_count += 1;
            if run_count > 4096 {
                return Err("Shaped text exceeds the run budget".into());
            }
            if *letter_spacing != 0. && !tracking_supported(run.script) {
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
                shaper.shape(buffer, ShapeOptions::new())
            }))
            .map_err(|_| "The selected font could not shape this text")?;
            glyphs = glyphs
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
                let id = checked_glyph(font, info.glyph_id)?;
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
                        advance += f64::from(*letter_spacing);
                    }
                    have_cluster = true;
                }
                let cluster = info.cluster as usize;
                let at = clusters
                    .binary_search(&cluster)
                    .map_err(|_| "Invalid shaped text cluster")?;
                let end = clusters.get(at + 1).copied().unwrap_or(part.len());
                positioned.push(Positioned {
                    id,
                    x: advance + f64::from(position.x_offset) * scale,
                    y: baseline - vertical - f64::from(position.y_offset) * scale,
                    blank: part[cluster..end]
                        .chars()
                        .all(|ch| spacing(ch) || matches!(ch, '\u{200c}' | '\u{200d}'))
                        || part[cluster..end]
                            .chars()
                            .any(|ch| spacing(ch) && font.glyph_id(ch) == id),
                });
                advance += f64::from(position.x_advance) * scale;
                vertical += f64::from(position.y_advance) * scale;
                coordinate(advance)?;
                coordinate(vertical)?;
                previous = Some(info.cluster);
            }
        }
        let start = match align.as_str() {
            "center" => (f64::from(source.width) - advance) * 0.5,
            "right" => f64::from(source.width) - advance,
            _ => 0.,
        };
        coordinate(start)?;
        for glyph in positioned {
            let glyph_position = point(coordinate(start + glyph.x)?, coordinate(glyph.y)?);
            let positioned = glyph.id.with_scale_and_position(*size, glyph_position);
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
        let actual = runs(text);
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
            runs(same_direction)
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
        let actual = runs(common);
        assert!(actual
            .iter()
            .all(|run| run.script == Script::Hebrew && run.rtl));
        assert_eq!(
            actual.iter().map(|run| run.range.len()).sum::<usize>(),
            common.len()
        );
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
                engine.doc = crate::engine::Document::new_depth(280, 120, depth).unwrap();
                let before = engine.doc.export_png().unwrap();
                let command =
                    serde_json::json!({"op":"source.add","source":definition,"x":7,"y":9});
                let preview =
                    crate::engine::Engine::preview_edits(engine.doc.clone(), &[command.clone()])
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
                assert_eq!(engine.undo.len(), 1);
                assert_eq!(engine.doc.layers[0].source, Some(definition.clone()));
                assert_eq!(engine.doc.layers[0].pixels.depth, depth);
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    preview.export_png().unwrap()
                );
                assert!(engine.doc.layers[0]
                    .pixels
                    .rgba16()
                    .chunks_exact(4)
                    .any(|pixel| pixel[3] > 0));
                if depth == 16 {
                    assert!(engine.doc.layers[0]
                        .pixels
                        .rgba16()
                        .iter()
                        .any(|word| word % 257 != 0));
                }
                for bytes in [
                    crate::psd::encode(&engine.doc).unwrap(),
                    crate::psd::encode_psb(&engine.doc).unwrap(),
                ] {
                    let reopened = crate::psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.bit_depth, depth);
                    assert_eq!(reopened.layers[0].source, Some(definition.clone()));
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
                let id = engine.doc.layers[0].id.clone();
                let before_reserved = serde_json::json!({"doc":engine.doc,"png":engine.doc.export_png().unwrap(),"undo":engine.undo.len(),"redo":engine.redo.len()});
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
                    serde_json::json!({"doc":engine.doc,"png":engine.doc.export_png().unwrap(),"undo":engine.undo.len(),"redo":engine.redo.len()}),
                    before_reserved
                );
                engine.leases.clear();
                assert!(engine
                    .edit(
                        "human",
                        &[serde_json::json!({"op":"source.update","layer":id,"source":definition})],
                        Some(0),
                        None,
                        "Stale shaped text"
                    )
                    .is_err());
                assert_eq!(
                    serde_json::json!({"doc":engine.doc,"png":engine.doc.export_png().unwrap(),"undo":engine.undo.len(),"redo":engine.redo.len()}),
                    before_reserved
                );
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.export_png().unwrap(), before);
                engine.redo("human").unwrap();
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    preview.export_png().unwrap()
                );
                let saved = engine.doc.clone();
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
                    assert_eq!(reopened.layers[0].source, Some(definition.clone()));
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
            engine.doc = crate::engine::Document::new_depth(256, 96, depth).unwrap();
            engine.doc.layers[0].pixels = baked;
            engine.doc.layers[0].source = Some(installed.clone());
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
                    crate::psd::encode(&engine.doc).unwrap(),
                    crate::psd::encode_psb(&engine.doc).unwrap(),
                ] {
                    let reopened = crate::psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].source, Some(installed.clone()));
                    assert_eq!(reopened.layers[0].pixels.rgba16(), expected.rgba16());
                    assert_eq!(
                        reopened.export_png().unwrap(),
                        engine.doc.export_png().unwrap()
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
