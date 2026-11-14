//! Qualify native Photoshop and image inputs with their held file and source session.
use super::*;
use crate::{
    engine::{
        authority::NativeSeed,
        native::{NativeDocument, StagedOutput},
    },
    file_ownership::Ownership,
    file_safety::{canonical_destination, native_source_version, open_native_read, Version},
    image_import::{native::OpenFormat, prepared_png::apng},
    large_document::{verified_transfer_buffer, FreshFragment, ResidentBudget, ScratchBytes},
    loading::Control,
    psd::{import36, memory36::MetadataCharge},
    workspace::{
        native_registration::Prepared,
        source_runtime::{SourceRuntime, SourceSession},
        Registry,
    },
};

const COMPOSITE_MISMATCH: &str = "Native render differs from the saved Photoshop composite";

#[derive(Clone, Copy)]
enum Kind {
    Photoshop,
    ConventionalPhotoshop,
    Image(OpenFormat),
}

enum Staging {
    Photoshop(FreshFragment),
    Image(StagedOutput),
}
impl Staging {
    fn mark_published(&self) {
        match self {
            Self::Photoshop(fragment) => fragment.mark_published(),
            Self::Image(output) => output.mark_source_published(),
        }
    }

    fn refuse(self, session: &SourceSession, reason: String) -> String {
        let cleanup = match self {
            Self::Photoshop(fragment) => {
                session.abort_fragment(fragment, &mut || session.cleanup_check())
            }
            Self::Image(output) => output.abort(session, &|| session.cleanup_check()),
        };
        match cleanup {
            Ok(()) => reason,
            Err(cleanup) => format!("{reason}; native open staging retained: {cleanup}"),
        }
    }
}

pub(super) fn compatible_copy(
    shared: &Shared,
    seed: NativeSeed,
    actor: &str,
    convert_to_srgb: bool,
) -> Result<(), String> {
    let _operation = {
        let engine = shared.lock().unwrap();
        engine.ensure_write_available()?;
        seed.validate(&engine)?;
        if engine.file_operations.load(Ordering::Acquire) != 0 {
            return Err("Finish the active file operation before creating a copy".into());
        }
        FileOperation::track(engine.file_operations.clone())
    };
    let base = seed.document();
    let meta = base.metadata();
    let layer = base.protected_composite()?;
    let profile = meta.profile();
    if profile.is_some() && !convert_to_srgb {
        return Err("An embedded profile requires explicit convert_to_srgb=true to create an editable copy".into());
    }
    let source = base.source(layer.pixels().slot())?;
    let runtime = crate::workspace::native_dispatch::runtime_for(shared)?;
    let session = runtime.begin_foreground()?;
    let check = || session.check();
    let staged = profile
        .filter(|profile| !crate::color_profile::is_srgb_profile(profile))
        .map(|profile| {
            crate::retained::transaction::stage(base, &session, &check, |store, guard| {
                crate::image_import::native::convert(source, Some(profile), store, &mut || guard())
            })
        })
        .transpose()?;
    let result = (|| {
        let surface = staged.as_ref().map_or(source, StagedOutput::surface);
        let name = compatible_copy_name(meta.name(), meta.depth(), profile.is_some(), "psb");
        let document = NativeDocument::new_compatible_copy(
            surface.clone(),
            &name,
            &format!("Flattened {}-bit artwork", meta.depth()),
            profile.is_some() || meta.srgb_tagged(),
            meta.psd_resources(),
            meta.share_psd_file_info()?,
            &session.resident(),
            &session.disk(),
            &check,
        )?;
        document.validate_editable(&check)?;
        let mut prepared = Engine::from_native(document, 40)?;
        prepared.project_id = seed.project_id().to_owned();
        prepared.saved_revision = u64::MAX;
        prepared.status = "Editing a flattened copy at original precision".into();
        check()?;
        {
            let mut engine = shared.lock().unwrap();
            let workspace = engine.workspace.as_ref().and_then(std::sync::Weak::upgrade)
                .ok_or("Project lost its source workspace; current canvas preserved")?;
            session.with_live_fence(|| -> Result<(), String> {
                let workspace = workspace.try_lock()
                    .map_err(|_| "Workspace is busy; current canvas preserved")?;
                if workspace.exiting
                    || !workspace.source_runtime.as_ref().is_some_and(|runtime| session.uses_runtime(runtime))
                    || engine.file_operations.load(Ordering::Acquire) != 1
                {
                    return Err("Source workspace or file operation changed; current canvas preserved".into());
                }
                seed.validate(&engine)?;
                engine.replace_prepared(&mut prepared, actor, false, None)?;
                if let Some(staged) = &staged {
                    staged.mark_source_published();
                }
                Ok(())
            })??;
        }
        prepared.release_file_ownership();
        Ok(())
    })();
    match (result, staged) {
        (Err(reason), Some(staged)) => Err(Staging::Image(staged).refuse(&session, reason)),
        (result, _) => result,
    }
}

