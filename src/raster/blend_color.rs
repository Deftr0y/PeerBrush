//! RGB blend modes whose output depends on all three channels.

pub(super) fn blend<T: Copy + Into<u32>>(
    dst: [T; 4],
    src: [T; 4],
    mode: &str,
    maximum: f64,
) -> Option<[f64; 3]> {
    if !matches!(
        mode,
        "darker_color" | "lighter_color" | "hue" | "saturation" | "color" | "luminosity"
    ) {
        return None;
    }
    let rgb = |[r, g, b, _]: [T; 4]| [r, g, b].map(|sample| f64::from(sample.into()));
    let (dst, src) = (rgb(dst), rgb(src));
    let dst_luma = luma(dst);
    let src_luma = luma(src);
    Some(match mode {
        "darker_color" if src_luma.round() < dst_luma.round() => src,
        "lighter_color" if src_luma.round() > dst_luma.round() => src,
        "hue" => with_luma(with_saturation(src, saturation(dst)), dst_luma, maximum),
        "saturation" => with_luma(with_saturation(dst, saturation(src)), dst_luma, maximum),
        "color" => with_luma(src, dst_luma, maximum),
        "luminosity" => with_luma(dst, src_luma, maximum),
        // Whole-color comparisons keep the backdrop on a tied luma.
        _ => dst,
    })
}

pub(crate) fn luma([r, g, b]: [f64; 3]) -> f64 {
    (30. * r + 59. * g + 11. * b) / 100.
}

fn bounds([r, g, b]: [f64; 3]) -> [f64; 2] {
    [r.min(g).min(b), r.max(g).max(b)]
}

fn saturation(rgb: [f64; 3]) -> f64 {
    let [low, high] = bounds(rgb);
    high - low
}

fn with_saturation(rgb: [f64; 3], amount: f64) -> [f64; 3] {
    let [low, high] = bounds(rgb);
    if high == low {
        [0.; 3]
    } else {
        rgb.map(|sample| (sample - low) * amount / (high - low))
    }
}

fn with_luma(rgb: [f64; 3], target: f64, maximum: f64) -> [f64; 3] {
    fit_luma(centered_at_luma(rgb, false), target, maximum)
}

pub(crate) fn centered_at_luma(rgb: [f64; 3], round: bool) -> [f64; 3] {
    let original = luma(rgb);
    let center = if round { original.round() } else { original };
    rgb.map(|sample| sample - center)
}

pub(crate) fn fit_luma(centered: [f64; 3], target: f64, maximum: f64) -> [f64; 3] {
    let [low, high] = bounds(centered);
    // Preserve luma while bringing the RGB components back into gamut.
    let scale = if -low > target {
        target / -low
    } else if high > maximum - target {
        (maximum - target) / high
    } else {
        1.
    };
    centered.map(|sample| (target + sample * scale).clamp(0., maximum))
}

#[cfg(test)]
mod tests {
    #[test]
    fn equal_luma_keeps_the_backdrop_only_where_its_alpha_overlaps() {
        for mode in ["darker_color", "lighter_color"] {
            let dst = [100, 100, 100, 255];
            let src = [101, 100, 97, 255];
            assert_eq!(super::super::blend(dst, src, 1., mode), dst);
            assert_eq!(super::super::blend([100, 100, 100, 0], src, 1., mode), src);
            let dst = [32768, 32768, 32768, 65535];
            let src = [32769, 32768, 32765, 65535];
            assert_eq!(super::super::blend16(dst, src, 1., mode), dst);
            assert_eq!(
                super::super::blend16([32768, 32768, 32768, 0], src, 1., mode),
                src
            );
        }
    }
}
