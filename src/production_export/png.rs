//! Authoritative native rows; display preview limits never determine export size.
//! Existing full effect/mask/filter preparation and its budgets still apply.
use super::{Control, Output, Resampling, Settings};
use crate::engine::Document;
use std::{borrow::Cow, io::Write};

const STRIP_PIXELS: u32 = 128 * 1024;
type Sample<'a> = dyn Fn(i32, i32) -> [u16; 4] + Sync + 'a;

fn prepared<T>(
    doc: &Document,
    control: &Control,
    use_pixels: impl FnOnce(&Sample<'_>) -> Result<T, String>,
) -> Result<T, String> {
    super::size(doc.width, doc.height)?;
    control.check()?;
    if doc.bit_depth == 16 {
        if crate::filters::active(doc) {
            let image = crate::filters::prepare16(doc)?;
            control.check()?;
            return use_pixels(&|x, y| image.get(x, y));
        }
        let plan = crate::depth16::Plan16::new(doc)?;
        control.check()?;
        use_pixels(&|x, y| plan.pixel(x, y))
    } else if doc.bit_depth == 8 {
        if crate::filters::active(doc) {
            let image = crate::filters::prepare8(doc)?;
            if (image.width, image.height) != (doc.width, doc.height) {
                return Err("Export render did not retain native dimensions".into());
            }
            control.check()?;
            return use_pixels(&|x, y| image.get(x, y).map(u16::from));
        }
        crate::mask::validate_budget(&doc.layers)?;
        let masks = doc
            .layers
            .iter()
            .map(|layer| {
                layer
                    .mask
                    .as_ref()
                    .and_then(|mask| mask.prepare(layer.pixels.width, layer.pixels.height))
            })
            .collect::<Vec<_>>();
        let colors = crate::effects::prepare(doc, &masks)?;
        let plan = crate::compositor::Plan::new(doc, &masks, &colors);
        let root = plan.group(None);
        control.check()?;
        use_pixels(&|x, y| plan.sample(root, x, y).map(u16::from))
    } else {
        Err("Exports require native 8/16-bit RGB channels".into())
    }
}

fn resampled(sample: &Sample<'_>, x: f64, y: f64, method: Resampling) -> [u16; 4] {
    if method == Resampling::Nearest {
        return sample((x + 0.5).floor() as i32, (y + 0.5).floor() as i32);
    }
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - f64::from(ix), y - f64::from(iy));
    if fx == 0. && fy == 0. {
        return sample(ix, iy);
    }
    let weights = [
        (1. - fx) * (1. - fy),
        fx * (1. - fy),
        (1. - fx) * fy,
        fx * fy,
    ];
    let pixels = [
        sample(ix, iy),
        sample(ix + 1, iy),
        sample(ix, iy + 1),
        sample(ix + 1, iy + 1),
    ];
    let alpha: f64 = (0..4).map(|i| weights[i] * f64::from(pixels[i][3])).sum();
    if alpha == 0. {
        return [0; 4];
    }
    let mut out = [0; 4];
    out[3] = alpha.round() as u16;
    for (c, value) in out.iter_mut().take(3).enumerate() {
        *value = ((0..4)
            .map(|i| weights[i] * f64::from(pixels[i][c]) * f64::from(pixels[i][3]))
            .sum::<f64>()
            / alpha)
            .round() as u16;
    }
    out
}

