// Copyright 2026 the PeerBrush contributors.
// Same BSD-3-Clause terms as the containing pinned tiny-skia package.
use crate::{
    alpha_runs::AlphaRun,
    blitter::{Blitter, Mask},
    color::AlphaU8,
    geom::ScreenIntRect,
    LengthU32, LineCap, Path,
};
use alloc::{string::String, vec::Vec};

#[derive(Clone, Copy, Debug)]
pub(crate) struct AliasedKernel {
    pub horizontal: bool,
    pub first: i32,
    pub stop: i32,
    pub ordinate: i32,
    pub slope: i32,
    pub maximum_y: i32,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Kernel {
    Aliased(AliasedKernel),
    Anti(super::hairline_aa::AntiKernel),
}
pub(crate) struct PreparedHairline {
    kernels: Vec<Kernel>,
    clip: ScreenIntRect,
}
struct Recorder {
    kernels: Vec<Kernel>,
    error: Option<&'static str>,
}
impl Blitter for Recorder {
    fn hairline_kernel(&mut self, kernel: Kernel) -> bool {
        if !kernel.valid() {
            self.error = Some("SVG hairline fixed-point program is invalid");
            return true;
        }
        tiny_skia_path::allocation::push(&mut self.kernels, kernel);
        true
    }
    fn refuse_hairline(&mut self, message: &'static str) {
        self.error = Some(message);
    }
    fn blit_h(&mut self, _x: u32, _y: u32, _width: LengthU32) {
        self.error = Some("SVG hairline emitted before program admission");
    }
    fn blit_anti_h(&mut self, _x: u32, _y: u32, _a: &mut [AlphaU8], _r: &mut [AlphaRun]) {
        self.error = Some("SVG hairline emitted before program admission");
    }
    fn blit_v(&mut self, _x: u32, _y: u32, _h: LengthU32, _a: AlphaU8) {
        self.error = Some("SVG hairline emitted before program admission");
    }
    fn blit_anti_h2(&mut self, _x: u32, _y: u32, _a: AlphaU8, _b: AlphaU8) {
        self.error = Some("SVG hairline emitted before program admission");
    }
    fn blit_anti_v2(&mut self, _x: u32, _y: u32, _a: AlphaU8, _b: AlphaU8) {
        self.error = Some("SVG hairline emitted before program admission");
    }
    fn blit_rect(&mut self, _rect: &ScreenIntRect) {
        self.error = Some("SVG hairline emitted before program admission");
    }
    fn blit_mask(&mut self, _mask: &Mask, _clip: &ScreenIntRect) {
        self.error = Some("SVG hairline emitted before program admission");
    }
}
impl PreparedHairline {
    pub(crate) fn new(
        path: &Path,
        cap: LineCap,
        clip: &ScreenIntRect,
        aa: bool,
    ) -> Result<Self, String> {
        let mut recorder = Recorder {
            kernels: Vec::new(),
            error: None,
        };
        if aa {
            super::hairline_aa::stroke_path(path, cap, clip, &mut recorder);
        } else {
            super::hairline::stroke_path(path, cap, clip, &mut recorder);
        }
        if let Some(error) = recorder.error {
            return Err(error.into());
        }
        Ok(Self {
            kernels: recorder.kernels,
            clip: *clip,
        })
    }
    pub(crate) fn allocation_bytes(&self) -> Option<usize> {
        self.kernels
            .capacity()
            .checked_mul(core::mem::size_of::<Kernel>())
    }
    pub(crate) fn render(&self, band: [u32; 2], blitter: &mut dyn Blitter) -> Result<(), String> {
        let mut target = BandBlitter {
            inner: blitter,
            band,
            error: None,
        };
        for kernel in &self.kernels {
            tiny_skia_path::allocation::checkpoint();
            match *kernel {
                Kernel::Aliased(kernel) => kernel.render(band, self.clip, &mut target),
                Kernel::Anti(kernel) => super::hairline_aa::emit_kernel(kernel, &mut target),
            }
            if let Some(error) = target.error {
                return Err(error.into());
            }
        }
        Ok(())
    }
}
impl Kernel {
    fn valid(self) -> bool {
        match self {
            Self::Aliased(kernel) => kernel.valid(),
            Self::Anti(kernel) => kernel.valid(),
        }
    }
}
impl AliasedKernel {
    fn valid(self) -> bool {
        self.first >= 0
            && self.stop > self.first
            && i64::from(self.ordinate)
                .checked_add(i64::from(self.slope) * i64::from(self.stop - self.first - 1))
                .is_some_and(|end| i32::try_from(end).is_ok())
    }
    fn render(self, band: [u32; 2], clip: ScreenIntRect, target: &mut dyn Blitter) {
        let count = (self.stop - self.first) as u32;
        let (begin, end) = if self.horizontal {
            index_range(
                self.ordinate,
                self.slope,
                count,
                i64::from(band[0]) << 16,
                i64::from(band[1]) << 16,
                false,
            )
        } else {
            (
                (self.first as u32)
                    .max(band[0])
                    .saturating_sub(self.first as u32)
                    .min(count),
                (self.stop as u32)
                    .min(band[1])
                    .saturating_sub(self.first as u32)
                    .min(count),
            )
        };
        for index in begin..end {
            tiny_skia_path::allocation::checkpoint();
            let value = i64::from(self.ordinate) + i64::from(self.slope) * i64::from(index);
            let Ok(value) = i32::try_from(value) else {
                target.refuse_hairline("SVG hairline fixed-point state overflow");
                return;
            };
            let major = self.first as u32 + index;
            let point = if self.horizontal {
                (value >= 0 && value < self.maximum_y).then_some((major, (value >> 16) as u32))
            } else {
                (value >= 0).then_some(((value >> 16) as u32, major))
            };
            if let Some((x, y)) = point {
                // Metadata-proven logical canvas exterior contributes no pixel.
                if x >= clip.x() && x < clip.right() && y >= clip.y() && y < clip.bottom() {
                    target.blit_h(x, y, crate::math::LENGTH_U32_ONE);
                }
            }
        }
    }
}
/// Exact state after repeated `value=max(value,0); value+=slope`.
/// No floating-point multiplication, division or reordered alpha arithmetic.
pub(crate) fn advance_clamped(value: i32, slope: i32, count: u32) -> Option<i32> {
    if count == 0 {
        return Some(value);
    }
    let end = i64::from(value.max(0)) + i64::from(slope) * i64::from(count);
    i32::try_from(if slope < 0 {
        end.max(i64::from(slope))
    } else {
        end
    })
    .ok()
}
fn lower_bound(count: u32, mut predicate: impl FnMut(u32) -> bool) -> u32 {
    let (mut first, mut end) = (0, count);
    while first < end {
        tiny_skia_path::allocation::checkpoint();
        let middle = first + (end - first) / 2;
        if predicate(middle) {
            end = middle;
        } else {
            first = middle + 1;
        }
    }
    first
}
fn refuse(target: &mut dyn Blitter, original: i32) -> i32 {
    target.refuse_hairline("SVG hairline fixed-point state overflow");
    original
}
/// Same per-X two-row commands and alpha arithmetic as the original Horish
/// kernel. Only provably noncontributing integer steps are omitted.
pub(crate) fn horish(
    target: &mut dyn Blitter,
    x: u32,
    stop: u32,
    ordinate: i32,
    slope: i32,
    band: [u32; 2],
) -> i32 {
    let Some(count) = stop.checked_sub(x).filter(|n| *n > 0) else {
        return refuse(target, ordinate);
    };
    let Some(initial) = ordinate.checked_add(crate::fixed_point::fdot16::HALF) else {
        return refuse(target, ordinate);
    };
    let Some(final_value) = advance_clamped(initial, slope, count)
        .and_then(|n| n.checked_sub(crate::fixed_point::fdot16::HALF))
    else {
        return refuse(target, ordinate);
    };
    // lower_y0 and lower_y1 both target rows0,1 because the original uses
    // lower_y.max(1)-1. Preserve that border behavior exactly.
    let lower = if band[0] <= 1 {
        0
    } else {
        i64::from(band[0]) << 16
    };
    let upper = (i64::from(band[1]) + 1) << 16;
    let (begin, end) = index_range(initial, slope, count, lower, upper, true);
    let Some(mut value) = advance_clamped(initial, slope, begin) else {
        return refuse(target, ordinate);
    };
    for index in begin..end {
        tiny_skia_path::allocation::checkpoint();
        value = value.max(0);
        let lower_y = (value >> 16) as u32;
        let alpha = super::hairline_aa::i32_to_alpha(value >> 8);
        target.blit_anti_v2(x + index, lower_y.max(1) - 1, 255 - alpha, alpha);
        let Some(next) = value.checked_add(slope) else {
            return refuse(target, ordinate);
        };
        value = next;
    }
    final_value
}
/// Same per-Y two-column commands as the original Vertish kernel; the final
/// return always represents the FULL span, so endpoint caps keep their phase.
pub(crate) fn vertish(
    target: &mut dyn Blitter,
    y: u32,
    stop: u32,
    ordinate: i32,
    slope: i32,
    band: [u32; 2],
) -> i32 {
    let Some(count) = stop.checked_sub(y).filter(|n| *n > 0) else {
        return refuse(target, ordinate);
    };
    let Some(initial) = ordinate.checked_add(crate::fixed_point::fdot16::HALF) else {
        return refuse(target, ordinate);
    };
    let Some(final_value) = advance_clamped(initial, slope, count)
        .and_then(|n| n.checked_sub(crate::fixed_point::fdot16::HALF))
    else {
        return refuse(target, ordinate);
    };
    let begin = y.max(band[0]).min(stop);
    let end = stop.min(band[1]).max(y);
    if begin < end {
        let Some(mut value) = advance_clamped(initial, slope, begin - y) else {
            return refuse(target, ordinate);
        };
        for row in begin..end {
            tiny_skia_path::allocation::checkpoint();
            value = value.max(0);
            let x = (value >> 16) as u32;
            let alpha = super::hairline_aa::i32_to_alpha(value >> 8);
            target.blit_anti_h2(x.max(1) - 1, row, 255 - alpha, alpha);
            let Some(next) = value.checked_add(slope) else {
                return refuse(target, ordinate);
            };
            value = next;
        }
    }
    final_value
}
/// Exact monotone integer step interval; upper is exclusive.
pub(crate) fn index_range(
    initial: i32,
    slope: i32,
    count: u32,
    lower: i64,
    upper: i64,
    clamped: bool,
) -> (u32, u32) {
    let value = |index: u32| {
        let start = if clamped { initial.max(0) } else { initial };
        let v = i64::from(start) + i64::from(slope) * i64::from(index);
        if clamped {
            v.max(0)
        } else {
            v
        }
    };
    if slope >= 0 {
        (
            lower_bound(count, |n| value(n) >= lower),
            lower_bound(count, |n| value(n) >= upper),
        )
    } else {
        (
            lower_bound(count, |n| value(n) < upper),
            lower_bound(count, |n| value(n) < lower),
        )
    }
}
struct BandBlitter<'a> {
    inner: &'a mut dyn Blitter,
    band: [u32; 2],
    error: Option<&'static str>,
}
impl Blitter for BandBlitter<'_> {
    fn hairline_band(&self) -> Option<[u32; 2]> {
        Some(self.band)
    }
    fn refuse_hairline(&mut self, error: &'static str) {
        self.error = Some(error);
    }
    fn blit_h(&mut self, x: u32, y: u32, width: LengthU32) {
        self.inner.blit_h(x, y, width)
    }
    fn blit_anti_h(&mut self, x: u32, y: u32, a: &mut [AlphaU8], r: &mut [AlphaRun]) {
        self.inner.blit_anti_h(x, y, a, r)
    }
    fn blit_v(&mut self, x: u32, y: u32, h: LengthU32, a: AlphaU8) {
        self.inner.blit_v(x, y, h, a)
    }
    fn blit_anti_h2(&mut self, x: u32, y: u32, a: AlphaU8, b: AlphaU8) {
        self.inner.blit_anti_h2(x, y, a, b)
    }
    fn blit_anti_v2(&mut self, x: u32, y: u32, a: AlphaU8, b: AlphaU8) {
        self.inner.blit_anti_v2(x, y, a, b)
    }
    fn blit_rect(&mut self, rect: &ScreenIntRect) {
        self.inner.blit_rect(rect)
    }
    fn blit_mask(&mut self, mask: &Mask, clip: &ScreenIntRect) {
        self.inner.blit_mask(mask, clip)
    }
}

