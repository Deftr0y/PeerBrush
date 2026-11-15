//! Modern Brightness/Contrast calibrated against Photoshop RGB ramps.
use serde_json::Value;
use super::legacy_brightness::Mapping;

pub(crate) const KIND: &str = "brightness_contrast";

pub(crate) fn parameters(settings: &Value) -> Result<Mapping, String> {
    Mapping::with_ranges(settings, 150, -50)
}

#[derive(Clone, Copy)]
struct Brightness {
    gain: f64,
    pivot: f64,
    quadratic: f64,
    cubic: f64,
    tangent: f64,
}
impl Brightness {
    fn new(amount: u16) -> Self {
        let gain = (f64::from(amount) / 110.).exp2();
        let pivot = 0.5 / gain;
        let width = 1. - pivot;
        let tangent = width * gain;
        let end_tangent = width * (1. / (1. + 12. * (gain - 1.))).max(0.1);
        // Cubic Hermite joins the gain ray to white with calibrated end slope.
        Self { gain, pivot, quadratic: 1.5 - 2. * tangent - end_tangent,
            cubic: tangent + end_tangent - 1., tangent }
    }
    fn apply(self, value: f64) -> f64 {
        if value <= self.pivot {
            return value * self.gain;
        }
        let x = (value - self.pivot) / (1. - self.pivot);
        (0.5 + x * (self.tangent + x * (self.quadratic + x * self.cubic))).min(1.)
    }
}

pub(crate) struct Prepared {
    amount: i16,
    brightness: Brightness,
    extra: Option<Brightness>,
    slope: f64,
}
impl Prepared {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        let mapping = parameters(settings)?;
        let amount = mapping.brightness.unsigned_abs();
        Ok(Self {
            amount: mapping.brightness,
            brightness: Brightness::new(amount.min(100)),
            extra: (amount > 100).then(|| Brightness::new(amount - 100)),
            slope: 1. - 0.0076 * f64::from(mapping.contrast),
        })
    }
    fn positive(&self, value: f64) -> f64 {
        let value = self.brightness.apply(value);
        self.extra.map_or(value, |extra| extra.apply(value))
    }
    fn brightness(&self, value: f64) -> f64 {
        if self.amount == 0 || value == 0. || value == 1. {
            return value;
        }
        if self.amount > 0 {
            return self.positive(value);
        }
        // Invert the forward curve. Its low segment remains a gain ray
        // after composition, so most dark input needs no search.
        if value <= 0.5 {
            return value / (self.brightness.gain * self.extra.map_or(1., |curve| curve.gain));
        }
        let (mut low, mut high) = (0., 1.);
        for _ in 0..48 {
            let middle = (low + high) * 0.5;
            if self.positive(middle) < value { low = middle; } else { high = middle; }
        }
        (low + high) * 0.5
    }
    pub(crate) fn sample(&self, value: u16, max: u16) -> u16 {
        let value = self.brightness(f64::from(value) / f64::from(max));
        let half = value.min(1. - value);
        let mapped = half * (self.slope + (2. - 2. * self.slope) * half);
        let result = if value <= 0.5 { mapped } else { 1. - mapped };
        (result * f64::from(max)).round() as u16
    }
}