struct Input {
    file: fs::File,
    ownership: Arc<Ownership>,
    scratch: ScratchBytes,
    version: Version,
    charge: Arc<MetadataCharge>,
    kind: Kind,
}

impl Input {
    fn held(&self) -> Result<(fs::File, [u8; 32]), String> {
        Ok((
            self.file.try_clone().map_err(|e| e.to_string())?,
            self.version.sha256(),
        ))
    }

    fn hold(
        path: PathBuf,
        mut file: fs::File,
        project: &str,
        pool: &ResidentBudget,
        kind: Kind,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        if let Kind::Image(format) = kind {
            if file.metadata().map_err(|e| e.to_string())?.len() > format.encoded_limit() {
                return Err("Image source bytes exceed admission".into());
            }
        }
        let header_bytes = path
            .as_os_str()
            .len()
            .checked_mul(16)
            .and_then(|n| n.checked_add(64 * 1024))
            .ok_or("Native open header admission overflow")?;
        let charge = MetadataCharge::reserve(pool, header_bytes, check)?;
        let ownership = Ownership::claim_native(path, project, charge.clone(), &file)?;
        if matches!(kind, Kind::Image(OpenFormat::Webp)) {
            let length = file.metadata().map_err(|e| e.to_string())?.len();
            crate::image_import::webp::preflight_large(&mut file, length, true, check)?;
        }
        let mut scratch = ScratchBytes::new(pool, 65536, 65536).map_err(|e| e.to_string())?;
        let version = Self::read_version(&ownership.path, &mut file, pool, &mut scratch, check)?;
        Ok(Self {
            file,
            ownership,
            scratch,
            version,
            charge,
            kind,
        })
    }

