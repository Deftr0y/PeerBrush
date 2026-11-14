//! Selected icon payloads share held-file proofs and native row producers.
use super::{
    png_admission,
    prepared_png::{ColorPolicy, FilePng},
    source_file::{FileLimits, FileSource},
};
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    workspace::source_runtime::SourceSession,
};
use std::{io::Read, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub offset: u64,
    pub length: u64,
    width: u32,
    height: u32,
}

impl Entry {
    /// None selects the patch decoder's best variant. Import commands require
    /// an explicit index whenever the file contains multiple variants.
    pub(super) fn read(
        input: &mut impl Read,
        command: Option<&serde_json::Value>,
    ) -> Result<Self, String> {
        let mut entries = [0; 16 * super::MAX_ITEMS];
        let count = read_directory(input, &mut entries)?;
        let entry = |i: usize| &entries[i * 16..(i + 1) * 16];
        for e in entries[..count * 16].chunks_exact(16) {
            if u16::from_le_bytes([e[4], e[5]]) > 256 || u16::from_le_bytes([e[6], e[7]]) > 256 {
                return Err("Invalid icon planes or bit depth".into());
            }
        }
        let selected = if let Some(command) = command {
            if command.get("frame").is_some() || command.get("page").is_some() {
                return Err("This image accepts variant, not frame or page".into());
            }
            match command.get("variant") {
                Some(v) => v
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|&n| n < count)
                    .ok_or("Image selection index is outside the available items")?,
                None if count == 1 => 0,
                None => {
                    return Err(
                        "Choose an explicit variant index before importing this image".into(),
                    )
                }
            }
        } else {
            let score = |e: &[u8]| {
                (
                    u16::from_le_bytes([e[6], e[7]]),
                    u32::from(dimension(e[0])) * u32::from(dimension(e[1])),
                )
            };
            // Match image's last-entry tie seed, including earlier higher ties.
            let mut best = count - 1;
            for i in 0..count - 1 {
                if score(entry(i)) > score(entry(best)) {
                    best = i;
                }
            }
            best
        };
        let e = entry(selected);
        let result = Self {
            offset: u64::from(u32::from_le_bytes(e[12..16].try_into().unwrap())),
            length: u64::from(u32::from_le_bytes(e[8..12].try_into().unwrap())),
            width: u32::from(dimension(e[0])),
            height: u32::from(dimension(e[1])),
        };
        if result.length < 8 || result.offset + result.length > super::ENCODED_LIMIT as u64 {
            return Err("Icon image exceeds admission".into());
        }
        Ok(result)
    }
    pub(super) fn check_dimensions(
        self,
        width: u32,
        height: u32,
        exact: bool,
    ) -> Result<(), String> {
        let actual = if exact {
            (width, height)
        } else {
            (width.min(256), height.min(256))
        };
        if actual != (self.width, self.height) {
            return Err("Icon directory dimensions do not match its image".into());
        }
        Ok(())
    }
}

fn dimension(value: u8) -> u16 {
    if value == 0 { 256 } else { u16::from(value) }
}

fn read_directory(input: &mut impl Read, entries: &mut [u8; 16 * super::MAX_ITEMS]) -> Result<usize, String> {
    let mut header = [0; 6];
    input
        .read_exact(&mut header)
        .map_err(|_| "Truncated icon directory")?;
    let count = usize::from(u16::from_le_bytes([header[4], header[5]]));
    if header[..4] != [0, 0, 1, 0] || count == 0 || count > super::MAX_ITEMS {
        return Err("Invalid icon directory".into());
    }
    input
        .read_exact(&mut entries[..count * 16])
        .map_err(|_| "Truncated icon entries")?;
    Ok(count)
}

pub(super) fn variant_sizes(input: &mut impl Read) -> Result<Vec<[u16; 2]>, String> {
    let mut entries = [0; 16 * super::MAX_ITEMS];
    let count = read_directory(input, &mut entries)?;
    Ok(entries[..count * 16]
        .chunks_exact(16)
        .map(|entry| [dimension(entry[0]), dimension(entry[1])])
        .collect())
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    command: Option<&serde_json::Value>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let limits = FileLimits {
        edge: 8192,
        pixels: 8192 * 8192,
        encoded_bytes: super::ENCODED_LIMIT as u64,
    };
    let source = FileSource::open(session, path, limits, "ico", &mut || check())?;
    prepare_source(base, session, source, limits, command, check)
}

