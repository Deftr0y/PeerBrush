mod fonts;
use crate::{
    engine::{self, Engine, Scope},
    psd, raster,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};

pub type Shared = Arc<Mutex<Engine>>;

pub(crate) struct FileOperation(pub(crate) Arc<std::sync::atomic::AtomicUsize>);
impl FileOperation {
    pub(crate) fn begin_workspace(workspace: &crate::workspace::Registry) -> Result<Self, String> {
        let state = workspace.lock().unwrap();
        if state.exiting {
            return Err("Workspace is exiting; settings and recovery remain protected".into());
        }
        Ok(Self::track(state.file_operations.clone()))
    }
    pub(crate) fn begin(shared: &Shared) -> Result<Self, String> {
        let engine = shared.lock().unwrap();
        engine.ensure_open()?;
        Ok(Self::track(engine.file_operations.clone()))
    }
    pub(crate) fn track(counter: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}
impl Drop for FileOperation {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
#[cfg(test)]
#[path = "png_export_tests.rs"]
mod png_export_tests;
#[cfg(test)]
#[path = "save_safety_tests.rs"]
mod save_safety_tests;
pub struct Connection {
    pub port: u16,
    pub token: String,
    pub state_dir: PathBuf,
    pub instance_lock: Option<fs::File>,
    pub(crate) availability: Arc<AtomicBool>,
}
impl Connection {
    pub fn is_available(&self) -> bool {
        self.availability.load(Ordering::Acquire)
    }
}
struct ServerAvailability(Arc<AtomicBool>);
impl Drop for ServerAvailability {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_write_checked(path, bytes, |_| Ok(()))
}
pub(crate) fn atomic_write_checked(
    path: &Path,
    bytes: &[u8],
    validate: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", engine::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        if path.exists() {
            crate::file_safety::preserve_permissions(path, &tmp)?;
        }
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        validate(&tmp)?;
        replace_file(&tmp, path)?;
        sync_parent(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    fs::rename(from, to).map_err(|e| e.to_string())
}
#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // Same-directory replacement, with Windows write-through durability.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}
pub(crate) fn sync_parent(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
pub fn open(shared: &Shared, path: &Path) -> Result<(), String> {
    open_progress(shared, path, &crate::loading::Control::default())
}
struct LoadGuard {
    shared: Shared,
    control: crate::loading::Control,
}
impl Drop for LoadGuard {
    fn drop(&mut self) {
        let mut e = self.shared.lock().unwrap();
        if e.loading.as_ref().is_some_and(|c| c.same(&self.control)) {
            e.loading = None;
        }
    }
}
pub fn open_progress(
    shared: &Shared,
    path: &Path,
    control: &crate::loading::Control,
) -> Result<(), String> {
    open_request(shared, path, control, &json!({}), None)
}
pub fn open_options(
    shared: &Shared,
    path: &Path,
    control: &crate::loading::Control,
    options: &Value,
) -> Result<(), String> {
    open_request(shared, path, control, options, None)
}
pub(crate) fn open_prepared_image(
    shared: &Shared,
    prepared: crate::image_open::Prepared,
    control: &crate::loading::Control,
    options: &Value,
) -> Result<(), String> {
    let path = prepared.path.clone();
    open_request(shared, &path, control, options, Some(prepared))
}
fn open_request(
    shared: &Shared,
    path: &Path,
    control: &crate::loading::Control,
    options: &Value,
    prepared: Option<crate::image_open::Prepared>,
) -> Result<(), String> {
    control.begin()?;
    let _loading = LoadGuard {
        shared: shared.clone(),
        control: control.clone(),
    };
    let result = open_progress_inner(shared, path, control, options, prepared);
    {
        let mut e = shared.lock().unwrap();
        if e.loading.as_ref().is_none_or(|c| c.same(control)) {
            e.status = result
                .as_ref()
                .map(|_| {
                    if path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("psb"))
                    {
                        "PSB opened".into()
                    } else if crate::image_open::photoshop(path) {
                        "PSD opened".into()
                    } else {
                        "Image opened — save as PSD or PSB".into()
                    }
                })
                .unwrap_or_else(|e| e.clone());
            e.loading = None;
        }
    }
    if let Err(error) = &result {
        let workspace = shared
            .lock()
            .unwrap()
            .workspace
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        if let Some(workspace) = workspace {
            crate::workspace::duplicate_warning(&workspace, path, error, false);
        }
    }
    result
}
fn open_progress_inner(
    shared: &Shared,
    path: &Path,
    control: &crate::loading::Control,
    options: &Value,
    prepared: Option<crate::image_open::Prepared>,
) -> Result<(), String> {
    if path.is_dir() {
        return Err("Choose a supported image, PSD or PSB file inside this folder".into());
    }
    let (before, project) = {
        let mut e = shared.lock().unwrap();
        e.ensure_open()?;
        if e.loading.as_ref().is_some_and(|c| !c.same(control)) {
            return Err("A document is already opening".into());
        }
        let before = control
            .source()
            .unwrap_or_else(|| (e.doc.id.clone(), e.doc.revision));
        if before != (e.doc.id.clone(), e.doc.revision) {
            return Err("Canvas changed before opening; current work preserved".into());
        }
        e.loading = Some(control.clone());
        (before, e.project_id.clone())
    };
    if !crate::image_open::photoshop(path) {
        let prepared = match prepared {
            Some(p) => p,
            None => crate::image_open::Prepared::read(path, &project, control)?,
        };
        let doc = prepared.document(options, control)?;
        let mut e = shared.lock().unwrap();
        prepared.validate(control)?;
        if (e.doc.id.clone(), e.doc.revision) != before {
            return Err("Canvas changed while opening; current work preserved".into());
        }
        e.replace(doc, None)?;
        e.release_file_ownership();
        prepared.ownership.published(prepared.version.identity);
        e.file_ownership = Some(prepared.ownership);
        e.file_version = None;
        e.saved_revision = u64::MAX;
        return Ok(());
    }
    if ["frame", "page", "variant"]
        .iter()
        .any(|key| options.get(key).is_some())
    {
        return Err(
            "Frame, page and variant choices apply to images, not Photoshop documents".into(),
        );
    }
    let ownership = crate::file_ownership::Ownership::open(path, &project)?;
    let version = crate::file_safety::version_with_check(path, || control.check())?;
    if version.bytes > 256 * 1024 * 1024 {
        return Err("Photoshop document exceeds the initial 256 MiB file limit".into());
    }
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    if crate::file_safety::version_with_check(path, || control.check())? != version {
        return Err("File changed while opening; try again".into());
    }
    let mut doc = psd::decode_reader(&mut file, control)?;
    doc.name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into();
    let mut e = shared.lock().unwrap();
    control.check()?;
    ownership.prepare(path)?;
    if crate::file_safety::version_with_check(path, || control.check())? != version {
        return Err("Photoshop file changed during decoding; current work preserved".into());
    }
    if (e.doc.id.clone(), e.doc.revision) != before {
        return Err("Canvas changed while opening. Save the current work and try again.".into());
    }
    e.replace(doc, Some(path.into()))?;
    ownership.published(version.identity);
    e.release_file_ownership();
    e.file_ownership = Some(ownership);
    e.file_version = Some(version);
    Ok(())
}
/// Explicitly flatten unsupported Photoshop structure without reducing channel precision or replacing its file.
pub fn compatible_copy(shared: &Shared, actor: &str, expected: Option<u64>) -> Result<(), String> {
    compatible_copy_with_color(shared, actor, expected, false)
}
pub fn compatible_copy_with_color(
    shared: &Shared,
    actor: &str,
    expected: Option<u64>,
    convert_to_srgb: bool,
) -> Result<(), String> {
    compatible_copy_guarded(shared, actor, expected, convert_to_srgb, None)
}
fn compatible_copy_guarded(
    shared: &Shared,
    actor: &str,
    expected: Option<u64>,
    convert_to_srgb: bool,
    document: Option<&str>,
) -> Result<(), String> {
    let source = {
        let mut e = shared.lock().unwrap();
        if document.is_some_and(|id| id != e.doc.id) {
            return Err("Project source changed before preparing a compatible copy".into());
        }
        e.expire();
        e.check(
            actor,
            &[Scope {
                target: None,
                rect: None,
            }],
        )?;
        if !e.doc.read_only {
            return Err("This document is already editable".into());
        }
        if expected.is_some_and(|rev| rev != e.doc.revision) {
            return Err("Document changed; observe again before converting".into());
        }
        e.doc.clone()
    };
    if let Some(profile) = source.icc_profile.as_deref() {
        if !convert_to_srgb {
            return Err("An embedded profile requires explicit convert_to_srgb=true to create an editable copy".into());
        }
        crate::color_profile::supported(profile)?;
    }
    let mut doc = engine::Document::new_depth(source.width, source.height, source.bit_depth)?;
    doc.srgb_tagged = source.icc_profile.is_some() || source.srgb_tagged;
    if source.bit_depth == 16 {
        let mut image = crate::depth16::render(&source)?;
        if let Some(profile) = source.icc_profile.as_deref() {
            crate::color_profile::convert16(profile, &mut image.words)?;
        }
        doc.layers[0].pixels =
            raster::Raster::from_rgba16(image.width, image.height, &image.words)?;
    } else {
        let (w, h, mut pixels, _) =
            source.preview_native8(None, source.width.max(source.height), None, false)?;
        if let Some(profile) = source.icc_profile.as_deref() {
            crate::color_profile::convert8(profile, &mut pixels)?;
        }
        doc.layers[0].pixels = raster::Raster::from_rgba(w, h, &pixels)?;
    }
    doc.name = format!(
        "{} - {}-bit{} copy.psd",
        std::path::Path::new(&source.name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&source.name),
        source.bit_depth,
        if source.icc_profile.is_some() {
            " sRGB"
        } else {
            ""
        }
    );
    doc.layers[0].name = format!("Flattened {}-bit artwork", source.bit_depth);
    doc.revision = 1;
    let mut e = shared.lock().unwrap();
    if e.doc.id != source.id || e.doc.revision != source.revision {
        return Err("Document changed while preparing the copy".into());
    }
    e.expire();
    e.check(
        actor,
        &[Scope {
            target: None,
            rect: None,
        }],
    )?;
    e.replace(doc, None)?;
    e.release_file_ownership();
    e.saved_revision = 0;
    e.file_version = None;
    Ok(())
}
pub fn save(shared: &Shared, path: &Path) -> Result<(), String> {
    save_guarded(shared, path, &json!({}))
}
pub fn save_source(shared: &Shared, path: &Path, document: &str) -> Result<(), String> {
    save_guarded(shared, path, &json!({"document_id":document}))
}
/// Native Save As dialog has already obtained explicit replacement consent.
pub fn save_confirmed_source(
    shared: &Shared,
    path: &Path,
    document: &str,
    revision: u64,
    overwrite: bool,
) -> Result<(), String> {
    save_guarded(
        shared,
        path,
        &json!({"document_id":document,"expected_revision":revision,"overwrite":overwrite}),
    )
}
pub fn save_reviewed_source(
    shared: &Shared,
    path: &Path,
    document: &str,
    revision: u64,
) -> Result<(), String> {
    save_guarded(
        shared,
        path,
        &json!({"document_id":document,"expected_revision":revision}),
    )
}
/// The caller explicitly accepted the compatibility report for this source.
pub fn save_accepted_source(
    shared: &Shared,
    path: &Path,
    document: &str,
    revision: u64,
    overwrite: bool,
) -> Result<(), String> {
    save_guarded(
        shared,
        path,
        &json!({"document_id":document,"expected_revision":revision,"overwrite":overwrite,"accept_editability_loss":true}),
    )
}
fn save_guarded(shared: &Shared, path: &Path, request: &Value) -> Result<(), String> {
    save_prepared(shared, path, request, || {})
}
fn save_prepared(
    shared: &Shared,
    path: &Path,
    request: &Value,
    before_commit: impl FnOnce(),
) -> Result<(), String> {
    let _operation = FileOperation::begin(shared)?;
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| !e.eq_ignore_ascii_case("psd") && !e.eq_ignore_ascii_case("psb"))
        .unwrap_or(true)
    {
        return Err("Working documents must use .psd or .psb".into());
    }
    let lease = crate::file_safety::Lease::acquire(path)?;
    let path = lease.path.as_path();
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb"))
    {
        return Err("Save alias must resolve to a .psd or .psb destination".into());
    }
    let (doc, source_path, source_version, saved_revision, project) = {
        let e = shared.lock().unwrap();
        crate::workspace::validate_target(&e, request)?;
        if request["expected_revision"]
            .as_u64()
            .is_some_and(|r| r != e.doc.revision)
        {
            return Err("Project revision changed before saving".into());
        }
        (
            e.doc.clone(),
            e.path.clone(),
            e.file_version,
            e.saved_revision,
            e.project_id.clone(),
        )
    };
    let saving_source = source_path
        .as_deref()
        .map(|p| crate::file_safety::same_destination(p, path))
        .transpose()?
        .unwrap_or(false);
    if saving_source {
        if lease.expected != source_version {
            return Err(
                "Photoshop file changed outside PeerBrush. Use Save As to preserve both versions."
                    .into(),
            );
        }
    } else if lease.expected.is_some() && request["overwrite"] != true {
        return Err(
            "Destination exists. Confirm overwrite=true or choose a new Save As path.".into(),
        );
    }
    crate::compatibility::authorize_save(&doc, request)?;
    let ownership = crate::file_ownership::Ownership::save(path, &project)?;
    let bytes = if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("psb"))
    {
        psd::encode_psb(&doc)?
    } else {
        psd::encode(&doc)?
    };
    // Preserve the previous exact file before touching its replacement.
    let backup = lease.preserve()?;
    before_commit();
    let mut e = shared.lock().unwrap();
    e.ensure_open()?;
    if e.doc.id != doc.id
        || e.doc.revision != doc.revision
        || e.path != source_path
        || e.file_version != source_version
        || e.saved_revision != saved_revision
    {
        return Err("Project source changed while saving; current work preserved".into());
    }
    lease.validate()?;
    atomic_write_checked(path, &bytes, |temporary| {
        ownership.prepare(temporary)?;
        lease.validate()
    })?;
    let version = crate::file_safety::published_version(path, &bytes)?;
    ownership.published(version.identity);
    if e.doc.id == doc.id {
        e.path = Some(path.into());
        e.file_version = Some(version);
        e.doc.name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into();
        e.saved_revision = doc.revision;
        e.release_file_ownership();
        e.file_ownership = Some(ownership);
    }
    if let Some(backup) = backup {
        lease.prune(&backup);
    }
    Ok(())
}
pub fn export(shared: &Shared, path: &Path) -> Result<(), String> {
    export_guarded(shared, path, &json!({}))
}
pub fn export_source(shared: &Shared, path: &Path, document: &str) -> Result<(), String> {
    export_guarded(shared, path, &json!({"document_id":document}))
}
pub fn export_confirmed_source(
    shared: &Shared,
    path: &Path,
    document: &str,
    revision: u64,
) -> Result<(), String> {
    export_guarded(
        shared,
        path,
        &json!({"document_id":document,"expected_revision":revision,"overwrite":true}),
    )
}
fn export_guarded(shared: &Shared, path: &Path, request: &Value) -> Result<(), String> {
    if request["skip_existing"] == true {
        return Err(
            "Use exports start with an explicit existing-file policy for batch skips".into(),
        );
    }
    let format = crate::production_export::Format::from_path(path)?;
    if format == crate::production_export::Format::Webp
        && request
            .get("bit_depth")
            .is_some_and(|value| value.as_u64() != Some(8))
    {
        return Err("WebP output supports only 8-bit channels; the working document stays at its native depth".into());
    }
    if format != crate::production_export::Format::Jxl && request.get("jxl").is_some() {
        return Err("JPEG XL options require a .jxl export destination".into());
    }
    let jxl: crate::jpeg_xl::Options = request
        .get("jxl")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|e| format!("Invalid JPEG XL options: {e}"))
        })
        .transpose()?
        .unwrap_or_default();
    jxl.validate()?;
    let control = crate::production_export::Control::default();
    export_derived_to_guarded(shared, path, request, &control, |doc, writer| {
        if format == crate::production_export::Format::Jxl
            && request
                .get("bit_depth")
                .is_some_and(|depth| depth.as_u64() != Some(u64::from(doc.bit_depth)))
        {
            return Err(
                "JPEG XL export preserves the working document's native channel depth".into(),
            );
        }
        crate::production_export::render_document_to(doc, format, jxl, &control, writer)
    })
    .map(|_| ())
}
#[cfg(test)]
pub(crate) fn export_derived_guarded(
    shared: &Shared,
    path: &Path,
    request: &Value,
    control: &crate::production_export::Control,
    encode: impl FnOnce(&crate::engine::Document) -> Result<Vec<u8>, String>,
) -> Result<bool, String> {
    export_derived_to_guarded(shared, path, request, control, |doc, writer| {
        writer.write_all(&encode(doc)?).map_err(|e| e.to_string())
    })
    .map(|bytes| bytes.is_some())
}

