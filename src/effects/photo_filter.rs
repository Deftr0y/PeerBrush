//! Photo Filter models transmission in D50 XYZ and preserves standard PSD color units.
use crate::color_profile::{linear_to_srgb, srgb_to_linear};
use serde_json::{Value, json};

pub(crate) const KIND: &str = "photo_filter";
const WARMING: [i16; 3] = [6719, 3314, 7279];
// Bradford-adapted sRGB matrix used by the published Photoshop ramp calibration.
const TO_XYZ: [[f64; 3]; 3] = [
    [0.4360747, 0.3850649, 0.1430804],
    [0.2225045, 0.7168786, 0.0606169],
    [0.0139322, 0.0971045, 0.7141733],
];
const FROM_XYZ: [[f64; 3]; 3] = [
    [3.133856367791652, -1.616866770291180, -0.490614772798302],
    [-0.978768563127027, 1.916141552624325, 0.033454116302537],
    [0.071945168281734, -0.228991277298882, 1.405242674153571],
];
const LAB_KNEE: f64 = 6. / 29.;
const LAB_SLOPE: f64 = 841. / 108.;

#[derive(Clone, Copy)]
pub(crate) enum Color {
    Lab([i16; 3]),
    Rgb([u16; 3]),
    Xyz([u32; 3]),
}
impl Color {
    pub(crate) fn rgb(self) -> [f64; 3] {
        let xyz = match self {
            Self::Rgb(channels) => return channels.map(|value| f64::from(value) / 65535.),
            Self::Xyz(channels) => channels.map(|value| f64::from(value) / 32768.),
            Self::Lab([l, a, b]) => {
                let fy = (f64::from(l) / 100. + 16.) / 116.;
                let axis = [fy + f64::from(a) / 50_000., fy, fy - f64::from(b) / 20_000.];
                std::array::from_fn(|i| {
                    let t = axis[i];
                    let y = if t > LAB_KNEE { t * t * t } else { (t - 4. / 29.) / LAB_SLOPE };
                    y * white(i)
                })
            }
        };
        multiply(FROM_XYZ, xyz).map(linear_to_srgb)
    }

    pub(crate) fn from_rgb(rgb: [f64; 3]) -> Self {
        let xyz = multiply(TO_XYZ, rgb.map(srgb_to_linear));
        let [x, y, z]: [f64; 3] = std::array::from_fn(|i| {
            let t = xyz[i] / white(i);
            if t > LAB_KNEE.powi(3) { t.cbrt() } else { LAB_SLOPE * t + 4. / 29. }
        });
        Self::Lab([
            ((116. * y - 16.) * 100.).round().clamp(0., 10_000.) as i16,
            ((x - y) * 50_000.).round().clamp(-12_800., 12_700.) as i16,
            ((y - z) * 20_000.).round().clamp(-12_800., 12_700.) as i16,
        ])
    }
}

fn white(axis: usize) -> f64 {
    TO_XYZ[axis].into_iter().sum()
}

fn multiply(matrix: [[f64; 3]; 3], [x, y, z]: [f64; 3]) -> [f64; 3] {
    matrix.map(|[a, b, c]| a * x + b * y + c * z)
}

