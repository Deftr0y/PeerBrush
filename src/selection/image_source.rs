//! Selection image transport and complete PNG admission precede sampling.
use super::{
    backed::CoverageSource,
    native_tiles::Rows,
    regional_import::{self, Channel, ImageChannels},
    worker::Combine,
    SelectionSource,
};
use crate::{
    image_import::prepared_png::{self, ColorPolicy, Complete, EncodedPng, Header, Limits},
    large_document::{Error, Result, ScratchBytes, Store},
    raster::surface::Surface,
};
use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

struct FileGuard {
    path: PathBuf,
    file: File,
    version: crate::file_safety::Version,
}
impl FileGuard {
    fn verify(
        &self,
        hash: [u8; 32],
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<(), String> {
        check().map_err(|e| e.to_string())?;
        if crate::file_safety::identity(&self.file)? != self.version.identity
            || self.file.metadata().map_err(|e| e.to_string())?.len() != self.version.bytes
        {
            return Err("Selection image file changed while preparing its source".into());
        }
        let actual =
            crate::file_safety::verified_version(&self.path, self.version.bytes, hash, || {
                check().map_err(|e| e.to_string())
            })?;
        if actual != self.version {
            return Err("Selection image path or bytes changed while preparing its source".into());
        }
        Ok(())
    }
}

fn transport(
    command: &Value,
    store: &Store,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<(EncodedPng, Option<FileGuard>), String> {
    let pool = store.resident_budget();
    match (command["png"].as_str(), command["path"].as_str()) {
        (Some(text), None) => Ok((
            EncodedPng::base64(text, Limits::SELECTION, &pool, &mut || {
                check().map_err(|e| e.to_string())
            })?,
            None,
        )),
        (None, Some(path)) => {
            let path = Path::new(path);
            if !path.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("Selection mask must be a regular PNG file".into());
            }
            let version = crate::file_safety::version_with_check(path, || {
                check().map_err(|e| e.to_string())
            })?;
            if version.bytes == 0 || version.bytes > Limits::SELECTION.encoded as u64 {
                return Err("Selection mask must be a bounded PNG image".into());
            }
            let mut file = File::open(path).map_err(|e| e.to_string())?;
            if crate::file_safety::identity(&file)? != version.identity
                || !file.metadata().map_err(|e| e.to_string())?.is_file()
            {
                return Err("Selection image ownership changed while opening it".into());
            }
            let mut bytes =
                ScratchBytes::new(&pool, version.bytes as usize, Limits::SELECTION.encoded)
                    .map_err(|e| e.to_string())?;
            for part in bytes.chunks_mut(64 * 1024) {
                check().map_err(|e| e.to_string())?;
                file.read_exact(part).map_err(|e| e.to_string())?;
            }
            let encoded = EncodedPng::from_charged_bytes(
                bytes.freeze(),
                version.bytes as usize,
                Limits::SELECTION,
                &pool,
                &mut || check().map_err(|e| e.to_string()),
            )?;
            Ok((
                encoded,
                Some(FileGuard {
                    path: path.to_owned(),
                    file,
                    version,
                }),
            ))
        }
        _ => Err("Supply exactly one PNG path or base64 png".into()),
    }
}

pub(crate) struct ImageSource {
    source: Surface,
    complete: Complete,
    file: Option<FileGuard>,
}
impl ImageSource {
    pub(crate) fn source(&self) -> &Surface {
        &self.source
    }
    pub(crate) fn header(&self) -> Header {
        self.complete.header()
    }
    pub(crate) fn source_sha256(&self) -> [u8; 32] {
        self.complete.source_sha256()
    }
    pub(crate) fn verify(&self, check: &dyn Fn() -> Result<()>) -> std::result::Result<(), String> {
        if let Some(file) = &self.file {
            file.verify(self.source_sha256(), check)?;
        }
        check().map_err(|e| e.to_string())
    }
}

pub(crate) fn decode(
    command: &Value,
    store: Store,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<ImageSource, String> {
    let (encoded, file) = transport(command, &store, check)?;
    let pool = store.resident_budget();
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
    let image = ImageSource {
        source,
        complete,
        file,
    };
    image.verify(check)?;
    Ok(image)
}

pub(crate) fn prepare(
    command: &Value,
    width: u32,
    height: u32,
    prior: Option<&SelectionSource>,
    store: Store,
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
    let image = decode(command, store.clone(), check)?;
    let header = image.header();
    // Preserve the current ImageReader PNG channel interpretation. Raw cICP
    // remains decoder metadata; it is not applied by that existing loader.
    let candidate = regional_import::prepare_source(
        image.source(),
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
        store,
        check,
    )?;
    image.verify(check)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{DiskBudget, Limits as StoreLimits, ResidentBudget},
        raster::backed::Region,
    };
    use base64::Engine;
    use serde_json::json;

    #[test]
    fn complete_file_and_base64_png_sources_match_legacy_import_and_refuse_late_changes_or_crc() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
            let base = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "peerbrush-selection-image-source-{}",
                    crate::engine::id()
                ));
            std::fs::create_dir(&base).unwrap();
            let store = Store::create_with_disk_budget(
                &base.join("store"),
                pool.clone(),
                DiskBudget::new(64 * 1024 * 1024, 4).unwrap(),
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
                let source = decode(&command, store.clone(), &|| Ok(())).unwrap();
                assert_eq!(source.source_sha256(), expected_hash);
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
                let actual = prepare(&command, 33, 31, None, store.clone(), &|| Ok(())).unwrap();
                let view = actual
                    .prepare_region(Region::new([0, 0, 33, 31]).unwrap(), &pool, &mut || Ok(()))
                    .unwrap();
                for y in 0..31 {
                    for x in 0..33 {
                        assert_eq!(view.value(x, y).unwrap(), expected.value(x, y));
                    }
                }
            }
            let source = decode(&json!({"path":path}), store.clone(), &|| Ok(())).unwrap();
            let mut changed = encoded.clone();
            let last = changed.len() - 1;
            changed[last] ^= 1;
            std::fs::write(&path, &changed).unwrap();
            assert!(source.verify(&|| Ok(())).is_err());
            drop(source);
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
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(base).unwrap();
        }
    }
}
