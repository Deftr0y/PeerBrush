//! Held SVG input rendered by the existing native vector producer.
use super::source_file::{FileLimits, FileSource};
use crate::{
    engine::{
        native::{Cancellation, NativeDocument, StagedOutput},
        scene::Source36,
    },
    psd::memory36::{Admitted, MetadataCharge},
    source::prepared::PreparedSource,
    workspace::source_runtime::SourceSession,
};
use std::{io::Read, path::Path, sync::Arc};

pub(crate) fn matches(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    cancellation: &Cancellation,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(StagedOutput, Admitted<Source36>), String> {
    let checked = || {
        cancellation.check()?;
        session.check()?;
        check()
    };
    let check: &dyn Fn() -> Result<(), String> = &checked;
    check()?;
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .filter(|_| matches(path))
        .ok_or("Native SVG requires an SVG/SVGZ extension")?;
    let compressed = extension.eq_ignore_ascii_case("svgz");
    let input = FileSource::open(
        session,
        path,
        FileLimits {
            edge: 8192,
            pixels: 8192 * 8192,
            encoded_bytes: if compressed {
                1024 * 1024
            } else {
                crate::svg::MARKUP_LIMIT as u64
            },
        },
        extension,
        &mut || check(),
    )?;
    prepare_source(base, session, input, compressed, cancellation, check)
}

pub(super) fn prepare_source(
    base: &NativeDocument,
    session: &SourceSession,
    mut input: FileSource<'_>,
    compressed: bool,
    cancellation: &Cancellation,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(StagedOutput, Admitted<Source36>), String> {
    // Complete the first hash before decoding the same held file on its second pass.
    input.start_read()?;
    input.admit(&mut || check())?;
    input.start_read()?;
    let owner = MetadataCharge::reserve(base.budget(), crate::svg::MARKUP_LIMIT + 4096, check)?;
    let markup = {
        // Bounded gzip header/inflater state and temporary reader buffers.
        let _decoder = MetadataCharge::reserve(base.budget(), 8 * 1024 * 1024, check)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(crate::svg::MARKUP_LIMIT + 1)
            .map_err(|_| "SVG markup allocation refused")?;
        if bytes.capacity() > crate::svg::MARKUP_LIMIT + 1 {
            return Err("SVG markup exceeds admission".into());
        }
        let mut read = |reader: &mut dyn Read| -> Result<(), String> {
            let mut chunk = [0; 8192];
            loop {
                check()?;
                let count = reader
                    .read(&mut chunk)
                    .map_err(|e| format!("Invalid SVG input: {e}"))?;
                if count == 0 {
                    break;
                }
                if bytes.len() + count > crate::svg::MARKUP_LIMIT {
                    return Err("SVG source exceeds 512 KiB".into());
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            Ok(())
        };
        if compressed {
            read(&mut flate2::read::GzDecoder::new(&mut input))?;
        } else {
            read(&mut input)?;
        }
        String::from_utf8(bytes).map_err(|_| "SVG import requires UTF-8 XML")?
    };
    input.finish(&mut || check())?;
    let (width, height) = {
        let _parser = MetadataCharge::reserve(base.budget(), 64 * 1024 * 1024, check)?;
        crate::svg::dimensions(&markup)?
    };
    super::check_budget(width, height, 8, super::BUDGET)?;
    let source = Admitted::from_parts(
        Arc::new(Source36::from_source(crate::source::Source {
            width,
            height,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: crate::source::Content::Svg { svg: markup },
        })),
        owner,
    );
    let prepared = PreparedSource::load(&source, base.budget(), &mut || check())?;
    let output = crate::source::stage::render_captured(
        &prepared,
        [width, height],
        8,
        base,
        session,
        cancellation,
        &mut || check(),
    )?;
    drop(prepared);
    if let Err(reason) = input.recheck().and_then(|()| check()) {
        return match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; SVG source retirement refused: {cleanup}"
            )),
        };
    }
    Ok((output, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::tests::Fixture;
    use std::io::Write;

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="270" height="9"><path d="M .25 .5 L 269.5 .5 L 180 8.75 Z" fill="#317fc3"/></svg>"##;

    #[test]
    fn relative_native_image_paths_refuse_before_file_or_memory_admission() {
        let fixture = Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let session = runtime.begin().unwrap();
        let resident = runtime.resident().used();
        let disk = runtime.disk().used().unwrap();
        for extension in ["png", "qoi", "pnm", "bmp", "tga", "svg", "svgz"] {
            let path = format!("relative.{extension}");
            let reason = FileSource::open(
                &session,
                Path::new(&path),
                FileLimits::SELECTION,
                extension,
                &mut || Ok(()),
            )
            .err()
            .unwrap();
            assert!(reason.contains("absolute"), "{reason}");
            assert_eq!(runtime.resident().used(), resident);
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        drop((session, runtime, connection));
    }

    #[test]
    fn svg_admission_and_cancelled_staging_release_temporary_sources() {
        for budget in [32, 128] {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(budget * 1024 * 1024, 64 * 1024 * 1024);
            let session = runtime.begin().unwrap();
            let base =
                NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| {
                    Ok(())
                })
                .unwrap();
            let path = fixture.path().join("source.svg");
            std::fs::write(&path, SVG).unwrap();
            let resident = runtime.resident().used();
            let disk = runtime.disk().used().unwrap();
            let cancelled = Cancellation::default();
            cancelled.cancel();
            let reason = prepare(&base, &session, &path, &cancelled, &|| Ok(()))
                .err()
                .unwrap();
            assert!(reason.contains("cancelled"), "{reason}");
            assert_eq!(runtime.resident().used(), resident);
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let result = prepare(&base, &session, &path, &Cancellation::default(), &|| {
                if runtime.disk().used().unwrap() > disk {
                    Err("SVG import interrupted".into())
                } else {
                    Ok(())
                }
            });
            let reason = result.err().expect("The refused producer returned artwork");
            if budget == 128 {
                assert!(reason.contains("SVG import interrupted"), "{reason}");
            } else {
                assert_eq!(runtime.resident().used(), resident);
            }
            assert_eq!(runtime.disk().used().unwrap(), disk);
            drop((base, session, runtime, connection));
        }
    }

    #[test]
    fn gzip_crc_truncation_and_expansion_limits_refuse_without_staging() {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 8, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let gzip = |bytes: &[u8]| {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        };
        let valid = gzip(SVG.as_bytes());
        let mut corrupt = valid.clone();
        let crc = corrupt.len() - 8;
        corrupt[crc] ^= 1;
        let path = fixture.path().join("source.svgz");
        let resident = runtime.resident().used();
        let disk = runtime.disk().used().unwrap();
        for bytes in [
            corrupt,
            valid[..valid.len() - 1].to_vec(),
            gzip(&vec![b' '; crate::svg::MARKUP_LIMIT + 1]),
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(prepare(&base, &session, &path, &Cancellation::default(), &|| Ok(())).is_err());
            assert_eq!(runtime.resident().used(), resident);
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        drop((base, session, runtime, connection));
    }
}
