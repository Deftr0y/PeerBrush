//! Selection image transport and complete PNG admission precede sampling.
use super::{
    backed::CoverageSource,
    native_tiles::Rows,
    regional_import::{self, Channel, ImageChannels},
    worker::Combine,
    SelectionSource,
};
use crate::{
    image_import::{
        png_admission,
        prepared_png::{self, ColorPolicy, Complete, EncodedPng, FilePng, Header, Limits},
    },
    large_document::{Result, Store},
    raster::surface::Surface,
    workspace::source_runtime::SourceSession,
};
use serde_json::Value;
use std::path::Path;

struct ImageSource {
    source: Surface,
    complete: Complete,
}
impl ImageSource {
    pub(crate) fn source(&self) -> &Surface {
        &self.source
    }
    pub(crate) fn header(&self) -> Header {
        self.complete.header()
    }
}

fn decode(
    command: &Value,
    store: Store,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<ImageSource, String> {
    let text = match (command["png"].as_str(), command["path"].as_str()) {
        (Some(text), None) => text,
        _ => return Err("Supply exactly one PNG path or base64 png".into()),
    };
    let pool = store.resident_budget();
    let encoded = EncodedPng::base64(text, Limits::SELECTION, &pool, &mut || {
        check().map_err(|e| e.to_string())
    })?;
    let mut rows: Option<Rows> = None;
    let mut first: Option<Header> = None;
    let complete = prepared_png::decode_rows(
        &encoded,
        Limits::SELECTION,
        ColorPolicy::RawSelection,
        &pool,
        &mut || check().map_err(|e| e.to_string()),
        |header, y, row| {
            if let Some(expected) = first {
                if (header.width, header.height, header.depth)
                    != (expected.width, expected.height, expected.depth)
                {
                    return Err("Decoded selection image frame changed".into());
                }
            } else {
                first = Some(header);
                rows = Some(
                    Rows::new(header.width, header.height, header.depth, store.clone())
                        .map_err(|e| e.to_string())?,
                );
            }
            rows.as_mut()
                .ok_or("Missing native selection row owner")?
                .row(y, row, &mut || check())
                .map_err(|e| e.to_string())
        },
    )?;
    let source = rows
        .ok_or("Selection PNG contains no decoded source rows")?
        .finish(&mut || check())
        .map_err(|e| e.to_string())?;
    Ok(ImageSource { source, complete })
}

pub(crate) fn prepare(
    command: &Value,
    width: u32,
    height: u32,
    prior: Option<&SelectionSource>,
    store: Store,
    session: &SourceSession,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<CoverageSource, String> {
    let area = command
        .get("rect")
        .and_then(crate::engine::rect)
        .ok_or("Selection mask needs an explicit document rectangle")?;
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
    {
        return Err("Selection mask rectangle must have positive area inside the canvas".into());
    }
    let mode = match command["mode"].as_str().unwrap_or("replace") {
        "replace" => Combine::Replace,
        "add" => Combine::Add,
        "subtract" => Combine::Subtract,
        "intersect" => Combine::Intersect,
        _ => return Err("Choose replace, add, subtract or intersect".into()),
    };
    let channel = match command["channel"].as_str().unwrap_or("luma") {
        "luma" => Channel::Luma,
        "alpha" => Channel::Alpha,
        _ => return Err("Selection mask channel must be luma or alpha".into()),
    };
    // Preserve the current ImageReader PNG channel interpretation. Raw cICP
    // remains decoder metadata; it is not applied by that existing loader.
    let sample = |source: &Surface, header: Header| {
        regional_import::prepare_source(
            source,
            ImageChannels {
                has_alpha: header.has_alpha,
                grayscale: header.grayscale,
                cicp: image::metadata::Cicp::SRGB,
            },
            channel,
            width,
            height,
            area,
            mode,
            prior,
            store.clone(),
            check,
        )
    };
    if let (None, Some(path)) = (command["png"].as_str(), command["path"].as_str()) {
        let mut checked = || check().map_err(|e| e.to_string());
        let mut file = FilePng::open(
            session,
            Path::new(path),
            prepared_png::apng::NATIVE_OPEN_LIMITS,
            ColorPolicy::RawSelection,
            &mut checked,
        )?;
        let staged = png_admission::stage_file(session, &mut file, &mut checked)?;
        let candidate = sample(staged.surface(), staged.complete().header())
            .and_then(|candidate| file.verify(&mut checked).map(|()| candidate));
        let cleanup = staged.abort(session);
        return match (candidate, cleanup) {
            (Ok(candidate), Ok(())) => Ok(candidate),
            (Err(reason), Ok(())) => Err(reason),
            (Ok(_), Err(cleanup)) => Err(format!(
                "Selection PNG source retirement refused: {cleanup}"
            )),
            (Err(reason), Err(cleanup)) => Err(format!(
                "{reason}; selection PNG source retirement refused: {cleanup}"
            )),
        };
    }
    let image = decode(command, store.clone(), check)?;
    let candidate = sample(image.source(), image.header())?;
    check().map_err(|e| e.to_string())?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{Error, Limits as StoreLimits},
        raster::backed::Region,
    };
    use base64::Engine;
    use serde_json::json;

    #[test]
    fn file_and_base64_png_sources_match_legacy_import_and_refuse_crc_or_cancellation() {
        for depth in [8, 16] {
            let fixture = crate::engine::native::worker::tests::Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
            let session = runtime.begin().unwrap();
            let pool = runtime.resident();
            let base = fixture.path();
            let store = Store::create_with_disk_budget(
                &base.join("store"),
                pool.clone(),
                runtime.disk(),
                StoreLimits {
                    disk_bytes: 64 * 1024 * 1024,
                    max_blocks: 256,
                    max_resident_blocks: 8,
                    manifest_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            let (w, h) = (17u32, 19u32);
            let mut words = Vec::new();
            let mut bytes = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let p = [
                        (x * 3271 + y * 1777 + 1) as u16,
                        (x * 773 + y * 3313 + 127) as u16,
                        (x * 911 + y * 1493 + 32769) as u16,
                        (x * 2917 + y * 1327 + 7171) as u16,
                    ];
                    words.extend_from_slice(&p);
                    bytes.extend(p.map(crate::raster::project16));
                }
            }
            let image = if depth == 16 {
                image::DynamicImage::ImageRgba16(
                    image::ImageBuffer::from_raw(w, h, words.clone()).unwrap(),
                )
            } else {
                image::DynamicImage::ImageRgba8(
                    image::RgbaImage::from_raw(w, h, bytes.clone()).unwrap(),
                )
            };
            let mut encoded = std::io::Cursor::new(Vec::new());
            image
                .write_to(&mut encoded, image::ImageFormat::Png)
                .unwrap();
            let encoded = encoded.into_inner();
            let path = base.join("input.png");
            std::fs::write(&path, &encoded).unwrap();
            let expected_hash: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &encoded)
                .as_ref()
                .try_into()
                .unwrap();
            for command in [
                json!({"path":path,"rect":[3,2,29,28],"channel":"luma"}),
                json!({"png":base64::engine::general_purpose::STANDARD.encode(&encoded),"rect":[3,2,29,28],"channel":"alpha"}),
            ] {
                let source = decode(
                    &json!({"png":base64::engine::general_purpose::STANDARD.encode(&encoded)}),
                    store.clone(),
                    &|| Ok(()),
                )
                .unwrap();
                assert_eq!(source.complete.source_sha256(), expected_hash);
                assert_eq!(source.source.depth(), depth);
                let view = source
                    .source
                    .prepare_region(
                        Region::new([0, 0, w as i32, h as i32]).unwrap(),
                        &mut || Ok(()),
                    )
                    .unwrap();
                for y in 0..h {
                    for x in 0..w {
                        let at = ((y * w + x) * 4) as usize;
                        let expected = if depth == 16 {
                            words[at..at + 4].try_into().unwrap()
                        } else {
                            std::array::from_fn(|c| u16::from(bytes[at + c]) * 257)
                        };
                        assert_eq!(view.pixel16(x as i32, y as i32).unwrap(), expected);
                    }
                }
                drop(view);
                drop(source);
                let mut doc = crate::engine::Document::new(33, 31).unwrap();
                crate::selection::import::apply(&mut doc, &command).unwrap();
                let expected = crate::selection::current(&doc).unwrap();
                let actual =
                    prepare(&command, 33, 31, None, store.clone(), &session, &|| Ok(())).unwrap();
                let view = actual
                    .prepare_region(Region::new([0, 0, 33, 31]).unwrap(), &pool, &mut || Ok(()))
                    .unwrap();
                for y in 0..31 {
                    for x in 0..33 {
                        assert_eq!(view.value(x, y).unwrap(), expected.value(x, y));
                    }
                }
            }
            let mut changed = encoded.clone();
            let last = changed.len() - 1;
            changed[last] ^= 1;
            std::fs::write(&path, &changed).unwrap();
            assert!(prepare(
                &json!({"path":path,"rect":[0,0,33,31]}),
                33,
                31,
                None,
                store.clone(),
                &session,
                &|| Ok(())
            )
            .is_err());
            assert!(decode(
                &json!({"png":base64::engine::general_purpose::STANDARD.encode(&changed)}),
                store.clone(),
                &|| Ok(())
            )
            .is_err());
            assert!(decode(
                &json!({"png":base64::engine::general_purpose::STANDARD.encode(&encoded)}),
                store.clone(),
                &|| Err(Error::Cancelled)
            )
            .is_err());
            drop((store, session, runtime, connection));
            assert_eq!(pool.used(), 0);
        }
    }
}
