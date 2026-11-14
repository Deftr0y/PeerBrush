//! Integer tonal quantization shared by the native 8/16-bit effect paths.
use serde_json::Value;

pub const DEFAULT_LEVELS: u16 = 4;
pub(crate) const PHOTOSHOP_MAX_LEVELS: u16 = 255;
pub(crate) const PHOTOSHOP_NATIVE: &str = "photoshop_native";

pub(crate) fn source_version(kind: &str, settings: &Value) -> Option<u8> {
    if kind != "posterize" {
        return None;
    }
    match settings["mapping"].as_str()? {
        "photoshop" => Some(1),
        PHOTOSHOP_NATIVE => Some(2),
        _ => None,
    }
}

pub(crate) struct Quantizer {
    pub levels: u16,
    version: Option<u8>,
}

impl Quantizer {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        let levels = levels(settings)?;
        let version = source_version("posterize", settings);
        if settings.get("mapping").is_some() && version.is_none() {
            return Err("Unknown Posterize mapping".into());
        }
        if version.is_some() && levels > PHOTOSHOP_MAX_LEVELS {
            return Err("Photoshop Posterize mapping requires 2–255 levels".into());
        }
        Ok(Self { levels, version })
    }

    pub(crate) fn standard_at_depth(&self, depth: u16) -> bool {
        match depth {
            8 => self.version.is_some(),
            16 => self.version == Some(2),
            _ => false,
        }
    }

    pub(crate) fn sample(&self, value: u16, max: u16) -> u16 {
        if self.version.is_none() {
            return sample(value, max, self.levels);
        }
        let levels = u32::from(self.levels);
        if max == u16::MAX && self.version == Some(2) {
            // Photoshop selects the bucket and floors the palette step on its
            // 0–32768 scale. The stored 16-bit result uses the full word range.
            let scale = super::PHOTOSHOP_WORD_SCALE;
            let value = super::photoshop_word(value);
            let bucket = (value * levels / scale).min(levels - 1);
            let step = bucket * scale / (levels - 1);
            return ((step * 65535 + scale / 2) / scale) as u16;
        }
        let max = u32::from(max);
        let bucket = u32::from(value) * levels / (max + 1);
        (bucket * max / (levels - 1)) as u16
    }
}

pub fn levels(settings: &Value) -> Result<u16, String> {
    match settings.get("levels") {
        None => Ok(DEFAULT_LEVELS),
        Some(value) => {
            let n = value
                .as_f64()
                .ok_or("Posterize Levels must be an integer from 2 to 256")?;
            if !n.is_finite() || !(2.0..=256.0).contains(&n) || n.fract() != 0.0 {
                return Err("Posterize Levels must be an integer from 2 to 256".into());
            }
            Ok(n as u16)
        }
    }
}

/// Choose the nearest of `levels` evenly spaced native channel samples.
/// Division stays integer at both channel depths; alpha never enters this map.
pub fn sample(value: u16, max: u16, levels: u16) -> u16 {
    let intervals = u64::from(levels - 1);
    let max = u64::from(max);
    let index = (u64::from(value) * intervals + max / 2) / max;
    ((index * max + intervals / 2) / intervals) as u16
}