pub(super) fn prepare_source(
    base: &NativeDocument,
    session: &SourceSession,
    mut source: FileSource<'_>,
    limits: FileLimits,
    command: Option<&serde_json::Value>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    source.start_read()?;
    let entry = Entry::read(&mut source, command)?;
    if entry.offset + entry.length > source.length() {
        return Err("Truncated icon image".into());
    }
    source.admit(&mut || check())?;
    source.start_read()?;
    source.skip(entry.offset, &mut || check())?;
    let mut signature = [0; 8];
    source
        .read_exact(&mut signature)
        .map_err(|_| "Truncated icon payload")?;
    source.finish(&mut || check())?;
    if signature != *b"\x89PNG\r\n\x1a\n" {
        return super::bmp::prepare_icon(base, session, source, entry, check);
    }
    let color = if command.is_some() {
        ColorPolicy::ImportSource
    } else {
        ColorPolicy::RawSelection
    };
    let mut png = FilePng::embedded(
        source,
        entry.offset,
        entry.length,
        limits,
        color,
        &mut || check(),
    )?;
    let input = png_admission::stage_file(session, &mut png, &mut || check())?;
    let result = (|| {
        let header = input.complete().header();
        super::check_budget(header.width, header.height, header.depth, super::BUDGET)?;
        if command.is_none() && (header.depth != 8 || !header.has_alpha || header.grayscale) {
            return Err("Icon patches require an RGBA8 PNG variant".into());
        }
        let output = if command.is_some() {
            super::native::prepare(base, session, &input, check)?
        } else {
            super::native::prepare_raw(base, session, &input, check)?
        };
        if let Err(reason) = entry
            .check_dimensions(
                output.surface().width(),
                output.surface().height(),
                command.is_some(),
            )
            .and_then(|()| png.recheck())
            .and_then(|()| check())
        {
            return match output.abort(session, &|| session.cleanup_check()) {
                Ok(()) => Err(reason),
                Err(cleanup) => Err(format!(
                    "{reason}; icon candidate retirement refused: {cleanup}"
                )),
            };
        }
        Ok(output)
    })();
    match (result, input.abort(session)) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => {
            Err(format!("{reason}; icon PNG retirement refused: {cleanup}"))
        }
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; icon candidate retirement refused: {cleanup}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::{
        ico_tests::{dib, fixture},
        tests::Fixture,
    };
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn chooser_sizes_follow_directory_order_and_keep_the_256_pixel_encoding() {
        let bytes = fixture(&[(32, 48, 32, vec![0; 8]), (256, 16, 32, vec![0; 8])]);
        assert_eq!(variant_sizes(&mut Cursor::new(&bytes)).unwrap(), [[32, 48], [256, 16]]);
        assert!(variant_sizes(&mut Cursor::new(&bytes[..37])).is_err());
    }

    #[test]
    fn patch_selection_matches_last_seed_ties_and_import_requires_an_index() {
        for depths in [[32, 32, 32], [32, 32, 8], [8, 32, 32]] {
            let items: Vec<_> = depths
                .into_iter()
                .enumerate()
                .map(|(i, depth)| (2, 1, depth, vec![i as u8; 8]))
                .collect();
            let bytes = fixture(&items);
            let selected = Entry::read(&mut Cursor::new(&bytes), None).unwrap();
            let expected = if depths == [32, 32, 8] { 0 } else { 2 };
            assert_eq!(bytes[selected.offset as usize], expected);
            assert!(Entry::read(&mut Cursor::new(&bytes), Some(&json!({}))).is_err());
            for index in 0..3 {
                let entry =
                    Entry::read(&mut Cursor::new(&bytes), Some(&json!({"variant":index}))).unwrap();
                assert_eq!(bytes[entry.offset as usize], index);
            }
        }
    }

    #[test]
    fn native_icon_refusal_cancellation_late_mask_failure_and_retry_retire_sources() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(64 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = root.path().join("source.ico");
        let bitmap = dib(
            crate::image_import::bmp::tests::fixture(270, 9, 40, 32, 0, false, true),
            true,
        );
        let png = (
            2,
            1,
            64,
            crate::raster::png16(2, 1, &[12347, 23459, 34571, 49153, 54321, 32109, 10987, 0])
                .unwrap(),
        );
        let command = json!({});
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        for item in [bitmap, png] {
            std::fs::write(&path, fixture(&[item.clone()])).unwrap();
            let hold = runtime
                .resident()
                .reserve_bytes(63 * 1024 * 1024 - resident, 64 * 1024 * 1024)
                .unwrap();
            assert!(prepare(&base, &session, &path, Some(&command), &|| Ok(())).is_err());
            assert_eq!(runtime.disk().used().unwrap(), disk);
            drop(hold);
            assert_eq!(runtime.resident().used(), resident);
            let reason = prepare(&base, &session, &path, Some(&command), &|| {
                if runtime.disk().used().unwrap() > disk {
                    Err("Icon interrupted".into())
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert!(reason.contains("Icon interrupted"), "{reason}");
            assert_eq!(runtime.disk().used().unwrap(), disk);
            if item.2 == 32 {
                let mut truncated_mask = item.clone();
                truncated_mask.3.pop();
                std::fs::write(&path, fixture(&[truncated_mask])).unwrap();
                let reason = prepare(&base, &session, &path, Some(&command), &|| Ok(()))
                    .err()
                    .unwrap();
                assert!(reason.contains("transparency mask"), "{reason}");
                assert_eq!(runtime.disk().used().unwrap(), disk);
                std::fs::write(&path, fixture(&[item.clone()])).unwrap();
            }
            let output = prepare(&base, &session, &path, Some(&command), &|| Ok(())).unwrap();
            assert_eq!(output.surface().depth(), if item.2 == 64 { 16 } else { 8 });
            output.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(runtime.resident().used(), resident);
            let mut wrong_shape = item.clone();
            wrong_shape.0 = 3;
            std::fs::write(&path, fixture(&[wrong_shape])).unwrap();
            assert!(prepare(&base, &session, &path, Some(&command), &|| Ok(())).is_err());
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(runtime.resident().used(), resident);
        }
        drop((base, session, runtime, connection));
    }

    #[test]
    fn embedded_png_proof_hashes_only_the_selected_payload() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let path = root.path().join("bound.ico");
        let items = [
            (
                2,
                1,
                32,
                crate::raster::png(2, 1, &[17, 29, 41, 0, 53, 67, 89, 193]).unwrap(),
            ),
            (
                2,
                1,
                64,
                crate::raster::png16(2, 1, &[12347, 23459, 34571, 49153, 54321, 32109, 10987, 0])
                    .unwrap(),
            ),
        ];
        std::fs::write(&path, fixture(&items)).unwrap();
        let limits = FileLimits {
            edge: 8192,
            pixels: 8192 * 8192,
            encoded_bytes: super::super::ENCODED_LIMIT as u64,
        };
        let disk = runtime.disk().used().unwrap();
        for (i, item) in items.iter().enumerate() {
            let mut source =
                FileSource::open(&session, &path, limits, "ico", &mut || Ok(())).unwrap();
            source.start_read().unwrap();
            let entry = Entry::read(&mut source, Some(&json!({"variant":i}))).unwrap();
            source.admit(&mut || Ok(())).unwrap();
            let mut png = FilePng::embedded(
                source,
                entry.offset,
                entry.length,
                limits,
                ColorPolicy::ImportSource,
                &mut || Ok(()),
            )
            .unwrap();
            let staged = png_admission::stage_file(&session, &mut png, &mut || Ok(())).unwrap();
            let expected = ring::digest::digest(&ring::digest::SHA256, &item.3);
            assert_eq!(
                staged.complete().source_sha256().as_slice(),
                expected.as_ref()
            );
            staged.abort(&session).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        drop((session, runtime, connection));
    }
}
