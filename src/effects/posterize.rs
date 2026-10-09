//! Integer tonal quantization shared by the native 8/16-bit effect paths.
use serde_json::Value;

pub const DEFAULT_LEVELS: u16 = 4;
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