#[cfg(all(test, feature = "std", panic = "unwind"))]
mod tests {
    use super::*;
    use alloc::{boxed::Box, sync::Arc};
    struct Check;
    impl tiny_skia_path::allocation::Cancellation for Check {
        fn check(&self) -> Result<(), String> {
            Ok(())
        }
    }
    #[test]
    fn clamped_integer_jump_matches_every_original_step_including_negative_border() {
        for initial in [-65537, -32768, -1, 0, 1, 32767, 65536, 131071] {
            for slope in [-65536, -256, -1, 0, 1, 256, 65536] {
                let mut expected = initial;
                for count in 0..64 {
                    assert_eq!(advance_clamped(initial, slope, count), Some(expected));
                    expected = expected.max(0) + slope;
                }
            }
        }
        // A plain multiply misses the original recurrence's repeated clamp.
        assert_ne!(advance_clamped(-17, -7, 20), Some(-17 - 7 * 20));
    }
    #[test]
    fn monotone_horish_band_selection_preserves_the_zero_and_one_row_pair() {
        for initial in [-65537, -1, 0, 1, 32767, 65536, 131071, 262144] {
            for slope in [-65536, -257, -1, 0, 1, 257, 65536] {
                for count in [1, 2, 9, 31] {
                    for top in 0..6 {
                        let band = [top, top + 1];
                        let mut expected = Vec::new();
                        let mut value = initial;
                        for index in 0..count {
                            value = value.max(0);
                            let lower = (value >> 16) as u32;
                            let first = lower.max(1) - 1;
                            if first < band[1] && first + 2 > band[0] {
                                expected.push(index);
                            }
                            value += slope;
                        }
                        let lower = if top <= 1 { 0 } else { i64::from(top) << 16 };
                        let upper = (i64::from(band[1]) + 1) << 16;
                        let (begin, end) = index_range(initial, slope, count, lower, upper, true);
                        assert_eq!(
                            expected,
                            (begin..end).collect::<Vec<_>>(),
                            "initial{initial} slope{slope} top{top}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn prepared_zero_width_and_subpixel_caps_match_actual_legacy_pixels() {
        let mut builder = crate::PathBuilder::new();
        builder.move_to(2.125, 1.375);
        builder.line_to(70.625, 58.125);
        builder.line_to(3.25, 55.75);
        builder.cubic_to(64.5, 51.25, 8.125, 7.75, 67.375, 3.125);
        let path = builder.finish().unwrap();
        for aa in [false, true] {
            for cap in [LineCap::Butt, LineCap::Round, LineCap::Square] {
                for width in [0.0, 0.625] {
                    let mut paint = crate::Paint::default();
                    paint.anti_alias = aa;
                    paint.set_color_rgba8(137, 53, 211, 173);
                    let stroke = crate::Stroke {
                        width,
                        line_cap: cap,
                        ..crate::Stroke::default()
                    };
                    let mut expected = crate::Pixmap::new(73, 61).unwrap();
                    expected.stroke_path(
                        &path,
                        &paint,
                        &stroke,
                        crate::Transform::identity(),
                        None,
                    );
                    let cache = crate::ScanCache::new(4 * 1024 * 1024, 32, Box::new(())).unwrap();
                    for y in (0..61).step_by(7) {
                        let count = 7.min(61 - y);
                        let result = tiny_skia_path::allocation::with_limits(
                            tiny_skia_path::allocation::Limits {
                                bytes: 8 * 1024 * 1024,
                                elements: 262144,
                                work: 1_000_000,
                                cancellation: Arc::new(Check),
                            },
                            Box::new(()),
                            || {
                                let mut image = crate::Pixmap::new_rows(73, 61, y, count).unwrap();
                                image.as_mut().stroke_path_prepared(
                                    &path,
                                    &paint,
                                    &stroke,
                                    crate::Transform::identity(),
                                    None,
                                    &cache,
                                    crate::InvocationKey::root(),
                                )?;
                                Ok::<_, String>(image)
                            },
                        )
                        .unwrap();
                        let begin = y as usize * 73 * 4;
                        assert_eq!(
                            result.get().data(),
                            &expected.data()[begin..begin + count as usize * 73 * 4],
                            "aa{aa} width{width} cap{cap:?} row{y}"
                        );
                    }
                }
            }
        }
    }
}
