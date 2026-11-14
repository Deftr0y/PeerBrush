//! Legacy Vibrance and Saturation on linear RGB.
//!
//! The Vibrance curve fit is adapted from Patchy's Photoshop 2026 measurements.
//! https://github.com/SethRobinson/Patchy/blob/main/docs/adjustment-kinds.md#vibrance
//!
//! Copyright (c) 2026 Seth A. Robinson
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use crate::color_profile::{linear_to_srgb, srgb_to_linear};
use serde_json::Value;

pub(crate) const KIND: &str = "vibrance";

pub(crate) fn parameters(settings: &Value) -> Result<[i32; 2], String> {
    let fields = settings.as_object().ok_or("Vibrance settings must be an object")?;
    if fields.keys().any(|key| !matches!(key.as_str(), "vibrance" | "saturation")) {
        return Err("Unknown Legacy Vibrance setting".into());
    }
    let mut values = [0; 2];
    for (field, key) in values.iter_mut().zip(["vibrance", "saturation"]) {
        if let Some(value) = fields.get(key) {
            *field = value.as_f64()
                .filter(|value| (-100. ..=100.).contains(value) && value.fract() == 0.)
                .ok_or("Legacy Vibrance settings must be integers from -100 to 100")? as i32;
        }
    }
    Ok(values)
}

pub(crate) struct Prepared {
    vibrance: f64,
    saturation: f64,
    boost: f64,
}
impl Prepared {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        let [vibrance, saturation] = parameters(settings)?;
        let vibrance = f64::from(vibrance) / 100.;
        Ok(Self {
            vibrance,
            saturation: f64::from(saturation) / 100.,
            boost: 0.888 * vibrance.max(0.).powf(1.137),
        })
    }
    fn identity(&self) -> bool {
        self.vibrance == 0. && self.saturation == 0.
    }
    pub(crate) fn apply8(&self, rgba: &mut [u8]) {
        if self.identity() {
            return;
        }
        let linear: [f64; 256] = std::array::from_fn(|index| srgb_to_linear(index as f64 / 255.));
        for pixel in rgba.chunks_exact_mut(4) {
            let input = [pixel[0], pixel[1], pixel[2]].map(|value| linear[usize::from(value)]);
            pixel[..3].copy_from_slice(&self.adjust(input).map(|value| (linear_to_srgb(value) * 255.).round() as u8));
        }
    }
    pub(crate) fn apply16(&self, rgba: &mut [u16]) {
        if self.identity() {
            return;
        }
        let max = f64::from(u16::MAX);
        for pixel in rgba.chunks_exact_mut(4) {
            let input = [pixel[0], pixel[1], pixel[2]].map(|value| srgb_to_linear(f64::from(value) / max));
            pixel[..3].copy_from_slice(&self.adjust(input).map(|value| (linear_to_srgb(value) * max).round() as u16));
        }
    }
    fn adjust(&self, mut rgb: [f64; 3]) -> [f64; 3] {
        if self.vibrance != 0. {
            let [r, g, b] = rgb;
            let high = r.max(g).max(b);
            let low = r.min(g).min(b);
            if high > low {
                let chroma = (high - low) / high;
                let neutral = 1. - chroma;
                let (pivot, gain) = if self.vibrance < 0. {
                    let pivot = high * (1. - 1.93 * (1. - high).powf(0.863)
                        * chroma.powf(1.136) * neutral.powf(0.800));
                    let dark = (high / 0.0488).min(1.).powf(0.740);
                    let gain = 1. + self.vibrance * (1. - 0.750 * dark * chroma.powf(1.788));
                    (pivot, gain)
                } else {
                    let pivot = high * (1. - 0.890 * (1. - high).powf(1.623)
                        * chroma.powf(2.663) * neutral.powf(0.0669));
                    let dark = (high / 0.0747).min(1.).powf(1.102);
                    let red_reduction = if r == high {
                        let fraction = (g.max(b) - low) / (high - low);
                        let weight = if g >= b {
                            ((0.740 - fraction) / (0.740 - 0.522)).clamp(0., 1.)
                        } else {
                            1. - fraction
                        };
                        0.170 * weight
                    } else {
                        0.
                    };
                    let gain = 1. + self.boost * (1. - red_reduction) * dark * neutral.powf(1.012)
                        * chroma.powf(0.597) / high.powf(0.423);
                    (pivot, gain)
                };
                rgb = rgb.map(|channel| (pivot + (channel - pivot) * gain).clamp(0., 1.));
            }
        }
        if self.saturation != 0. {
            let gray = 0.288 * rgb[0] + 0.712 * rgb[1];
            rgb = rgb.map(|channel| channel + (channel - gray) * self.saturation);
        }
        rgb
    }
}