pub(crate) fn export_derived_to_guarded(
    shared: &Shared,
    path: &Path,
    request: &Value,
    control: &crate::production_export::Control,
    encode: impl FnOnce(&crate::engine::Document, &mut dyn Write) -> Result<(), String>,
) -> Result<Option<usize>, String> {
    let _operation = FileOperation::begin(shared)?;
    control.check()?;
    let format = crate::production_export::Format::from_path(path)?;
    let lease = crate::file_safety::Lease::acquire(path)?;
    let path = lease.path.as_path();
    if crate::production_export::Format::from_path(path)? != format {
        return Err("Export alias must resolve to the same PNG, WebP or JPEG XL format".into());
    }
    if lease.expected.is_some() && request["overwrite"] != true && request["skip_existing"] != true
    {
        return Err(
            "Export destination exists. Confirm overwrite=true or choose a new path.".into(),
        );
    }
    let doc = {
        let e = shared.lock().unwrap();
        crate::workspace::validate_target(&e, request)?;
        if e.path
            .as_deref()
            .or_else(|| e.file_ownership.as_ref().map(|owner| owner.path.as_path()))
            .map(|source| crate::file_safety::same_destination(source, path))
            .transpose()?
            .unwrap_or(false)
        {
            return Err("Image export cannot replace this project's working source. Choose a different destination.".into());
        }
        if request["expected_revision"]
            .as_u64()
            .is_some_and(|r| r != e.doc.revision)
        {
            return Err("Project changed before exporting".into());
        }
        e.doc.clone()
    };
    // Protect sources owned by another project or application instance too.
    if lease.expected.is_some() && request["skip_existing"] == true {
        return Ok(None);
    }
    let project = shared.lock().unwrap().project_id.clone();
    let _ownership = crate::file_ownership::Ownership::save(path, &project)?;
    // Keep the existing 256 MiB file-safety envelope while encoding directly to
    // a private same-directory file outside the document lock.
    struct Output<'a> {
        file: &'a mut fs::File,
        bytes: usize,
        fingerprint: [u64; 2],
        control: &'a crate::production_export::Control,
    }
    impl Write for Output<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.control.check().map_err(std::io::Error::other)?;
            if self
                .bytes
                .checked_add(bytes.len())
                .is_none_or(|n| n > 256 * 1024 * 1024)
            {
                return Err(std::io::Error::other(
                    "Export exceeds the 256 MiB file safety limit",
                ));
            }
            let n = self.file.write(bytes)?;
            crate::file_safety::hash(&bytes[..n], &mut self.fingerprint);
            self.bytes += n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.control.check().map_err(std::io::Error::other)?;
            self.file.flush()
        }
    }
    struct Temporary(PathBuf);
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let temporary = Temporary(path.with_extension(format!("{}.tmp", engine::id())));
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&temporary.0)
        .map_err(|e| e.to_string())?;
    let identity = crate::file_safety::identity(&file)?;
    if path.exists() {
        crate::file_safety::preserve_permissions(path, &temporary.0)?;
    }
    let mut output = Output {
        file: &mut file,
        bytes: 0,
        fingerprint: [0xcbf29ce484222325, 0x84222325cbf29ce4],
        control,
    };
    encode(&doc, &mut output)?;
    output.flush().map_err(|e| e.to_string())?;
    let bytes = output.bytes;
    let fingerprint = output.fingerprint;
    drop(output);
    file.sync_all().map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let stamp = (
        metadata.len(),
        metadata.modified().map_err(|e| e.to_string())?,
    );
    let named_metadata = fs::symlink_metadata(&temporary.0).map_err(|e| e.to_string())?;
    if !named_metadata.is_file() || named_metadata.file_type().is_symlink() {
        return Err("Export temporary path changed; destination retained".into());
    }
    let staged =
        crate::file_safety::verified_version(&temporary.0, bytes as u64, fingerprint, || {
            control.check()
        })?;
    if staged.identity != identity || staged.bytes != bytes as u64 {
        return Err("Export temporary file changed; destination retained".into());
    }
    control.check()?;
    let backup = lease.preserve()?;
    let e = shared.lock().unwrap();
    e.ensure_open()?;
    if e.doc.id != doc.id || e.doc.revision != doc.revision {
        return Err("Project source changed before exporting".into());
    }
    lease.validate()?;
    control.check()?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let named_metadata = fs::symlink_metadata(&temporary.0).map_err(|e| e.to_string())?;
    let named = fs::File::open(&temporary.0).map_err(|e| e.to_string())?;
    if !named_metadata.is_file()
        || named_metadata.file_type().is_symlink()
        || (
            metadata.len(),
            metadata.modified().map_err(|e| e.to_string())?,
        ) != stamp
        || crate::file_safety::identity(&file)? != identity
        || crate::file_safety::identity(&named)? != identity
    {
        return Err("Export temporary file changed; destination retained".into());
    }
    drop(named);
    replace_file(&temporary.0, path)?;
    sync_parent(path)?;
    drop(e);
    drop(file);
    let published = crate::file_safety::version_with_check(path, || Ok(()))?;
    if published != staged {
        return Err("Published file changed before verification. Current work remains open; inspect the destination and use a new export path.".into());
    }
    if let Some(backup) = backup {
        lease.prune(&backup);
    }
    Ok(Some(bytes))
}