pub(crate) struct Settings {
    pub color: Color,
    pub density: u8,
    pub preserve_luminosity: bool,
}
impl Settings {
    pub(crate) fn new(settings: &Value) -> Result<Self, String> {
        let fields = settings.as_object().ok_or("Photo Filter settings must be an object")?;
        if fields.keys().any(|key| !matches!(key.as_str(), "color_space" | "color" | "density" | "preserve_luminosity")) {
            return Err("Unknown Photo Filter setting".into());
        }
        let space = fields.get("color_space").map(|value| {
            value.as_str().ok_or("Invalid Photo Filter color space")
        }).transpose()?.unwrap_or("lab");
        let bounds = match space {
            "lab" => [(0, 10_000), (-12_800, 12_700), (-12_800, 12_700)],
            "rgb" => [(0, 65_535); 3],
            "xyz" => [(0, 65_536); 3],
            _ => return Err("Unsupported Photo Filter color space".into()),
        };
        let integer = |value: &Value, (low, high)| {
            value.as_f64().filter(|value| (f64::from(low)..=f64::from(high)).contains(value) && value.fract() == 0.)
                .map(|value| value as i32).ok_or("Invalid Photo Filter component or density")
        };
        let mut channels = WARMING.map(i32::from);
        if let Some(color) = fields.get("color") {
            let values = color.as_array().filter(|values| values.len() == 3)
                .ok_or("Photo Filter color requires three components")?;
            for ((field, value), bounds) in channels.iter_mut().zip(values).zip(bounds) {
                *field = integer(value, bounds)?;
            }
        } else if space != "lab" {
            return Err("Photo Filter color components are required for RGB and XYZ".into());
        }
        let color = match space {
            "rgb" => Color::Rgb(channels.map(|value| value as u16)),
            "xyz" => Color::Xyz(channels.map(|value| value as u32)),
            _ => Color::Lab(channels.map(|value| value as i16)),
        };
        let density = fields.get("density").map(|value| integer(value, (0, 100)))
            .transpose()?.unwrap_or(25) as u8;
        let preserve_luminosity = fields.get("preserve_luminosity").map(|value| {
            value.as_bool().ok_or("Photo Filter Preserve Luminosity must be a boolean")
        }).transpose()?.unwrap_or(true);
        Ok(Self { color, density, preserve_luminosity })
    }

    pub(crate) fn value(&self) -> Value {
        let (space, channels) = match self.color {
            Color::Lab(color) => ("lab", color.map(i32::from)),
            Color::Rgb(color) => ("rgb", color.map(i32::from)),
            Color::Xyz(color) => ("xyz", color.map(|value| value as i32)),
        };
        json!({"color_space":space, "color":channels, "density":self.density,
            "preserve_luminosity":self.preserve_luminosity})
    }

    pub(crate) fn prepare(&self, max: u16) -> Prepared {
        let byte = max == 255;
        let filter = self.color.rgb().map(|value| if byte { (value * 255.).round() / 255. } else { value });
        let density = f64::from(self.density) / 100.;
        let xyz = multiply(TO_XYZ, filter.map(srgb_to_linear));
        let transmission: [f64; 3] = std::array::from_fn(|i| 1. - density + density * xyz[i] / white(i));
        let matrix = FROM_XYZ.map(|row| std::array::from_fn(|column| {
            (0..3).map(|axis| row[axis] * transmission[axis] * TO_XYZ[axis][column]).sum()
        }));
        Prepared { matrix, max: f64::from(max), byte, preserve: self.preserve_luminosity, identity: self.density == 0 }
    }
}

pub(crate) fn defaults() -> Value {
    Settings { color: Color::Lab(WARMING), density: 25, preserve_luminosity: true }.value()
}

pub(crate) struct Prepared {
    matrix: [[f64; 3]; 3],
    max: f64,
    byte: bool,
    preserve: bool,
    identity: bool,
}
impl Prepared {
    fn sample(&self, original: [f64; 3], linear: [f64; 3]) -> [u16; 3] {
        let mut output = multiply(self.matrix, linear).map(|value| linear_to_srgb(value) * self.max);
        if self.preserve {
            let mut target = crate::raster::rgb_luma(original);
            if self.byte {
                target = target.round();
                output = output.map(f64::round);
            }
            let centered = crate::raster::centered_at_luma(output, self.byte);
            output = crate::raster::fit_luma(centered, target, self.max);
        }
        output.map(|value| value.round() as u16)
    }

    pub(crate) fn apply8(&self, rgba: &mut [u8]) {
        if self.identity { return; }
        let linear: [f64; 256] = std::array::from_fn(|index| srgb_to_linear(index as f64 / 255.));
        for pixel in rgba.chunks_exact_mut(4) {
            let rgb = [pixel[0], pixel[1], pixel[2]];
            let out = self.sample(rgb.map(f64::from), rgb.map(|value| linear[usize::from(value)]));
            pixel[..3].copy_from_slice(&out.map(|value| value as u8));
        }
    }

    pub(crate) fn apply16(&self, rgba: &mut [u16]) {
        if self.identity { return; }
        for pixel in rgba.chunks_exact_mut(4) {
            let rgb = [pixel[0], pixel[1], pixel[2]].map(f64::from);
            let out = self.sample(rgb, rgb.map(|value| srgb_to_linear(value / self.max)));
            pixel[..3].copy_from_slice(&out);
        }
    }
}