    fn decode(
        &mut self,
        project: &str,
        session: &SourceSession,
        options: &Value,
        control: &Control,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(NativeDocument, Staging), String> {
        let pool = session.resident();
        let name = self
            .ownership
            .path
            .file_name()
            .ok_or("Native open filename missing")?
            .to_string_lossy();
        match self.kind {
            Kind::Photoshop => {
                control.progress("Reading native Photoshop structure", 0, 0)?;
                let import36::PreparedOpen { document, fragment } = import36::open(
                    &mut self.file,
                    self.version,
                    session,
                    project.to_owned(),
                    &name,
                    control,
                    check,
                )?;
                Ok((document, Staging::Photoshop(fragment)))
            }
            Kind::Image(format) => {
                control.progress("Decoding native image rows", 0, 0)?;
                let base = NativeDocument::new_blank(1, 1, 8, &pool, &session.disk(), check)?;
                let (output, definition) = format.prepare(
                    &base,
                    session,
                    &self.ownership.path,
                    self.held()?,
                    options,
                    &|| control.check(),
                )?;
                let layer = self
                    .ownership
                    .path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy();
                let document = NativeDocument::new_image(
                    output.surface().clone(),
                    &name,
                    &layer,
                    definition,
                    &pool,
                    &session.disk(),
                    check,
                );
                let staged = Staging::Image(output);
                match document {
                    Ok(document) => Ok((document, staged)),
                    Err(reason) => Err(staged.refuse(session, reason)),
                }
            }
            Kind::ConventionalPhotoshop => {
                Err("Conventional Photoshop uses the compatibility decoder".into())
            }
        }
    }

    fn conventional_engine(
        &mut self,
        project: &str,
        session: &SourceSession,
        control: &Control,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(Engine, Option<Staging>), String> {
        let name = self
            .ownership
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let reason = match
            self.qualify_conventional(session, check, |input, fragment| {
                let mut decoded = psd::decode_backed(
                    &mut input.file,
                    control,
                    input.version,
                    fragment.store().clone(),
                    check,
                )?;
                decoded.document.name = name.clone();
                control.progress("Qualifying native Photoshop backing", 0, 0)?;
                let stage = |decoded: &psd::DecodedBacked| {
                    engine::scene::stage_decoded(
                        &decoded.document, Some(decoded), fragment, session, check,
                    )
                };
                let native = stage(&decoded)?;
                let proof = decoded.composite.clone();
                // Release decode working memory before rendering when there
                // is no alternative to try.
                let multiply = psd::has_photoshop_multiply_alternate(&decoded.document);
                let gradient = psd::has_integer_gradient_alternate(&decoded.document);
                let retry = (multiply || gradient).then_some(decoded);
                if let Some(engine) = input.qualify_candidate(project, native, &proof, session, control, check)? {
                    return Ok(engine);
                }
                if let Some(mut decoded) = retry {
                    check()?;
                    // Only the interpretation changed. Keep the same admitted
                    // raster and mask surfaces from the first qualification.
                    if multiply {
                        psd::set_photoshop_multiply(&mut decoded.document, true);
                        control.progress("Checking Photoshop RGB8 Multiply rounding", 0, 0)?;
                        let native = stage(&decoded)?;
                        if let Some(engine) = input.qualify_candidate(project, native, &proof, session, control, check)? {
                            return Ok(engine);
                        }
                        psd::set_photoshop_multiply(&mut decoded.document, false);
                    }
                    if gradient {
                        check()?;
                        psd::use_integer_gradient_grid(&mut decoded.document);
                        control.progress("Checking alternate Photoshop gradient placement", 0, 0)?;
                        let native = stage(&decoded)?;
                        drop(decoded);
                        if let Some(engine) = input.qualify_candidate(project, native, &proof, session, control, check)? {
                            return Ok(engine);
                        }
                    }
                }
                Err(COMPOSITE_MISMATCH.into())
            })?
        {
            Ok((engine, staged)) => return Ok((engine, Some(staged))),
            Err(reason) => reason,
        };

        if self.version.bytes > crate::file_safety::RESIDENT_FILE_BYTES {
            return self.protected_engine(project, &name, &reason, session, control, check);
        }
        let (mut document, proof) = match psd::decode_held_reader(&mut self.file, control, self.version) {
            Ok(decoded) => decoded,
            Err(_) => {
                check()?;
                return self.protected_engine(project, &name, &reason, session, control, check);
            }
        };
        document.name = name;
        let mut prepared = Engine::new();
        prepared.replace(document, None)?;
        self.set_file_state(&mut prepared, project);
        let Some(proof) = proof else {
            return Ok((prepared, None));
        };
        control.progress("Qualifying native Photoshop backing", 0, 0)?;
        if let Ok((engine, staged)) =
            self.qualify_conventional(session, check, |input, fragment| {
                let native = engine::scene::stage_decoded(
                    prepared.resident()?,
                    None,
                    fragment,
                    session,
                    check,
                )?;
                input.qualify_candidate(project, native, &proof, session, control, check)?
                    .ok_or_else(|| COMPOSITE_MISMATCH.into())
            })?
        {
            return Ok((engine, Some(staged)));
        }
        Ok((prepared, None))
    }

    fn protected_engine(
        &mut self,
        project: &str,
        name: &str,
        reason: &str,
        session: &SourceSession,
        control: &Control,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(Engine, Option<Staging>), String> {
        check()?;
        let fragment = session.fresh_fragment()?;
        let prepared: Result<Engine, String> = (|| {
            let preview = psd::decode_protected(
                &mut self.file,
                self.version,
                session,
                fragment.store().clone(),
                control,
                check,
            )?;
            let mut reason = preview.notice.unwrap_or(reason).to_owned();
            for warning in &preview.resource_warnings {
                if &reason != warning {
                    reason.push_str(". ");
                    reason.push_str(warning);
                }
            }
            let document = NativeDocument::new_protected_preview(
                preview.surface,
                name,
                &reason,
                preview.profile,
                preview.srgb_tagged,
                preview.document_resources,
                preview.file_info,
                &session.resident(),
                &session.disk(),
                check,
            )?.with_protected_structure(preview.structure)?;
            let engine = self.engine(project, document)?;
            let scene = engine.capture_scene(None)?.into_native()?;
            engine::scene::seal_decoded(&scene, session, check)?;
            Ok(engine)
        })();
        let staged = Staging::Photoshop(fragment);
        match prepared {
            Ok(engine) => Ok((engine, Some(staged))),
            Err(preview) => Err(staged.refuse(session, format!("{reason}; preview unavailable: {preview}"))),
        }
    }

    fn qualify_candidate(
        &mut self,
        project: &str,
        native: NativeDocument,
        proof: &psd::ConventionalComposite,
        session: &SourceSession,
        control: &Control,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<Engine>, String> {
        let engine = self.engine(project, native)?;
        let scene = engine.capture_scene(None)?.into_native()?;
        if !proof.matches_native(&mut self.file, &scene, session, control, check)? {
            return Ok(None);
        }
        engine::scene::seal_decoded(&scene, session, check)?;
        Ok(Some(engine))
    }

    fn qualify_conventional(
        &mut self,
        session: &SourceSession,
        check: &dyn Fn() -> Result<(), String>,
        build: impl FnOnce(&mut Self, &FreshFragment) -> Result<Engine, String>,
    ) -> Result<Result<(Engine, Staging), String>, String> {
        check()?;
        let fragment = match session.fresh_fragment() {
            Ok(fragment) => fragment,
            Err(reason) => {
                check()?;
                return Ok(Err(reason));
            }
        };
        let reason = match build(self, &fragment) {
            Ok(native) => return Ok(Ok((native, Staging::Photoshop(fragment)))),
            Err(reason) => reason,
        };
        if let Err(cleanup) = session.abort_fragment(fragment, &mut || session.cleanup_check()) {
            return Err(format!("{reason}; native open staging retained: {cleanup}"));
        }
        check()?;
        Ok(Err(reason))
    }

    fn prepare_engine(
        &mut self,
        project: &str,
        session: &SourceSession,
        options: &Value,
        control: &Control,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(Engine, Option<Staging>), String> {
        if matches!(self.kind, Kind::ConventionalPhotoshop) {
            return self.conventional_engine(project, session, control, check);
        }
        let (document, staged) = match self.decode(project, session, options, control, check) {
            Err(reason) if matches!(self.kind, Kind::Photoshop)
                && matches!(reason.as_str(), psd::SOURCE_WORKING_PROFILE_MISMATCH
                    | psd::file_info::SOURCE_MISMATCH | psd::standard36::LAYER_INFO_MISMATCH) =>
            {
                let name = self.ownership.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                return self.protected_engine(project, &name, &reason, session, control, check);
            }
            result => result?,
        };
        match self.engine(project, document) {
            Ok(engine) => Ok((engine, Some(staged))),
            Err(reason) => Err(staged.refuse(session, reason)),
        }
    }

    fn engine(&self, project: &str, document: NativeDocument) -> Result<Engine, String> {
        let mut engine = Engine::from_native(document, 40)?;
        self.set_file_state(&mut engine, project);
        Ok(engine)
    }

    fn set_file_state(&self, engine: &mut Engine, project: &str) {
        engine.project_id = project.to_owned();
        match self.kind {
            Kind::Photoshop | Kind::ConventionalPhotoshop => {
                engine.saved_revision = view::current(engine).revision();
                engine.path = Some(self.ownership.path.clone());
                engine.file_version = Some(self.version);
                engine.status = if view::current(engine).read_only() {
                    "Protected Photoshop composite opened".into()
                } else if engine.native_document().is_some() {
                    "Native Photoshop document opened".into()
                } else {
                    "Photoshop document opened in compatibility mode".into()
                };
            }
            Kind::Image(_) => {
                engine.saved_revision = u64::MAX;
                engine.status = "Image opened — save as PSD or PSB".into();
            }
        }
        engine.file_ownership = Some(self.ownership.clone());
        engine.file_state_charge = Some(self.charge.clone());
    }

    fn read_version(
        path: &Path,
        file: &mut fs::File,
        pool: &ResidentBudget,
        scratch: &mut ScratchBytes,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Version, String> {
        let bytes = file.metadata().map_err(|e| e.to_string())?.len();
        let mut transfer = if bytes > scratch.len() as u64 {
            verified_transfer_buffer(pool, bytes).ok()
        } else {
            None
        };
        native_source_version(
            path,
            file,
            transfer.as_deref_mut().unwrap_or(scratch),
            check,
        )
    }

    fn recheck(
        &mut self,
        pool: &ResidentBudget,
        control: &Control,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        control.progress("Checking the source file before opening", 0, 0)?;
        if Self::read_version(
            &self.ownership.path,
            &mut self.file,
            pool,
            &mut self.scratch,
            check,
        )? != self.version
        {
            return Err("File changed during decoding; current work preserved".into());
        }
        Ok(())
    }
}

fn check_choices(kind: Kind, options: &Value) -> Result<(), String> {
    let accepted = match kind {
        Kind::Photoshop | Kind::ConventionalPhotoshop => None,
        Kind::Image(format) => format.choice_key(),
    };
    if ["frame", "page", "variant"]
        .iter()
        .any(|&key| Some(key) != accepted && options.get(key).is_some())
    {
        return Err(accepted.map_or_else(
            || "This file format does not accept frame, page or variant choices".into(),
            |key| format!("This image format accepts only {key} selection"),
        ));
    }
    Ok(())
}

fn prepare_reply(
    engine: &mut Engine,
    session: &Arc<SourceSession>,
    admission: Option<&mut reply::ReplyAdmission>,
) -> Result<Option<Value>, String> {
    let Some(admission) = admission else {
        return Ok(None);
    };
    let Some(capture) = observation::capture_engine(engine, admission)? else {
        let state = engine.capture_state()?.project()?;
        if admission.pool_option().is_some() {
            admission.admit_projection(&session.resident(), state.as_ref())?;
        }
        return Ok(Some(state.as_ref().clone()));
    };
    let mut observed =
        capture.observe_admitted(session.clone().into(), &json!({"image":false}), admission)?;
    Ok(Some(observed["state"].take()))
}

fn candidate(
    path: &Path,
    file: &mut fs::File,
    runtime: &SourceRuntime,
    control: &Control,
) -> Result<Option<Kind>, String> {
    control.check()?;
    let mut prefix = [0; 16];
    let file_bytes = file.metadata().map_err(|error| error.to_string())?.len();
    let length = file_bytes.min(prefix.len() as u64) as usize;
    let prefix = &mut prefix[..length];
    file.read_exact(prefix).map_err(|error| error.to_string())?;
    if !prefix.starts_with(b"8BPS") {
        return Ok(OpenFormat::from_path(path)
            .filter(|format| file_bytes <= format.encoded_limit() && format.recognizes(prefix))
            .map(Kind::Image));
    }
    // One bounded zlib decoder and prefix, released before the real import.
    let _probe = MetadataCharge::reserve(&runtime.resident(), 256 * 1024, &|| control.check())?;
    psd::native_manifest_candidate(file, || control.check()).map(|native| {
        Some(if native {
            Kind::Photoshop
        } else {
            Kind::ConventionalPhotoshop
        })
    })
}

pub(super) fn project(
    workspace: &Registry,
    path: &Path,
    control: &Control,
    options: &Value,
    activate_if: Option<&str>,
    mut admission: Option<&mut reply::ReplyAdmission>,
) -> Result<Option<(Shared, Option<Value>)>, String> {
    let Some(runtime) = workspace_runtime(workspace)? else {
        return Ok(None);
    };
    let path = canonical_destination(path)?;
    let mut file = open_native_read(&path).map_err(|error| error.to_string())?;
    let Some(kind) = candidate(&path, &mut file, &runtime, control)? else {
        return Ok(None);
    };
    check_choices(kind, options)?;
    if !matches!(kind, Kind::ConventionalPhotoshop) {
        if let Some(admission) = admission.as_deref_mut() {
            admission.admit(&runtime.resident(), 0)?;
        }
    }
    ProjectOpen::hold(runtime, path, file, kind, control)?
        .finish(workspace, control, options, activate_if, admission)
        .map(Some)
}

fn workspace_runtime(workspace: &Registry) -> Result<Option<Arc<SourceRuntime>>, String> {
    let workspace = workspace.lock().unwrap();
    if workspace.exiting {
        return Err("Workspace is exiting; current projects preserved".into());
    }
    Ok(workspace.source_runtime.clone())
}

struct ProjectOpen {
    input: Input,
    project: String,
    runtime: Arc<SourceRuntime>,
}
impl ProjectOpen {
    fn hold(
        runtime: Arc<SourceRuntime>,
        path: PathBuf,
        file: fs::File,
        kind: Kind,
        control: &Control,
    ) -> Result<Self, String> {
        control.progress("Checking native file source", 0, 0)?;
        let project = engine::id();
        let input = Input::hold(path, file, &project, &runtime.resident(), kind, &|| {
            control.check()
        })?;
        Ok(Self {
            input,
            project,
            runtime,
        })
    }

    fn finish(
        mut self,
        workspace: &Registry,
        control: &Control,
        options: &Value,
        activate_if: Option<&str>,
        admission: Option<&mut reply::ReplyAdmission>,
    ) -> Result<(Shared, Option<Value>), String> {
        if !workspace_runtime(workspace)?
            .is_some_and(|current| Arc::ptr_eq(&current, &self.runtime))
        {
            return Err("Native source service changed; current projects preserved".into());
        }
        control.begin()?;
        let session = Arc::new(self.runtime.begin_foreground()?);
        let check = || {
            control.check()?;
            session.check()
        };
        let (mut engine, staged) =
            self.input
                .prepare_engine(&self.project, &session, options, control, &check)?;
        let result = (|| {
            let reply = prepare_reply(&mut engine, &session, admission)?;
            let mut prepared =
                Prepared::from_qualified_engine(workspace, &self.runtime, engine, control)?;
            self.input.recheck(&session.resident(), control, &check)?;
            session.with_live_fence(|| {
                prepared.publish_with(workspace, control, activate_if, || {
                    if let Some(staged) = &staged {
                        staged.mark_published();
                    }
                })
            })??;
            Ok((prepared.shared, reply))
        })();
        result.map_err(|reason| match staged {
            Some(staged) => staged.refuse(&session, reason),
            None => reason,
        })
    }
}

pub(crate) enum ImageOpen {
    Other,
    Opened,
    Choose(ImageChoice),
}
pub(crate) struct ImageChoice {
    open: ProjectOpen,
    pub(crate) key: &'static str,
    pub(crate) count: usize,
    pub(crate) sizes: Vec<[u16; 2]>,
}
impl ImageChoice {
    pub(crate) fn path(&self) -> &Path {
        &self.open.input.ownership.path
    }
    pub(crate) fn finish(
        self,
        workspace: &Registry,
        control: &Control,
        initiating: &str,
        index: usize,
    ) -> Result<(), String> {
        if index >= self.count {
            return Err("Image selection index is outside the available items".into());
        }
        self.open.finish(
            workspace,
            control,
            &json!({(self.key):index}),
            Some(initiating),
            None,
        )?;
        Ok(())
    }
}

pub(super) fn begin_image_open(
    workspace: &Registry,
    path: &Path,
    control: &Control,
    initiating: &str,
) -> Result<ImageOpen, String> {
    if OpenFormat::from_path(path).is_none() {
        return Ok(ImageOpen::Other);
    }
    let Some(runtime) = workspace_runtime(workspace)? else {
        return Ok(ImageOpen::Other);
    };
    let path = canonical_destination(path)?;
    let mut file = open_native_read(&path).map_err(|error| error.to_string())?;
    let Some(kind @ Kind::Image(format)) = candidate(&path, &mut file, &runtime, control)? else {
        return Ok(ImageOpen::Other);
    };
    let mut open = ProjectOpen::hold(runtime, path, file, kind, control)?;
    if let Some(key) = format.choice_key() {
        let choices = if matches!(format, OpenFormat::Png) {
            apng::open_frame_count(
                &mut open.input.file,
                open.input.version,
                &open.runtime.resident(),
                &mut || control.check(),
            )?
            .map(|frames| (frames as usize, Vec::new()))
        } else {
            let session = open.runtime.begin_foreground()?;
            let (count, sizes) = format.choices(
                &session,
                &open.input.ownership.path,
                open.input.held()?,
                &|| control.check(),
            )?;
            (count > 1).then_some((count, sizes))
        };
        if let Some((count, sizes)) = choices {
            crate::file_safety::check_native_source_binding(
                &open.input.ownership.path,
                &open.input.file,
                open.input.version,
            )?;
            control.check()?;
            return Ok(ImageOpen::Choose(ImageChoice { open, key, count, sizes }));
        }
    }
    open.finish(workspace, control, &Value::Null, Some(initiating), None)?;
    Ok(ImageOpen::Opened)
}

pub(super) fn replace(
    shared: &Shared,
    seed: NativeSeed,
    path: &Path,
    control: &Control,
    options: &Value,
    admission: Option<&mut reply::ReplyAdmission>,
) -> Result<Option<Value>, String> {
    let discard = options["discard"] == true;
    let actor = options["actor"].as_str().unwrap_or("agent");
    let _operation = {
        let engine = shared.lock().unwrap();
        engine.ensure_write_available()?;
        seed.validate(&engine)?;
        if engine.saved_revision != seed.document().metadata().revision() && !discard {
            return Err("Unsaved work: save first or explicitly pass discard=true".into());
        }
        if engine.file_operations.load(Ordering::Acquire) != 0 {
            return Err("Finish the active file operation before opening a canvas".into());
        }
        FileOperation::track(engine.file_operations.clone())
    };
    let runtime = crate::workspace::native_dispatch::runtime_for(shared)?;
    let path = canonical_destination(path)?;
    let mut file = open_native_read(&path).map_err(|error| error.to_string())?;
    let kind = candidate(&path, &mut file, &runtime, control)?
        .ok_or("Native Open requires a supported raster image or PeerBrush native PSD/PSB file")?;
    check_choices(kind, options)?;
    let session = Arc::new(runtime.begin_foreground()?);
    let check = || {
        control.check()?;
        session.check()
    };
    control.progress("Checking native file source", 0, 0)?;
    let mut input = Input::hold(
        path,
        file,
        seed.project_id(),
        &runtime.resident(),
        kind,
        &check,
    )?;
    let (mut prepared, staged) =
        input.prepare_engine(seed.project_id(), &session, options, control, &check)?;
    let result = (|| {
        let reply = prepare_reply(&mut prepared, &session, admission)?;
        input.recheck(&session.resident(), control, &check)?;
        let mut engine = shared.lock().unwrap();
        let workspace = engine
            .workspace
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or("Project lost its source workspace; current canvas preserved")?;
        session.with_live_fence(|| {
            let workspace = workspace
                .try_lock()
                .map_err(|_| "Workspace is busy; current canvas preserved")?;
            if workspace.exiting
                || !workspace
                    .source_runtime
                    .as_ref()
                    .is_some_and(|current| session.uses_runtime(current))
                || engine.file_operations.load(Ordering::Acquire) != 1
            {
                return Err(
                    "Source workspace or file operation changed; current canvas preserved".into(),
                );
            }
            seed.validate(&engine)?;
            engine.replace_prepared(&mut prepared, actor, discard, Some(control))?;
            if let Some(staged) = &staged {
                staged.mark_published();
            }
            Ok(reply)
        })?
    })();
    match result {
        Ok(reply) => {
            prepared.release_file_ownership();
            drop(seed);
            drop(prepared);
            Ok(reply)
        }
        Err(reason) => {
            drop(prepared);
            match staged {
                Some(staged) => Err(staged.refuse(&session, reason)),
                None => Err(reason),
            }
        }
    }
}
