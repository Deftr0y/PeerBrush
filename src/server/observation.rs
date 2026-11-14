//! Native observation captures one authoritative generation, then performs all
//! source reads and response construction after releasing Engine/registry locks.
#[cfg(test)]
mod fixture;
use crate::engine::report::header;
pub(crate) use crate::engine::report::metadata;
#[cfg(test)]
mod metadata_tests;
pub(super) mod pixels;
#[cfg(test)]
mod pixels_tests;
use super::{reply::ReplyAdmission, Shared};
use crate::{
    engine::{self, native::NativeDocument, scene::SceneSnapshot, view::DocumentView},
    large_document::ResidentBudget,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
type Result<T> = std::result::Result<T, String>;

pub(super) struct Capture {
    document: NativeDocument,
    scene: Arc<SceneSnapshot>,
    header: Value,
    saving: Option<crate::saving::Control>,
    loading: Option<crate::loading::Control>,
}
pub(super) fn capture(
    shared: &Shared,
    p: &Value,
    admission: &mut ReplyAdmission,
) -> Result<Option<Capture>> {
    let mut engine = shared.lock().unwrap();
    crate::workspace::validate_target(&engine, p)?;
    capture_engine(&mut engine, admission)
}

pub(super) fn capture_engine(
    engine: &mut engine::Engine,
    admission: &mut ReplyAdmission,
) -> Result<Option<Capture>> {
    if !matches!(engine::view::current(engine), DocumentView::Native(_)) {
        return Ok(None);
    }
    engine.expire();
    let crate::engine::view::DocumentSnapshot::Native(document) = engine.doc_snapshot() else {
        return Err("Native observation snapshot authority changed".into());
    };
    let header = header::Header::new(engine, document.metadata().revision());
    admission.admit_projection(document.budget(), &header)?;
    let header = serde_json::to_value(header).map_err(|e| e.to_string())?;
    let scene = engine.capture_scene(None)?.into_native()?;
    if scene.key() != document.key() {
        return Err("Native observation capture changed its source generation".into());
    }
    Ok(Some(Capture {
        document,
        scene: Arc::new(scene),
        header,
        saving: engine.saving.clone(),
        loading: engine.loading.clone(),
    }))
}
fn project<T: Serialize + ?Sized>(
    admission: &mut ReplyAdmission,
    pool: &ResidentBudget,
    value: &T,
) -> Result<Value> {
    admission.admit_projection(pool, value)?;
    serde_json::to_value(value).map_err(|e| e.to_string())
}
// Field order also drains source owners before admission on every error path.
struct ReadCapture {
    capture: Capture,
    session: crate::workspace::source_runtime::SourceReadSession,
}
impl Capture {
    pub(super) fn with_loading(mut self, control: crate::loading::Control) -> Self {
        self.loading = Some(control);
        self
    }

    pub(super) fn scene(&self) -> &SceneSnapshot {
        &self.scene
    }

    pub(super) fn observe(
        self,
        shared: &Shared,
        p: &Value,
        admission: &mut ReplyAdmission,
    ) -> Result<Value> {
        let session = crate::workspace::native_dispatch::begin_read_scene(shared, &self.scene)?;
        self.observe_admitted(session, p, admission)
    }

    pub(super) fn observe_admitted(
        self,
        session: crate::workspace::source_runtime::SourceReadSession,
        p: &Value,
        admission: &mut ReplyAdmission,
    ) -> Result<Value> {
        session.check_scene(&self.scene)?;
        ReadCapture {
            capture: self,
            session,
        }
        .observe(p, admission)
    }
}
impl ReadCapture {
    pub(super) fn observe(mut self, p: &Value, admission: &mut ReplyAdmission) -> Result<Value> {
        let pool = self.capture.document.budget().clone();
        let revision = self.capture.document.metadata().revision();
        let session = &self.session;
        // Status callbacks hold only their own mutex and do pure memory work.
        self.capture.header["saving"] = self
            .capture
            .saving
            .as_ref()
            .map(|control| control.read_status(|status| project(admission, &pool, status)))
            .transpose()?
            .unwrap_or(Value::Null);
        self.capture.header["loading"] = self
            .capture
            .loading
            .as_ref()
            .map(|control| {
                control.read_status(|status| {
                    #[derive(Serialize)]
                    struct Loading<'a> {
                        stage: &'a str,
                        completed: usize,
                        total: usize,
                    }
                    project(
                        admission,
                        &pool,
                        &Loading {
                            stage: &status.stage,
                            completed: status.completed,
                            total: status.total,
                        },
                    )
                })
            })
            .transpose()?
            .unwrap_or(Value::Null);
        // This is an uncached worker operation; only its small status reaches
        // the reply. Unsupported profiles permit raw feedback; resource and
        // cancellation failures propagate instead of changing color policy.
        let profile = {
            use crate::color_profile::prepared::{DisplayPlan, DisplayStatus};
            let plan = DisplayPlan::prepare(
                self.capture.scene.metadata().profile(),
                false,
                &pool,
                &mut || session.check(),
            )?;
            match plan.status() {
                DisplayStatus::Unprofiled => {
                    json!({"source":"sRGB","preview":"sRGB","conversion_available":false})
                }
                DisplayStatus::Converted => {
                    json!({"source":"embedded RGB ICC","preview":"sRGB","conversion_available":true})
                }
                DisplayStatus::Unsupported => {
                    json!({"source":"embedded ICC","preview":"unmanaged","conversion_available":false,"reason":plan.warning().unwrap()})
                }
                DisplayStatus::Mask => {
                    return Err("Observation profile summary cannot use mask display policy".into())
                }
            }
        };
        self.capture.header["document"] = project(
            admission,
            &pool,
            &metadata::Document {
                scene: &self.capture.scene,
                profile: &profile,
            },
        )?;
        self.capture.header["layers"] =
            project(admission, &pool, &metadata::Layers(&self.capture.scene))?;
        session.check()?;
        let image = p.get("image").and_then(Value::as_bool).unwrap_or(true)
            && p.get("since_revision").and_then(Value::as_u64) != Some(revision);
        if !image {
            let header = std::mem::take(&mut self.capture.header);
            drop(self);
            return Ok(json!({"state":header,"revision":revision,"images":[]}));
        }
        let request = crate::render::native_preview::Request::capture_selection(
            self.capture.scene.clone(),
            session.clone(),
            p.get("rect").and_then(engine::rect),
            p["max_edge"].as_u64().unwrap_or(1024).clamp(32, 4096) as u32,
            p["layer"].as_str(),
            p["mask"].as_bool().unwrap_or(false),
            p["selection_view"].as_str(),
        )?;
        let frame = request.render(&mut || session.check())?;
        drop(request);
        let pixels = pixels::Pixels::new(frame);
        session.check()?;
        let ReadCapture {
            capture:
                Capture {
                    header,
                    document,
                    scene,
                    saving,
                    loading,
                },
            session,
        } = self;
        // Large metadata/source/profile/session owners never enter PNG or the
        // HTTP presentation handoff; only admitted display bytes/tags remain.
        drop(document);
        drop(scene);
        drop(saving);
        drop(loading);
        drop(session);
        let (png, tag) = pixels.encode(&pool, &mut || Ok(()))?;
        let key = tag.key();
        let image = json!({"mime_type":"image/png","data":null,"width":key.width,"height":key.height,"document_rect":key.rect,"revision":revision});
        admission.admit_projection(&pool, &image)?;
        admission.image(0, png)?;
        Ok(json!({"state":header,"revision":revision,"images":[image]}))
    }
}
