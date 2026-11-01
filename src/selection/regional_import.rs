//! Channel sampling from an already validated immutable image source.
use super::{
    backed::CoverageSource, coverage_writer::Writer, rows::Feather, worker::Combine,
    SelectionSource,
};
use crate::{
    large_document::{Error, Result, ScratchBytes, Store},
    raster::{backed::Region, project16, surface::Surface},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Luma,
    Alpha,
}

#[derive(Clone, Copy, Debug)]
pub struct ImageChannels {
    pub has_alpha: bool,
    pub grayscale: bool,
    pub cicp: image::metadata::Cicp,
}

fn gray_row(
    view: &crate::raster::surface::SurfaceRegion,
    source: &Surface,
    channels: ImageChannels,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<Vec<u8>, String> {
    let [left, top, right, bottom] = view.region().bounds();
    let count = (right - left) as usize * (bottom - top) as usize;
    if source.depth() == 8 {
        let mut row = Vec::new();
        row.try_reserve_exact(count * if channels.grayscale { 1 } else { 4 })
            .map_err(|_| Error::Budget.to_string())?;
        for y in top..bottom {
            check().map_err(|e| e.to_string())?;
            for x in left..right {
                let p = view.pixel(x, y).map_err(|e| e.to_string())?;
                if channels.grayscale {
                    row.push(p[0]);
                } else {
                    row.extend_from_slice(&p);
                }
            }
        }
        if channels.grayscale {
            return Ok(row);
        }
        let mut buffer = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(
            (right - left) as u32,
            (bottom - top) as u32,
            row,
        )
        .ok_or("Invalid channel row dimensions")?;
        buffer
            .set_color_space(channels.cicp)
            .map_err(|e| e.to_string())?;
        Ok(image::DynamicImage::ImageRgba8(buffer)
            .to_luma8()
            .into_raw())
    } else {
        let mut row = Vec::new();
        row.try_reserve_exact(count * if channels.grayscale { 1 } else { 4 })
            .map_err(|_| Error::Budget.to_string())?;
        for y in top..bottom {
            check().map_err(|e| e.to_string())?;
            for x in left..right {
                let p = view.pixel16(x, y).map_err(|e| e.to_string())?;
                if channels.grayscale {
                    row.push(p[0]);
                } else {
                    row.extend_from_slice(&p);
                }
            }
        }
        if channels.grayscale {
            return Ok(row.into_iter().map(project16).collect());
        }
        let mut buffer = image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(
            (right - left) as u32,
            (bottom - top) as u32,
            row,
        )
        .ok_or("Invalid channel row dimensions")?;
        buffer
            .set_color_space(channels.cicp)
            .map_err(|e| e.to_string())?;
        Ok(image::DynamicImage::ImageRgba16(buffer)
            .to_luma8()
            .into_raw())
    }
}

/// PNG/container admission and alpha-presence evidence belong to the decoded
/// image source owner. This function never opens or decodes an image itself.
pub fn prepare_source(
    source: &Surface,
    channels: ImageChannels,
    channel: Channel,
    width: u32,
    height: u32,
    area: [i32; 4],
    mode: Combine,
    prior: Option<&SelectionSource>,
    store: Store,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<CoverageSource, String> {
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Selection import needs shared disk admission")?;
    if width == 0
        || height == 0
        || width > 16000
        || height > 16000
        || area[0] < 0
        || area[1] < 0
        || area[2] > width as i32
        || area[3] > height as i32
        || area[0] >= area[2]
        || area[1] >= area[3]
        || !source.uses_budget(&pool)
        || !source.uses_disk_budget(&disk)
    {
        return Err("Selection image differs from its admitted frame or shared pools".into());
    }
    if channel == Channel::Alpha && !channels.has_alpha {
        return Err("The selection PNG has no alpha channel".into());
    }
    let _channel_scratch = pool
        .reserve_bytes(1024 * 1024, 1024 * 1024)
        .map_err(|e| e.to_string())?;
    let mut writer = Writer::new(width, height, store, &pool).map_err(|e| e.to_string())?;
    let allocate = |bytes| ScratchBytes::new(&pool, bytes, bytes).map_err(|e| e.to_string());
    let mut ring = allocate(width as usize)?;
    let mut sums = allocate(width as usize * 4)?;
    let mut input = allocate(width as usize)?;
    let mut output = allocate(width as usize)?;
    let complete = Feather::new(
        width,
        height,
        0,
        &mut ring,
        &mut sums,
        &mut input,
        &mut output,
    )
    .map_err(str::to_owned)?
    .run(
        || check().map_err(|e| e.to_string()),
        |y, row| {
            check().map_err(|e| e.to_string())?;
            let old = if mode == Combine::Replace {
                None
            } else {
                prior
                    .filter(|p| !matches!(p, SelectionSource::All))
                    .map(|p| {
                        p.prepare_region(
                            Region::new([0, y as i32, width as i32, y as i32 + 1])?,
                            &pool,
                            &mut || check(),
                        )
                    })
                    .transpose()
                    .map_err(|e| e.to_string())?
            };
            let sy = ((y as i32 - area[1]) as f32 + 0.5) * source.height() as f32
                / (area[3] - area[1]) as f32
                - 0.5;
            let iy = sy.floor() as i32;
            let fy = sy - iy as f32;
            let view = if y as i32 >= area[1] && (y as i32) < area[3] {
                Some(
                    source
                        .prepare_region(
                            Region::new([
                                0,
                                iy.clamp(0, source.height() as i32 - 1),
                                source.width() as i32,
                                (iy + 1).clamp(0, source.height() as i32 - 1) + 1,
                            ])
                            .map_err(|e| e.to_string())?,
                            &mut || check(),
                        )
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            let gray = if channel == Channel::Luma {
                view.as_ref()
                    .map(|v| gray_row(v, source, channels, check))
                    .transpose()?
            } else {
                None
            };
            for (x, value) in row.iter_mut().enumerate() {
                if x % 4096 == 0 {
                    check().map_err(|e| e.to_string())?;
                }
                let new = if let Some(view) = view
                    .as_ref()
                    .filter(|_| x as i32 >= area[0] && (x as i32) < area[2])
                {
                    let sx = ((x as i32 - area[0]) as f32 + 0.5) * source.width() as f32
                        / (area[2] - area[0]) as f32
                        - 0.5;
                    let ix = sx.floor() as i32;
                    let fx = sx - ix as f32;
                    let at = |xx: i32, yy: i32| {
                        let xx = xx.clamp(0, source.width() as i32 - 1);
                        let yy = yy.clamp(0, source.height() as i32 - 1);
                        let byte = if let Some(gray) = &gray {
                            gray[(yy - view.region().bounds()[1]) as usize
                                * source.width() as usize
                                + xx as usize]
                        } else {
                            project16(view.pixel16(xx, yy).map_err(|e| e.to_string())?[3])
                        };
                        Ok::<_, String>(f32::from(byte) / 255.)
                    };
                    at(ix, iy)? * (1. - fx) * (1. - fy)
                        + at(ix + 1, iy)? * fx * (1. - fy)
                        + at(ix, iy + 1)? * (1. - fx) * fy
                        + at(ix + 1, iy + 1)? * fx * fy
                } else {
                    0.
                };
                let old = old
                    .as_ref()
                    .map(|p| p.value(x as i32, y as i32))
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .unwrap_or(0.);
                let combined = match mode {
                    Combine::Replace => new,
                    Combine::Add => old.max(new),
                    Combine::Subtract => old * (1. - new),
                    Combine::Intersect => old.min(new),
                };
                *value = (combined * 255.).round() as u8;
            }
            Ok(())
        },
        |y, row| {
            writer
                .row(y, row, &mut || check())
                .map_err(|e| e.to_string())
        },
    )?;
    writer
        .finish(&complete, &mut || check())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{DiskBudget, Limits, ResidentBudget},
        raster::Raster,
    };
    use base64::Engine;
    use serde_json::json;

    #[test]
    fn native8_and16_channel_bilinear_combination_matches_actual_legacy_png_operator() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "peerbrush-selection-import-{}",
                    crate::engine::id()
                ));
            let store = Store::create_with_disk_budget(
                &path,
                pool.clone(),
                DiskBudget::new(128 * 1024 * 1024, 4).unwrap(),
                Limits {
                    disk_bytes: 128 * 1024 * 1024,
                    max_blocks: 2048,
                    max_resident_blocks: 16,
                    manifest_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            let (sw, sh) = (17u32, 13u32);
            let mut input = Raster::new_depth(sw, sh, depth);
            let mut bytes8 = Vec::new();
            let mut words16 = Vec::new();
            for y in 0..sh {
                for x in 0..sw {
                    let native = [
                        (x * 2347 + y * 941 + 1) as u16,
                        (x * 1591 + y * 3179 + 129) as u16,
                        (x * 3539 + y * 727 + 32767) as u16,
                        (x * 3727 + y * 5101 + 1729) as u16,
                    ];
                    input.set16(x as i32, y as i32, native);
                    if depth == 8 {
                        bytes8.extend_from_slice(&input.get(x as i32, y as i32));
                    } else {
                        words16.extend_from_slice(&native);
                    }
                }
            }
            let image = if depth == 8 {
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(sw, sh, bytes8).unwrap())
            } else {
                image::DynamicImage::ImageRgba16(
                    image::ImageBuffer::from_raw(sw, sh, words16).unwrap(),
                )
            };
            let mut png = std::io::Cursor::new(Vec::new());
            image.write_to(&mut png, image::ImageFormat::Png).unwrap();
            let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
            let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
            let (w, h) = (257u32, 29u32);
            let mut old_mask = Raster::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let value = if (2..255).contains(&x) && (1..28).contains(&y) {
                        (x * 3 + y * 7) as u8
                    } else {
                        0
                    };
                    old_mask.set(x as i32, y as i32, [value; 4]);
                }
            }
            let old = crate::selection::from_mask(old_mask).unwrap();
            let prior = SelectionSource::Coverage(
                CoverageSource::stage_legacy(&old, store.clone(), &mut || Ok(())).unwrap(),
            );
            for (channel, name) in [(Channel::Luma, "luma"), (Channel::Alpha, "alpha")] {
                for (mode, mode_name) in [
                    (Combine::Replace, "replace"),
                    (Combine::Add, "add"),
                    (Combine::Subtract, "subtract"),
                    (Combine::Intersect, "intersect"),
                ] {
                    let mut doc = crate::engine::Document::new(w, h).unwrap();
                    doc.selection = Some(old.bounds);
                    doc.selection_coverage = Some(old.clone());
                    let area = [3, 2, 256, 28];
                    crate::selection::import::apply(
                        &mut doc,
                        &json!({"rect":area,"png":encoded,"channel":name,"mode":mode_name}),
                    )
                    .unwrap();
                    let expected = crate::selection::current(&doc);
                    let output = prepare_source(
                        &source,
                        ImageChannels {
                            has_alpha: true,
                            grayscale: false,
                            cicp: image.color_space(),
                        },
                        channel,
                        w,
                        h,
                        area,
                        mode,
                        Some(&prior),
                        store.clone(),
                        &|| Ok(()),
                    )
                    .unwrap();
                    let view = output
                        .prepare_region(
                            Region::new([0, 0, w as i32, h as i32]).unwrap(),
                            &pool,
                            &mut || Ok(()),
                        )
                        .unwrap();
                    for y in 0..h as i32 {
                        for x in 0..w as i32 {
                            assert_eq!(
                                view.value(x, y).unwrap(),
                                expected.as_ref().map_or(0., |c| c.value(x, y)),
                                "{depth} {name} {mode_name} at {x},{y}"
                            );
                        }
                    }
                    assert_eq!(Some(output.bounds()), expected.as_ref().map(|c| c.bounds));
                }
            }
            assert!(prepare_source(
                &source,
                ImageChannels {
                    has_alpha: false,
                    grayscale: false,
                    cicp: image.color_space()
                },
                Channel::Alpha,
                w,
                h,
                [0, 0, w as i32, h as i32],
                Combine::Replace,
                None,
                store.clone(),
                &|| Ok(())
            )
            .is_err());
            assert!(prepare_source(
                &source,
                ImageChannels {
                    has_alpha: true,
                    grayscale: false,
                    cicp: image.color_space()
                },
                Channel::Luma,
                w,
                h,
                [0, 0, w as i32, h as i32],
                Combine::Replace,
                None,
                store.clone(),
                &|| Err(Error::Cancelled)
            )
            .is_err());
            drop(prior);
            drop(source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
