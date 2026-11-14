//! Photoshop's Exposure transfer on encoded RGB channels.
use serde_json::Value;

pub(crate) const KIND: &str = "exposure";

pub(crate) fn parameters(settings: &Value) -> Result<[f32; 3], String> {
    let fields = settings.as_object().ok_or("Exposure settings must be an object")?;
    if fields.keys().any(|key| !matches!(key.as_str(), "exposure" | "offset" | "gamma" | "preset")) {
        return Err("Unknown Exposure setting".into());
    }
    let parameter = |key, default, min, max| -> Result<f32, String> {
        let Some(value) = fields.get(key) else { return Ok(default); };
        // Evaluate the same single-precision values that the standard PSD record stores.
        value.as_f64().map(|value| value as f32).filter(|value| (min..=max).contains(value))
            .ok_or_else(|| format!("Exposure {key} must be from {min} to {max}"))
    };
    Ok([
        parameter("exposure", 0., -20., 20.)?,
        parameter("offset", 0., -0.5, 0.5)?,
        parameter("gamma", 1., 0.01, 9.99)?,
    ])
}

pub(crate) struct Prepared {
    gain: f64,
    offset: f64,
    exponent: f64,
}
impl Prepared {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        let [exposure, offset, gamma] = parameters(settings)?;
        Ok(Self {
            gain: f64::from(exposure).exp2(),
            offset: f64::from(offset),
            exponent: 1. / (2.2 * f64::from(gamma)),
        })
    }
    pub(crate) fn sample(&self, sample: u16, max: u16) -> u16 {
        let input = f64::from(sample) / f64::from(max);
        let linear = input.powf(2.2) * self.gain + self.offset;
        (linear.clamp(0., 1.).powf(self.exponent) * f64::from(max)).round() as u16
    }
}
