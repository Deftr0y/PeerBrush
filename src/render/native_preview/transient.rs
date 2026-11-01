//! An unpublished preview binds its committed baseline and exact candidate tag.
//! Rendering and provisional retirement complete before display owners escape.
use super::*;
use crate::{
    engine::{native::worker::PreparedNative, scene::Source36},
    workspace::source_runtime::SourceSession,
};

pub(crate) struct Origin {
    base: DocumentKey,
    identity: SceneIdentity,
    candidate: Tag,
    _admission: Reservation,
}
impl Origin {
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
pub(crate) struct TransientFrame {
    origin: Arc<Origin>,
    frame: Frame,
    source: Option<SourceSupplement>,
}
impl TransientFrame {
    pub(crate) fn origin(&self) -> &Origin {
        &self.origin
    }
    pub(crate) fn frame(&self) -> &Frame {
        &self.frame
    }
    pub(crate) fn source(&self) -> Option<&SourceSupplement> {
        self.source.as_ref()
    }
    pub(crate) fn into_parts(self) -> (Arc<Origin>, Frame, Option<SourceSupplement>) {
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
    check: &dyn Fn() -> Result<()>,
) -> Result<TransientFrame> {
    check()?;
    let _operation = crate::server::FileOperation::begin(shared)?;
    let (seed, captured) = {
        let engine = shared.lock().unwrap();
        engine.ensure_open()?;
        let view = crate::engine::view::current(&engine);
        if document_id.is_some_and(|id| id != view.id())
            || expected.is_some_and(|revision| revision != view.revision())
        {
            return Err("Preview source changed; current work preserved".into());
        }
        let seed = engine
            .native_seed()?
            .ok_or("Preview requires a native document")?;
        (seed, engine.capture_scene(None)?)
    };
    let scene = captured.into_native()?;
    let descriptor = crate::engine::native::CommandDescriptor::from_normalized(
        seed.document(),
        seed.document().metadata().revision(),
        commands,
        check,
    )?;
    let capture = {
        let engine = shared.lock().unwrap();
        engine.capture_native(&seed, descriptor, actor, expected, document_id)?
    };
    // Admission is against the committed baseline, never a fresh candidate key.
    let session = Arc::new(crate::workspace::native_dispatch::begin_scene(
        shared, &scene,
    )?);
    let active = || {
        check()?;
        session.check()
    };
    let prepared =
        crate::engine::native::worker::prepare(capture, &session, &active).map_err(|failure| {
            match failure.cleanup_error {
                None => failure.reason,
                Some(cleanup) => format!(
                    "{}; preview cleanup retained debt: {cleanup}",
                    failure.reason
                ),
            }
        })?;
    drop(seed);
    render_prepared(
        Arc::new(scene),
        prepared,
        session,
        rect,
        edge,
        target,
        mask,
        check,
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
    check: &dyn Fn() -> Result<()>,
) -> Result<TransientFrame> {
    let rendered = (|| {
        check()?;
        session.check()?;
        let candidate = base.with_prepared_preview(&prepared)?;
        let request = Request::capture(
            Arc::new(candidate),
            session.clone(),
            rect,
            edge,
            target,
            mask,
        )?;
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
            _admission: admission,
        });
        let source = SourceSupplement::capture(&request, origin.clone(), check)?;
        let frame = request.render(&mut || check())?;
        if !origin.matches_frame(&frame) {
            return Err("Transient pixels differ from their candidate request".into());
        }
        drop(request);
        Ok(TransientFrame {
            origin,
            frame,
            source,
        })
    })();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        authority::DocumentAuthority,
        native::{worker, NativeDocument},
    };

    #[test]
    fn actual_candidate_pixels_keep_baseline_identity_and_abort_provisional_files() {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let session = Arc::new(runtime.begin().unwrap());
        for depth in [8, 16] {
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
            let commands = [serde_json::json!({
                "op":"paint.fill", "layer":layer, "rect":[0,0,4,2],
                "color":[20,101,201,177]
            })];
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
                &|| Ok(()),
            )
            .unwrap();
            assert!(frame.origin().matches_source("preview", base_key, 0));
            assert!(!frame.origin().matches_source("preview", candidate_key, 0));
            assert!(!frame.origin().matches_source("preview", base_key, 1));
            assert!(frame.origin().matches_frame(frame.frame()));
            assert_eq!(frame.frame().key().source, candidate_key);
            let pixels = frame.frame().copy_rgba8(&mut || Ok(())).unwrap();
            assert!(pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [20, 101, 201, 177]));
            assert_eq!(authority.native().unwrap().key(), base_key);
            assert_eq!(authority.history_lengths(), [0, 0]);
            assert_eq!(session.disk().used().unwrap(), disk_before);
            drop((pixels, frame, authority));
        }
        drop((session, runtime, connection));
    }
}
