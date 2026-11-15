//! The standard Photoshop fill-opacity tag owns the editable byte.
use super::LayerMetadata;
use crate::raster::fill_blend::SpecialFill;

pub(super) const INACTIVE_EFFECTS_FORMAT: u32 = 76;

pub(super) fn source_format(layer: &crate::engine::Layer) -> u32 {
    if value(&layer.psd_metadata) != 255 && !layer.effects.is_empty() {
        INACTIVE_EFFECTS_FORMAT
    } else {
        0
    }
}

pub(crate) fn value(metadata: &[LayerMetadata]) -> u8 {
    metadata
        .iter()
        .find(|entry| entry.key == "iOpa")
        .and_then(|entry| parse(&entry.data))
        .unwrap_or(255)
}

pub(crate) fn render(
    opacity: f32,
    blend: &str,
    metadata: &[LayerMetadata],
) -> (f64, Option<SpecialFill>) {
    let fill = value(metadata);
    if let Some(special) = SpecialFill::new(blend, fill) {
        (f64::from(opacity), Some(special))
    } else {
        (f64::from(opacity) * (f64::from(fill) / 255.), None)
    }
}

pub(super) fn parse(bytes: &[u8]) -> Option<u8> {
    match bytes {
        [amount] | [amount, 0, 0, 0] => Some(*amount),
        _ => None,
    }
}

pub(crate) fn update(
    metadata: &mut Vec<LayerMetadata>,
    command: &serde_json::Value,
) -> Result<(), String> {
    let Some(input) = command.get("fill_opacity") else {
        return Ok(());
    };
    let amount = input
        .as_u64()
        .and_then(|amount| u8::try_from(amount).ok())
        .ok_or("Fill opacity must be an integer from 0 to 255")?;
    if amount == value(metadata) {
        return Ok(());
    }
    metadata.retain(|entry| entry.key != "iOpa");
    if amount != 255 {
        metadata.push(LayerMetadata {
            key: "iOpa".into(),
            data: vec![amount, 0, 0, 0],
        });
    }
    super::validate_metadata(metadata)
}

pub(crate) fn supported(kind: &str, blend: &str, mut effects: impl Iterator<Item = bool>) -> bool {
    blend != "pass_through" && match kind {
        "paint" => effects.all(|enabled| !enabled),
        "fill" => effects.next().is_none(),
        _ => false,
    }
}

pub(crate) fn validate(
    amount: u8,
    kind: &str,
    blend: &str,
    effects: impl Iterator<Item = bool>,
) -> Result<(), String> {
    if amount != 255 && !supported(kind, blend, effects) {
        return Err("Partial Fill requires paint with no enabled effects or a Solid Color layer without effects. Restore full Fill before enabling a paint effect".into());
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::psd) fn clipped_fixture(depth: u16) -> (crate::engine::Document, [u16; 4]) {
    use crate::{
        engine::{Document, Engine, Layer},
        raster::Raster,
    };
    use serde_json::json;

    let (base, follower, expected) = if depth == 8 {
        (
            [80, 120, 200, 128].map(|sample| sample * 257),
            [170, 36, 128, 255].map(|sample| sample * 257),
            [122, 27, 200, 64],
        )
    } else {
        (
            [20483, 30149, 50539, 32769],
            [43009, 9287, 32768, 65535],
            [30764, 6575, 50539, 16449],
        )
    };
    let mut doc = Document::new_depth(2, 1, depth).unwrap();
    let base_id = doc.layers[0].id.clone();
    doc.layers[0].pixels.set16(0, 0, base);
    let mut top = Layer::new("Follower", "paint", 2, 1);
    top.pixels = Raster::new_depth(2, 1, depth);
    top.pixels.set16(0, 0, follower);
    top.pixels.set16(1, 0, follower);
    let top_id = top.id.clone();
    doc.layers.insert(0, top);
    let doc = Engine::preview_edits(
        doc,
        &[
            json!({"op":"layer.clip","layer":top_id,"base":base_id}),
            json!({"op":"layer.update","layer":base_id,"fill_opacity":128}),
            json!({"op":"layer.update","layer":top_id,"fill_opacity":128,"blend":"linear_light"}),
        ],
    )
    .unwrap();
    (doc, expected)
}