pub fn tools() -> Value {
    let mut tools = json!([
        {"name":"peerbrush_observe","description":"Inspect live document structure and actual PNG image content. Coordinates are document pixels, origin top left. Request layer/mask/rect views, max_edge and since_revision to reduce image traffic.","inputSchema":{"type":"object","properties":{"layer":{"type":"string"},"mask":{"type":"boolean"},"selection_view":{"type":"string","enum":["cutout","mask","overlay"]},"rect":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4},"max_edge":{"type":"integer","minimum":32,"maximum":4096},"since_revision":{"type":"integer"},"image":{"type":"boolean"}},"additionalProperties":false}},
        {"name":"peerbrush_edit","description":"Atomically apply typed editing commands to the shared document. First observe for IDs and revision. Include expected_revision and task when reserved. Inspect capabilities for command examples. All changes are undoable; no screen-coordinate clicking or code evaluation.","inputSchema":{"type":"object","properties":{"actor":{"type":"string"},"commands":{"type":"array","items":{"type":"object"},"minItems":1,"maxItems":100},"expected_revision":{"type":"integer"},"task":{"type":"string"},"label":{"type":"string"},"feedback":{"type":"string","enum":["batch","request","always"]},"max_edge":{"type":"integer"}},"required":["commands"]}},
        {"name":"peerbrush_proposal","description":"Create and render frozen proposed edits for explicit human review. create needs observed document_id/expected_revision and ordinary engine commands; the shared project and history stay unchanged. preview returns actual PNG pixels with document coordinates. Only actor human can accept; agents can list, preview or reject their proposals. A changed project or ended/expired/taken-over task invalidates its draft. Never silently reacquire a task. Direct edits remain available when already authorized.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["create","list","preview","accept","reject"]},"actor":{"type":"string"},"proposal":{"type":"string"},"document_id":{"type":"string"},"expected_revision":{"type":"integer"},"commands":{"type":"array","items":{"type":"object"},"minItems":1,"maxItems":100},"task":{"type":"string"},"label":{"type":"string"},"rect":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4},"max_edge":{"type":"integer","minimum":32,"maximum":4096},"feedback":{"type":"string","enum":["batch","request"]}},"required":["action"],"additionalProperties":false}},
        {"name":"peerbrush_task","description":"Begin/update/end a selective reservation for layers or rectangular document regions. Scopes have optional target (layer ID) and rect [left,top,right,bottom]. Descriptions appear live in the top bar: write concise natural-language activity, update as you work. Empty scopes permit cooperative edits without locking. Reservations expire after five idle minutes; user takeover revokes them. Never silently reacquire after takeover.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["begin","update","end","status","recovery"]},"actor":{"type":"string"},"task":{"type":"string"},"description":{"type":"string"},"scopes":{"type":"array","items":{"type":"object","properties":{"target":{"type":["string","null"]},"rect":{"type":["array","null"],"items":{"type":"integer"},"minItems":4,"maxItems":4}}}},"feedback":{"type":"string","enum":["batch","request","always"]}},"required":["action"]}},
        {"name":"peerbrush_document","description":"Open supported images or PSD v1/PSB v2, save PSD/PSB, or export flattened PNG/WebP/JPEG XL selected by destination extension. PNG and JPEG XL keep native 8/16-bit channels; lossless WebP is explicitly 8-bit output only, including from native16 sources. All retain alpha and supported color handling; JPEG XL assigns untagged RGB to sRGB and defaults to lossless, with optional jxl lossless/distance/effort settings; exporting never changes the editable document's precision. Image sources open unsaved at native dimensions and 8/16-bit depth; select explicit frame/page/variant for multi-content images. Working saves remain PSD/PSB under the same 8192-edge, 32 MP and 256 MiB limits. open_async returns immediately; observe loading progress and file_status, cancel_open preserves the current project. compatible_copy explicitly flattens protected Photoshop structure into a new project at the same 8/16-bit depth; source file is retained. Supported embedded RGB matrix/TRC and classic LUT ICC profiles require convert_to_srgb=true; unsupported profiles cannot become editable copies. save_report requires observed document_id/expected_revision and describes actual standard-layer baking and supplementary-source editability without saving. Saves with reported losses require explicit accept_editability_loss=true for that exact document_id/expected_revision; no continuing consent or bypass of protected originals/file conflicts. pattern_sources inspects protected raw RGB pattern assets; it requires observed document_id/expected_revision. Optional pattern_index returns native PNG content in pattern_asset coordinates, without interpreting Photoshop color handling or style placement. Use explicit local paths. Opening replaces the current document and refuses to discard unsaved work unless discard=true.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["new","open","open_async","cancel_open","save","save_report","export","compatible_copy","pattern_sources"]},"path":{"type":"string"},"frame":{"type":"integer","minimum":0},"page":{"type":"integer","minimum":0},"variant":{"type":"integer","minimum":0},"width":{"type":"integer"},"height":{"type":"integer"},"pattern_index":{"type":"integer","minimum":0,"maximum":15},"jxl":{"type":"object","properties":{"lossless":{"type":"boolean","default":true},"distance":{"type":"number","minimum":0.1,"maximum":15,"default":1},"effort":{"type":"integer","minimum":1,"maximum":9,"default":3}},"additionalProperties":false},"bit_depth":{"type":"integer","enum":[8,16]},"discard":{"type":"boolean"},"convert_to_srgb":{"type":"boolean"},"document_id":{"type":"string"},"project_id":{"type":"string"},"expected_revision":{"type":"integer"}},"required":["action"]}},
        {"name":"peerbrush_history","description":"Inspect results, undo/redo chronological batches, or inspect_task/undo_task to compensate an agent task while preserving later work. task identifies the agent reservation; task_actor defaults to actor, human may select any agent. Conflicting pixels/settings/structure reject the whole task undo. Mutations require the current expected_revision for agents and return visual feedback.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","undo","redo","inspect_task","undo_task"]},"actor":{"type":"string"},"task":{"type":"string"},"task_actor":{"type":"string"},"expected_revision":{"type":"integer"},"feedback":{"type":"string","enum":["batch","request"]},"max_edge":{"type":"integer"}}}},
        {"name":"peerbrush_place_image","description":"Place generated or edited image pixels in one undoable operation. Supply a supported absolute local image path or base64 PNG. SVG/SVGZ placement creates rendered pixels; image.import retains editable vector markup. Use image_info and explicit frame/page/variant for animations, multipage TIFF and icons. Supply an exact destination rect [left,top,right,bottom] in document pixels. new_layer defaults true; layer chooses sibling/folder context and parent can override it. Set new_layer:false to modify that layer; mode replace replaces transparent pixels too, over composites. Surrounding pixels, masks and editable effects are retained. Returns placed layer ID, rectangle and cropped PNG feedback.","inputSchema":{"type":"object","properties":{"frame":{"type":"integer","minimum":0},"page":{"type":"integer","minimum":0},"variant":{"type":"integer","minimum":0},"path":{"type":"string"},"png":{"type":"string"},"rect":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4},"layer":{"type":"string"},"parent":{"type":["string","null"]},"new_layer":{"type":"boolean"},"name":{"type":"string"},"mode":{"type":"string","enum":["over","replace"]},"actor":{"type":"string"},"expected_revision":{"type":"integer"},"task":{"type":"string"},"label":{"type":"string"},"feedback":{"type":"string","enum":["batch","request","always"]},"max_edge":{"type":"integer","minimum":32,"maximum":4096}},"required":["rect"],"oneOf":[{"required":["path"]},{"required":["png"]}],"additionalProperties":false}},
        {"name":"peerbrush_segment","description":"Create a learned subject/object confidence selection using the externally configured local provider. Model choice stays outside PeerBrush. Supply current expected_revision and optional document-pixel rect, point, layer and selection mode. Human edits cancel inference; original 8/16-bit image channels remain untouched. Returns actual selection PNG feedback with coordinates.","inputSchema":{"type":"object","properties":{"actor":{"type":"string"},"expected_revision":{"type":"integer"},"document_id":{"type":"string"},"task":{"type":"string"},"rect":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4},"point":{"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2},"layer":{"type":"string"},"mode":{"type":"string","enum":["replace","add","subtract","intersect"]},"feedback":{"type":"string","enum":["batch","request"]},"max_edge":{"type":"integer"}},"required":["expected_revision"],"additionalProperties":false}},
        {"name":"peerbrush_capabilities","description":"Get concise supported operations and runnable JSON examples before editing.","inputSchema":{"type":"object","properties":{}}}
        ,{"name":"peerbrush_brushes","description":"Browse original brush presets, render actual stroke previews, and save/update/delete instance-local custom brushes. Presets work in paint/smudge/clone/heal commands via preset ID with explicit setting overrides. This library is outside document history; curated presets are immutable. Preview coordinates refer to brush_preview, not the document.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","preview","save","delete"]},"id":{"type":"string"},"name":{"type":"string"},"category":{"type":"string"},"settings":{"type":"object"},"width":{"type":"integer","minimum":64,"maximum":512},"height":{"type":"integer","minimum":24,"maximum":128}},"required":["action"],"additionalProperties":false}}
    ]);
    tools.as_array_mut().unwrap().push(json!({"name":"peerbrush_recovery","description":"List complete instance-local autosave versions and failures. Restore an explicit snapshot into a new independent unsaved project, retaining original files and recovery versions. Returns actual image content with coordinates. Only human input may discard an explicit version; never discard to hide an autosave failure.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","restore","discard"]},"snapshot":{"type":"string"},"actor":{"type":"string"}},"required":["action"],"additionalProperties":false}}));
    tools.as_array_mut().unwrap().push(json!({"name":"peerbrush_filters","description":"Browse whole-image filter presets; thumbnail renders a standard reference image, preview renders the explicitly targeted current project without history and returns frozen commands. Apply with peerbrush_edit filter.add/update; edit settings, strength, bypass, delete and reorder non-destructively. Top filters run last after the composite. Save/rename/delete/import/export validated instance-local custom presets outside history. Curated presets are immutable. Preview requires current project_id/document_id/expected_revision; original 8/16-bit sources stay editable.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","thumbnail","preview","save","rename","delete","import","export"]},"id":{"type":"string"},"name":{"type":"string"},"category":{"type":"string"},"kind":{"type":"string"},"settings":{"type":"object"},"weight":{"type":"number","minimum":0,"maximum":1},"data":{"type":"object"},"max_edge":{"type":"integer","minimum":32,"maximum":4096}},"required":["action"],"additionalProperties":false}}));
    tools.as_array_mut().unwrap().push(json!({"name":"peerbrush_code","description":"Run bounded Rhai code against a frozen native 8/16-bit project, then commit shared commands atomically. start requires explicit project/document/revision, a named AI actor, active owned task, description and declared scopes. Code cannot bypass locks/reservations or read files/network. Returns run ID immediately; status returns completion and actual PNG with document coordinates. cancel or human takeover discards unfinished work. read_pixel, begin_pixels, write_pixel, commit_pixels and edit are documented in capabilities. One undo step; source changes reject the whole run.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["start","status","cancel"]},"actor":{"type":"string"},"task":{"type":"string"},"run":{"type":"string"},"description":{"type":"string","maxLength":240},"scopes":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","properties":{"target":{"type":["string","null"]},"rect":{"type":["array","null"],"items":{"type":"integer"},"minItems":4,"maxItems":4}},"additionalProperties":false}},"script":{"type":"string","maxLength":65536},"max_edge":{"type":"integer","minimum":32,"maximum":1024}},"required":["action","project_id","document_id"],"additionalProperties":false}}));
    tools.as_array_mut().unwrap().push(json!({"name":"peerbrush_exports","description":"Flattened PNG/WebP/JPEG XL production presets and selected layer/folder batches. list returns instance-local preset revision; save_preset/delete_preset require expected_library_revision and never enter document history. plan/preview/start require observed project_id/document_id/expected_revision. Targets are isolated selected roots with their masks/effects and internal clipping; ancestor folders and document filters are excluded. An empty target list exports the current composite with document filters. Standalone adjustments/clipped layers and protected-source layers require their containing folder/composite. Presets define format (png by default), portable filenames, explicit numbered duplicates, canvas/alpha bounds, transparent padding and named scale variants. PNG and JPEG XL preserve native 8/16-bit channels. JPEG XL defaults to lossless; jxl options select lossless/distance/effort, retaining alpha losslessly. JPEG XL previews and reopening use the validated actual decoded RGB profile; lossless retains the exact ICC, while a differing decoder profile selects explicit SDR sRGB. Lossless WebP has explicit 8-bit output; native16 working sources remain unchanged. All retain alpha and supported color handling without changing the working document. Plan/receipts report output format/bit_depth and native_source_depth. preview returns actual output pixels as PNG with document coordinates. start requires an existing output directory and explicit existing-file policy (error by default, skip, or replace); status reports each written/skipped/failed/not-written output. cancel preserves completed exports and editable sources. Loaded sources, file ownership, concurrent edits and destination conflicts remain guarded.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","save_preset","delete_preset","plan","preview","start","status","cancel"]},"preset":{"type":"string"},"name":{"type":"string"},"actor":{"type":"string"},"expected_library_revision":{"type":"integer","minimum":0},"targets":{"type":"array","maxItems":1024,"items":{"type":"string"}},"directory":{"type":"string"},"existing":{"type":"string","enum":["error","skip","replace"]},"run":{"type":"string"},"output_index":{"type":"integer","minimum":0,"maximum":63},"max_edge":{"type":"integer","minimum":32,"maximum":1024},"settings":{"type":"object","properties":{"jxl":{"type":"object","properties":{"lossless":{"type":"boolean","default":true},"distance":{"type":"number","minimum":0.1,"maximum":15,"default":1},"effort":{"type":"integer","minimum":1,"maximum":9,"default":3}},"additionalProperties":false},"format":{"type":"string","enum":["png","webp","jxl"],"default":"png"},"template":{"type":"string","maxLength":160},"crop":{"type":"string","enum":["canvas","alpha"]},"padding":{"type":"integer","minimum":0,"maximum":4096},"resampling":{"type":"string","enum":["nearest","bilinear"]},"duplicate_names":{"type":"string","enum":["error","number"]},"variants":{"type":"array","minItems":1,"maxItems":12,"items":{"type":"object","properties":{"name":{"type":"string","maxLength":48},"scale":{"type":"number","minimum":0.01,"maximum":8}},"required":["name","scale"],"additionalProperties":false}}},"required":["template","crop","padding","resampling","duplicate_names","variants"],"additionalProperties":false}},"required":["action"],"additionalProperties":false}}));
    tools.as_array_mut().unwrap().push(fonts::tool());
    for tool in tools.as_array_mut().unwrap() {
        // Project/source properties are added consistently below.
        if !matches!(
            tool["name"].as_str(),
            Some("peerbrush_capabilities" | "peerbrush_brushes" | "peerbrush_fonts")
        ) {
            let properties = &mut tool["inputSchema"]["properties"];
            properties["project_id"] = json!({"type":"string"});
            properties["document_id"] = json!({"type":"string"});
            properties["expected_revision"] = json!({"type":"integer"});
            if tool["name"] == "peerbrush_document" {
                tool["inputSchema"]["properties"]["overwrite"] = json!({"type":"boolean","description":"Explicit replacement consent for an existing Save As/export destination; never bypasses a loaded source conflict."});
                tool["inputSchema"]["properties"]["accept_editability_loss"] = json!({"type":"boolean","description":"For PSD/PSB saves with reported losses, explicitly accept document.save_report for this exact document_id and expected_revision. Required for baking, supplementary-only definitions and standard opacity quantization; never bypasses protected originals or file conflicts."});
            }
        }
    }
    tools.as_array_mut().unwrap().push(json!({"name":"peerbrush_image_info","description":"Inspect a supported local image without editing. Reports channel depth and explicit zero-based frame/page/variant choices for image.import and place_image. Float, CMYK and PNG16 animation require explicit supported conversion.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}));
    tools.as_array_mut().unwrap().push(json!({"name":"peerbrush_tool_settings","description":"Inspect/update independent instance-local Brush, Eraser, Smudge, Clone and Heal preferences, plus Liquify radius/strength/mode. Settings and preset choice stay outside document history and PSD sources; set never selects the visible tool or paints. Supply expected_tool_revision from get when changing preferences as an agent. Settings partially override the current snapshot; choosing a preset first copies its settings. Use returned profile.settings as explicit parameters in shared edit commands. preview returns actual brush-preview PNG pixels with explicit coordinates.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["get","set","preview"]},"tool":{"type":"string","enum":["brush","eraser","smudge","clone","heal","liquify"]},"actor":{"type":"string"},"expected_tool_revision":{"type":"integer","minimum":0},"preset":{"type":["string","null"]},"settings":{"type":"object"}},"required":["action"],"additionalProperties":false}}));
    tools.as_array_mut().unwrap().push(json!({"name":"peerbrush_projects","description":"List stable project IDs, create/open independent projects, select/reorder tabs or request native Save/Don't Save/Cancel close review (human only), close with exact source guards, or copy/move editable layer trees between projects. Background AI writes must supply project_id, document_id and expected_revision; never depend on the visible tab. Transfers need both source identities/revisions and preserve native precision, masks and effects. preview_transfer returns actual destination pixels without history. move creates one undo step in each document; failures change neither.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["list","new","open","select","reorder","request_close","close","copy","move","preview_transfer"]},"actor":{"type":"string"},"project_id":{"type":"string"},"document_id":{"type":"string"},"expected_revision":{"type":"integer"},"path":{"type":"string"},"frame":{"type":"integer","minimum":0},"page":{"type":"integer","minimum":0},"variant":{"type":"integer","minimum":0},"width":{"type":"integer"},"height":{"type":"integer"},"bit_depth":{"type":"integer","enum":[8,16]},"expected_order":{"type":"array","items":{"type":"string"}},"order":{"type":"array","items":{"type":"string"}},"activate":{"type":"boolean"},"discard":{"type":"boolean"},"source_project_id":{"type":"string"},"source_document_id":{"type":"string"},"source_revision":{"type":"integer"},"source_task":{"type":"string"},"task":{"type":"string"},"layers":{"type":"array","items":{"type":"string"}},"target":{"type":"string"},"move":{"type":"boolean"},"max_edge":{"type":"integer"}},"required":["action"],"additionalProperties":false}}));
    tools
}
pub fn capabilities() -> Value {
    static DATA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let mut result = DATA
        .get_or_init(|| {
            let mut value: Value = serde_json::from_str(include_str!("capabilities.json"))
                .expect("bundled capabilities");
            value["version"] = json!(env!("CARGO_PKG_VERSION"));
            value
        })
        .clone();
    result["rendering"] = json!({
        "canvas": "wgpu",
        "effects": crate::gpu::status(),
        "brush": "incremental tiled CPU coverage",
        "compositing": "compiled CPU layer tree with regional gesture updates",
        "tile_compositor": {
            "mode": "downsampled native 8/16-bit layers; all blend modes, masks, clipping, prepared adjustments, isolated and pass-through folders",
            "fallback": "budget limits, busy/failed GPU, isolated targets, whole-image filter preparation and full-resolution saves use CPU; rounding-boundary samples are repaired by the shared engine",
            "status": crate::gpu::composite::status()
        },
        "liquify": "CPU displacement grid"
    });
    result["segmentation"]["configured"] = json!(crate::segmentation::configured());
    result["photoshop_files"] = json!({"formats":["PSD v1","PSB v2"],"native_depths":[8,16],"mode":"RGB","file_limit_bytes":256*1024*1024,"max_edge":8192,"max_pixels":32*1024*1024,"protection":"Unsupported structures remain read-only; explicit compatible copies retain original precision. Both formats write current standard raster/masks and merged composites."});
    result["projects"] = json!({"tool":"peerbrush_projects","max_open":crate::workspace::MAX_PROJECTS,"targeting":"Use project_id, document_id and expected_revision for every background AI mutation. Switching or reordering tabs never changes a project's engine. Human-only reorder requires the complete expected_order and new order.","transfer":"Editable trees, masks, effects and native16 sources; copy changes destination, move is atomic with one undo step per project."});
    result["image_import"] = json!({"extensions":crate::image_import::EXTENSIONS,"inspect_tool":"peerbrush_image_info","choices":"Explicit zero-based frame/page/variant when image_info reports choice; never silently flatten animations or multipage files.","precision":"Raster imports retain native 8/16-bit RGB and gray. Supported embedded RGB ICC is converted to sRGB at original depth. Float, CMYK and PNG16 animation are rejected. JPEG XL accepts static SDR RGB with optional straight alpha at native8/16; animation, gray, associated alpha and extra channels reject. Static supported SVG/SVGZ retains editable markup; its colors and coverage project through RGBA8 and promote in native16 projects without quantizing existing raster channels. image.place and external image-file paste rasterize SVG."});
    result["production_exports"] = json!({"tool":"peerbrush_exports","formats":["png","webp","jxl"],"default_format":"png","precision":"PNG and JPEG XL retain native8/16. Lossless WebP produces8-bit RGBA from the rendered output only; native16 working channels are never replaced.","color":"Alpha remains lossless. PNG/WebP retain supported ICC; lossless JPEG XL retains exact ICC. JPEG XL previews/reopening use the actual decoded RGB profile, selecting explicit SDR sRGB when the default differs. Untagged RGB is assigned sRGB for JPEG XL.","preview":"Actual encoded output returned as display PNG with document coordinates; output format/bit_depth and native_source_depth are distinct.","publication":"Existing guarded atomic writes, source revision/ownership checks, cancellation and exact previous-file preservation apply to every format."});
    result["effect_catalog"] = crate::effects::catalog::discovery();
    result["custom_contours"] = json!({"effects":["contour","bevel_emboss","inner_shadow","inner_glow","satin","outer_glow","drop_shadow"],"settings":{"contour":"custom","contour_points":"2–16 objects: position and value, each finite 0–1; positions strictly increase with fixed 0/1 endpoints"},"rendering":"Piecewise linear native contour mapping; original sources remain intact. Shared previews, history, reservations and document coordinates apply.","limits":"Unknown fields, missing points, duplicate positions and points on preset contours reject. Native contours do not establish Photoshop contour/style equivalence.","source_format":21});
    result["bevel_channels"] = json!({"effect":"bevel_emboss","settings":{"highlight_opacity":"0–1, default 1; multiplies common opacity","shadow_opacity":"0–1, default 1; multiplies common opacity","highlight_blend":"shared blend key or inherit (default)","shadow_blend":"shared blend key or inherit (default)"},"limits":"Native alpha-field relief; independent channels do not establish Photoshop bevel technique or backdrop equivalence. Shared previews, reservations, history and original native sources apply.","source_format":22});
    result["gradient_placement"] = json!({"effect":"gradient_overlay","settings":{"offset_x":"optional −1–1 canvas widths, default 0","offset_y":"optional −1–1 canvas heights, default 0"},"coordinates":"The gradient center moves in document coordinates; positive X moves right and positive Y moves down. Shape, stops, angle, scale and reverse remain editable. Screen navigation does not change placement.","limits":"Shared engine previews, reservations, history and original native sources apply. Explicit offsets, including disabled definitions, require source format 23; older readers protect them.","source_format":23});
    result["gradient_stops"] = json!({"effect":"gradient_overlay","settings":{"color_stops":"optional 2–16 objects: position 0–1, color [R,G,B] 0–1, optional midpoint 0.01–0.99 (default 0.5)","alpha_stops":"optional independent 2–16 objects: position 0–1, opacity 0–1, optional midpoint 0.01–0.99"},"limits":"Positions strictly increase with endpoints 0 and 1. Empty arrays use existing two-color/full-opacity defaults. Unknown fields reject.","rendering":"Native RGB interpolation, with a linear midpoint on each segment and independent opacity coverage. Existing gradient shape, angle, scale and reverse use document coordinates. Original layer alpha is preserved.","source_format":20});
    result["bitmap_patterns"] = json!({"effect":"pattern_overlay","effects":["pattern_overlay","texture"],"pattern":"bitmap","source":"bitmap_png contains base64 static PNG bytes, never a local path. Supported embedded RGB ICC converts to sRGB at original depth; a PNG16 source promotes the document, including original raster/mask channels.","settings":{"bitmap_scale":"0.01–8, default 1","bitmap_angle":"clockwise degrees −180–180, default 0","phase_x":"document pixels −32768–32768, default 0","phase_y":"document pixels −32768–32768, default 0","sampling":"bilinear (default, premultiplied) or nearest"},"coordinates":"Tiles use document pixel centers and the canvas origin, independent of layer position and screen pan/zoom. Proportional image resize scales the tile and phase; crop/canvas origin changes preserve alignment.","limits":"4 MiB encoded PNG, 512×512 pixels, 32 MiB aggregate source budget and bounded derived cache. Procedural scale/invert overrides reject for bitmap sources. Pattern Overlay also rejects procedural colors; Texture retains its shadow/highlight colors and light angle/depth. Bitmap Texture derives neutral-alpha luminance height at native depth, separate from tile angle. Faithful Photoshop style/color/placement import remains bounded or protected.","source_format":18,"texture_source_format":19});
    result["paths"] = json!({"tool":"Pen · P","source_format":17,"commands":["path.create","path.update","path.node.add","path.node.move","path.handle.move","path.node.split","path.node.convert","path.node.delete","path.to_selection","path.to_mask"],"coordinates":"Command points/handles and observed path_document_nodes use document pixels. Supplementary source nodes use the source frame and matrix.","precision":"Native 8/16-bit rendering and RGBA16 source colors. 8-bit screen feedback only.","limits":"128 anchors, 4096 flattened vertices, 0.125 source-pixel subdivision tolerance, four-million-pixel source frame and bounded rendering work. Single contour, even-odd fill, round segment ends. Standard PSD/PSB rasters/masks/current composite plus PeerBrush editable paths; Photoshop native Pen import/export is unsupported."});
    result["stroke_constraints"] = json!({"commands":["paint","smudge","clone","heal"],"stroke_axis":["horizontal","vertical","axis"],"stroke_axis_from":"Optional zero-based point index; default 0. Constrains this tail in document coordinates and retains preceding points and pressure samples. UI Shift-click joins the last successful point in the same document, revision, tool, layer and channel; Shift-drag locks the first dominant axis. Actual previews commit once on release."});
    result["commands"].as_array_mut().unwrap().extend([
        json!({"op":"path.create","nodes":[{"point":[40,40]},{"point":[180,120],"in_handle":[80,40]}],"stroke_width":2}),
        json!({"op":"path.handle.move","layer":"PATH_ID","index":1,"handle":"out","point":[210,150]}),
        json!({"op":"path.to_selection","path":"PATH_ID","feather":1}),
    ]);
    result["mask_link"] = json!({"command":{"op":"mask.link","layer":"LAYER_ID","linked":true},"default":true,"transforms":"Linked masks and Color move/rotate/scale together, including selected areas. Unlinked targets transform independently. Whole mask transforms include all paint steps; an explicit step selects one unlinked paint step. Independent Color and selected linked transforms require fill/paint mask steps; unsupported mask effects are refused without rasterization.","precision":"Native 8/16-bit rasters and retained originals; shared previews, reservations and undo.","psd":"Source format 29 retains link state. Standard mask pixels and composite remain current; external Photoshop link editing is not written or verified and appears in the save report."});
    result["commands"]
        .as_array_mut()
        .unwrap()
        .push(json!({"op":"mask.link","layer":"LAYER_ID","linked":false}));
    result["effect_bake"] = json!({"commands":["effect.bake","mask.step.bake"],"scope":"Selected effect plus preceding stack; following effects remain editable. Folder Color flattens all children; editable Color sources rasterize. Mask keeps Color/hierarchy. Whole-target reservations, native 8/16-bit fidelity check and one undo. Inexact prefixes, backdrop-dependent adjustments and expanding styles that shift an attached mask frame are refused."});
    result["commands"].as_array_mut().unwrap().extend([
        json!({"op":"effect.bake","layer":"LAYER_ID","effect":"EFFECT_ID"}),
        json!({"op":"mask.step.bake","layer":"LAYER_ID","step":"STEP_ID"}),
    ]);
    result["commands"]
        .as_array_mut()
        .unwrap()
        .push(json!({"op":"filter.add","preset":"posterize","settings":{"levels":5},"weight":0.6}));
    result["filter_library"] = json!({"tool":"peerbrush_filters","actions":["list","thumbnail","preview","save","rename","delete","import","export"],"curated":15,"custom_limit":crate::filter_library::LIMIT,"file_limit":crate::filter_library::FILE_LIMIT,"scope":"Whole document; filter.add/update/delete/reorder run after the composite, independent of selected layer. Top runs last. Locks, reservations and revision guards apply.","precision":"Native 8/16-bit sources, effects and strength; previews project only for display.","preview":"Explicit current project/document/revision. Actual PNG/document_rect plus frozen commands; no shared edit or history. Thumbnails use filter_thumbnail reference coordinates.","storage":"Configured preferences directory filters.json outside history; strict versioned import/export. Curated immutable, invalid or externally changed files preserved.","psd":"Format 15 editable sources plus current filtered standard raster/mask/composite; older readers remain protected.","cache_bytes":crate::filters::CACHE_BUDGET,"limits":"32 document filters; native derived output/scratch budgets apply. Liquify is a layer effect."});
    result
}

fn observation(shared: &Shared, p: &Value) -> Result<Value, String> {
    let (state, doc) = {
        let mut e = shared.lock().unwrap();
        crate::workspace::validate_target(&e, p)?;
        (e.state(), e.doc.clone())
    };
    let revision = doc.revision;
    let image = p.get("image").and_then(Value::as_bool).unwrap_or(true)
        && p.get("since_revision").and_then(Value::as_u64) != Some(revision);
    let mut result = json!({"state":state,"revision":revision,"images":[]});
    if image {
        let (w, h, rgba, rect) = doc.preview(
            p.get("rect").and_then(engine::rect),
            p.get("max_edge")
                .and_then(Value::as_u64)
                .unwrap_or(1024)
                .clamp(32, 4096) as u32,
            p.get("layer").and_then(Value::as_str),
            p.get("mask").and_then(Value::as_bool).unwrap_or(false),
        )?;
        let mut rgba = rgba;
        if let Some(mode) = p.get("selection_view").and_then(Value::as_str) {
            if !["cutout", "mask", "overlay"].contains(&mode) {
                return Err("Unknown selection view".into());
            }
            crate::selection::display::apply_region(&doc, w, h, &mut rgba, rect, mode);
        }
        let bytes = raster::png(w, h, &rgba)?;
        result["images"] = json!([{"mime_type":"image/png","data":STANDARD.encode(bytes),"width":w,"height":h,"document_rect":rect,"revision":revision}]);
    }
    Ok(result)
}
pub fn dispatch(shared: &Shared, method: &str, p: &Value) -> Result<Value, String> {
    let persistent = match method {
        "brushes" => matches!(p["action"].as_str(), Some("save" | "delete")),
        "tool_settings" => p["action"] == "set",
        "filters" => matches!(
            p["action"].as_str(),
            Some("save" | "rename" | "delete" | "import")
        ),
        _ => false,
    };
    let _operation = persistent
        .then(|| FileOperation::begin_workspace(&crate::workspace::attach(shared)))
        .transpose()?;
    if method == "fonts" {
        return fonts::request(p);
    }
    if method == "exports" {
        return crate::production_export::request(shared, p);
    }
    if method == "recovery" {
        let workspace = crate::workspace::attach(shared);
        let (dir, error) = {
            let state = workspace.lock().unwrap();
            (
                state
                    .recovery_dir
                    .clone()
                    .ok_or("Recovery storage is not initialized")?,
                state.recovery_error.clone(),
            )
        };
        return match p["action"].as_str().unwrap_or("list") {
            "list" => Ok(
                json!({"versions":crate::recovery::catalog(&dir),"autosave_error":error,"versions_per_project":crate::recovery::VERSIONS_PER_PROJECT,"max_bytes":crate::recovery::MAX_BYTES}),
            ),
            "restore" => {
                let snapshot = p["snapshot"]
                    .as_str()
                    .ok_or("Choose an explicit recovery snapshot")?;
                let restored = crate::recovery::restore(
                    &workspace,
                    &dir,
                    snapshot,
                    &crate::loading::Control::default(),
                    None,
                )?;
                let mut result = observation(&restored, &json!({}))?;
                result["project_id"] = json!(restored.lock().unwrap().project_id);
                result["recovery_retained"] = json!(true);
                Ok(result)
            }
            "discard" if p["actor"] == "human" => {
                crate::recovery::discard_in(
                    &workspace,
                    &dir,
                    p["snapshot"]
                        .as_str()
                        .ok_or("Choose an explicit recovery snapshot")?,
                )?;
                Ok(json!({"discarded":p["snapshot"]}))
            }
            "discard" => Err("Only human input may discard recovery versions".into()),
            _ => Err("Unknown recovery action".into()),
        };
    }
    if method == "projects" {
        return projects(shared, p);
    }
    if method == "filters" && p["action"] != "preview" {
        return filter_library(shared, p);
    }
    if matches!(
        method,
        "capabilities"
            | "brushes"
            | "tool_settings"
            | "image_info"
            | "focus"
            | "native_open"
            | "client_state"
            | "capture_ui"
    ) {
        return dispatch_project(shared, method, p);
    }
    let target = crate::workspace::route(shared, method, p)?;
    let mut result = dispatch_project(&target, method, p)?;
    if result.is_object() {
        result["project_id"] = json!(target.lock().unwrap().project_id);
    }
    Ok(result)
}
fn projects(root: &Shared, p: &Value) -> Result<Value, String> {
    use crate::workspace as w;
    let actor = p["actor"].as_str().unwrap_or("agent");
    let text = |key: &str| p[key].as_str().ok_or_else(|| format!("Missing {key}"));
    let number = |key: &str| p[key].as_u64().ok_or_else(|| format!("Missing {key}"));
    match text("action")? {
        "list" => Ok(w::state(root)),
        "new" | "open" => {
            let active = w::active_id(root);
            let activate = actor == "human" && p["activate"] != false;
            let engine = if p["action"] == "open" {
                let shared = Arc::new(Mutex::new(Engine::new()));
                let path = Path::new(text("path")?);
                if let Err(error) =
                    open_options(&shared, path, &crate::loading::Control::default(), p)
                {
                    w::duplicate_warning(&w::attach(root), path, &error, activate);
                    return Err(error);
                }
                let result = shared.lock().unwrap().clone();
                result
            } else {
                let mut engine = Engine::new();
                let size = |key: &str, default: u32| {
                    p.get(key).map_or(Ok(default), |v| {
                        v.as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or("Invalid canvas size")
                    })
                };
                let depth = p.get("bit_depth").map_or(Ok(8), |v| {
                    v.as_u64()
                        .filter(|n| [8, 16].contains(n))
                        .map(|n| n as u16)
                        .ok_or("Color depth must be 8 or 16 bits")
                })?;
                engine.doc =
                    engine::Document::new_depth(size("width", 1024)?, size("height", 768)?, depth)?;
                engine
            };
            let shared = w::register(root, engine, activate.then_some(active.as_str()))?;
            let result = shared.lock().unwrap().state();
            Ok(result)
        }
        "select" => {
            if actor != "human" {
                return Err("Only human input can select the visible project".into());
            }
            w::select(root, text("project_id")?)?;
            Ok(w::state(root))
        }
        "reorder" => {
            if actor != "human" {
                return Err("Only human input can reorder visible project tabs".into());
            }
            let ids = |key: &str| {
                p[key]
                    .as_array()
                    .ok_or_else(|| format!("Missing {key}"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(String::from)
                            .ok_or_else(|| format!("Invalid {key}"))
                    })
                    .collect::<Result<Vec<_>, String>>()
            };
            w::reorder_in(&w::attach(root), &ids("expected_order")?, &ids("order")?)?;
            Ok(w::state(root))
        }
        "request_close" => {
            w::lifecycle::request_close(
                root,
                text("project_id")?,
                text("document_id")?,
                number("expected_revision")?,
                actor,
            )?;
            Ok(json!({"requested":true,"closed":false,"native_review":true}))
        }
        "close" => {
            w::close(
                root,
                text("project_id")?,
                text("document_id")?,
                number("expected_revision")?,
                p["discard"] == true,
                actor,
            )?;
            Ok(w::state(root))
        }
        action @ ("copy" | "move" | "preview_transfer") => {
            let layers = p["layers"]
                .as_array()
                .ok_or("Choose layer roots")?
                .iter()
                .map(|v| v.as_str().map(String::from).ok_or("Invalid layer ID"))
                .collect::<Result<Vec<_>, _>>()?;
            let request = w::Transfer {
                source: text("source_project_id")?,
                destination: text("project_id")?,
                source_document: text("source_document_id")?,
                destination_document: text("document_id")?,
                source_revision: number("source_revision")?,
                destination_revision: number("expected_revision")?,
                layers: &layers,
                target: text("target")?,
                move_layers: action == "move"
                    || (action == "preview_transfer" && p["move"] == true),
                actor,
                source_task: p["source_task"].as_str(),
                destination_task: p["task"].as_str(),
            };
            if action != "preview_transfer" {
                return w::transfer(root, &request);
            }
            let doc = w::transfer_preview(root, &request)?;
            let (width, height, rgba, rect) = doc.preview(
                None,
                p["max_edge"].as_u64().unwrap_or(1024).clamp(32, 4096) as u32,
                None,
                false,
            )?;
            Ok(
                json!({"project_id":request.destination,"document_id":request.destination_document,"revision":request.destination_revision,"preview_only":true,"images":[{"mime_type":"image/png","data":STANDARD.encode(raster::png(width,height,&rgba)?),"width":width,"height":height,"document_rect":rect}]}),
            )
        }
        _ => Err("Unknown project action".into()),
    }
}
fn filter_library(shared: &Shared, p: &Value) -> Result<Value, String> {
    let library = shared.lock().unwrap().filter_library.clone();
    let action = p["action"]
        .as_str()
        .ok_or("Missing filter library action")?;
    let id = || p["id"].as_str().ok_or("Missing filter preset ID");
    match action {
        "list" => {
            let l = library.lock().unwrap();
            Ok(
                json!({"presets":l.presets(),"error":l.error,"kinds":crate::effects::catalog::ENTRIES.iter().filter(|e|crate::filters::supports(e.kind)).map(|e|json!({"kind":e.kind,"name":e.name,"category":e.category})).collect::<Vec<_>>()}),
            )
        }
        "save" => {
            let preset = library.lock().unwrap().save(
                p.get("id")
                    .map(|v| v.as_str().ok_or("Filter ID must be text"))
                    .transpose()?,
                p["name"].as_str().ok_or("Missing filter name")?,
                p["category"].as_str().ok_or("Missing filter category")?,
                p["kind"].as_str().ok_or("Missing filter kind")?,
                p["settings"].clone(),
                crate::effects::command_weight(p)?,
            )?;
            Ok(json!({"preset":preset}))
        }
        "rename" => Ok(
            json!({"preset":library.lock().unwrap().rename(id()?,p["name"].as_str().ok_or("Missing filter name")?)?}),
        ),
        "delete" => {
            library.lock().unwrap().delete(id()?)?;
            Ok(json!({"deleted":true}))
        }
        "import" => Ok(json!({"preset":library.lock().unwrap().import(&p["data"])?})),
        "export" => Ok(json!({"data":library.lock().unwrap().export(id()?)?})),
        "thumbnail" => {
            let preset = library.lock().unwrap().get(id()?)?;
            let rgba = crate::filter_library::thumbnail(&preset)?;
            Ok(
                json!({"preset":preset,"images":[{"mime_type":"image/png","data":STANDARD.encode(raster::png(96,64,&rgba)?),"width":96,"height":64,"coordinate_space":"filter_thumbnail","rect":[0,0,96,64]}]}),
            )
        }
        "preview" => {
            let doc = {
                let mut e = shared.lock().unwrap();
                crate::workspace::validate_target(&e, p)?;
                if p["document_id"].as_str() != Some(e.doc.id.as_str())
                    || p["expected_revision"].as_u64() != Some(e.doc.revision)
                {
                    return Err(
                        "Filter preview needs the current document_id and expected_revision".into(),
                    );
                }
                e.expire();
                e.check(
                    p["actor"].as_str().unwrap_or("agent"),
                    &[Scope {
                        target: None,
                        rect: None,
                    }],
                )?;
                e.doc.clone()
            };
            let mut command = json!({"op":"filter.add","preset":id()?,"settings":p.get("settings").cloned().unwrap_or(json!({}))});
            if let Some(weight) = p.get("weight") {
                command["weight"] = weight.clone();
            }
            let mut commands = library.lock().unwrap().resolve_commands(&[command])?;
            for command in &mut commands {
                command["document_id"] = json!(doc.id);
                command["source_revision"] = json!(doc.revision);
            }
            let preview = crate::engine::Engine::preview_edits(doc.clone(), &commands)?;
            let edge = p.get("max_edge").map_or(Ok(1024), |v| {
                v.as_u64()
                    .filter(|v| (32..=4096).contains(v))
                    .map(|v| v as u32)
                    .ok_or("Preview max_edge must be 32–4096")
            })?;
            let (w, h, rgba, rect) = preview.preview(None, edge, None, false)?;
            let e = shared.lock().unwrap();
            if e.closed || e.doc.id != doc.id || e.doc.revision != doc.revision {
                return Err("Project changed during filter preview; observe again".into());
            }
            Ok(
                json!({"project_id":e.project_id,"document_id":doc.id,"revision":doc.revision,"preview_only":true,"commands":commands,"images":[{"mime_type":"image/png","data":STANDARD.encode(raster::png(w,h,&rgba)?),"width":w,"height":h,"document_rect":rect,"revision":doc.revision}]}),
            )
        }
        _ => Err("Unknown filter library action".into()),
    }
}
fn dispatch_project(shared: &Shared, method: &str, p: &Value) -> Result<Value, String> {
    let actor = p.get("actor").and_then(Value::as_str).unwrap_or("agent");
    match method {
        "capabilities" => Ok(capabilities()),
        "image_info" => serde_json::to_value(crate::image_import::inspect(Path::new(
            p["path"].as_str().ok_or("Missing image path")?,
        ))?)
        .map_err(|e| e.to_string()),
        "focus" => {
            shared.lock().unwrap().focus_requested = true;
            Ok(json!({"focused":true}))
        }
        "native_open" => {
            let paths = p["paths"].as_array().ok_or("Missing activation paths")?;
            if paths.len() > crate::activation::MAX_PENDING_FILES {
                return Err("Too many files in this activation".into());
            }
            let paths = paths
                .iter()
                .map(|p| {
                    p.as_str()
                        .map(PathBuf::from)
                        .ok_or("Invalid activation path")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let queued = crate::activation::enqueue(shared, paths)?;
            Ok(json!({"queued":queued}))
        }
        "observe" => observation(shared, p),
        "code" => crate::code::dispatch(shared, actor, p),
        "filters" => filter_library(shared, p),
        "tool_settings" => {
            let (store, library) = {
                let e = shared.lock().unwrap();
                (e.tool_settings.clone(), e.brush_library.clone())
            };
            let action = p["action"]
                .as_str()
                .ok_or("Missing tool preference action")?;
            if action == "get" && p.get("tool").is_none() {
                return Ok(store.lock().unwrap().state());
            }
            let tool = p["tool"].as_str().ok_or("Choose a brush-based tool")?;
            let snapshot =
                match action {
                    "get" | "preview" => store.lock().unwrap().get(tool)?,
                    "set" => {
                        let mut store = store.lock().unwrap();
                        let mut profile = store.get(tool)?.profile;
                        if tool == "liquify" && p.get("preset").is_some_and(|v| !v.is_null()) {
                            return Err("Liquify does not use brush presets".into());
                        }
                        if let Some(preset) = p.get("preset") {
                            if preset.is_null() {
                                profile.preset = None;
                            } else {
                                let preset = library
                                    .lock()
                                    .unwrap()
                                    .get(preset.as_str().ok_or("Preset must be an ID or null")?)?;
                                profile.settings = preset.settings;
                                profile.preset = Some(preset.id);
                            }
                        }
                        if let Some(settings) = p.get("settings") {
                            let mut value = serde_json::to_value(profile.settings)
                                .map_err(|e| e.to_string())?;
                            for (key, setting) in settings
                                .as_object()
                                .ok_or("Tool settings must be an object")?
                            {
                                if tool == "liquify" {
                                    match key.as_str() {
                                        "radius" => value["radius"] = setting.clone(),
                                        "strength" => value["opacity"] = setting.clone(),
                                        "mode" => {
                                            profile.liquify_mode = Some(
                                                setting
                                                    .as_str()
                                                    .ok_or("Liquify mode must be text")?
                                                    .into(),
                                            )
                                        }
                                        _ => return Err(
                                            "Liquify preferences support radius, strength and mode"
                                                .into(),
                                        ),
                                    }
                                } else {
                                    value[key] = setting.clone();
                                }
                            }
                            profile.settings = serde_json::from_value(value)
                                .map_err(|e| format!("Invalid tool settings: {e}"))?;
                        }
                        store.set(
                            tool,
                            profile,
                            actor,
                            p.get("expected_tool_revision")
                                .map(|v| {
                                    v.as_u64()
                                        .ok_or("Tool revision must be an unsigned integer")
                                })
                                .transpose()?,
                        )?
                    }
                    _ => return Err("Choose get, set or preview tool preferences".into()),
                };
            let mut result = snapshot.state(tool);
            if action == "preview" {
                if tool == "liquify" {
                    return Err(
                        "Preview Liquify with liquify.stroke on an explicit document source".into(),
                    );
                }
                let raster = crate::brush_library::preview(snapshot.profile.settings, 240, 48)?;
                let png = raster::png(240, 48, &raster.rgba())?;
                result["images"] = json!([{"mime_type":"image/png","data":STANDARD.encode(png),"width":240,"height":48,"coordinate_space":"brush_preview","rect":[0,0,240,48]}]);
            }
            Ok(result)
        }
        "brushes" => {
            let library = shared.lock().unwrap().brush_library.clone();
            let mut library = library.lock().unwrap();
            match p["action"].as_str().ok_or("Missing brush action")? {
                "list" => Ok(
                    json!({"presets":library.presets(),"categories":crate::brush_library::CATEGORIES,"error":library.error}),
                ),
                "save" => {
                    let settings: crate::brush::Settings =
                        serde_json::from_value(p["settings"].clone())
                            .map_err(|e| format!("Invalid brush settings: {e}"))?;
                    let id = p
                        .get("id")
                        .map(|v| v.as_str().ok_or("Brush ID must be text"))
                        .transpose()?;
                    Ok(
                        json!({"preset":library.save(id,p["name"].as_str().ok_or("Missing brush name")?,p["category"].as_str().ok_or("Missing brush category")?,settings)?}),
                    )
                }
                "delete" => {
                    library.delete(p["id"].as_str().ok_or("Missing brush ID")?)?;
                    Ok(json!({"deleted":true}))
                }
                "preview" => {
                    let preset = library.get(p["id"].as_str().ok_or("Missing brush ID")?)?;
                    let size = |key: &str, default: u32| -> Result<u32, String> {
                        p.get(key).map_or(Ok(default), |v| {
                            v.as_u64()
                                .and_then(|n| u32::try_from(n).ok())
                                .ok_or_else(|| format!("Invalid preview {key}"))
                        })
                    };
                    let width = size("width", 240)?;
                    let height = size("height", 48)?;
                    drop(library);
                    let raster = crate::brush_library::preview(preset.settings, width, height)?;
                    let bytes = raster::png(width, height, &raster.rgba())?;
                    Ok(
                        json!({"preset":preset,"images":[{"mime_type":"image/png","data":STANDARD.encode(bytes),"width":width,"height":height,"coordinate_space":"brush_preview","rect":[0,0,width,height]}]}),
                    )
                }
                _ => Err("Unknown brush action".into()),
            }
        }
        "proposal" => {
            let action = p["action"].as_str().ok_or("Missing proposal action")?;
            if action == "list" {
                let mut e = shared.lock().unwrap();
                return Ok(json!({"proposals":e.proposals_state(),"tasks":e.task_recovery()}));
            }
            let id = if action == "create" {
                crate::collaboration::propose(shared, actor, p)?["id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            } else {
                p["proposal"]
                    .as_str()
                    .ok_or("Missing proposal ID")?
                    .to_owned()
            };
            if action == "reject" {
                let mut e = shared.lock().unwrap();
                crate::workspace::validate_target(&e, p)?;
                return e.reject_proposal(actor, &id);
            }
            if action == "accept" {
                let result = shared.lock().unwrap().accept_proposal(
                    actor,
                    &id,
                    p["document_id"]
                        .as_str()
                        .ok_or("Acceptance needs the reviewed document_id")?,
                    p["expected_revision"]
                        .as_u64()
                        .ok_or("Acceptance needs the reviewed expected_revision")?,
                )?;
                let mut result = result;
                if p["feedback"] != "request" {
                    result["images"] = observation(shared, p)?["images"].clone();
                }
                return Ok(result);
            }
            if !["create", "preview"].contains(&action) {
                return Err("Unknown proposal action".into());
            }
            let (doc, proposal) = {
                let mut e = shared.lock().unwrap();
                let doc = e.proposal_document(&id)?;
                let proposal = e
                    .proposals_state()
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|p| p["id"] == id)
                    .unwrap()
                    .clone();
                (doc, proposal)
            };
            let mut result = json!({"proposal":proposal,"images":[]});
            if action == "preview" || p["feedback"] != "request" {
                let (w, h, rgba, rect) = doc.preview(
                    p["rect"].as_array().and_then(|_| engine::rect(&p["rect"])),
                    p["max_edge"].as_u64().unwrap_or(768).clamp(32, 4096) as u32,
                    None,
                    false,
                )?;
                result["images"] = json!([{"mime_type":"image/png","data":STANDARD.encode(raster::png(w,h,&rgba)?),"width":w,"height":h,"document_rect":rect,"revision":proposal["source_revision"],"proposal":id,"preview_only":true}]);
            }
            // Rendering runs without the shared lock. Report interruption rather than a
            // ready proposal if human input changed its source while pixels were sampled.
            shared.lock().unwrap().proposal_document(&id)?;
            Ok(result)
        }
        "client_state" => {
            let client = p
                .get("client")
                .and_then(Value::as_str)
                .ok_or("Missing client ID")?;
            let mut e = shared.lock().unwrap();
            if p.get("connected") == Some(&Value::Bool(false)) {
                e.mcp_clients.remove(client);
            } else {
                e.mcp_clients.insert(client.into(), engine::now() + 90);
            }
            Ok(json!({"connected":!e.mcp_clients.is_empty()}))
        }
        "capture_ui" => {
            let path = p
                .get("path")
                .and_then(Value::as_str)
                .ok_or("Missing PNG path")?;
            let panel = p.get("panel").and_then(Value::as_str);
            if panel.is_some_and(|name| {
                ![
                    "workspace",
                    "brush",
                    "color",
                    "filters",
                    "exports",
                    "selection",
                    "pen",
                    "solo_color",
                    "solo_mask",
                    "document_info",
                    "document_sources",
                    "document_patterns",
                    "save_report",
                    "saved_versions",
                    "calibration",
                    "view",
                    "shortcuts",
                    "updates",
                    "application_info",
                ]
                .contains(&name)
            }) {
                return Err("Unknown capture panel".into());
            }
            let mut engine = shared.lock().unwrap();
            engine.capture_ui = Some(PathBuf::from(path));
            engine.capture_panel = panel.map(String::from);
            Ok(json!({"queued":path}))
        }
        "place_image" => {
            let area = p
                .get("rect")
                .and_then(engine::rect)
                .ok_or("Choose an explicit image destination rect")?;
            let mut command = p.clone();
            let object = command
                .as_object_mut()
                .ok_or("Image placement arguments must be an object")?;
            for field in [
                "actor",
                "expected_revision",
                "task",
                "label",
                "feedback",
                "max_edge",
            ] {
                object.remove(field);
            }
            command["op"] = json!("image.place");
            let mut edit = p.clone();
            edit["commands"] = json!([command]);
            edit["feedback_rect"] = json!(area);
            if p.get("label").is_none() {
                edit["label"] = json!("Place image");
            }
            let mut result = dispatch_project(shared, "edit", &edit)?;
            let layer = if p["new_layer"] == false {
                p.get("layer").cloned().unwrap_or(Value::Null)
            } else {
                result["created"][0].clone()
            };
            result["placement"] =
                json!({"layer":layer,"rect":area,"new_layer":p["new_layer"] != false});
            Ok(result)
        }
        "segment" => {
            let mut result = crate::segmentation::run(shared, actor, p)?;
            if p["feedback"] != "request" {
                let view = observation(
                    shared,
                    &json!({"selection_view":"mask","rect":p.get("rect"),"max_edge":p.get("max_edge").and_then(Value::as_u64).unwrap_or(768)}),
                )?;
                result["images"] = view["images"].clone();
            }
            Ok(result)
        }
        "edit" => {
            let commands = p
                .get("commands")
                .and_then(Value::as_array)
                .ok_or("commands must be an array")?;
            let result = if commands.len() == 1 && commands[0]["op"] == "layer.merge" {
                let ids = crate::merge::requested(&commands[0])?;
                crate::merge::edit_guarded(
                    shared,
                    actor,
                    &ids,
                    p.get("expected_revision").and_then(Value::as_u64),
                    p.get("task").and_then(Value::as_str),
                    commands[0].get("name").and_then(Value::as_str),
                    p["document_id"].as_str(),
                )?
            } else {
                let mut e = shared.lock().unwrap();
                crate::workspace::validate_target(&e, p)?;
                e.edit(
                    actor,
                    commands,
                    p.get("expected_revision").and_then(Value::as_u64),
                    p.get("task").and_then(Value::as_str),
                    p.get("label").and_then(Value::as_str).unwrap_or("AI edit"),
                )?
            };
            let feedback = p
                .get("feedback")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| shared.lock().unwrap().feedback.get(actor).cloned())
                .unwrap_or("batch".into());
            let mut out = result;
            if feedback != "request" {
                let view = observation(
                    shared,
                    &json!({"max_edge":p.get("max_edge").and_then(Value::as_u64).unwrap_or(768),"rect":p.get("feedback_rect")}),
                )?;
                out["images"] = view["images"].clone();
            }
            Ok(out)
        }
        "task" => {
            let action = p.get("action").and_then(Value::as_str).unwrap_or("status");
            let mut e = shared.lock().unwrap();
            crate::workspace::validate_target(&e, p)?;
            if crate::workspace::mutating(method, p)
                && p["expected_revision"]
                    .as_u64()
                    .is_some_and(|r| r != e.doc.revision)
            {
                return Err("Project revision changed; observe again".into());
            }
            e.expire();
            if let Some(f) = p.get("feedback").and_then(Value::as_str) {
                e.feedback.insert(actor.into(), f.into());
            }
            match action {
                "begin" => {
                    let scopes: Vec<Scope> =
                        serde_json::from_value(p.get("scopes").cloned().unwrap_or(json!([])))
                            .map_err(|e| e.to_string())?;
                    Ok(json!(e.reserve(
                        actor,
                        p.get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("AI editing"),
                        scopes
                    )?))
                }
                "end" => {
                    let task = p
                        .get("task")
                        .and_then(Value::as_str)
                        .ok_or("Missing task ID")?;
                    if !e.leases.iter().any(|l| l.id == task && l.owner == actor) {
                        return Err("Task not owned or already released".into());
                    }
                    e.finish_task(task, "ended");
                    e.expire_proposals();
                    Ok(json!({"released":task}))
                }
                "update" => {
                    let task = p
                        .get("task")
                        .and_then(Value::as_str)
                        .ok_or("Missing task ID")?;
                    let pos = e
                        .leases
                        .iter()
                        .position(|l| l.id == task && l.owner == actor)
                        .ok_or("Task expired or was released; do not silently reacquire")?;
                    if let Some(scopes) = p.get("scopes") {
                        let scopes: Vec<Scope> =
                            serde_json::from_value(scopes.clone()).map_err(|e| e.to_string())?;
                        e.check(actor, &scopes)?;
                        e.leases[pos].scopes = scopes;
                    }
                    if let Some(d) = p.get("description").and_then(Value::as_str) {
                        e.leases[pos].description = d.into();
                        e.activity = d.into();
                    }
                    e.leases[pos].expires = engine::now() + 300;
                    Ok(json!(e.leases[pos]))
                }
                "status" => Ok(json!(e.leases)),
                "recovery" => Ok(e.task_recovery()),
                _ => Err("Unknown task action".into()),
            }
        }
        "history" => {
            let mut e = shared.lock().unwrap();
            crate::workspace::validate_target(&e, p)?;
            let action = p.get("action").and_then(Value::as_str).unwrap_or("list");
            let task = p.get("task").and_then(Value::as_str).unwrap_or("");
            let owner = p.get("task_actor").and_then(Value::as_str).unwrap_or(actor);
            if action == "inspect_task" {
                return e.inspect_task_undo(actor, owner, task);
            }
            if action != "list" {
                if actor != "human" && p.get("expected_revision").and_then(Value::as_u64).is_none()
                {
                    return Err(
                        "AI history changes require expected_revision from the latest observation"
                            .into(),
                    );
                }
                if p.get("expected_revision")
                    .and_then(Value::as_u64)
                    .is_some_and(|r| r != e.doc.revision)
                {
                    return Err("Document changed before undo/redo. Observe again.".into());
                }
            }
            match action {
                "undo" => e.undo(actor)?,
                "redo" => e.redo(actor)?,
                "undo_task" => {
                    e.undo_task(actor, owner, task)?;
                }
                "list" => {}
                _ => return Err("Unknown history action".into()),
            }
            let mut result = json!({"revision":e.doc.revision,"undo":e.undo.iter().map(|h|json!({"actor":h.actor,"label":h.label,"task":h.task,"revision":h.after.revision,"reverted_batches":h.reverted})).collect::<Vec<_>>(),"redo_count":e.redo.len(),"tasks":e.task_history()});
            drop(e);
            if action != "list" && p["feedback"] != "request" {
                result["images"] = observation(
                    shared,
                    &json!({"max_edge":p.get("max_edge").and_then(Value::as_u64).unwrap_or(768)}),
                )?["images"]
                    .clone();
            }
            Ok(result)
        }
        "document" => {
            let action = p
                .get("action")
                .and_then(Value::as_str)
                .ok_or("Missing action")?;
            let path = p.get("path").and_then(Value::as_str).map(PathBuf::from);
            let opening_source = if ["new", "open", "open_async"].contains(&action) {
                let e = shared.lock().unwrap();
                crate::workspace::validate_target(&e, p)?;
                if p["expected_revision"]
                    .as_u64()
                    .is_some_and(|r| r != e.doc.revision)
                {
                    return Err("Document revision changed; observe again before opening".into());
                }
                if e.doc.revision != e.saved_revision
                    && p.get("discard") != Some(&Value::Bool(true))
                {
                    return Err("Unsaved work: save first or explicitly pass discard=true".into());
                }
                Some((e.doc.id.clone(), e.doc.revision))
            } else {
                None
            };
            match action {
                "save_report" => {
                    let e = shared.lock().unwrap();
                    crate::workspace::validate_target(&e, p)?;
                    let document = p["document_id"]
                        .as_str()
                        .ok_or("Observe the document_id before requesting a save report")?;
                    let revision = p["expected_revision"]
                        .as_u64()
                        .ok_or("Observe expected_revision before requesting a save report")?;
                    if document != e.doc.id || revision != e.doc.revision {
                        return Err(
                            "Document changed; observe again before requesting a save report"
                                .into(),
                        );
                    }
                    return serde_json::to_value(crate::compatibility::report(&e.doc)?)
                        .map_err(|error| error.to_string());
                }
                "pattern_sources" => {
                    let e = shared.lock().unwrap();
                    crate::workspace::validate_target(&e, p)?;
                    if p["document_id"].as_str() != Some(e.doc.id.as_str())
                        || p["expected_revision"].as_u64() != Some(e.doc.revision)
                    {
                        return Err("Pattern inspection requires the observed document_id and expected_revision".into());
                    }
                    let mut result = json!({"document_id":e.doc.id,"revision":e.doc.revision,"read_only":e.doc.read_only,"coordinate_space":"pattern_asset","color_space":"unmanaged_source_samples","reason":"Raw pattern pixels are inspected without interpreting Photoshop profile/color handling or layer-style placement. Original documents remain protected.","patterns":e.doc.source_patterns});
                    if let Some(index) = p.get("pattern_index") {
                        let index = index
                            .as_u64()
                            .ok_or("pattern_index must be a zero-based integer")?
                            as usize;
                        let source = e
                            .doc
                            .source_patterns
                            .get(index)
                            .ok_or("Unknown inspected pattern index")?;
                        let png = if source.bit_depth == 16 {
                            raster::png16(source.width, source.height, &source.pixels.rgba16())?
                        } else {
                            raster::png(source.width, source.height, &source.pixels.rgba())?
                        };
                        result["images"] = json!([{"data":STANDARD.encode(png),"mime_type":"image/png","width":source.width,"height":source.height,"native_depth":source.bit_depth,"coordinate_space":"pattern_asset","asset_rect":source.bounds,"source_index":source.source_index,"document_id":e.doc.id,"revision":e.doc.revision,"preview_only":true,"color_space":"unmanaged_source_samples"}]);
                    }
                    return Ok(result);
                }
                "new" => {
                    let depth = p.get("bit_depth").map_or(Ok(8), |v| {
                        v.as_u64()
                            .filter(|d| [8, 16].contains(d))
                            .map(|d| d as u16)
                            .ok_or("Color depth must be 8 or 16 bits")
                    })?;
                    let doc = engine::Document::new_depth(
                        p.get("width").and_then(Value::as_u64).unwrap_or(1024) as u32,
                        p.get("height").and_then(Value::as_u64).unwrap_or(768) as u32,
                        depth,
                    )?;
                    let mut e = shared.lock().unwrap();
                    if opening_source.as_ref() != Some(&(e.doc.id.clone(), e.doc.revision)) {
                        return Err("Canvas changed before opening; current work preserved".into());
                    }
                    e.replace(doc, None)?;
                    e.release_file_ownership();
                    e.file_version = None;
                }
                "open" => {
                    let control = crate::loading::Control::default();
                    let (document, revision) = opening_source.unwrap();
                    control.bind(document, revision);
                    open_options(shared, path.as_deref().ok_or("Missing path")?, &control, p)?;
                }
                "open_async" => {
                    let path = path.ok_or("Missing path")?;
                    let control = crate::loading::Control::default();
                    {
                        let mut e = shared.lock().unwrap();
                        if e.loading.is_some() {
                            return Err("A document is already opening".into());
                        }
                        let (document, revision) = opening_source.unwrap();
                        if (document.clone(), revision) != (e.doc.id.clone(), e.doc.revision) {
                            return Err(
                                "Canvas changed before opening; current work preserved".into()
                            );
                        }
                        control.bind(document, revision);
                        e.loading = Some(control.clone());
                        e.status = "Opening document…".into();
                    }
                    let worker = shared.clone();
                    let worker_control = control.clone();
                    let options = p.clone();
                    if let Err(error) =
                        thread::Builder::new()
                            .name("peerbrush-open".into())
                            .spawn(move || {
                                let _ = open_options(&worker, &path, &worker_control, &options);
                            })
                    {
                        drop(LoadGuard {
                            shared: shared.clone(),
                            control,
                        });
                        return Err(format!("Could not start opening: {error}"));
                    }
                }
                "cancel_open" => {
                    let e = shared.lock().unwrap();
                    crate::workspace::validate_target(&e, p)?;
                    if let Some(control) = &e.loading {
                        control.cancel();
                    }
                }
                "compatible_copy" => compatible_copy_guarded(
                    shared,
                    actor,
                    p["expected_revision"].as_u64(),
                    p["convert_to_srgb"].as_bool().unwrap_or(false),
                    p["document_id"].as_str(),
                )?,
                "save" => {
                    let path = path
                        .or_else(|| shared.lock().unwrap().path.clone())
                        .ok_or("Choose a PSD or PSB path")?;
                    save_guarded(shared, &path, p)?;
                }
                "export" => export_guarded(shared, path.as_deref().ok_or("Missing path")?, p)?,
                _ => return Err("Unknown document action".into()),
            }
            Ok(shared.lock().unwrap().state())
        }
        _ => Err(format!("Unknown API method: {method}")),
    }
}

fn initialization(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2025-03-26");
    let version = if LEGACY_MCP_VERSIONS.contains(&requested) {
        requested
    } else {
        MCP_HTTP_VERSIONS[1]
    };
    json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"PeerBrush","version":env!("CARGO_PKG_VERSION")},"instructions":"Observe before edits. Use explicit IDs, expected_revision, optional selective task reservations, and PNG feedback. Never silently reacquire after user takeover. Work like an artist: inspect actual images after edits, undo weak attempts, refine and retry; use redo to compare. Publish concise human-language activity through task descriptions. Do not assume the first edit is final."})
}

pub fn mcp(shared: &Shared, request: &Value) -> Value {
    let request_id = request.get("id").cloned().unwrap_or(Value::Null);
    let p = request.get("params").cloned().unwrap_or(json!({}));
    let client_id = request
        .get("_client")
        .and_then(Value::as_str)
        .unwrap_or("direct-mcp");
    {
        if let Ok(mut e) = shared.try_lock() {
            if request["method"] == "initialize" || e.mcp_clients.contains_key(client_id) {
                e.mcp_clients.insert(client_id.into(), engine::now() + 45);
            }
        }
    }
    let result: Result<Value, String> = match request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "initialize" => Ok(initialization(&p)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools":tools()})),
        "tools/call" => {
            let name = p.get("name").and_then(Value::as_str).unwrap_or("");
            let method = name.strip_prefix("peerbrush_").unwrap_or(name);
            if ![
                "capabilities",
                "fonts",
                "exports",
                "brushes",
                "tool_settings",
                "filters",
                "image_info",
                "projects",
                "recovery",
                "observe",
                "edit",
                "code",
                "proposal",
                "place_image",
                "segment",
                "task",
                "document",
                "history",
            ]
            .contains(&method)
            {
                return json!({"jsonrpc":"2.0","id":request_id,"result":{"content":[{"type":"text","text":"Unknown PeerBrush tool"}],"isError":true}});
            }
            {
                if let Ok(mut e) = shared.try_lock() {
                    if e.activity.is_empty() || e.leases.is_empty() {
                        e.activity = match method {
                            "observe" => "Looking at the canvas",
                            "edit" => "Updating the shared canvas",
                            "proposal" => "Reviewing proposed canvas changes",
                            "place_image" => "Placing image pixels",
                            "segment" => "Selecting the subject",
                            "document" => "Working with your document",
                            "history" => "Reviewing recent changes",
                            _ => "Getting ready to work together",
                        }
                        .into();
                    }
                }
            }
            match dispatch(shared, method, p.get("arguments").unwrap_or(&json!({}))) {
                Ok(mut result) => {
                    let images = result
                        .get("images")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(o) = result.as_object_mut() {
                        o.remove("images");
                    }
                    if !images.is_empty() {
                        result["views"] = json!(images
                            .iter()
                            .map(|i| {
                                let mut meta = i.clone();
                                meta.as_object_mut().unwrap().remove("data");
                                meta
                            })
                            .collect::<Vec<_>>());
                    }
                    let mut content =
                        vec![json!({"type":"text","text":serde_json::to_string(&result).unwrap()})];
                    for img in images {
                        content.push(
                            json!({"type":"image","mimeType":"image/png","data":img["data"]}),
                        );
                    }
                    Ok(json!({"content":content,"isError":false}))
                }
                Err(e) => Ok(json!({"content":[{"type":"text","text":e}],"isError":true})),
            }
        }
        method if method.starts_with("notifications/") => return Value::Null,
        _ => Err("Method not found".into()),
    };
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":request_id,"result":result}),
        Err(error) => {
            json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":error}})
        }
    }
}
/// Both per-request metadata and initialization-based Streamable HTTP clients.
pub const MCP_HTTP_VERSIONS: [&str; 3] = ["2026-07-28", "2025-11-25", "2025-06-18"];
// Stdio keeps the installed adapter's older initialization compatibility.
const LEGACY_MCP_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const HTTP_BODY_LIMIT: usize = 4 * 1024 * 1024;
const HTTP_SESSION_LIMIT: usize = 128;
const HTTP_SESSION_TTL: u64 = 300;

struct HttpSession {
    version: String,
    expires: u64,
}

fn header<'a>(request: &'a tiny_http::Request, name: &'static str) -> Result<Option<&'a str>, ()> {
    let mut values = request.headers().iter().filter(|h| h.field.equiv(name));
    let value = values.next().map(|h| h.value.as_str());
    if values.next().is_some() {
        Err(())
    } else {
        Ok(value)
    }
}

fn rpc_error(request: Option<&Value>, code: i32, message: &str) -> Value {
    let mut error = json!({"jsonrpc":"2.0","error":{"code":code,"message":message}});
    if let Some(id) = request.and_then(|q| q.get("id")) {
        if id.is_string() || id.is_number() {
            error["id"] = id.clone();
        }
    }
    error
}

fn respond_http(
    request: tiny_http::Request,
    status: u16,
    body: Option<Value>,
    session: Option<&str>,
) {
    let mut response =
        tiny_http::Response::from_string(body.as_ref().map(Value::to_string).unwrap_or_default())
            .with_status_code(status)
            .with_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
    if body.is_some() {
        response
            .add_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    }
    if status == 405 {
        response.add_header(tiny_http::Header::from_bytes("Allow", "POST, DELETE").unwrap());
    }
    if let Some(id) = session {
        response.add_header(tiny_http::Header::from_bytes("MCP-Session-Id", id).unwrap());
    }
    let _ = request.respond(response);
}

fn fail_http(
    request: tiny_http::Request,
    status: u16,
    q: Option<&Value>,
    code: i32,
    message: &str,
) {
    respond_http(request, status, Some(rpc_error(q, code, message)), None);
}

fn unsupported_version(request: tiny_http::Request, q: &Value, requested: &str) {
    let mut error = rpc_error(Some(q), -32022, "Unsupported MCP protocol version");
    error["error"]["data"] = json!({"requested":requested,"supported":MCP_HTTP_VERSIONS});
    respond_http(request, 400, Some(error), None);
}

fn touch_http_presence(shared: &Shared, client: &str) {
    if let Ok(mut engine) = shared.try_lock() {
        engine.expire();
        engine.mcp_clients.insert(client.into(), engine::now() + 90);
    }
}
/// Slow edits must not serialize independent projects at the transport layer.
fn http_work(
    request: tiny_http::Request,
    shared: &Shared,
    work: impl FnOnce(&Shared) -> Value + Send + 'static,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static RUNNING: AtomicUsize = AtomicUsize::new(0);
    if RUNNING
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 64).then_some(n + 1)
        })
        .is_err()
    {
        fail_http(
            request,
            429,
            None,
            -32000,
            "Too many concurrent operations; retry after a request finishes",
        );
        return;
    }
    let shared = shared.clone();
    thread::spawn(move || {
        struct Permit;
        impl Drop for Permit {
            fn drop(&mut self) {
                RUNNING.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let _permit = Permit;
        let result = work(&shared);
        respond_http(request, 200, Some(result), None);
    });
}

fn decode_name_header(value: &str) -> Option<String> {
    if let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|v| v.strip_suffix("?="))
    {
        String::from_utf8(STANDARD.decode(encoded).ok()?).ok()
    } else if value
        .bytes()
        .all(|c| c == b'\t' || (0x20..=0x7e).contains(&c))
    {
        Some(value.into())
    } else {
        None
    }
}

fn handle_modern_http(
    request: tiny_http::Request,
    shared: &Shared,
    mut q: Value,
    version: Option<&str>,
) {
    // Notifications have no core request metadata schema and never execute editing tools.
    if q.get("id").is_none() {
        if version != Some(MCP_HTTP_VERSIONS[0]) {
            unsupported_version(request, &q, version.unwrap_or(""));
        } else {
            respond_http(request, 202, None, None);
        }
        return;
    }
    let meta = &q["params"]["_meta"];
    let body_version = meta["io.modelcontextprotocol/protocolVersion"].as_str();
    let method = q["method"].as_str().unwrap_or("").to_owned();
    let method_header = header(&request, "Mcp-Method");
    if version.is_none() || body_version != version || method_header != Ok(Some(method.as_str())) {
        fail_http(
            request,
            400,
            Some(&q),
            -32020,
            "MCP version or method header is missing or does not match the request body",
        );
        return;
    }
    if ["tools/call", "resources/read", "prompts/get"].contains(&method.as_str()) {
        let field = if method == "resources/read" {
            "uri"
        } else {
            "name"
        };
        let name = q["params"][field].as_str();
        let name_header = header(&request, "Mcp-Name")
            .ok()
            .flatten()
            .and_then(decode_name_header);
        if name.is_none() || name_header.as_deref() != name {
            fail_http(
                request,
                400,
                Some(&q),
                -32020,
                "Mcp-Name header is missing, malformed or does not match the request body",
            );
            return;
        }
    }
    if version != Some(MCP_HTTP_VERSIONS[0]) {
        unsupported_version(request, &q, version.unwrap_or(""));
        return;
    }
    if !meta["io.modelcontextprotocol/clientCapabilities"].is_object()
        || meta
            .get("io.modelcontextprotocol/clientInfo")
            .is_some_and(|info| !info["name"].is_string() || !info["version"].is_string())
    {
        fail_http(
            request,
            400,
            Some(&q),
            -32602,
            "Request metadata requires clientCapabilities and valid optional clientInfo",
        );
        return;
    }
    if !["server/discover", "ping", "tools/list", "tools/call"].contains(&method.as_str()) {
        fail_http(request, 404, Some(&q), -32601, "Method not found");
        return;
    }
    // Identity is presentation only. One bounded presence entry covers stateless clients.
    q["_client"] = json!("http-stateless");
    touch_http_presence(shared, "http-stateless");
    http_work(request, shared, move |shared| {
        let mut result = if method == "server/discover" {
            json!({"jsonrpc":"2.0","id":q["id"],"result":{
                "supportedVersions":MCP_HTTP_VERSIONS,"capabilities":{"tools":{}},
                "instructions":initialization(&json!({}))["instructions"],"ttlMs":300000,"cacheScope":"private"
            }})
        } else {
            mcp(shared, &q)
        };
        if result.get("result").is_some() {
            result["result"]["resultType"] = json!("complete");
            result["result"]["_meta"]["io.modelcontextprotocol/serverInfo"] =
                json!({"name":"PeerBrush","version":env!("CARGO_PKG_VERSION")});
            if method == "tools/list" {
                result["result"]["ttlMs"] = json!(300000);
                result["result"]["cacheScope"] = json!("private");
            }
        }
        touch_http_presence(shared, "http-stateless");
        result
    });
}

fn handle_http(
    mut request: tiny_http::Request,
    shared: &Shared,
    auth: &str,
    port: u16,
    sessions: &mut std::collections::BTreeMap<String, HttpSession>,
) {
    let localhost = format!("localhost:{port}");
    let loopback = format!("127.0.0.1:{port}");
    let origin_ok = match header(&request, "Origin") {
        Ok(None) => true,
        Ok(Some(origin)) => {
            origin == format!("http://{localhost}") || origin == format!("http://{loopback}")
        }
        Err(()) => false,
    };
    if !origin_ok
        || !matches!(header(&request, "Host"), Ok(Some(host)) if host == localhost || host == loopback)
    {
        fail_http(
            request,
            403,
            None,
            -32600,
            "Browser origin or host is not permitted",
        );
        return;
    }
    if header(&request, "Authorization") != Ok(Some(auth)) {
        fail_http(
            request,
            401,
            None,
            -32600,
            "PeerBrush bearer token required",
        );
        return;
    }
    let is_mcp = request.url() == "/mcp";
    if !is_mcp && request.url() != "/rpc" {
        fail_http(request, 404, None, -32601, "Not found");
        return;
    }
    let now = engine::now();
    sessions.retain(|id, session| {
        if session.expires <= now {
            if let Ok(mut e) = shared.try_lock() {
                e.mcp_clients.remove(&format!("http-{id}"));
            }
            false
        } else {
            true
        }
    });
    if is_mcp && request.method() == &tiny_http::Method::Delete {
        let session_id = match header(&request, "MCP-Session-Id") {
            Ok(value) => value.map(String::from),
            Err(()) => {
                fail_http(
                    request,
                    400,
                    None,
                    -32600,
                    "Duplicate MCP-Session-Id header",
                );
                return;
            }
        };
        let version = match header(&request, "MCP-Protocol-Version") {
            Ok(value) => value,
            Err(()) => {
                fail_http(
                    request,
                    400,
                    None,
                    -32020,
                    "Duplicate MCP-Protocol-Version header",
                );
                return;
            }
        };
        if version == Some(MCP_HTTP_VERSIONS[0]) {
            fail_http(
                request,
                405,
                None,
                -32600,
                "Stateless MCP does not use DELETE sessions",
            );
        } else if let Some(id) = session_id {
            if let Some(session) = sessions.get(&id) {
                if version.is_some_and(|v| v != session.version) {
                    fail_http(
                        request,
                        400,
                        None,
                        -32602,
                        "MCP protocol version does not match the session",
                    );
                    return;
                }
                sessions.remove(&id);
                if let Ok(mut e) = shared.try_lock() {
                    e.mcp_clients.remove(&format!("http-{id}"));
                }
                respond_http(request, 204, None, None);
            } else {
                fail_http(
                    request,
                    404,
                    None,
                    -32600,
                    "MCP session expired or unknown; initialize again",
                );
            }
        } else {
            fail_http(request, 400, None, -32600, "MCP-Session-Id required");
        }
        return;
    }
    if request.method() != &tiny_http::Method::Post {
        fail_http(
            request,
            405,
            None,
            -32600,
            "Use POST for MCP messages; standalone SSE is not supported",
        );
        return;
    }
    if is_mcp {
        let accepts = header(&request, "Accept").ok().flatten().unwrap_or("");
        let accepts_type = |kind| {
            accepts.split(',').any(|part| {
                part.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case(kind)
            })
        };
        if !accepts_type("application/json") || !accepts_type("text/event-stream") {
            fail_http(
                request,
                406,
                None,
                -32600,
                "Accept must include application/json and text/event-stream",
            );
            return;
        }
        let content_type = header(&request, "Content-Type")
            .ok()
            .flatten()
            .unwrap_or("");
        if !content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
        {
            fail_http(
                request,
                415,
                None,
                -32600,
                "Content-Type must be application/json",
            );
            return;
        }
    }
    if request.body_length().unwrap_or(0) > HTTP_BODY_LIMIT {
        fail_http(
            request,
            413,
            None,
            -32600,
            "Request exceeds the 4 MiB limit",
        );
        return;
    }
    let mut body = Vec::new();
    if request
        .as_reader()
        .take((HTTP_BODY_LIMIT + 1) as u64)
        .read_to_end(&mut body)
        .is_err()
    {
        fail_http(request, 400, None, -32700, "Could not read request body");
        return;
    }
    if body.len() > HTTP_BODY_LIMIT {
        fail_http(
            request,
            413,
            None,
            -32600,
            "Request exceeds the 4 MiB limit",
        );
        return;
    }
    let mut q: Value = match serde_json::from_slice(&body) {
        Ok(q) => q,
        Err(_) if !is_mcp => {
            respond_http(
                request,
                200,
                Some(json!({"ok":false,"error":"Invalid JSON"})),
                None,
            );
            return;
        }
        Err(_) => {
            fail_http(request, 400, None, -32700, "Parse error");
            return;
        }
    };
    if !is_mcp {
        // The private authenticated bridge lets stdio keep its offline handshake and reconnect.
        http_work(request, shared, move |shared| {
            let result = if q["method"] == "mcp" {
                if q["params"]["request"].is_object() {
                    if q["params"]["request"].get("id").is_none() {
                        Ok(Value::Null)
                    } else {
                        Ok(mcp(shared, &q["params"]["request"]))
                    }
                } else {
                    Err("MCP bridge requires a request object".into())
                }
            } else {
                dispatch(
                    shared,
                    q.get("method").and_then(Value::as_str).unwrap_or(""),
                    q.get("params").unwrap_or(&json!({})),
                )
            };
            match result {
                Ok(value) => json!({"ok":true,"result":value}),
                Err(error) => json!({"ok":false,"error":error}),
            }
        });
        return;
    }
    let id_ok = q
        .get("id")
        .is_none_or(|id| id.is_string() || id.is_number());
    let response = q.get("method").is_none()
        && q.get("id").is_some()
        && (q.get("result").is_some() != q.get("error").is_some());
    if !q.is_object()
        || q["jsonrpc"] != "2.0"
        || !id_ok
        || (!response && !q["method"].is_string())
        || q.get("params").is_some_and(|p| !p.is_object())
        || (q.get("id").is_some()
            && q["method"]
                .as_str()
                .is_some_and(|m| m.starts_with("notifications/")))
    {
        fail_http(
            request,
            400,
            Some(&q),
            -32600,
            "Invalid JSON-RPC request; batches are not supported",
        );
        return;
    }
    if q["method"] == "tools/call"
        && q.get("id").is_some()
        && (!q["params"]["name"].is_string()
            || q["params"]
                .get("arguments")
                .is_some_and(|arguments| !arguments.is_object()))
    {
        fail_http(
            request,
            400,
            Some(&q),
            -32602,
            "Tool calls require a name and object arguments",
        );
        return;
    }
    let version = match header(&request, "MCP-Protocol-Version") {
        Ok(value) => value.map(String::from),
        Err(()) => {
            fail_http(
                request,
                400,
                Some(&q),
                -32020,
                "Duplicate MCP-Protocol-Version header",
            );
            return;
        }
    };
    if version.as_deref() == Some("2025-03-26") {
        unsupported_version(request, &q, "2025-03-26");
        return;
    }
    let body_version = q["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"].as_str();
    let modern = version
        .as_deref()
        .is_some_and(|v| !MCP_HTTP_VERSIONS[1..].contains(&v))
        || body_version.is_some_and(|v| !MCP_HTTP_VERSIONS[1..].contains(&v));
    if modern {
        if response {
            fail_http(
                request,
                400,
                Some(&q),
                -32600,
                "Stateless HTTP clients cannot send JSON-RPC responses",
            );
        } else {
            handle_modern_http(request, shared, q, version.as_deref());
        }
        return;
    }
    let session_id = match header(&request, "MCP-Session-Id") {
        Ok(value) => value.map(String::from),
        Err(()) => {
            fail_http(
                request,
                400,
                Some(&q),
                -32600,
                "Duplicate MCP-Session-Id header",
            );
            return;
        }
    };
    if q["method"] == "initialize" && q.get("id").is_some() {
        if session_id.is_some() {
            fail_http(
                request,
                400,
                Some(&q),
                -32600,
                "Initialize a new connection without MCP-Session-Id",
            );
            return;
        }
        if q["params"]
            .get("protocolVersion")
            .is_some_and(|v| !v.is_string())
        {
            fail_http(
                request,
                400,
                Some(&q),
                -32602,
                "protocolVersion must be a string",
            );
            return;
        }
        if sessions.len() >= HTTP_SESSION_LIMIT {
            fail_http(
                request,
                429,
                Some(&q),
                -32000,
                "Too many MCP sessions; close unused connections and retry",
            );
            return;
        }
        let requested = q["params"]["protocolVersion"]
            .as_str()
            .unwrap_or("2025-06-18");
        let version = if MCP_HTTP_VERSIONS[1..].contains(&requested) {
            requested
        } else {
            MCP_HTTP_VERSIONS[1]
        };
        let version = version.to_owned();
        q["params"]["protocolVersion"] = json!(version);
        let id = engine::id();
        q["_client"] = json!(format!("http-{id}"));
        let result = mcp(shared, &q);
        sessions.insert(
            id.clone(),
            HttpSession {
                version: result["result"]["protocolVersion"].as_str().unwrap().into(),
                expires: now + HTTP_SESSION_TTL,
            },
        );
        touch_http_presence(shared, &format!("http-{id}"));
        respond_http(request, 200, Some(result), Some(&id));
        return;
    }
    let Some(id) = session_id else {
        fail_http(
            request,
            400,
            Some(&q),
            -32600,
            "MCP-Session-Id required; initialize the connection first",
        );
        return;
    };
    if id.is_empty() || !id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        fail_http(request, 400, Some(&q), -32600, "Invalid MCP-Session-Id");
        return;
    }
    let Some(session) = sessions.get_mut(&id) else {
        fail_http(
            request,
            404,
            Some(&q),
            -32600,
            "MCP session expired or unknown; initialize again",
        );
        return;
    };
    if version.as_deref().is_some_and(|v| v != session.version) {
        fail_http(
            request,
            400,
            Some(&q),
            -32602,
            "MCP protocol version does not match the session",
        );
        return;
    }
    session.expires = now + HTTP_SESSION_TTL;
    let client = format!("http-{id}");
    touch_http_presence(shared, &client);
    if response || q.get("id").is_none() {
        respond_http(request, 202, None, None);
    } else {
        q["_client"] = json!(client);
        http_work(request, shared, move |shared| {
            let result = mcp(shared, &q);
            touch_http_presence(shared, &client);
            result
        });
    }
}

pub fn start(shared: Shared, state_dir: PathBuf) -> Result<Connection, String> {
    start_with_preferences(shared, state_dir.clone(), &state_dir, &state_dir)
}

/// Preferences/cache may be per-user while external discovery and recovery stay stable.
pub fn start_with_preferences(
    shared: Shared,
    state_dir: PathBuf,
    preferences: &Path,
    cache: &Path,
) -> Result<Connection, String> {
    fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    let instance_lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_dir.join("instance.lock"))
        .map_err(|e| e.to_string())?;
    instance_lock.try_lock().map_err(|_|"PeerBrush is already running for this workspace. Use that window or a different --state-dir.".to_string())?;
    crate::disk_cache::configure(cache);
    let library = crate::brush_library::Library::load(preferences.join("brushes.json"));
    if let Some(error) = &library.error {
        shared.lock().unwrap().status = error.clone();
    }
    *shared.lock().unwrap().brush_library.lock().unwrap() = library;
    let tools = crate::tool_settings::Store::load(preferences.join("tool-settings.json"));
    if let Some(error) = &tools.error {
        shared.lock().unwrap().status = error.clone();
    }
    *shared.lock().unwrap().tool_settings.lock().unwrap() = tools;
    let library = crate::filter_library::Library::load(preferences.join("filters.json"));
    if let Some(error) = &library.error {
        shared.lock().unwrap().status = error.clone();
    }
    *shared.lock().unwrap().filter_library.lock().unwrap() = library;
    let library =
        crate::production_export::library::Library::load(preferences.join("export-presets.json"));
    if let Some(error) = &library.error {
        shared.lock().unwrap().status = error.clone();
    }
    *shared.lock().unwrap().export_library.lock().unwrap() = library;
    let workspace = crate::workspace::attach(&shared);
    workspace.lock().unwrap().recovery_dir = Some(state_dir.clone());
    // Preserve the preceding session's index before the running session updates its own.
    if let Ok(bytes) = fs::read(state_dir.join("recoveries.json")) {
        if bytes.len() < 65536
            && serde_json::from_slice::<Value>(&bytes)
                .ok()
                .is_some_and(|index| index["projects"].as_array().is_some_and(|p| !p.is_empty()))
        {
            atomic_write(&state_dir.join("previous-recoveries.json"), &bytes)?;
        }
    }
    let http = tiny_http::Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = http
        .server_addr()
        .to_ip()
        .ok_or("No server address")?
        .port();
    let token = engine::id();
    let auth = format!("Bearer {token}");
    atomic_write(&state_dir.join("connection.json"),serde_json::to_string_pretty(&json!({"url":format!("http://127.0.0.1:{port}"),"token":token,"pid":std::process::id()})).unwrap().as_bytes())?;
    let recovery_dir = state_dir.clone();
    thread::spawn(move || {
        let mut previous = std::collections::HashMap::new();
        loop {
            thread::sleep(std::time::Duration::from_secs(5));
            checkpoint_projects(&workspace, &recovery_dir, &mut previous);
        }
    });
    let availability = Arc::new(AtomicBool::new(true));
    let listening = availability.clone();
    thread::spawn(move || {
        let _availability = ServerAvailability(listening);
        let mut sessions = std::collections::BTreeMap::new();
        for request in http.incoming_requests() {
            handle_http(request, &shared, &auth, port, &mut sessions);
        }
    });
    Ok(Connection {
        port,
        token,
        state_dir,
        instance_lock: Some(instance_lock),
        availability,
    })
}
pub type RecoveryVersions = crate::recovery::Versions;
pub fn checkpoint_projects(
    workspace: &crate::workspace::Registry,
    state_dir: &Path,
    previous: &mut RecoveryVersions,
) {
    crate::recovery::checkpoint(workspace, state_dir, previous);
}
pub fn recovery_projects(state_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut seen = std::collections::HashSet::new();
    let mut results: Vec<_> = crate::recovery::catalog(state_dir)
        .into_iter()
        .filter(|e| seen.insert(e.project_id.clone()))
        .map(|e| (e.name, state_dir.join(e.file)))
        .collect();
    results.extend(crate::recovery::legacy(state_dir));
    results
}
use std::io::Read;
pub fn default_state_dir() -> PathBuf {
    std::env::temp_dir().join("PeerBrush")
}
pub fn client(state_dir: &Path, endpoint: &str, payload: &Value) -> Result<Value, String> {
    client_with_timeout(
        state_dir,
        endpoint,
        payload,
        std::time::Duration::from_secs(60),
    )
}
pub fn focus_existing(state_dir: &Path) -> bool {
    client_with_timeout(
        state_dir,
        "rpc",
        &json!({"method":"focus","params":{}}),
        std::time::Duration::from_millis(500),
    )
    .is_ok_and(|r| r["ok"] == true)
}
/// Forward files only after confirming a running listener. The same bearer-authenticated
/// local endpoint is used; receiving a request never discards or replaces a project.
pub fn activate_existing(state_dir: &Path, files: &[PathBuf]) -> Result<bool, String> {
    if !focus_existing(state_dir) {
        return Ok(false);
    }
    if files.is_empty() {
        return Ok(true);
    }
    let paths = files
        .iter()
        .map(|p| p.to_str().ok_or("This file path cannot be sent to the existing window without changing its characters"))
        .collect::<Result<Vec<_>, _>>()?;
    let reply = client_with_timeout(
        state_dir,
        "rpc",
        &json!({"method":"native_open","params":{"paths":paths}}),
        std::time::Duration::from_secs(5),
    )?;
    if reply["ok"] != true {
        return Err(reply["error"]
            .as_str()
            .unwrap_or(
                "The running PeerBrush could not accept these files. Close it and try again.",
            )
            .into());
    }
    Ok(true)
}
fn client_with_timeout(
    state_dir: &Path,
    endpoint: &str,
    payload: &Value,
    timeout: std::time::Duration,
) -> Result<Value, String> {
    let response = connection_request(state_dir, endpoint, payload, timeout)
        .and_then(|response| {
            serde_json::from_reader::<_, Value>(response.into_reader()).map_err(|error| {
                ConnectionProbe::new(if error.is_io() {
                    ConnectionCategory::ConnectionInterrupted
                } else {
                    ConnectionCategory::ResponseInvalid
                })
            })
        })
        .map_err(|failure| failure.message.to_owned())?;
    if endpoint == "mcp" {
        if response["ok"] == true {
            Ok(response["result"].clone())
        } else {
            Err(response["error"]
                .as_str()
                .unwrap_or("PeerBrush MCP bridge failed")
                .into())
        }
    } else {
        Ok(response)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionCategory {
    Ready,
    ConnectionMissing,
    ConnectionUnreadable,
    ConnectionInvalid,
    ListenerUnreachable,
    AuthenticationRejected,
    ConnectionInterrupted,
    ResponseRejected,
    ResponseInvalid,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(crate) struct ConnectionProbe {
    pub category: ConnectionCategory,
    pub message: &'static str,
    pub recovery: &'static str,
}
impl ConnectionProbe {
    fn new(category: ConnectionCategory) -> Self {
        use ConnectionCategory::*;
        let (message, recovery) = match category {
            Ready => (
                "PeerBrush's local MCP bridge is ready.",
                "Use the exact stdio command and arguments from discovery.",
            ),
            ConnectionMissing => (
                "PeerBrush connection information is missing. Open the application, then retry.",
                "Open PeerBrush for the selected state directory; use Connect AI if setup is missing.",
            ),
            ConnectionUnreadable => (
                "PeerBrush connection information cannot be read.",
                "Check access to the selected state directory, then reopen the application.",
            ),
            ConnectionInvalid => (
                "PeerBrush connection information is invalid. Reopen the application.",
                "Reopen PeerBrush for the selected state directory to refresh its local connection information.",
            ),
            ListenerUnreachable => (
                "PeerBrush's local listener cannot be reached. Open the application, allow it to finish starting, then retry.",
                "Open PeerBrush for the selected state directory and allow startup to finish; if it is already open, reopen it.",
            ),
            AuthenticationRejected => (
                "PeerBrush rejected stale connection information. Reopen the application, then retry.",
                "Reopen PeerBrush to refresh the selected instance's local connection information.",
            ),
            ConnectionInterrupted => (
                "The PeerBrush connection was interrupted or timed out. Check the application and observe the current document before retrying an edit.",
                "Check PeerBrush and observe the current document before retrying any edit; a timed out edit may have completed.",
            ),
            ResponseRejected => (
                "The local endpoint rejected or redirected the PeerBrush request.",
                "Reopen PeerBrush to refresh the selected instance's local connection information, then retry.",
            ),
            ResponseInvalid => (
                "The local endpoint returned an invalid PeerBrush response.",
                "Reopen PeerBrush for the selected state directory, then retry.",
            ),
        };
        Self {
            category,
            message,
            recovery,
        }
    }
}

fn canonical_local_url(url: &str) -> bool {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0 && url == format!("http://127.0.0.1:{port}"))
}

fn connection_request(
    state_dir: &Path,
    endpoint: &str,
    payload: &Value,
    timeout: std::time::Duration,
) -> Result<ureq::Response, ConnectionProbe> {
    use ConnectionCategory::*;
    if !matches!(endpoint, "rpc" | "mcp") {
        return Err(ConnectionProbe::new(ConnectionInvalid));
    }
    let io_failure = |error: std::io::Error| {
        ConnectionProbe::new(if error.kind() == std::io::ErrorKind::NotFound {
            ConnectionMissing
        } else {
            ConnectionUnreadable
        })
    };
    let path = state_dir.join("connection.json");
    if !fs::metadata(&path).map_err(io_failure)?.is_file() {
        return Err(ConnectionProbe::new(ConnectionUnreadable));
    }
    let file = fs::File::open(&path).map_err(io_failure)?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConnectionProbe::new(ConnectionUnreadable))?;
    if bytes.len() > 64 * 1024 {
        return Err(ConnectionProbe::new(ConnectionInvalid));
    }
    let conn: Value =
        serde_json::from_slice(&bytes).map_err(|_| ConnectionProbe::new(ConnectionInvalid))?;
    let url = conn["url"]
        .as_str()
        .filter(|url| canonical_local_url(url))
        .ok_or_else(|| ConnectionProbe::new(ConnectionInvalid))?;
    let token = conn["token"]
        .as_str()
        .filter(|token| {
            !token.is_empty()
                && token.len() <= 512
                && token.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .ok_or_else(|| ConnectionProbe::new(ConnectionInvalid))?;
    let bridge = endpoint == "mcp";
    let endpoint = if bridge { "rpc" } else { endpoint };
    let payload = if bridge {
        json!({"method":"mcp","params":{"request":payload}})
    } else {
        payload.clone()
    };
    // Local authentication must never follow redirects or use an environment proxy.
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .try_proxy_from_env(false)
        .build();
    let response = agent
        .post(&format!("{url}/{endpoint}"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(timeout)
        .send_json(payload)
        .map_err(|error| match error {
            ureq::Error::Transport(ref transport)
                if transport.kind() == ureq::ErrorKind::ConnectionFailed =>
            {
                ConnectionProbe::new(ListenerUnreachable)
            }
            ureq::Error::Transport(ref transport) if transport.kind() == ureq::ErrorKind::Io => {
                ConnectionProbe::new(ConnectionInterrupted)
            }
            ureq::Error::Status(401, _) => ConnectionProbe::new(AuthenticationRejected),
            _ => ConnectionProbe::new(ResponseRejected),
        })?;
    if response.status() != 200 {
        return Err(ConnectionProbe::new(ResponseRejected));
    }
    Ok(response)
}

pub(crate) fn probe_connection(state_dir: &Path) -> ConnectionProbe {
    let id = format!("discovery-probe-{}", engine::id());
    let ping = json!({"jsonrpc":"2.0","id":id,"method":"ping","_client":id});
    let response = connection_request(state_dir, "mcp", &ping, std::time::Duration::from_secs(1))
        .and_then(|response| {
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ConnectionProbe::new(ConnectionCategory::ConnectionInterrupted))?;
            if bytes.len() > 64 * 1024 {
                return Err(ConnectionProbe::new(ConnectionCategory::ResponseInvalid));
            }
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|_| ConnectionProbe::new(ConnectionCategory::ResponseInvalid))
        });
    match response {
        Ok(response)
            if response["ok"] == true
                && response["result"] == json!({"jsonrpc":"2.0","id":id,"result":{}}) =>
        {
            ConnectionProbe::new(ConnectionCategory::Ready)
        }
        Ok(_) => ConnectionProbe::new(ConnectionCategory::ResponseInvalid),
        Err(failure) => failure,
    }
}

fn stdio_static_response(request: &Value) -> Option<Value> {
    let result = match request.get("method").and_then(Value::as_str)? {
        "initialize" => initialization(request.get("params").unwrap_or(&json!({}))),
        "ping" => json!({}),
        "tools/list" => json!({"tools":tools()}),
        _ => return None,
    };
    Some(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
}

fn stdio_unavailable(request: &Value, error: &str) -> Value {
    if request["method"] == "tools/call" {
        json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":format!("{error} The MCP adapter remains ready and will reconnect when PeerBrush opens.")}],"isError":true}})
    } else {
        json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":error}})
    }
}

fn announce_presence(state_dir: &Path, client_id: &str, connected: bool) {
    let _ = client_with_timeout(
        state_dir,
        "rpc",
        &json!({"method":"client_state","params":{"client":client_id,"connected":connected}}),
        std::time::Duration::from_secs(1),
    );
}

pub fn stdio(state_dir: &Path) -> Result<(), String> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let client_id = format!("stdio-{}", engine::id());
    let initialized = Arc::new(AtomicBool::new(false));
    let (heartbeat_stop, stop) = mpsc::channel::<()>();
    let (ready, dir, id) = (initialized.clone(), state_dir.to_owned(), client_id.clone());
    let heartbeat = thread::spawn(move || loop {
        match stop.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if ready.load(Ordering::Relaxed) {
                    announce_presence(&dir, &id, true);
                }
            }
        }
    });
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let result = (|| -> Result<(), String> {
        for line in stdin.lock().lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            let mut q: Value = match serde_json::from_str(&line) {
                Ok(q) => q,
                Err(_) => {
                    writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}))
                        .map_err(|e| e.to_string())?;
                    stdout.flush().map_err(|e| e.to_string())?;
                    continue;
                }
            };
            if !q.is_object() || !q.get("method").is_some_and(Value::is_string) {
                writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":q.get("id").cloned().unwrap_or(Value::Null),"error":{"code":-32600,"message":"Invalid Request"}}))
                    .map_err(|e| e.to_string())?;
                stdout.flush().map_err(|e| e.to_string())?;
                continue;
            }
            q["_client"] = json!(client_id);
            if q.get("id").is_none() {
                // Notifications never need a reply, including while the GUI is closed.
                let _ =
                    client_with_timeout(state_dir, "mcp", &q, std::time::Duration::from_secs(1));
                continue;
            }
            let response = if let Some(response) = stdio_static_response(&q) {
                if q["method"] == "initialize" {
                    initialized.store(true, Ordering::Relaxed);
                    announce_presence(state_dir, &client_id, true);
                }
                response
            } else {
                if initialized.load(Ordering::Relaxed) {
                    announce_presence(state_dir, &client_id, true);
                }
                client(state_dir, "mcp", &q).unwrap_or_else(|error| stdio_unavailable(&q, &error))
            };
            writeln!(stdout, "{}", response).map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    let _ = heartbeat_stop.send(());
    // Join before removing presence: an in-flight heartbeat must not revive a closed client.
    let _ = heartbeat.join();
    announce_presence(state_dir, &client_id, false);
    result
}

#[cfg(test)]
mod save_loss_tests {
    use super::*;
    use crate::engine::Document;

    #[test]
    fn derived_format_failure_cancellation_and_later_human_work_preserve_the_destination() {
        use crate::production_export::{self, Control, Format};
        for format in [Format::Webp, Format::Jxl] {
            for depth in [8, 16] {
                for failure in ["encode", "cancel", "source"] {
                    let root =
                        std::env::temp_dir().join(format!("peerbrush-webp-race-{}", engine::id()));
                    std::fs::create_dir(&root).unwrap();
                    let path = root.join(format!("Original.{}", format.extension()));
                    let mut engine = Engine::new();
                    engine.doc = Document::new_depth(8, 6, depth).unwrap();
                    let shared = Arc::new(Mutex::new(engine));
                    export(&shared, &path).unwrap();
                    let original = std::fs::read(&path).unwrap();
                    let source = shared.lock().unwrap().doc.clone();
                    let before = serde_json::to_value(&source).unwrap();
                    let control = Control::default();
                    let error = export_derived_guarded(&shared, &path,
                    &json!({"document_id":source.id,"expected_revision":source.revision,"overwrite":true}),
                    &control, |doc| {
                        let bytes = production_export::render_document(doc, format, crate::jpeg_xl::Options::default(), &control)?;
                        match failure {
                            "encode" => return Err("Injected encoder failure before publication".into()),
                            "cancel" => control.cancel(),
                            "source" => {
                                shared.lock().unwrap().edit("human",
                                    &[json!({"op":"fill","layer":source.layers[0].id,"rect":[1,2,3,4],"color":[53,97,173,255]})],
                                    None, None, "Later human pixels")?;
                            }
                            _ => unreachable!(),
                        }
                        Ok(bytes)
                    }).unwrap_err();
                    assert!(
                        error.contains(match failure {
                            "encode" => "encoder failure",
                            "cancel" => "canceled",
                            _ => "source changed",
                        }),
                        "{error}"
                    );
                    assert_eq!(std::fs::read(&path).unwrap(), original);
                    let engine = shared.lock().unwrap();
                    assert_eq!(engine.doc.bit_depth, depth);
                    assert_eq!(engine.saved_revision, 0);
                    if failure == "source" {
                        assert_eq!(
                            engine.doc.layers[0].pixels.get16(1, 2),
                            [53 * 257, 97 * 257, 173 * 257, 65535]
                        );
                        assert_eq!(engine.undo.len(), 1);
                    } else {
                        assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
                        assert!(engine.undo.is_empty());
                    }
                    assert_eq!(
                        std::fs::read_dir(&root)
                            .unwrap()
                            .filter_map(Result::ok)
                            .filter(|entry| entry.file_type().unwrap().is_file())
                            .count(),
                        1
                    );
                    drop(engine);
                    drop(shared);
                    std::fs::remove_dir_all(root).unwrap();
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn derived_format_alias_cannot_change_the_resolved_output_format() {
        for format in [
            crate::production_export::Format::Webp,
            crate::production_export::Format::Jxl,
        ] {
            let root = std::env::temp_dir().join(format!("peerbrush-webp-alias-{}", engine::id()));
            std::fs::create_dir(&root).unwrap();
            let original = root.join("Original.png");
            std::fs::write(&original, b"Exact original PNG").unwrap();
            let alias = root.join(format!("Alias.{}", format.extension()));
            std::os::unix::fs::symlink(&original, &alias).unwrap();
            let shared = Arc::new(Mutex::new(Engine::new()));
            let doc = shared.lock().unwrap().doc.clone();
            let error =
                export_confirmed_source(&shared, &alias, &doc.id, doc.revision).unwrap_err();
            assert!(
                error.contains("same PNG, WebP or JPEG XL format"),
                "{error}"
            );
            assert_eq!(std::fs::read(&original).unwrap(), b"Exact original PNG");
            assert!(shared.lock().unwrap().undo.is_empty());
            drop(shared);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn derived_exports_refuse_later_human_work_and_cancellation_at_publication() {
        for depth in [8, 16] {
            for cancel in [false, true] {
                let root =
                    std::env::temp_dir().join(format!("peerbrush-export-race-{}", engine::id()));
                std::fs::create_dir_all(&root).unwrap();
                let path = root.join("Original.png");
                let mut engine = Engine::new();
                engine.doc = Document::new_depth(8, 6, depth).unwrap();
                let shared = Arc::new(Mutex::new(engine));
                export(&shared, &path).unwrap();
                let original = std::fs::read(&path).unwrap();
                let source = shared.lock().unwrap().doc.clone();
                let before = serde_json::to_value(&source).unwrap();
                let control = crate::production_export::Control::default();
                let error=export_derived_guarded(&shared,&path,&json!({"document_id":source.id,"expected_revision":source.revision,"overwrite":true}),&control,|doc|{
                let bytes=doc.export_png()?;
                if cancel {control.cancel();} else {shared.lock().unwrap().edit("human",&[json!({"op":"fill","layer":source.layers[0].id,"rect":[1,2,3,4],"color":[53,97,173,255]})],None,None,"Later human pixels")?;}
                Ok(bytes)
            }).unwrap_err();
                assert!(
                    error.contains(if cancel { "canceled" } else { "source changed" }),
                    "{error}"
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
                let engine = shared.lock().unwrap();
                assert_eq!(engine.saved_revision, 0);
                if cancel {
                    assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
                    assert!(engine.undo.is_empty());
                } else {
                    assert_eq!(
                        engine.doc.layers[0].pixels.get16(1, 2),
                        [53 * 257, 97 * 257, 173 * 257, 65535]
                    );
                    assert_eq!(engine.undo.len(), 1);
                }
                drop(engine);
                drop(shared);
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn accepted_output_cannot_publish_over_later_human_work() {
        for depth in [8, 16] {
            let root = std::env::temp_dir().join(format!("peerbrush-save-race-{}", engine::id()));
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("Original.psd");
            let mut engine = Engine::new();
            engine.doc = Document::new_depth(8, 6, depth).unwrap();
            let shared = Arc::new(Mutex::new(engine));
            save(&shared, &path).unwrap();
            let original = std::fs::read(&path).unwrap();
            let (document, revision, layer) = {
                let mut engine = shared.lock().unwrap();
                let layer = engine.doc.layers[0].id.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"effect.add","layer":layer,"kind":"invert","settings":{}})],
                        None,
                        None,
                        "Editable effect",
                    )
                    .unwrap();
                (engine.doc.id.clone(), engine.doc.revision, layer)
            };
            let error = save_prepared(&shared, &path, &json!({"document_id":document,"expected_revision":revision,"accept_editability_loss":true}), || {
                shared.lock().unwrap().edit("human", &[json!({"op":"fill","layer":layer,"rect":[1,2,3,4],"color":[53,97,173,255]})], None, None, "Later human pixels").unwrap();
            }).unwrap_err();
            assert!(error.contains("source changed"), "{error}");
            assert_eq!(std::fs::read(&path).unwrap(), original);
            let engine = shared.lock().unwrap();
            assert_eq!(engine.doc.revision, revision + 1);
            assert_eq!(engine.saved_revision, 0);
            assert_eq!(engine.undo.len(), 2);
            assert_ne!(engine.doc.layers[0].pixels.get16(1, 2)[3], 0);
            assert_eq!(engine.doc.layers[0].effects.len(), 1);
            drop(engine);
            drop(shared);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use std::{net::TcpListener, time::Duration};

    const CANARY: &str = "SYNTHETIC_PRIVATE_CONNECTION_CANARY";

    struct State(PathBuf);
    impl State {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("peerbrush-probe-{}", engine::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, url: &str) {
            fs::write(
                self.0.join("connection.json"),
                json!({"url":url,"token":CANARY}).to_string(),
            )
            .unwrap();
        }
    }
    impl Drop for State {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0.join("connection.json"));
            let _ = fs::remove_dir(self.0.join("connection.json"));
            let _ = fs::remove_file(self.0.join("linked-connection.json"));
            let _ = fs::remove_dir(&self.0);
        }
    }
    fn mock(
        state: &State,
        reply: impl FnOnce(tiny_http::Request, Value) + Send + 'static,
    ) -> thread::JoinHandle<()> {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        state.write(&format!(
            "http://127.0.0.1:{}",
            server.server_addr().to_ip().unwrap().port()
        ));
        thread::spawn(move || {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("local probe request");
            assert_eq!(request.method(), &tiny_http::Method::Post);
            assert_eq!(request.url(), "/rpc");
            assert!(request
                .headers()
                .iter()
                .any(|header| header.field.equiv("Authorization")
                    && header.value.as_str() == format!("Bearer {CANARY}")));
            let body = serde_json::from_reader(request.as_reader()).unwrap();
            reply(request, body);
        })
    }
    fn check(probe: ConnectionProbe, expected: ConnectionCategory) {
        assert_eq!(probe.category, expected);
        let text = serde_json::to_string(&probe).unwrap();
        assert!(!text.contains(CANARY));
        assert!(!text.contains("http://"));
    }

    #[test]
    fn canonical_loopback_origin_rejects_userinfo_and_other_url_components() {
        for port in [1, 80, 65535] {
            assert!(canonical_local_url(&format!("http://127.0.0.1:{port}")));
        }
        for url in [
            "http://127.0.0.1:0",
            "http://127.0.0.1:65536",
            "http://127.0.0.1:00080",
            "http://127.0.0.1:+80",
            "http://127.0.0.1:80/",
            "http://127.0.0.1:80/rpc",
            "http://127.0.0.1:80?query",
            "http://127.0.0.1:80#fragment",
            "http://127.0.0.1:80@example.invalid",
            "http://127.0.0.1:80\\@example.invalid",
            "http://127.0.0.1:80\n",
            " http://127.0.0.1:80",
            "HTTP://127.0.0.1:80",
            "https://127.0.0.1:80",
            "http://localhost:80",
            "http://127.1:80",
            "http://[::1]:80",
        ] {
            assert!(!canonical_local_url(url), "must reject {url:?}");
        }
    }

    #[test]
    fn connection_metadata_failures_are_bounded_and_secret_free() {
        use ConnectionCategory::*;
        let state = State::new();
        check(probe_connection(&state.0), ConnectionMissing);
        let path = state.0.join("connection.json");
        fs::create_dir(&path).unwrap();
        check(probe_connection(&state.0), ConnectionUnreadable);
        fs::remove_dir(&path).unwrap();
        for bytes in [
            format!("{{{CANARY}"),
            CANARY.repeat(3000),
            json!({"url":"http://127.0.0.1:1"}).to_string(),
            json!({"url":"http://127.0.0.1:1","token":""}).to_string(),
            json!({"url":"http://127.0.0.1:1","token":format!("{CANARY}\r\nHeader: value")})
                .to_string(),
            json!({"url":format!("http://127.0.0.1:1@{CANARY}.invalid"),"token":CANARY})
                .to_string(),
        ] {
            fs::write(&path, bytes).unwrap();
            check(probe_connection(&state.0), ConnectionInvalid);
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        state.write(&format!(
            "http://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        ));
        drop(listener);
        check(probe_connection(&state.0), ListenerUnreachable);
    }

    #[test]
    fn invalid_routes_never_send_authorization() {
        let state = State::new();
        let trap = TcpListener::bind("127.0.0.1:0").unwrap();
        trap.set_nonblocking(true).unwrap();
        let origin = format!("http://127.0.0.1:{}", trap.local_addr().unwrap().port());
        for url in [
            format!(
                "http://127.0.0.1:1@127.0.0.1:{}",
                trap.local_addr().unwrap().port()
            ),
            format!("{origin}/{CANARY}"),
            format!("{origin}?{CANARY}"),
            format!("{origin}#{CANARY}"),
        ] {
            state.write(&url);
            check(
                connection_request(&state.0, "rpc", &json!({}), Duration::from_millis(100))
                    .unwrap_err(),
                ConnectionCategory::ConnectionInvalid,
            );
        }
        state.write(&origin);
        check(
            connection_request(
                &state.0,
                "rpc/other",
                &json!({}),
                Duration::from_millis(100),
            )
            .unwrap_err(),
            ConnectionCategory::ConnectionInvalid,
        );
        assert_eq!(
            trap.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn probe_ping_leaves_presence_focus_history_and_sources_unchanged() {
        let state = State::new();
        let shared = Arc::new(Mutex::new(Engine::new()));
        shared
            .lock()
            .unwrap()
            .mcp_clients
            .insert("direct-mcp".into(), 123);
        let snapshot = |engine: &Engine| {
            json!({"doc":engine.doc,"clients":engine.mcp_clients,
            "undo":engine.undo.len(),"redo":engine.redo.len(),"saved":engine.saved_revision,
            "focus":engine.focus_requested,"activity":engine.activity,"status":engine.status})
        };
        let before = snapshot(&shared.lock().unwrap());
        let engine = shared.clone();
        let worker = mock(&state, move |request, body| {
            assert_eq!(body["method"], "mcp");
            let ping = &body["params"]["request"];
            assert_eq!(ping["method"], "ping");
            assert_eq!(ping["_client"], ping["id"]);
            assert!(ping["_client"]
                .as_str()
                .unwrap()
                .starts_with("discovery-probe-"));
            let result = mcp(&engine, ping);
            request
                .respond(tiny_http::Response::from_string(
                    json!({"ok":true,"result":result}).to_string(),
                ))
                .unwrap();
        });
        #[cfg(unix)]
        {
            fs::rename(
                state.0.join("connection.json"),
                state.0.join("linked-connection.json"),
            )
            .unwrap();
            std::os::unix::fs::symlink("linked-connection.json", state.0.join("connection.json"))
                .unwrap();
        }
        let metadata = fs::read(state.0.join("connection.json")).unwrap();
        check(probe_connection(&state.0), ConnectionCategory::Ready);
        worker.join().unwrap();
        assert_eq!(snapshot(&shared.lock().unwrap()), before);
        assert_eq!(fs::read(state.0.join("connection.json")).unwrap(), metadata);
        #[cfg(unix)]
        assert!(fs::symlink_metadata(state.0.join("connection.json"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn probe_authentication_and_bad_replies_have_fixed_categories() {
        use ConnectionCategory::*;
        for (status, body, category) in [
            (401, CANARY, AuthenticationRejected),
            (500, CANARY, ResponseRejected),
            (200, CANARY, ResponseInvalid),
            (200, "{}", ResponseInvalid),
            (
                200,
                r#"{"ok":true,"result":{"jsonrpc":"2.0","id":"wrong","result":{}}}"#,
                ResponseInvalid,
            ),
        ] {
            let state = State::new();
            let worker = mock(&state, move |request, _| {
                request
                    .respond(tiny_http::Response::from_string(body).with_status_code(status))
                    .unwrap();
            });
            check(probe_connection(&state.0), category);
            worker.join().unwrap();
        }
        for field in ["jsonrpc", "result", "error"] {
            let state = State::new();
            let worker = mock(&state, move |request, body| {
                let mut result =
                    json!({"jsonrpc":"2.0","id":body["params"]["request"]["id"],"result":{}});
                result[field] = json!(CANARY);
                request
                    .respond(tiny_http::Response::from_string(
                        json!({"ok":true,"result":result}).to_string(),
                    ))
                    .unwrap();
            });
            check(probe_connection(&state.0), ResponseInvalid);
            worker.join().unwrap();
        }
    }

    #[test]
    fn local_redirects_are_rejected_without_following_them() {
        let state = State::new();
        let trap = TcpListener::bind("127.0.0.1:0").unwrap();
        trap.set_nonblocking(true).unwrap();
        let target = format!(
            "http://127.0.0.1:{}/{CANARY}",
            trap.local_addr().unwrap().port()
        );
        let worker = mock(&state, move |request, _| {
            let redirect = tiny_http::Response::empty(302)
                .with_header(tiny_http::Header::from_bytes("Location", target).unwrap());
            request.respond(redirect).unwrap();
        });
        check(
            probe_connection(&state.0),
            ConnectionCategory::ResponseRejected,
        );
        worker.join().unwrap();
        assert_eq!(
            trap.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn public_client_preserves_actual_engine_and_mcp_errors() {
        let message = "Project has unsaved changes; save or explicitly discard them";
        for endpoint in ["rpc", "mcp"] {
            let state = State::new();
            let worker = mock(&state, move |request, _| {
                request
                    .respond(tiny_http::Response::from_string(
                        json!({"ok":false,"error":message}).to_string(),
                    ))
                    .unwrap();
            });
            let result = client(&state.0, endpoint, &json!({"method":"document"}));
            if endpoint == "mcp" {
                assert_eq!(result.unwrap_err(), message);
            } else {
                assert_eq!(result.unwrap(), json!({"ok":false,"error":message}));
            }
            worker.join().unwrap();
        }
        let state = State::new();
        let tool_error = json!({"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":message}]}});
        let expected = tool_error.clone();
        let worker = mock(&state, move |request, _| {
            request
                .respond(tiny_http::Response::from_string(
                    json!({"ok":true,"result":tool_error}).to_string(),
                ))
                .unwrap();
        });
        assert_eq!(
            client(&state.0, "mcp", &json!({"id":1,"method":"tools/call"})).unwrap(),
            expected
        );
        worker.join().unwrap();
    }

    #[test]
    fn probe_times_out_with_fixed_recovery_instead_of_hanging() {
        let state = State::new();
        let worker = mock(&state, |request, _| {
            thread::sleep(Duration::from_millis(1250));
            let _ = request.respond(tiny_http::Response::from_string(CANARY));
        });
        let started = std::time::Instant::now();
        check(
            probe_connection(&state.0),
            ConnectionCategory::ConnectionInterrupted,
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        worker.join().unwrap();
    }

    #[test]
    fn probe_response_budget_rejects_oversize_without_limiting_normal_client_replies() {
        let state = State::new();
        let worker = mock(&state, |request, body| {
            let response = json!({"ok":true,"result":{"jsonrpc":"2.0",
                "id":body["params"]["request"]["id"],"result":{}}})
            .to_string()
                + " ".repeat(64 * 1024).as_str();
            let _ = request.respond(tiny_http::Response::from_string(response));
        });
        check(
            probe_connection(&state.0),
            ConnectionCategory::ResponseInvalid,
        );
        worker.join().unwrap();
        let worker = mock(&state, |request, _| {
            let result = json!({"ok":true,"result":{"large":CANARY.repeat(3000)}});
            request
                .respond(tiny_http::Response::from_string(result.to_string()))
                .unwrap();
        });
        let result = client(&state.0, "mcp", &json!({"method":"tools/call"})).unwrap();
        assert_eq!(result["large"], CANARY.repeat(3000));
        worker.join().unwrap();
    }

    #[test]
    fn interrupted_response_body_does_not_expose_content_or_claim_invalid_json() {
        let state = State::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        state.write(&format!(
            "http://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        ));
        let worker = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "local interruption request"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("local interruption listener failed: {error}"),
                }
            };
            // BSD accepts inherit the listener's mode; reads use the timeout below.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{{\"{CANARY}").as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        check(
            probe_connection(&state.0),
            ConnectionCategory::ConnectionInterrupted,
        );
        worker.join().unwrap();
    }
}
