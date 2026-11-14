//! Photoshop's RGB channel exclusions in the standard brst block.
use super::LayerMetadata;

pub(crate) const ALL: u8 = 7;
pub(super) const STYLED_CLIPPING_FORMAT: u32 = 77;

pub(crate) fn parse(payload: &[u8]) -> Option<u8> {
    if payload.len() > 12 || payload.len() % 4 != 0 {
        return None;
    }
    let mut excluded = 0;
    for id in payload.chunks_exact(4) {
        let index = u32::from_be_bytes(id.try_into().unwrap());
        if index > 2 || excluded & (1 << index) != 0 {
            return None;
        }
        excluded |= 1 << index;
    }
    Some(excluded)
}

pub(crate) fn excluded(metadata: &[LayerMetadata]) -> u8 {
    metadata
        .iter()
        .find(|entry| entry.key == "brst")
        .and_then(|entry| parse(&entry.data))
        .unwrap_or(0)
}

pub(crate) fn enabled(metadata: &[LayerMetadata]) -> [bool; 3] {
    let excluded = excluded(metadata);
    std::array::from_fn(|channel| excluded & (1 << channel) == 0)
}

pub(super) fn source_format(doc: &crate::engine::Document, layer: &crate::engine::Layer) -> u32 {
    if excluded(&layer.psd_metadata) == 0 {
        return 0;
    }
    let base = layer.clip_to.as_ref().and_then(|id| doc.layers.iter().find(|node| &node.id == id));
    let in_stack = layer.clip_to.is_some()
        || doc.layers.iter().any(|member| member.clip_to.as_deref() == Some(&layer.id));
    let styled_paint = |layer: &crate::engine::Layer| layer.kind == "paint" && !layer.effects.is_empty();
    if in_stack && (styled_paint(layer) || base.is_some_and(styled_paint)) {
        STYLED_CLIPPING_FORMAT
    } else if styled_paint(layer) {
        73
    } else if layer.kind == "group" && layer.blend == "pass_through" {
        72
    } else if in_stack {
        71
    } else {
        70
    }
}

pub(crate) fn update(
    metadata: &mut Vec<LayerMetadata>,
    command: &serde_json::Value,
) -> Result<(), String> {
    let Some(input) = command.get("blend_channels") else {
        return Ok(());
    };
    let invalid = "Blend channels requires three RGB booleans";
    let rgb = input
        .as_array()
        .filter(|rgb| rgb.len() == 3)
        .ok_or(invalid)?;
    let mut data = Vec::new();
    for (channel, enabled) in (0u32..3).zip(rgb) {
        if !enabled.as_bool().ok_or(invalid)? {
            data.extend_from_slice(&channel.to_be_bytes());
        }
    }
    if excluded(metadata) == parse(&data).unwrap() {
        return Ok(());
    }
    metadata.retain(|entry| entry.key != "brst");
    if !data.is_empty() {
        metadata.push(LayerMetadata {
            key: "brst".into(),
            data,
        });
    }
    super::validate_metadata(metadata)
}

pub(crate) fn supported<'a>(
    kind: &str,
    blend: &str,
    mut effects: impl Iterator<Item = &'a str>,
    unsupported_base: bool,
) -> bool {
    !unsupported_base
        && (kind == "group" || blend != "pass_through")
        && match kind {
            "adjustment" => true,
            "paint" => overlay_stack(effects),
            "fill" | "group" => effects.next().is_none(),
            _ => false,
        }
}

pub(super) fn overlay_stack<'a>(mut effects: impl Iterator<Item = &'a str>) -> bool {
    effects.all(|effect| matches!(effect, "color_overlay" | "gradient_overlay" | "pattern_overlay"))
}

pub(crate) fn supports_clip_base<'a>(
    kind: &str,
    mut effects: impl Iterator<Item = &'a str>,
    grouped: bool,
) -> bool {
    grouped && match kind {
        "paint" => overlay_stack(effects),
        "fill" => effects.next().is_none(),
        _ => false,
    }
}

