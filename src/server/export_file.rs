//! Flattened exports from an immutable resident document or admitted native scene.
use super::*;
use crate::{
    engine::{authority::NativeSeed, scene::SceneSnapshot},
    file_safety::{self, owned_rename, Lease},
    large_document::root::Directory,
    production_export::{native, Control, Format, Output, Settings},
    workspace::native_dispatch,
};

use native::MAX_OUTPUT_BYTES as NATIVE_BYTES;
const LEGACY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum NativeOutput<'a> {
    Canvas(crate::jpeg_xl::Options),
    Planned(&'a NativeSeed, &'a Output, &'a Settings),
}

enum Source {
    Resident(engine::Document),
    Native(NativeSeed, SceneSnapshot),
}

pub(super) fn write(
    shared: &Shared,
    path: &Path,
    request: &Value,
    control: &Control,
    native_options: Option<NativeOutput<'_>>,
    encode: impl FnOnce(&engine::Document, &mut dyn Write) -> Result<(), String>,
) -> Result<Option<usize>, String> {
    let _operation = FileOperation::begin(shared)?;
    control.check()?;
    let format = Format::from_path(path)?;
    let (source, working, project) = {
        let engine = shared.lock().unwrap();
        crate::workspace::validate_target(&engine, request)?;
        if request["expected_revision"]
            .as_u64()
            .is_some_and(|revision| revision != view::current(&engine).revision())
        {
            return Err("Project changed before exporting".into());
        }
        let source = match engine.native_seed()? {
            Some(seed) if native_options.is_some() => {
                if let Some(NativeOutput::Planned(planned, _, _)) = &native_options {
                    planned.validate(&engine)?;
                }
                Source::Native(seed, engine.capture_scene(None)?.into_native()?)
            }
            Some(_) => return Err("Production export requires a supported export source".into()),
            None => {
                if matches!(native_options, Some(NativeOutput::Planned(..))) {
                    return Err("Export source authority changed".into());
                }
                Source::Resident(engine.resident()?.clone())
            }
        };
        let working = engine.path.clone().or_else(|| {
            engine
                .file_ownership
                .as_ref()
                .map(|owner| owner.path.clone())
        });
        (source, working, engine.project_id.clone())
    };
    let read = match &source {
        Source::Native(_, scene) => Some(native_dispatch::begin_read_scene(shared, scene)?),
        Source::Resident(_) => None,
    };
    let check = || {
        control.check()?;
        if let Some(read) = &read {
            read.check()?;
        }
        Ok(())
    };
    let lease = if read.is_some() {
        Lease::acquire_native_export(path, check)?
    } else {
        Lease::acquire(path)?
    };
    let path = lease.path.as_path();
    if Format::from_path(path)? != format {
        return Err("Export alias must resolve to the same PNG, WebP or JPEG XL format".into());
    }
    if let Some(working) = &working {
        let working = file_safety::canonical_destination(working)?;
        let aliases = match file_safety::open_native_identity(&working) {
            Ok(file) => {
                let id = file_safety::identity(&file)?;
                lease.expected.is_some_and(|v| v.identity == id)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.to_string()),
        };
        if working == path || aliases {
            return Err("Image export cannot replace this project's working source. Choose a different destination.".into());
        }
    }
    if lease.expected.is_some() {
        if request["skip_existing"] == true {
            return Ok(None);
        }
        if request["overwrite"] != true {
            return Err(
                "Export destination exists. Confirm overwrite=true or choose a new path.".into(),
            );
        }
    }
    let _ownership = crate::file_ownership::Ownership::save(path, &project)?;
    let name = path.with_extension(format!("{}.tmp", engine::id()));
    let mut file = if read.is_some() {
        owned_rename::staging_options().open(&name)
    } else {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&name)
    }
    .map_err(|e| e.to_string())?;
    let identity = file_safety::identity(&file)?;
    let temporary = Temporary { name, identity };
    if lease.expected.is_some() {
        file_safety::preserve_permissions(path, &temporary.name)?;
    }
    let limit = if read.is_some() {
        NATIVE_BYTES
    } else {
        LEGACY_BYTES
    };
    let mut output = Sink {
        file: &mut file,
        bytes: 0,
        limit,
        fingerprint: ring::digest::Context::new(&ring::digest::SHA256),
        check: &check,
    };
    match (&source, &read, native_options) {
        (Source::Resident(doc), _, _) => encode(doc, &mut output)?,
        (Source::Native(_, scene), Some(read), Some(options)) => {
            if format == Format::Jxl
                && request.get("bit_depth").is_some_and(|depth| {
                    depth.as_u64() != Some(u64::from(scene.metadata().depth()))
                })
            {
                return Err(
                    "JPEG XL export preserves the working document's native channel depth".into(),
                );
            }
            match options {
                NativeOutput::Canvas(jxl) => {
                    let settings = Settings {
                        format,
                        jxl,
                        ..Settings::default()
                    };
                    let plan = native::canvas(scene, format);
                    native::render(
                        scene,
                        read,
                        &plan,
                        &settings,
                        &mut output,
                        NATIVE_BYTES,
                        &check,
                    )?;
                }
                NativeOutput::Planned(_, plan, settings) if plan.format == format => {
                    native::render(
                        scene,
                        read,
                        plan,
                        settings,
                        &mut output,
                        NATIVE_BYTES,
                        &check,
                    )?;
                }
                NativeOutput::Planned(..) => {
                    return Err("Export format differs from the plan".into())
                }
            }
        }
        _ => return Err("Native export lost its admitted source".into()),
    }
    output.flush().map_err(|e| e.to_string())?;
    let bytes = output.bytes;
    let reported_bytes = usize::try_from(bytes).map_err(|_| "Export byte count overflow")?;
    let fingerprint = file_safety::finish_fingerprint(output.fingerprint.clone());
    drop(output);
    if read.is_some() {
        owned_rename::finish_write_stamp(&file)?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    let mut verify = || check();
    let (directory, sealed, staged) = match &source {
        Source::Native(_, scene) => {
            let directory = Directory::open(path.parent().ok_or("Export parent missing")?)?;
            let sealed = owned_rename::Sealed::verify(
                &file,
                bytes,
                fingerprint,
                scene.budget(),
                &mut verify,
            )?;
            (Some(directory), Some(sealed), None)
        }
        Source::Resident(_) => {
            let staged = file_safety::verified_version(&temporary.name, bytes, fingerprint, check)?;
            file_safety::check_native_source_binding(&temporary.name, &file, staged)?;
            (None, None, Some(staged))
        }
    };
    let backup = lease.preserve_checked(check)?;
    // Large destination hashing and backup stay outside the document lock.
    lease.validate_checked(check)?;
    check()?;
    let engine = shared.lock().unwrap();
    engine.ensure_open()?;
    let publish = || -> Result<(), String> {
        control.check()?;
        if let Some(version) = lease.expected {
            let named = file_safety::open_native_identity(path).map_err(|e| e.to_string())?;
            file_safety::check_native_source_binding(path, &named, version)?;
        }
        if let (Some(directory), Some(sealed)) = (&directory, &sealed) {
            directory.check()?;
            if lease.expected.is_none() {
                return owned_rename::publish_new(
                    directory.path(),
                    directory.held(),
                    Path::new(
                        temporary
                            .name
                            .file_name()
                            .ok_or("Export staging name missing")?,
                    ),
                    Path::new(path.file_name().ok_or("Export filename missing")?),
                    &file,
                    sealed,
                    &mut || control.check(),
                );
            }
            sealed.check_path(&file, &temporary.name)?;
        } else if let Some(staged) = staged {
            lease.validate()?;
            file_safety::check_native_source_binding(&temporary.name, &file, staged)?;
        }
        publish_legacy_file(&temporary.name, path, lease.expected.is_some())
    };
    match &source {
        Source::Resident(doc) => {
            if !matches!(view::current(&engine), view::DocumentView::Resident(current)
                if current.id == doc.id && current.revision == doc.revision)
            {
                return Err("Project source changed before exporting".into());
            }
            publish()?;
        }
        Source::Native(seed, _) => {
            let read = read.as_ref().ok_or("Native export read session missing")?;
            seed.validate(&engine)?;
            let workspace = engine
                .workspace
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .ok_or("Native export workspace disappeared")?;
            read.with_live_fence(|| {
                let workspace = workspace
                    .try_lock()
                    .map_err(|_| "Export workspace is busy")?;
                if workspace.exiting
                    || !workspace
                        .source_runtime
                        .as_ref()
                        .is_some_and(|runtime| read.uses_runtime(runtime))
                {
                    return Err("Export source workspace changed".into());
                }
                publish()
            })??;
        }
    }
    drop(engine);
    sync_parent(path)?;
    drop(file);
    let published = lease.version_at(path, || Ok(()))?;
    if published.identity != identity
        || published.bytes != bytes
        || published.sha256() != fingerprint
        || staged.is_some_and(|staged| staged != published)
    {
        return Err("Published file changed before verification. Current work remains open; inspect the export destination.".into());
    }
    if let Some(backup) = backup {
        lease.prune(&backup);
    }
    Ok(Some(reported_bytes))
}

struct Sink<'a> {
    file: &'a mut fs::File,
    bytes: u64,
    limit: u64,
    fingerprint: ring::digest::Context,
    check: &'a dyn Fn() -> Result<(), String>,
}
impl Write for Sink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        (self.check)().map_err(std::io::Error::other)?;
        if self
            .bytes
            .checked_add(bytes.len() as u64)
            .is_none_or(|n| n > self.limit)
        {
            return Err(std::io::Error::other("Export exceeds the file size limit"));
        }
        let n = self.file.write(bytes)?;
        self.fingerprint.update(&bytes[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (self.check)().map_err(std::io::Error::other)?;
        self.file.flush()
    }
}
