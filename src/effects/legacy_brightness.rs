//! Photoshop's legacy Brightness/Contrast adjustment, evaluated in native channels.
use serde_json::Value;

pub(crate) const KIND: &str = "legacy_brightness";
pub(crate) const DEFAULT_MEAN: u16 = 127;

#[derive(Clone, Copy)]
pub(crate) struct Mapping {
    pub brightness: i16,
    pub contrast: i16,
    pub mean: u16,
}
impl Mapping {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        Self::with_ranges(settings, 100, -100)
    }
    pub(super) fn with_ranges(settings: &Value, brightness_limit: i32, contrast_minimum: i32) -> Result<Self, String> {
        let fields = settings.as_object().ok_or("Brightness/Contrast settings must be an object")?;
        if fields.keys().any(|key| !matches!(key.as_str(), "brightness" | "contrast" | "mean")) {
            return Err("Unknown Brightness/Contrast setting".into());
        }
        let integer = |key: &str, low: i32, high: i32, default: i32| -> Result<i32, String> {
            let Some(value) = fields.get(key) else { return Ok(default); };
            let value = value.as_f64().filter(|n| {
                (f64::from(low)..=f64::from(high)).contains(n) && n.fract() == 0.
            }).ok_or_else(|| format!("Brightness/Contrast {key} must be an integer from {low} to {high}"))?;
            Ok(value as i32)
        };
        Ok(Self {
            brightness: integer("brightness", -brightness_limit, brightness_limit, 0)? as i16,
            contrast: integer("contrast", contrast_minimum, 100, 0)? as i16,
            mean: integer("mean", 0, 65535, i32::from(DEFAULT_MEAN))? as u16,
        })
    }

    pub(crate) fn sample(self, sample: u16, max: u16) -> u16 {
        let max = i64::from(max);
        let brightness = i64::from(self.brightness) * (max / 255);
        let sample = i64::from(sample);
        let contrast = i64::from(self.contrast);
        if contrast == 100 {
            return if sample + brightness >= 127 * (max / 255) { max as u16 } else { 0 };
        }
        // Twice the sample keeps the half-channel midpoint exact. Positive
        // contrast shifts the input. Negative contrast shifts the output.
        let (numerator, denominator) = if contrast > 0 {
            let divisor = 100 - contrast;
            ((2 * (sample + brightness) - max) * 100 + max * divisor, 2 * divisor)
        } else {
            ((2 * sample - max) * (100 + contrast) + (max + 2 * brightness) * 100, 200)
        };
        ((numerator + denominator / 2) / denominator).clamp(0, max) as u16
    }
}

pub(super) fn apply8(bytes: &mut [u8], mapping: Mapping) {
    let table: [u8; 256] = std::array::from_fn(|index| mapping.sample(index as u16, 255) as u8);
    for pixel in bytes.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            *channel = table[*channel as usize];
        }
    }
}
