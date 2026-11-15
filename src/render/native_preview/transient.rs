//! An unpublished preview binds its committed baseline and exact candidate tag.
//! Rendering and provisional retirement complete before display owners escape.
use super::*;
use crate::{
    engine::{native::worker::PreparedNative, scene::Source36},
    source::text_position::NativeCarets,
    workspace::{native_dispatch::try_source_engine, source_runtime::SourceSession},
};

pub(crate) struct Origin {
    base: DocumentKey,
    identity: SceneIdentity,
    candidate: Tag,
    presentation: Option<[u8; 32]>,
    _admission: Reservation,
}
impl Origin {
    pub(super) fn blended(base: &Tag, candidate: Tag, signature: [u8; 32]) -> Result<Arc<Self>> {
        let a = base.key();
        let b = candidate.key();
        if a.identity != b.identity
            || a.rect != b.rect
            || a.width != b.width
            || a.height != b.height
            || a.target != b.target
            || a.mask != b.mask
            || a.selection_view != b.selection_view
            || b.source == a.source
            || !candidate.0.pool.same_pool(&base.0.pool)
        {
            return Err("Canvas fade provenance differs from its committed frame".into());
        }
        let bytes = a
            .identity
            .project_id
            .len()
            .checked_add(a.identity.document_id.len())
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>() + 256))
            .ok_or("Canvas fade origin size overflow")?;
        let admission = base
            .0
            .pool
            .reserve_bytes(bytes, 64 * 1024)
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            base: base.key().source,
            identity: base.key().identity.clone(),
            candidate,
            presentation: Some(signature),
            _admission: admission,
        }))
    }
    pub(super) fn presentation(
        base: &SceneSnapshot,
        request: &Request,
        signature: [u8; 32],
    ) -> Result<Arc<Self>> {
        if request.scene.read_source_key() != base.key()
            || request.scene.identity() != base.identity()
        {
            return Err("Animation provenance differs from its source".into());
        }
        let bytes = base
            .identity()
            .project_id
            .len()
            .checked_add(base.identity().document_id.len())
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>() + 256))
            .ok_or("Animation origin overflow")?;
        let admission = base
            .budget()
            .reserve_bytes(bytes, 64 * 1024)
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            base: base.key(),
            identity: base.identity().clone(),
            candidate: request.tag(),
            presentation: Some(signature),
            _admission: admission,
        }))
    }
    pub(crate) fn presentation_signature(&self) -> Option<[u8; 32]> {
        self.presentation
    }
    pub(crate) fn matches_source(&self, project: &str, key: DocumentKey, revision: u64) -> bool {
        self.identity.project_id == project
            && self.base == key
            && self.identity.revision == revision
    }
    pub(crate) fn candidate_tag(&self) -> &Tag {
        &self.candidate
    }
    pub(crate) fn matches_frame(&self, frame: &Frame) -> bool {
        frame.matches(&self.candidate)
    }
}
/// Source-editor handles retain only actual shared source metadata. No candidate
/// document, raster, source session or filesystem capability escapes with it.
pub(crate) struct SourceSupplement {
    source: Source36,
    target: String,
    layer_origin: [i32; 2],
    origin: Arc<Origin>,
    _admission: Reservation,
}
impl SourceSupplement {
    pub(crate) fn source(&self) -> &Source36 {
        &self.source
    }
    pub(crate) fn target(&self) -> &str {
        &self.target
    }
    pub(crate) fn layer_origin(&self) -> [i32; 2] {
        self.layer_origin
    }
    pub(crate) fn origin(&self) -> &Origin {
        &self.origin
    }
    fn capture(
        request: &Request,
        origin: Arc<Origin>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Option<Self>> {
        if request.key().mask {
            return Ok(None);
        }
        let Some(target) = request.key().target.as_deref() else {
            return Ok(None);
        };
        let layer = request
            .scene
            .metadata()
            .layers()
            .find(|layer| layer.id() == target)
            .ok_or("Preview source target disappeared from its candidate")?;
        let Some(source) = layer.source() else {
            return Ok(None);
        };
        if !source.content.uses_budget(request.scene.budget()) {
            return Err("Preview source definition has no captured physical admission".into());
        }
        let bytes = target
            .len()
            .checked_add(std::mem::size_of::<Self>() + 256)
            .ok_or("Preview source metadata footprint overflow")?;
        let admission = request
            .scene
            .budget()
            .reserve_bytes(bytes, 64 * 1024)
            .map_err(|e| e.to_string())?;
        check()?;
        Ok(Some(Self {
            source: source.copy_runtime(request.scene.budget(), check)?,
            target: target.into(),
            layer_origin: layer.origin(),
            origin,
            _admission: admission,
        }))
    }
}
pub(crate) struct TransientFrame<T: DisplayOutput = Frame> {
    origin: Arc<Origin>,
    frame: T,
    source: Option<SourceSupplement>,
}
impl<T: DisplayOutput> TransientFrame<T> {
    pub(crate) fn origin(&self) -> &Origin {
        &self.origin
    }
    pub(crate) fn frame(&self) -> &T {
        &self.frame
    }
    pub(crate) fn source(&self) -> Option<&SourceSupplement> {
        self.source.as_ref()
    }
    pub(crate) fn into_parts(self) -> (Arc<Origin>, T, Option<SourceSupplement>) {
        (self.origin, self.frame, self.source)
    }
}

/// Prepare the same commands used by a committed edit, render their candidate,
/// and retire provisional sources before returning display-only owners.
/// The caller runs this on a worker and checks the returned origin against the
/// current committed document before showing it.
pub(crate) fn preview_shared(
    shared: &crate::server::Shared,
    actor: &str,
    commands: &[serde_json::Value],
    expected: Option<u64>,
    document_id: Option<&str>,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
    check: &dyn Fn() -> Result<()>,
) -> Result<TransientFrame> {
    preview_shared_with(shared, actor, commands, expected, document_id, rect, edge, target, mask, selection_view, check, false, None, |request, check| request.render(&mut || check()))
}

pub(crate) fn preview_shared_with<T: DisplayOutput>(
    shared: &crate::server::Shared,
    actor: &str,
    commands: &[serde_json::Value],
    expected: Option<u64>,
    document_id: Option<&str>,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
    check: &dyn Fn() -> Result<()>,
    canvas: bool,
    carets: Option<&mut Option<NativeCarets>>,
    render: impl FnOnce(&Request, &dyn Fn() -> Result<()>) -> Result<T>,
) -> Result<TransientFrame<T>> {
    check()?;
    let (seed, captured, _operation) = {
        let engine = try_source_engine(shared)?;
        check()?;
        engine.ensure_write_available()?;
        let view = crate::engine::view::current(&engine);
        if document_id.is_some_and(|id| id != view.id())
            || expected.is_some_and(|revision| revision != view.revision())
        {
            return Err("Preview source changed; current work preserved".into());
        }
        let seed = engine
            .native_seed()?
            .ok_or("Preview requires a native document")?;
        let captured = engine.capture_scene(None)?;
        let operation = crate::server::FileOperation::track(engine.file_operations.clone());
        (seed, captured, operation)
    };
    let scene = captured.into_native()?;
    let frozen =
        crate::engine::native::presets::freeze_shared(shared, seed.document(), commands, check)?;
    let commands = frozen.as_deref().map_or(commands, Vec::as_slice);
    let descriptor = crate::engine::native::CommandDescriptor::from_normalized(
        seed.document(),
        seed.document().metadata().revision(),
        commands,
        check,
    )?;
    let capture = {
        let engine = try_source_engine(shared)?;
        check()?;
        engine.capture_native(&seed, descriptor, actor, expected, document_id)?
    };
    // Admission is against the committed baseline, never a fresh candidate key.
    let session = Arc::new(crate::workspace::native_dispatch::begin_edit_scene(
        shared, &scene,
    )?);
    use crate::engine::native::worker;
    let prepared = match carets {
        Some(carets) => worker::prepare_text_preview(capture, &session, check, carets),
        None => worker::prepare(capture, &session, check),
    }.map_err(|failure| {
            match failure.cleanup_error {
                None => failure.reason,
                Some(cleanup) => format!(
                    "{}; preview cleanup retained debt: {cleanup}",
                    failure.reason
                ),
            }
        })?;
    drop(seed);
    render_prepared_with(
        Arc::new(scene),
        prepared,
        session,
        rect,
        edge,
        target,
        mask,
        selection_view,
        check,
        canvas,
        render,
    )
}

/// Run on the worker after the shared native command producer. The caller must
/// acquire the ordinary mutation session against BASE before preparing edits.
pub(crate) fn render_prepared(
    base: Arc<SceneSnapshot>,
    prepared: PreparedNative,
    session: Arc<SourceSession>,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
    check: &dyn Fn() -> Result<()>,
) -> Result<TransientFrame> {
    render_prepared_with(base, prepared, session, rect, edge, target, mask, selection_view, check, false, |request, check| request.render(&mut || check()))
}

pub(crate) fn render_prepared_with<T: DisplayOutput>(
    base: Arc<SceneSnapshot>,
    prepared: PreparedNative,
    session: Arc<SourceSession>,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
    check: &dyn Fn() -> Result<()>,
    canvas: bool,
    render: impl FnOnce(&Request, &dyn Fn() -> Result<()>) -> Result<T>,
) -> Result<TransientFrame<T>> {
    let rendered = render_review_with(
        &base,
        &prepared,
        session.clone(),
        rect,
        edge,
        target,
        mask,
        selection_view,
        check,
        canvas,
        render,
    );
    drop(base);
    // Candidate/provider views have all dropped. Only admitted display/source
    // metadata remains; it never keeps a Surface alive through abort.
    let retired = prepared.abort(&session, &|| session.cleanup_check());
    drop(session);
    match (rendered, retired) {
        (Ok(frame), Ok(())) => Ok(frame),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("Preview source cleanup retained debt: {error}")),
        (Err(error), Err(cleanup)) => {
            Err(format!("{error}; preview cleanup retained debt: {cleanup}"))
        }
    }
}

