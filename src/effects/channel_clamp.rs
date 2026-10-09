//! Clamp one native channel without remapping in-range samples or other channels.
use serde_json::Value;

pub fn parameters(settings: &Value) -> Result<(usize, f64, f64), String> {
    let channel = match settings.get("channel") {
        None => 0,
        Some(value) => match value.as_str() {
            Some("r") => 0,
            Some("g") => 1,
            Some("b") => 2,
            Some("o") => 3,
            _ => return Err("Clamp channel must be r, g, b or o (opacity)".into()),
        },
    };
    let bound = |key, default| -> Result<f64, String> {
        let value = match settings.get(key) {
            None => default,
            Some(value) => value.as_f64().ok_or("Clamp bounds must be numbers")?,
        };
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err("Clamp bounds must be between 0 and 1".into());
        }
        Ok(value)
    };
    let minimum = bound("minimum", 0.0)?;
    let maximum = bound("maximum", 1.0)?;
    if minimum > maximum {
        return Err("Clamp minimum must not exceed maximum".into());
    }
    Ok((channel, minimum, maximum))
}

/// Convert bounds once at native precision. Samples already within them are exact.
pub fn native_bounds(settings: &Value, maximum_sample: u16) -> Result<(usize, u16, u16), String> {
    let (channel, minimum, maximum) = parameters(settings)?;
    let scale = f64::from(maximum_sample);
    Ok((
        channel,
        (minimum * scale).round() as u16,
        (maximum * scale).round() as u16,
    ))
}
