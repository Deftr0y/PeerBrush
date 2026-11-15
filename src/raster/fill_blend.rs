//! Photoshop's eight blends with independent Fill and overlap coverage.

#[derive(Clone, Copy)]
enum Mode {
    ColorBurn,
    LinearBurn,
    ColorDodge,
    LinearDodge,
    Difference,
    VividLight,
    LinearLight,
    HardMix,
}

#[derive(Clone, Copy)]
pub(crate) struct SpecialFill {
    mode: Mode,
    amount: u8,
}

impl SpecialFill {
    pub(crate) fn new(mode: &str, amount: u8) -> Option<Self> {
        if amount == 255 {
            return None;
        }
        let mode = match mode {
            "color_burn" => Mode::ColorBurn,
            "linear_burn" => Mode::LinearBurn,
            "color_dodge" => Mode::ColorDodge,
            "linear_dodge" => Mode::LinearDodge,
            "difference" => Mode::Difference,
            "vivid_light" => Mode::VividLight,
            "linear_light" => Mode::LinearLight,
            "hard_mix" => Mode::HardMix,
            _ => return None,
        };
        Some(Self { mode, amount })
    }

    pub(crate) fn amount(self) -> u8 {
        self.amount
    }

    fn channel(self, dst: u16, src: u16, max: u16) -> f64 {
        let (d, s, m, fill) = (
            i64::from(dst),
            i64::from(src),
            i64::from(max),
            i64::from(self.amount),
        );
        let fade = |value: i64| {
            let product = value * fill;
            product.signum() * ((product.abs() + 127) / 255)
        };
        let result = match self.mode {
            Mode::ColorBurn => m - ((m - d) * (m + 1) * 255) / (m * 255 - (m - s) * fill),
            Mode::LinearBurn => d - fade(m - s),
            Mode::ColorDodge => nearest(d * (m + 1) * 255, (m + 1) * 255 - s * fill),
            Mode::LinearDodge => d + fade(s),
            Mode::Difference => (d - fade(s)).abs(),
            Mode::LinearLight => d + fade(2 * s - m) - 1,
            Mode::VividLight => {
                let middle = (m + 1) / 2;
                if s < middle {
                    let ramp = m - fade(m - nearest(s * m, middle));
                    // The burn ramp rounds ties down.
                    ((d + ramp - m).max(0) * m + (ramp - 1) / 2) / ramp
                } else {
                    let divisor = m - fade(nearest((s - middle) * m, m - middle));
                    nearest(d * m, divisor)
                }
            }
            Mode::HardMix => {
                let strength = nearest(fill * (m - 1), 255);
                let anchor = nearest((m - s) * strength, m);
                nearest((d - anchor).max(0) * m, m - strength)
            }
        };
        result.clamp(0, m) as f64
    }

    pub(crate) fn compose8(self, dst: [u8; 4], src: [u8; 4], opacity: f32) -> [u8; 4] {
        self.compose(
            dst.map(u16::from),
            src.map(u16::from),
            f64::from(opacity),
            255,
        )
        .map(|sample| sample as u8)
    }

    pub(crate) fn compose(self, dst: [u16; 4], src: [u16; 4], opacity: f64, max: u16) -> [u16; 4] {
        if self.amount == 0 || opacity == 0. || src[3] == 0 {
            return if dst[3] == 0 { [0; 4] } else { dst };
        }
        let coverage = f64::from(src[3]) / f64::from(max) * opacity;
        let backdrop_alpha = f64::from(dst[3]) / f64::from(max);
        // Fill changes the exposed source and alpha. The overlap still uses
        // the full coverage with the mode's Fill-adjusted RGB result.
        let source_only = coverage * (f64::from(self.amount) / 255.) * (1. - backdrop_alpha);
        let overlap = coverage * backdrop_alpha;
        let destination_only = (1. - coverage) * backdrop_alpha;
        let alpha = source_only + backdrop_alpha;
        if alpha <= 0. {
            return [0; 4];
        }
        let mut out = [0, 0, 0, (alpha * f64::from(max)).round() as u16];
        for c in 0..3 {
            let color = f64::from(src[c]) * source_only
                + self.channel(dst[c], src[c], max) * overlap
                + f64::from(dst[c]) * destination_only;
            out[c] = (color / alpha).round().clamp(0., f64::from(max)) as u16;
        }
        out
    }
}

fn nearest(numerator: i64, denominator: i64) -> i64 {
    (numerator + denominator / 2) / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_fill_uses_mode_specific_ramps_at_both_native_depths() {
        for (mode, byte, word) in [
            (
                "color_burn",
                [45, 17, 182, 255],
                [11089, 3364, 45514, 65535],
            ),
            (
                "linear_burn",
                [37, 10, 136, 255],
                [9176, 1915, 34091, 65535],
            ),
            (
                "color_dodge",
                [120, 129, 255, 255],
                [30545, 32458, 65535, 65535],
            ),
            (
                "linear_dodge",
                [165, 138, 255, 255],
                [42072, 34811, 65535, 65535],
            ),
            (
                "difference",
                [5, 102, 136, 255],
                [1106, 25487, 34091, 65535],
            ),
            (
                "linear_light",
                [122, 27, 200, 255],
                [30764, 6575, 50539, 65535],
            ),
            (
                "vivid_light",
                [96, 44, 200, 255],
                [24294, 10270, 50539, 65535],
            ),
            ("hard_mix", [76, 22, 255, 255], [18424, 3847, 65535, 65535]),
        ] {
            let fill = SpecialFill::new(mode, 128).unwrap();
            assert_eq!(
                fill.compose8([80, 120, 200, 255], [170, 36, 128, 255], 1.),
                byte
            );
            assert_eq!(
                fill.compose(
                    [20483, 30149, 50539, 65535],
                    [43009, 9287, 32768, 65535],
                    1.,
                    65535
                ),
                word
            );
            assert!(SpecialFill::new(mode, 255).is_none());
        }
    }

    #[test]
    fn fill_scales_source_only_alpha_and_keeps_full_overlap_coverage() {
        let mode = "linear_light";
        let src = [170, 170, 170, 255];
        let dst = [80, 80, 80, 128];
        let fill = SpecialFill::new(mode, 128).unwrap();
        assert_eq!(fill.compose8(dst, src, 0.5), [115, 115, 115, 160]);
        assert_eq!(fill.compose8([0; 4], src, 0.5), [170, 170, 170, 64]);
        assert_eq!(
            SpecialFill::new(mode, 0).unwrap().compose8(dst, src, 1.),
            dst
        );
    }
}