/// Borrow a held candidate for review. The proposal owner must explicitly
/// publish or retire it after every renderer/provider has returned.
pub(crate) fn render_review(
    base: &SceneSnapshot,
    prepared: &PreparedNative,
    session: Arc<SourceSession>,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
    check: &dyn Fn() -> Result<()>,
) -> Result<TransientFrame> {
    render_review_with(base, prepared, session, rect, edge, target, mask, selection_view, check, false, |request, check| request.render(&mut || check()))
}

pub(crate) fn render_review_with<T: DisplayOutput>(
    base: &SceneSnapshot,
    prepared: &PreparedNative,
    session: Arc<SourceSession>,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
    check: &dyn Fn() -> Result<()>,
    canvas: bool,
    render: impl FnOnce(&Request, &dyn Fn() -> Result<()>) -> Result<T>,
) -> Result<TransientFrame<T>> {
    check()?;
    prepared.check()?;
    session.check()?;
    let candidate = base.with_prepared_preview(prepared)?;
    let request = Request::capture_selection(
        Arc::new(candidate),
        session.clone(),
        rect,
        edge,
        target,
        mask,
        selection_view,
    )?;
    let request = if canvas { request.for_canvas(edge)?.with_raster_damage(prepared.raster_damage())? } else { request };
    let bytes = base
        .identity()
        .project_id
        .len()
        .checked_add(base.identity().document_id.len())
        .and_then(|n| n.checked_add(std::mem::size_of::<Origin>() + 256))
        .ok_or("Preview origin footprint overflow")?;
    let admission = base
        .budget()
        .reserve_bytes(bytes, 64 * 1024)
        .map_err(|e| e.to_string())?;
    let origin = Arc::new(Origin {
        base: prepared.base_key(),
        identity: base.identity().clone(),
        candidate: request.tag(),
        presentation: None,
        _admission: admission,
    });
    let source = SourceSupplement::capture(&request, origin.clone(), check)?;
    let frame = render(&request, check)?;
    check()?;
    if !origin.candidate_tag().same_allocation(&frame.display_tag()) {
        return Err("Transient pixels differ from their candidate request".into());
    }
    drop(request);
    Ok(TransientFrame {
        origin,
        frame,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        authority::DocumentAuthority,
        native::{worker, NativeDocument},
    };

    #[test]
    fn failed_or_foreign_display_outputs_abort_real_provisional_edits() {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        for depth in [8, 16] {
            for foreign in [false, true] {
                let session = Arc::new(runtime.begin().unwrap());
                let authority = DocumentAuthority::from_native(
                    NativeDocument::new_blank(19, 17, depth, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap(), 8,
                ).unwrap();
                let base_key = authority.native().unwrap().key();
                let metadata = serde_json::to_value(authority.native().unwrap().metadata()).unwrap();
                let disk_before = runtime.disk().used().unwrap();
                let layer = authority.view().layer(0).unwrap().id().to_owned();
                let commands = [serde_json::json!({"op":"paint.fill","layer":layer,"rect":[1,2,17,15],"color":[31,73,149,199]})];
                let prepared = worker::prepare(worker::tests::capture(&authority, &commands), &session, &|| Ok(())).unwrap();
                assert!(runtime.disk().used().unwrap() > disk_before);
                let scene = Arc::new(authority.capture_scene(None, "preview".into(), vec![], None, None).unwrap().into_native().unwrap());
                let result = render_prepared_with(
                    scene, prepared, session.clone(), None, 19, None, false, None, &|| Ok(()), true,
                    |request, check| {
                        let frame = request.render(&mut || check())?;
                        assert_eq!(frame.key().width, 19);
                        drop(frame);
                        if !foreign { return Err("Display delivery cancelled".into()); }
                        // Even a second capture of the same candidate may not
                        // impersonate the exact output receipt being returned.
                        Request::capture(request.scene.clone(), session.clone(), None, 19, None, false)?
                            .render(&mut || check())
                    },
                );
                let reason = result.err().unwrap();
                assert!(reason.contains(if foreign { "Transient pixels differ" } else { "Display delivery cancelled" }));
                assert_eq!(runtime.disk().used().unwrap(), disk_before);
                assert_eq!(authority.native().unwrap().key(), base_key);
                assert_eq!(authority.history_lengths(), [0, 0]);
                assert_eq!(serde_json::to_value(authority.native().unwrap().metadata()).unwrap(), metadata);
            }
        }
    }
    #[test]
    fn actual_candidate_pixels_keep_baseline_identity_and_abort_provisional_files() {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let session = Arc::new(runtime.begin().unwrap());
        for depth in [8, 16] {
            for mode in [None, Some("mask"), Some("cutout"), Some("overlay")] {
                let authority = DocumentAuthority::from_native(
                    NativeDocument::new_blank(
                        4,
                        2,
                        depth,
                        &runtime.resident(),
                        &runtime.disk(),
                        &|| Ok(()),
                    )
                    .unwrap(),
                    8,
                )
                .unwrap();
                let layer = authority.view().layer(0).unwrap().id().to_owned();
                let base_key = authority.native().unwrap().key();
                let scene = authority
                    .capture_scene(None, "preview".into(), vec![], None, None)
                    .unwrap()
                    .into_native()
                    .unwrap();
                let disk_before = session.disk().used().unwrap();
                let commands = [
                    serde_json::json!({
                        "op":"paint.fill", "layer":layer, "rect":[0,0,4,2],
                        "color":[20,101,201,177]
                    }),
                    serde_json::json!({"op":"selection", "kind":"rectangle", "rect":[1,0,3,2]}),
                ];
                let prepared = worker::prepare(
                    worker::tests::capture(&authority, &commands),
                    &session,
                    &|| Ok(()),
                )
                .unwrap();
                let candidate_key = prepared.preview().key();
                let frame = render_prepared(
                    Arc::new(scene),
                    prepared,
                    session.clone(),
                    None,
                    4,
                    None,
                    false,
                    mode,
                    &|| Ok(()),
                )
                .unwrap();
                assert!(frame.origin().matches_source("preview", base_key, 0));
                assert!(!frame.origin().matches_source("preview", candidate_key, 0));
                assert!(!frame.origin().matches_source("preview", base_key, 1));
                assert!(frame.origin().matches_frame(frame.frame()));
                assert_eq!(frame.frame().key().source, candidate_key);
                let pixels = frame.frame().copy_rgba8(&mut || Ok(())).unwrap();
                for (index, pixel) in pixels.chunks_exact(4).enumerate() {
                    let selected = [1, 2].contains(&(index % 4));
                    let original = [20, 101, 201, 177];
                    let expected = match mode {
                        Some("mask") => {
                            if selected {
                                [255; 4]
                            } else {
                                [0, 0, 0, 255]
                            }
                        }
                        Some("cutout") if !selected => [20, 101, 201, 0],
                        Some("overlay") if !selected => {
                            crate::raster::blend(original, [233, 84, 32, 140], 1., "normal")
                        }
                        _ => original,
                    };
                    assert_eq!(pixel, expected, "depth{depth} mode{mode:?} pixel{index}");
                }
                assert_eq!(authority.native().unwrap().key(), base_key);
                assert_eq!(authority.history_lengths(), [0, 0]);
                assert_eq!(session.disk().used().unwrap(), disk_before);
                drop((pixels, frame, authority));
            }
        }
        drop((session, runtime, connection));
    }
}