pub(crate) fn validate_layer<'a>(
    metadata: &[LayerMetadata],
    kind: &str,
    blend: &str,
    effects: impl Iterator<Item = &'a str>,
    unsupported_base: bool,
) -> Result<(), String> {
    if excluded(metadata) != 0 && !supported(kind, blend, effects, unsupported_base) {
        return Err("Blend channels (brst) support adjustments, unstyled Solid Color and folders, and paint with only Color, Gradient or Pattern Overlay effects. Clipping bases must use grouped blending".into());
    }
    Ok(())
}

pub(crate) fn supports_clipped_member<'a>(
    kind: &str,
    base_kind: &str,
    base_effects: impl Iterator<Item = &'a str>,
    grouped: bool,
) -> bool {
    matches!(kind, "paint" | "fill" | "adjustment") && supports_clip_base(base_kind, base_effects, grouped)
}

pub(crate) fn validate_clipped_member<'a>(
    metadata: &[LayerMetadata],
    kind: &str,
    base_kind: &str,
    base_effects: impl Iterator<Item = &'a str>,
    grouped: bool,
) -> Result<(), String> {
    if excluded(metadata) != 0
        && !supports_clipped_member(kind, base_kind, base_effects, grouped)
    {
        return Err("Clipped blend channels (brst) require a paint, Solid Color or adjustment member over grouped Solid Color or grouped paint with only Color, Gradient or Pattern Overlay effects".into());
    }
    Ok(())
}

pub(crate) fn keep8(before: [u8; 4], after: [u8; 4], excluded: u8) -> [u8; 4] {
    if excluded == 0 {
        return after;
    }
    keep(before.map(u16::from), after.map(u16::from), excluded, 255).map(|sample| sample as u8)
}

pub(crate) fn keep(before: [u16; 4], mut after: [u16; 4], excluded: u8, max: u16) -> [u16; 4] {
    if excluded == ALL {
        return before;
    }
    if excluded == 0 || after[3] == 0 {
        return after;
    }
    let alpha = u32::from(after[3]);
    for channel in 0..3 {
        if excluded & (1 << channel) != 0 {
            let premultiplied = u32::from(before[channel]) * u32::from(before[3]);
            after[channel] = ((premultiplied + alpha / 2) / alpha).min(u32::from(max)) as u16;
        }
    }
    after
}

pub(crate) fn keep_clipped8(
    before: [u8; 4],
    after: [u8; 4],
    excluded: u8,
    before_visible: f64,
    after_visible: f64,
) -> [u8; 4] {
    if excluded == 0 {
        return after;
    }
    keep_clipped(
        before.map(u16::from),
        after.map(u16::from),
        excluded,
        before_visible,
        after_visible,
    )
    .map(|sample| sample as u8)
}

pub(crate) fn keep_clipped(
    before: [u16; 4],
    mut after: [u16; 4],
    excluded: u8,
    before_visible: f64,
    after_visible: f64,
) -> [u16; 4] {
    if excluded == 0 || after_visible == 0. {
        return after;
    }
    // The shared clipping shape cancels from the two premultiplied terms.
    for channel in 0..3 {
        if excluded & (1 << channel) != 0 {
            after[channel] = if before_visible == after_visible {
                before[channel]
            } else {
                (f64::from(before[channel]) * before_visible / after_visible).round() as u16
            };
        }
    }
    after
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Document, Layer};

    #[test]
    fn clipped_adjustment_retains_its_original_source_format_over_unstyled_paint() {
        let mut doc = Document::new(1, 1).unwrap();
        let mut member = Layer::new("Invert", "adjustment", 1, 1);
        member.clip_to = Some(doc.layers[0].id.clone());
        member.effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "invert".into(),
            enabled: true,
            weight: 1.0,
            settings: serde_json::json!({}).into(),
        });
        update(&mut member.psd_metadata, &serde_json::json!({
            "blend_channels": [true, false, true],
        })).unwrap();
        doc.layers.insert(0, member);
        assert_eq!(source_format(&doc, &doc.layers[0]), 71);
        doc.layers[1].effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "color_overlay".into(),
            enabled: true,
            weight: 1.0,
            settings: serde_json::json!({}).into(),
        });
        assert_eq!(source_format(&doc, &doc.layers[0]), STYLED_CLIPPING_FORMAT);
    }
}
