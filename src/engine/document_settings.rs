//! Document settings use the same dimensions and precision policy in every engine.
use crate::raster::CanvasLimits;
use serde_json::Value;

pub(crate) const PATTERN_PRECISION: &str = "Replace 16-bit bitmap patterns with explicitly converted 8-bit PNGs before lowering document precision";

pub(super) fn global_light_angle(command: &Value) -> Result<u32, String> {
    if !command["layer"].is_null() || !command["rect"].is_null() {
        return Err("Global Light applies to the whole document".into());
    }
    command["angle"].as_u64().filter(|&angle| angle < 360).map(|angle| angle as u32)
        .ok_or_else(|| "Global Light angle must be a whole number from 0 to 359".into())
}

pub(super) fn may_initialize_global_light(angle: Option<[u8; 4]>, command: &Value) -> bool {
    angle.is_none() && command["use_global_light"] == true
        && matches!(command["op"].as_str(), Some("effect.add" | "effect.update"))
}

/// Source precision is not known until worker decoding. Promotion changes all
/// color/mask sources, so capture must protect the document before that I/O.
pub(crate) fn may_promote(depth: u16, command: &Value) -> bool {
    depth == 8
        && (matches!(
            command["op"].as_str(),
            Some("image.import" | "image.place" | "image.paste" | "layer.paste")
        ) || (matches!(command["op"].as_str(), Some("effect.add" | "effect.update"))
            && command["settings"]["bitmap_png"].is_string()))
}

pub(crate) struct Settings {
    pub size: [u32; 2],
    pub depth: u16,
}
impl Settings {
    pub(crate) fn new(
        size: [u32; 2],
        depth: u16,
        command: &Value,
        limits: CanvasLimits,
    ) -> Result<Self, String> {
        if command
            .get("rect")
            .is_some_and(|r| !r.is_null() && super::rect(r).is_none())
        {
            return Err("Invalid rectangle coordinates".into());
        }
        let w = command["width"].as_u64().unwrap_or(u64::from(size[0]));
        let h = command["height"].as_u64().unwrap_or(u64::from(size[1]));
        if w > u64::from(u32::MAX) || h > u64::from(u32::MAX) {
            return Err("Invalid canvas dimensions".into());
        }
        limits.check(w as u32, h as u32)?;
        let depth = command["bit_depth"].as_u64().unwrap_or(u64::from(depth));
        if ![8, 16].contains(&depth) {
            return Err("Choose 8 or 16 bit channels".into());
        }
        Ok(Self {
            size: [w as u32, h as u32],
            depth: depth as u16,
        })
    }
}
