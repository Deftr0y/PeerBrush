//! Standard Photoshop clipping flags retained with editable layer metadata.
use super::LayerMetadata;

pub(crate) fn grouped(metadata: &[LayerMetadata]) -> bool {
    !metadata.iter().any(|entry| entry.key == "clbl")
}

pub(crate) fn update(
    metadata: &mut Vec<LayerMetadata>,
    command: &serde_json::Value,
) -> Result<(), String> {
    let Some(input) = command.get("blend_clipped_as_group") else {
        return Ok(());
    };
    let enabled = input
        .as_bool()
        .ok_or("Blend Clipped Layers As Group requires a boolean")?;
    if enabled == grouped(metadata) {
        return Ok(());
    }
    metadata.retain(|entry| entry.key != "clbl");
    if !enabled {
        metadata.push(LayerMetadata {
            key: "clbl".into(),
            data: vec![0; 4],
        });
    }
    super::validate_metadata(metadata)
}

pub(crate) fn validate_base(
    metadata: &[LayerMetadata],
    kind: &str,
    blend: &str,
    has_effects: bool,
) -> Result<(), String> {
    if !supports_disabled(
        kind,
        blend,
        has_effects,
        super::fill_opacity::value(metadata),
    ) {
        return Err("Enable Blend Clipped Layers As Group for a base with this blend, effects or partial Fill".into());
    }
    Ok(())
}

pub(crate) fn supports_disabled(kind: &str, blend: &str, has_effects: bool, fill: u8) -> bool {
    matches!(kind, "paint" | "fill") && blend == "normal" && !has_effects && fill == 255
}

pub(crate) fn supports_advanced_base(kind: &str, has_effects: bool, grouped: bool) -> bool {
    matches!(kind, "paint" | "fill") && !has_effects && grouped
}

pub(crate) fn supports_advanced_member(
    kind: &str,
    base_kind: &str,
    base_has_effects: bool,
    grouped: bool,
) -> bool {
    matches!(kind, "paint" | "fill" | "adjustment")
        && supports_advanced_base(base_kind, base_has_effects, grouped)
}