fn stream(
    output: &Output,
    depth: u16,
    profile: Option<&[u8]>,
    writer: &mut dyn Write,
    control: &Control,
    sample: &Sample<'_>,
    #[cfg(test)] mut peak: Option<&mut usize>,
) -> Result<(), String> {
    super::size(output.width, output.height)?;
    control.check()?;
    let mut info = ::png::Info::with_size(output.width, output.height);
    info.icc_profile = profile.map(Cow::Borrowed);
    let mut encoder = ::png::Encoder::with_info(writer, info).map_err(|e| e.to_string())?;
    encoder.set_color(::png::ColorType::Rgba);
    encoder.set_depth(if depth == 16 {
        ::png::BitDepth::Sixteen
    } else {
        ::png::BitDepth::Eight
    });
    encoder.set_compression(::png::Compression::Fast);
    encoder.set_filter(::png::Filter::Adaptive);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let mut stream = writer
        .stream_writer_with_size(64 * 1024)
        .map_err(|e| e.to_string())?;
    // Match the shared row renderer's parallel threshold without retaining a
    // full output. One extra partial row keeps the bound independent of height.
    let rows = STRIP_PIXELS.div_ceil(output.width);
    for top in (0..output.height).step_by(rows as usize) {
        control.check()?;
        let height = rows.min(output.height - top);
        let bytes = if depth == 16 {
            let words = crate::render::rgba16(output.width, height, |x, y| {
                sample(x as i32, (top + y) as i32)
            });
            let bytes = words
                .iter()
                .flat_map(|v| v.to_be_bytes())
                .collect::<Vec<_>>();
            #[cfg(test)]
            if let Some(peak) = peak.as_deref_mut() {
                *peak = (*peak).max(words.len() * 2 + bytes.len());
            }
            bytes
        } else {
            let bytes = crate::render::rgba8(output.width, height, |x, y| {
                sample(x as i32, (top + y) as i32).map(|v| v as u8)
            });
            #[cfg(test)]
            if let Some(peak) = peak.as_deref_mut() {
                *peak = (*peak).max(bytes.len());
            }
            bytes
        };
        let row = output.width as usize * if depth == 16 { 8 } else { 4 };
        for row in bytes.chunks_exact(row) {
            control.check()?;
            stream.write_all(row).map_err(|e| e.to_string())?;
        }
    }
    control.check()?;
    stream.finish().map_err(|e| e.to_string())?;
    control.check()?;
    writer.finish().map_err(|e| e.to_string())?;
    control.check()
}

pub(super) fn write(
    doc: &Document,
    output: &Output,
    settings: &Settings,
    control: &Control,
    writer: &mut dyn Write,
) -> Result<(), String> {
    let doc = super::target_document(doc, output.target.as_deref())?;
    let rect = output.document_rect;
    let (rw, rh) = (f64::from(rect[2] - rect[0]), f64::from(rect[3] - rect[1]));
    let profile = doc
        .icc_profile
        .as_deref()
        .map(Vec::as_slice)
        .or_else(|| doc.srgb_tagged.then(crate::color_profile::srgb_profile));
    prepared(&doc, control, |source| {
        let clipped = |x, y| {
            if x < 0 || y < 0 || x >= doc.width as i32 || y >= doc.height as i32 {
                [0; 4]
            } else {
                source(x, y)
            }
        };
        let sample = |x, y| {
            let sx = (f64::from(rect[0]) + (f64::from(x) + 0.5) * rw / f64::from(output.width)
                - 0.5)
                .clamp(f64::from(rect[0]), f64::from(rect[2] - 1));
            let sy = (f64::from(rect[1]) + (f64::from(y) + 0.5) * rh / f64::from(output.height)
                - 0.5)
                .clamp(f64::from(rect[1]), f64::from(rect[3] - 1));
            resampled(&clipped, sx, sy, settings.resampling)
        };
        stream(
            output,
            doc.bit_depth,
            profile,
            writer,
            control,
            &sample,
            #[cfg(test)]
            None,
        )
    })
}

pub(super) fn bounds(
    doc: &Document,
    target: Option<&str>,
    control: &Control,
) -> Result<[i32; 4], String> {
    let doc = super::target_document(doc, target)?;
    prepared(&doc, control, |sample| {
        let mut bounds = [doc.width as i32, doc.height as i32, 0, 0];
        for y in 0..doc.height as i32 {
            control.check()?;
            for x in 0..doc.width as i32 {
                if sample(x, y)[3] > 0 {
                    bounds = [
                        bounds[0].min(x),
                        bounds[1].min(y),
                        bounds[2].max(x + 1),
                        bounds[3].max(y + 1),
                    ];
                }
            }
        }
        if bounds[2] <= bounds[0] || bounds[3] <= bounds[1] {
            return Err(
                "Target is transparent; choose canvas bounds to export an empty image".into(),
            );
        }
        Ok(bounds)
    })
}

#[cfg(test)]
mod tests {
    use super::super::{Crop, Format, Image, Pixels, Variant};
    use super::*;
    use crate::{
        engine::{Engine, Layer},
        raster::Raster,
    };
    use serde_json::json;

