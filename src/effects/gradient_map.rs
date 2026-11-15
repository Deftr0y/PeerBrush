//! RGB Gradient Map using the integer stop grid of standard Photoshop records.
use crate::color_profile::{linear_to_srgb, srgb_to_linear};
use serde_json::{Value, json};

pub(crate) const KIND: &str = "gradient_map";
pub(crate) const MAX_STOPS: usize = 64;
pub(crate) const MAX_NAME: usize = 4095;
pub(crate) const SOLID_FIELDS: [u8; 36] = [
    0, 32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 3,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Classic,
    Linear,
    Perceptual,
}
impl Method {
    fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Linear => "linear",
            Self::Perceptual => "perceptual",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Stop<T> {
    pub position: u32,
    pub midpoint: u32,
    pub value: T,
}
const DEFAULT_COLORS: [Stop<[u16; 3]>; 2] = [
    Stop { position: 0, midpoint: 50, value: [0; 3] },
    Stop { position: 4096, midpoint: 50, value: [65535; 3] },
];
const DEFAULT_ALPHA: [Stop<u8>; 2] = [
    Stop { position: 0, midpoint: 50, value: 255 },
    Stop { position: 4096, midpoint: 50, value: 255 },
];

pub(crate) struct Settings<'a> {
    pub name: &'a str,
    pub method: Method,
    pub reverse: bool,
    pub smoothness: u16,
    pub colors: Vec<Stop<[u16; 3]>>,
    pub alpha: Vec<Stop<u8>>,
    pub solid_fields: [u8; 36],
}

fn unit(value: &Value, max: u32) -> Result<u32, String> {
    value.as_f64().filter(|n| (0. ..=1.).contains(n))
        .map(|n| (n * f64::from(max)).round() as u32).ok_or("Gradient Map value must be within 0–1".into())
}

fn parse_stops<T>(
    source: &Value,
    value_key: &str,
    minimum: usize,
    value: impl Fn(&Value) -> Result<T, String>,
) -> Result<Vec<Stop<T>>, String> {
    let entries = source.as_array().filter(|entries| (minimum..=MAX_STOPS).contains(&entries.len()))
        .ok_or("Invalid Gradient Map stop count")?;
    let mut stops = Vec::with_capacity(entries.len());
    for entry in entries {
        let fields = entry.as_object().ok_or("Invalid Gradient Map stop")?;
        if fields.keys().any(|key| !matches!(key.as_str(), "position" | "midpoint") && key != value_key) {
            return Err("Unsupported Gradient Map stop property".into());
        }
        let position = unit(&entry["position"], 4096)?;
        if stops.last().is_some_and(|stop: &Stop<T>| stop.position >= position) {
            return Err("Gradient Map stops must increase on the 4096-point grid".into());
        }
        stops.push(Stop {
            position,
            midpoint: fields.get("midpoint").map(|midpoint| unit(midpoint, 100)).transpose()?.unwrap_or(50),
            value: value(&entry[value_key])?,
        });
    }
    Ok(stops)
}

impl<'a> Settings<'a> {
    pub(crate) fn new(source: &'a Value) -> Result<Self, String> {
        let fields = source.as_object().ok_or("Gradient Map settings must be an object")?;
        if fields.keys().any(|key| !matches!(key.as_str(),
            "name" | "method" | "reverse" | "smoothness" | "color_stops" | "alpha_stops" | "solid_fields"))
        {
            return Err("Unsupported Gradient Map setting".into());
        }
        let name = fields.get("name").map(|value| {
            value.as_str().filter(|name| name.len() <= 3 * MAX_NAME
                && !name.contains('\0') && name.encode_utf16().count() <= MAX_NAME)
                .ok_or("Invalid Gradient Map name")
        }).transpose()?.unwrap_or("Black, White");
        let method = match fields.get("method").map(Value::as_str) {
            None | Some(Some("classic")) => Method::Classic,
            Some(Some("linear")) => Method::Linear,
            Some(Some("perceptual")) => Method::Perceptual,
            _ => return Err("Unsupported Gradient Map method".into()),
        };
        let reverse = fields.get("reverse").map(|value| value.as_bool().ok_or("Invalid Gradient Map reverse switch"))
            .transpose()?.unwrap_or(false);
        let smoothness = fields.get("smoothness").map(|value| unit(value, 4096)).transpose()?.unwrap_or(4096) as u16;
        let colors = if let Some(source) = fields.get("color_stops") {
            parse_stops(source, "color", 2, |color| {
                let rgb = color.as_array().filter(|rgb| rgb.len() == 3).ok_or("Gradient Map stop requires RGB")?;
                Ok([unit(&rgb[0], 65535)? as u16, unit(&rgb[1], 65535)? as u16, unit(&rgb[2], 65535)? as u16])
            })?
        } else {
            DEFAULT_COLORS.to_vec()
        };
        let alpha = if let Some(source) = fields.get("alpha_stops") {
            parse_stops(source, "opacity", 0, |opacity| Ok(unit(opacity, 255)? as u8))?
        } else {
            DEFAULT_ALPHA.to_vec()
        };
        let mut solid_fields = SOLID_FIELDS;
        if let Some(source) = fields.get("solid_fields") {
            let values = source.as_array().filter(|values| values.len() == solid_fields.len())
                .ok_or("Invalid Gradient Map solid record")?;
            for (byte, value) in solid_fields.iter_mut().zip(values) {
                *byte = value.as_u64().and_then(|value| u8::try_from(value).ok())
                    .ok_or("Invalid Gradient Map solid record byte")?;
            }
            if solid_fields[..4] != [0, 32, 0, 0] {
                return Err("Gradient Map requires a solid gradient record".into());
            }
        }
        Ok(Self { name, method, reverse, smoothness, colors, alpha, solid_fields })
    }

