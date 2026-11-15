//! Photoshop's depth-specific RGB luminosity threshold.
use serde_json::Value;

pub(crate) const DEFAULT_LEVEL: u8 = 128;

pub(crate) fn level(settings: &Value) -> Result<u8, String> {
    let fields = settings.as_object().ok_or("Threshold settings must be an object")?;
    if fields.keys().any(|key| key != "level") {
        return Err("Unknown Threshold setting".into());
    }
    let Some(value) = fields.get("level") else {
        return Ok(DEFAULT_LEVEL);
    };
    let value = value.as_f64().ok_or("Threshold Level must be an integer from 1 to 255")?;
    if !(1.0..=255.0).contains(&value) || value.fract() != 0.0 {
        return Err("Threshold Level must be an integer from 1 to 255".into());
    }
    Ok(value as u8)
}

fn luminosity(r: u32, g: u32, b: u32) -> u32 {
    30 * r + 59 * g + 11 * b
}

pub(super) fn apply8(bytes: &mut [u8], level: u8) {
    // A half-step tie falls on the darker side of the byte threshold.
    let cutoff = 100 * u32::from(level) - 50;
    for pixel in bytes.chunks_exact_mut(4) {
        let white = luminosity(pixel[0].into(), pixel[1].into(), pixel[2].into()) > cutoff;
        pixel[..3].fill(u8::MAX * u8::from(white));
    }
}

pub(crate) fn apply16(words: &mut [u16], level: u8) {
    let cutoff = 100 * ((u32::from(level) * super::PHOTOSHOP_WORD_SCALE + 127) / 255);
    for pixel in words.chunks_exact_mut(4) {
        let white = luminosity(
            super::photoshop_word(pixel[0]),
            super::photoshop_word(pixel[1]),
            super::photoshop_word(pixel[2]),
        ) >= cutoff;
        pixel[..3].fill(u16::MAX * u16::from(white));
    }
}