    fn document(depth: u16) -> Document {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(259, 73, depth).unwrap();
        let id = engine.doc.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[
                    json!({"op":"mask.add","layer":id}),
                    json!({"op":"effect.add","layer":id,"kind":"blur","settings":{"radius":1.}}),
                ],
                None,
                None,
                "Fixture",
            )
            .unwrap();
        let layer = &mut engine.doc.layers[0];
        for y in 2..70 {
            for x in 3..257 {
                layer.pixels.set16(
                    x,
                    y,
                    [
                        12347 + x as u16 * 97,
                        23459 + y as u16 * 193,
                        34571,
                        if (x + y) % 11 == 0 { 0 } else { 45679 },
                    ],
                );
            }
        }
        layer.mask.as_mut().unwrap().steps[1]
            .pixels
            .set16(255, 33, [23459, 23459, 23459, 65535]);
        let mut overlay = Layer::new("Overlay", "paint", 259, 73);
        overlay.pixels = Raster::new_depth(259, 73, depth);
        overlay.opacity = 0.37;
        overlay.pixels.set16(256, 32, [53001, 43003, 33007, 22009]);
        engine.doc.layers.insert(0, overlay);
        engine.doc.srgb_tagged = true;
        engine.doc
    }

    #[test]
    fn streamed_png_matches_existing_full_native_composition_crop_resampling_and_profile() {
        for depth in [8, 16] {
            for filtered in [false, true] {
                let mut doc = document(depth);
                if filtered {
                    let mut engine = Engine::new();
                    engine.doc = doc;
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"filter.add","kind":"invert"})],
                            None,
                            None,
                            "Filter",
                        )
                        .unwrap();
                    doc = engine.doc;
                }
                let before = serde_json::to_value(&doc).unwrap();
                for target in [None, Some(doc.layers[1].id.clone())] {
                    for method in [Resampling::Nearest, Resampling::Bilinear] {
                        for crop in [Crop::Canvas, Crop::Alpha] {
                            let settings = Settings {
                                format: Format::Png,
                                crop,
                                resampling: method,
                                padding: 3,
                                variants: vec![Variant {
                                    name: "scaled".into(),
                                    scale: 0.73,
                                }],
                                ..Default::default()
                            };
                            let plan = super::super::plan(
                                &doc,
                                &target.clone().into_iter().collect::<Vec<_>>(),
                                &settings,
                                &Control::default(),
                            )
                            .unwrap();
                            let output = &plan.outputs[0];
                            let legacy =
                                Image::render(&doc, target.as_deref(), &Control::default())
                                    .unwrap();
                            if crop == Crop::Alpha {
                                assert_eq!(
                                    bounds(&doc, target.as_deref(), &Control::default()).unwrap(),
                                    legacy.bounds(&Control::default()).unwrap()
                                );
                            }
                            let mut bytes = Vec::new();
                            write(&doc, output, &settings, &Control::default(), &mut bytes)
                                .unwrap();
                            let image = image::load_from_memory(&bytes).unwrap();
                            assert_eq!(
                                (image.width(), image.height()),
                                (output.width, output.height)
                            );
                            let actual = image.into_rgba16().into_raw();
                            let rect = output.document_rect;
                            let (rw, rh) = ((rect[2] - rect[0]) as f64, (rect[3] - rect[1]) as f64);
                            for y in 0..output.height {
                                for x in 0..output.width {
                                    let sx = (rect[0] as f64
                                        + (x as f64 + 0.5) * rw / output.width as f64
                                        - 0.5)
                                        .clamp(rect[0] as f64, (rect[2] - 1) as f64);
                                    let sy = (rect[1] as f64
                                        + (y as f64 + 0.5) * rh / output.height as f64
                                        - 0.5)
                                        .clamp(rect[1] as f64, (rect[3] - 1) as f64);
                                    let expected = legacy.resampled(sx, sy, method).map(|v| {
                                        if depth == 8 {
                                            v * 257
                                        } else {
                                            v
                                        }
                                    });
                                    let at = ((y * output.width + x) * 4) as usize;
                                    assert_eq!(&actual[at..at + 4], &expected);
                                }
                            }
                            let reader = ::png::Decoder::new(std::io::Cursor::new(&bytes))
                                .read_info()
                                .unwrap();
                            assert_eq!(
                                reader.info().icc_profile.as_deref(),
                                Some(crate::color_profile::srgb_profile())
                            );
                            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
                        }
                    }
                }
                let legacy = Image::render(&doc, None, &Control::default()).unwrap();
                let encoded = doc.export_png().unwrap();
                let expected = match legacy.pixels {
                    Pixels::Eight(bytes) => bytes
                        .into_iter()
                        .map(|v| u16::from(v) * 257)
                        .collect::<Vec<_>>(),
                    Pixels::Sixteen(words) => words,
                };
                assert_eq!(
                    image::load_from_memory(&encoded)
                        .unwrap()
                        .into_rgba16()
                        .into_raw(),
                    expected
                );
            }
        }
    }

    #[test]
    fn strip_working_pixels_are_bounded_by_pixel_budget_and_one_row_not_height() {
        for depth in [8, 16] {
            for width in [2049, 8191, 8192] {
                for height in [129, 257] {
                    let doc = Document::new_depth(width, height, depth).unwrap();
                    let settings = Settings::default();
                    let output = super::super::plan(&doc, &[], &settings, &Control::default())
                        .unwrap()
                        .outputs
                        .remove(0);
                    let mut peak = 0;
                    let mut sink = std::io::sink();
                    stream(
                        &output,
                        depth,
                        None,
                        &mut sink,
                        &Control::default(),
                        &|x, y| {
                            [
                                x as u16,
                                y as u16,
                                73,
                                if depth == 16 { 65535 } else { 255 },
                            ]
                        },
                        Some(&mut peak),
                    )
                    .unwrap();
                    let pixel_bytes = if depth == 16 { 16 } else { 4 };
                    assert_eq!(
                        peak,
                        width as usize * STRIP_PIXELS.div_ceil(width) as usize * pixel_bytes
                    );
                    assert!(peak <= (128 * 1024 + 8191) * pixel_bytes);
                    assert!(peak < width as usize * height as usize * pixel_bytes);
                }
            }
        }
    }

    #[test]
    fn row_transfer_cancellation_and_writer_failure_return_errors_without_source_changes() {
        struct Failed;
        impl Write for Failed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("Injected write failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        struct Canceled<'a> {
            control: &'a Control,
            bytes: Vec<u8>,
        }
        impl Write for Canceled<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                if bytes == b"IDAT" {
                    self.control.cancel();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for depth in [8, 16] {
            let doc = document(depth);
            let before = serde_json::to_value(&doc).unwrap();
            let settings = Settings::default();
            let output = super::super::plan(&doc, &[], &settings, &Control::default())
                .unwrap()
                .outputs
                .remove(0);
            assert!(
                write(&doc, &output, &settings, &Control::default(), &mut Failed)
                    .unwrap_err()
                    .contains("Injected write failure")
            );
            let control = Control::default();
            let mut output_writer = Canceled {
                control: &control,
                bytes: Vec::new(),
            };
            assert!(
                write(&doc, &output, &settings, &control, &mut output_writer)
                    .unwrap_err()
                    .contains("canceled")
            );
            assert!(!output_writer.bytes.is_empty());
            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        }
    }

    #[test]
    fn cancellation_during_second_native_strip_stops_before_a_third_strip() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for depth in [8, 16] {
            let doc = Document::new_depth(4096, 97, depth).unwrap();
            let output = super::super::plan(&doc, &[], &Settings::default(), &Control::default())
                .unwrap()
                .outputs
                .remove(0);
            let control = Control::default();
            let count = AtomicUsize::new(0);
            let mut sink = std::io::sink();
            let rows = STRIP_PIXELS.div_ceil(4096);
            let error = stream(
                &output,
                depth,
                None,
                &mut sink,
                &control,
                &|x, y| {
                    count.fetch_add(1, Ordering::SeqCst);
                    if y >= rows as i32 {
                        control.cancel();
                    }
                    [x as u16, y as u16, 73, 255]
                },
                None,
            )
            .unwrap_err();
            assert!(error.contains("canceled"));
            assert_eq!(count.load(Ordering::SeqCst), 4096 * rows as usize * 2);
        }
    }

    #[test]
    fn original_icc_or_untagged_native_output_is_retained_without_display_conversion() {
        for depth in [8, 16] {
            for tagged in [false, true] {
                let mut doc = Document::new_depth(3, 2, depth).unwrap();
                doc.layers[0]
                    .pixels
                    .set16(1, 1, [12347, 23459, 34571, 45679]);
                if tagged {
                    doc.icc_profile = Some(std::sync::Arc::new(
                        crate::color_profile::srgb_profile().to_vec(),
                    ));
                }
                let before = serde_json::to_value(&doc).unwrap();
                let encoded = doc.export_png().unwrap();
                let reader = ::png::Decoder::new(std::io::Cursor::new(&encoded))
                    .read_info()
                    .unwrap();
                assert_eq!(
                    reader.info().icc_profile.as_deref(),
                    tagged.then(crate::color_profile::srgb_profile)
                );
                let legacy = Image::render(&doc, None, &Control::default()).unwrap();
                let expected = match legacy.pixels {
                    Pixels::Eight(bytes) => bytes
                        .into_iter()
                        .map(|v| u16::from(v) * 257)
                        .collect::<Vec<_>>(),
                    Pixels::Sixteen(words) => words,
                };
                assert_eq!(
                    image::load_from_memory(&encoded)
                        .unwrap()
                        .into_rgba16()
                        .into_raw(),
                    expected
                );
                assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            }
        }
    }
}