    pub(crate) fn value(&self) -> Value {
        let colors: Vec<_> = self.colors.iter().map(|stop| json!({
            "position":f64::from(stop.position)/4096., "midpoint":f64::from(stop.midpoint)/100.,
            "color":stop.value.map(|value| f64::from(value)/65535.)
        })).collect();
        let alpha: Vec<_> = self.alpha.iter().map(|stop| json!({
            "position":f64::from(stop.position)/4096., "midpoint":f64::from(stop.midpoint)/100.,
            "opacity":f64::from(stop.value)/255.
        })).collect();
        json!({"name":self.name,"method":self.method.name(),"reverse":self.reverse,"smoothness":f64::from(self.smoothness)/4096.,
            "color_stops":colors,"alpha_stops":alpha,"solid_fields":self.solid_fields.as_slice()})
    }

    pub(crate) fn prepare(&self) -> Prepared {
        let stops = self.colors.iter().map(|stop| {
            let rgb = stop.value.map(|value| f64::from(value) / 65535.);
            let value = match self.method {
                Method::Classic => rgb,
                Method::Linear => rgb.map(srgb_to_linear),
                Method::Perceptual => to_oklab(rgb.map(srgb_to_linear)),
            };
            Stop { position: stop.position, midpoint: stop.midpoint, value }
        }).collect();
        Prepared { method: self.method, reverse: self.reverse, smoothness: f64::from(self.smoothness) / 4096., stops }
    }
}

pub(crate) fn defaults() -> Value {
    Settings::new(&json!({})).unwrap().value()
}

pub(crate) struct Prepared {
    method: Method,
    reverse: bool,
    smoothness: f64,
    stops: Vec<Stop<[f64; 3]>>,
}
impl Prepared {
    pub(crate) fn color(&self, position: f64) -> [f64; 3] {
        let position = position * 4096.;
        let end = self.stops.partition_point(|stop| f64::from(stop.position) < position);
        let color = if end == 0 || end == self.stops.len() {
            self.stops[end.min(self.stops.len() - 1)].value
        } else {
            let left = self.stops[end - 1];
            let right = self.stops[end];
            let ratio = (position - f64::from(left.position)) / f64::from(right.position - left.position);
            let midpoint = (f64::from(right.midpoint) / 100.).clamp(0.0001, 0.9999);
            let t = super::gradient::remap_midpoint(ratio, midpoint);
            if self.method == Method::Classic {
                let previous = self.stops[end.saturating_sub(2)].value;
                let next = self.stops[(end + 1).min(self.stops.len() - 1)].value;
                std::array::from_fn(|c| super::gradient::smooth_channel(
                    [previous[c], left.value[c], right.value[c], next[c]], t, self.smoothness))
            } else {
                let t = super::gradient::smooth_channel([0., 0., 1., 1.], t, self.smoothness);
                std::array::from_fn(|c| left.value[c] * (1. - t) + right.value[c] * t)
            }
        };
        match self.method {
            Method::Classic => color,
            Method::Linear => color.map(linear_to_srgb),
            Method::Perceptual => from_oklab(color).map(linear_to_srgb),
        }
    }
    pub(crate) fn lookup(&self, position: f64, max: u16) -> [u16; 3] {
        let position = if self.reverse { 1. - position } else { position };
        self.color(position).map(|value| (value * f64::from(max)).round() as u16)
    }
    pub(crate) fn apply8(&self, rgba: &mut [u8]) {
        let table: [[u8; 3]; 256] = std::array::from_fn(|i| self.lookup(i as f64 / 255., 255).map(|n| n as u8));
        for pixel in rgba.chunks_exact_mut(4) {
            let [r, g, b] = [pixel[0], pixel[1], pixel[2]].map(usize::from);
            pixel[..3].copy_from_slice(&table[(30 * r + 59 * g + 11 * b + 50) / 100]);
        }
    }
    pub(crate) fn apply16(&self, rgba: &mut [u16]) {
        for pixel in rgba.chunks_exact_mut(4) {
            let luma = crate::raster::rgb_luma([pixel[0], pixel[1], pixel[2]].map(f64::from));
            pixel[..3].copy_from_slice(&self.lookup(luma / 65535., 65535));
        }
    }
}

// OKLab matrices published by Björn Ottosson, using linear sRGB.
fn to_oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}
fn from_oklab([lightness, a, b]: [f64; 3]) -> [f64; 3] {
    let l = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s = (lightness - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}
