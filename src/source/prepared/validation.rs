//! Borrowed definition checks. No font-file lookup or resident raster render.
use crate::{
    engine::scene::{Content36, Source36},
    large_document::ResidentBudget,
    source::TextLayout,
};
pub(super) fn matrix(m: [f32; 6]) -> Result<(), String> {
    if m.iter().any(|v| !v.is_finite() || v.abs() > 100000.)
        || (m[0] * m[3] - m[1] * m[2]).abs() < 0.000001
    {
        Err("Invalid editable-source transform".into())
    } else {
        Ok(())
    }
}
pub(crate) fn validate_definition(
    source: &Source36,
    pool: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    matrix(source.matrix)?;
    if source.width == 0 || source.height == 0 || source.width > 16000 || source.height > 16000 {
        return Err("Editable native source frame exceeds admission".into());
    }
    match source.definition() {
        Content36::Bezier {
            nodes,
            closed,
            stroke_width,
            ..
        } => {
            if !stroke_width.is_finite() || !(0. ..=256.).contains(stroke_width) {
                return Err("Invalid path stroke width".into());
            }
            let _work = pool
                .reserve_bytes(256 * 1024, 256 * 1024)
                .map_err(|e| e.to_string())?;
            let points = crate::path::flatten(nodes, *closed)?;
            check()?;
            drop(points);
        }
        Content36::Shape {
            shape,
            bounds,
            points,
            stroke_width,
            ..
        } => {
            if !["rectangle", "ellipse", "path"].contains(&shape.as_str())
                || bounds.iter().any(|v| !v.is_finite() || v.abs() > 100000.)
                || bounds[0] >= bounds[2]
                || bounds[1] >= bounds[3]
                || !stroke_width.is_finite()
                || !(0. ..=256.).contains(stroke_width)
                || points.len() > 128
                || points
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || v.abs() > 100000.)
                || (shape == "path" && points.len() < 2)
            {
                return Err("Invalid vector geometry or stroke; paths need2..128 points".into());
            }
        }
        Content36::Text {
            text,
            layout,
            font,
            size,
            line_height,
            letter_spacing,
            align,
            ..
        } => {
            if text.len() > 16384
                || text.chars().count() > 4096
                || text.lines().count() > 96
                || (*layout == TextLayout::Shaped && text.split('\n').count() > 96)
                || !size.is_finite()
                || !(1. ..=1024.).contains(size)
                || !line_height.is_finite()
                || !(0.5..=4.).contains(line_height)
                || !letter_spacing.is_finite()
                || !(-1024. ..=1024.).contains(letter_spacing)
                || !["left", "center", "right"].contains(&align.as_str())
            {
                return Err("Invalid text length, size, line/letter spacing or alignment".into());
            }
            font.validate().map_err(|e| e.to_string())?;
            for ch in text.chars().filter(|c| *c != '\n') {
                if ch.is_control()
                    || matches!(ch as u32,0x200e..=0x200f|0x202a..=0x202e|0x2066..=0x2069)
                    || (*layout == TextLayout::Shaped
                        && matches!(ch, '\u{061c}' | '\u{2028}' | '\u{2029}'))
                    || (*layout == TextLayout::Legacy
                        && (matches!(ch as u32, 0x200c..=0x200d)
                            || (font.bundled_name().is_some()
                                && matches!(ch as u32,0x0300..=0x036f|0x0590..=0x08ff|0x0900..=0x0fff))))
                {
                    return Err("This text needs an unsupported glyph or script shaping".into());
                }
            }
            if *layout == TextLayout::Legacy && matches!(font, crate::fonts::TextFont::Installed(_))
            {
                super::super::validate_installed_text(text)?;
            }
        }
        Content36::Svg { svg } => {
            drop(crate::svg::prepared::PreparedSvg::new(
                svg,
                [source.width, source.height],
                pool,
                check,
            )?);
        }
    }
    check()
}
