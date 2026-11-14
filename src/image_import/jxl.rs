//! Native JPEG XL import through the isolated, admitted row receiver.
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    image_import::source_file::{FileLimits, FileSource},
    raster::NATIVE_EDGE,
    workspace::source_runtime::SourceSession,
};
use std::path::Path;

pub(super) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    encoded_bytes: crate::jpeg_xl::NATIVE_ENCODED_LIMIT,
};

pub(super) fn inspect(path: &Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let file = crate::file_safety::open_native_read(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let info = crate::jpeg_xl::inspect_native_file(file)?;
    Ok(super::Info {
        format: "JPEG XL".into(), width: info.width, height: info.height, bit_depth: info.bit_depth,
        choice: None, count: 1,
    })
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    command: &serde_json::Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    if ["frame", "page", "variant"]
        .iter()
        .any(|key| command.get(*key).is_some())
    {
        return Err("Static JPEG XL does not accept frame/page/variant choices".into());
    }
    let file = FileSource::open(
        session,
        path,
        FILE_LIMITS,
        "jxl",
        &mut || check(),
    )?;
    prepare_source(base, session, file, check)
}

pub(super) fn prepare_source(
    base: &NativeDocument,
    session: &SourceSession,
    file: FileSource<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    prepare_input(base, session, file, None, check)
}

pub(super) fn prepare_patch(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let file = FileSource::open(session, path, FILE_LIMITS, "jxl", &mut || check())?;
    prepare_input(base, session, file, Some(base.metadata().depth()), check)
}

fn prepare_input(
    base: &NativeDocument,
    session: &SourceSession,
    mut file: FileSource<'_>,
    patch_depth: Option<u16>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let encoded = file.snapshot(&mut || check())?;
    let may_retire = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let result = (|| {
        let decoded = crate::jpeg_xl::rows_parent::decode(
            base,
            session,
            &encoded,
            patch_depth,
            may_retire.clone(),
            check,
        )?;
        let output = if (
            decoded.output.surface().width(),
            decoded.output.surface().height(),
            decoded.output.surface().depth(),
        ) != (
            decoded.info.width,
            decoded.info.height,
            decoded.info.bit_depth,
        ) {
            retire(Err("JPEG XL staged rows differ from inspected dimensions or precision".into()), decoded.output, session)
        } else if patch_depth.is_some()
            || (decoded.orientation == image::metadata::Orientation::NoTransforms
                && &*decoded.profile == crate::color_profile::srgb_profile())
        {
            Ok(decoded.output)
        } else {
            // The returned ICC describes these samples, including XYB conversion.
            let output = super::native::normalize(
                base,
                session,
                decoded.output.surface(),
                Some(&decoded.profile),
                decoded.orientation,
                check,
            );
            retire(output, decoded.output, session)
        }?;
        if let Err(reason) = file
            .recheck()
            .and_then(|()| encoded.check())
            .and_then(|()| check())
        {
            return retire(Err(reason), output, session);
        }
        Ok(output)
    })();
    if !may_retire.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(format!(
            "{}; JPEG XL source retained for verified recovery",
            result
                .err()
                .unwrap_or_else(|| "JPEG XL worker exit is unconfirmed".into())
        ));
    }
    match (result, encoded.retire(&mut || session.cleanup_check())) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!(
            "{reason}; JPEG XL encoded source retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => retire(Err(reason), output, session),
    }
}

fn retire(
    result: Result<StagedOutput, String>,
    input: StagedOutput,
    session: &SourceSession,
) -> Result<StagedOutput, String> {
    match (result, input.abort(session, &|| session.cleanup_check())) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!(
            "{reason}; JPEG XL source retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; JPEG XL candidate retirement refused: {cleanup}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::tests::Fixture;
    use crate::jpeg_xl::{self, Options, PixelsRef};

    #[test]
    fn native_jxl_reservations_and_snapshots_retire_after_refusal_cancel_and_retry() {
        let _codec = jpeg_xl::test_process_guard();
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(768 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = root.path().join("source.jxl");
        let bytes = jpeg_xl::encode(
            3,
            2,
            PixelsRef::Sixteen(&[12347; 24]),
            None,
            Options::default(),
            &|| Ok(()),
        )
        .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let command = serde_json::json!({});
        let resident = runtime.resident().used();
        let disk = runtime.disk().used().unwrap();
        let holds: Vec<_> = (0..10)
            .map(|_| {
                runtime
                    .resident()
                    .reserve_bytes(64 * 1024 * 1024, 64 * 1024 * 1024)
                    .unwrap()
            })
            .collect();
        assert!(prepare(&base, &session, &path, &command, &|| Ok(())).is_err());
        drop(holds);
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        // The inspected decode's child reservation is live, but its process
        // must not launch after this cancellation check refuses.
        let reason = prepare(&base, &session, &path, &command, &|| {
            if runtime.resident().used() > 600 * 1024 * 1024 {
                Err("JXL decode cancelled".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(reason.contains("JXL decode cancelled"), "{reason}");
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(prepare(&base, &session, &path, &command, &|| Ok(())).is_err());
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        std::fs::write(&path, &bytes).unwrap();
        let output = prepare(&base, &session, &path, &command, &|| Ok(())).unwrap();
        assert_eq!(
            (
                output.surface().width(),
                output.surface().height(),
                output.surface().depth()
            ),
            (3, 2, 16)
        );
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop((base, session, connection, runtime));
    }
}
