//! Completed code keeps its admitted receipt and reusable PNG, never a source
//! session. Feedback failure after publication cannot turn an applied edit into
//! a discarded run.
use super::{
    observation::pixels::Pixels,
    reply::{ReplyAdmission, Segments},
    EditReply, Shared,
};
use crate::{
    engine::{
        native::{DocumentKey, NativeDocument},
        scene::SceneSnapshot,
        Scope,
    },
    large_document::ResidentBudget,
    psd::memory36::MetadataCharge,
    render::native_preview::Request,
    workspace::{native_dispatch, source_runtime::SourceReadSession},
};
use serde::{ser::SerializeSeq, Serialize, Serializer};
use serde_json::Value;
use std::sync::Arc;
type Result<T> = std::result::Result<T, String>;
const WARNING: &str = "Edit applied; canvas feedback is unavailable. Observe the current project.";

pub(crate) struct Preparation {
    document: String,
    revision: u64,
    edge: u32,
    pool: ResidentBudget,
    capacity: Arc<MetadataCharge>,
}
pub(crate) struct Completion {
    reply: EditReply,
    image: Option<Image>,
    document: String,
    pool: ResidentBudget,
    _capacity: Arc<MetadataCharge>,
}
#[derive(Serialize)]
struct Image {
    mime_type: &'static str,
    data: (),
    width: u32,
    height: u32,
    document_rect: [i32; 4],
    revision: u64,
    #[serde(skip)]
    bytes: Arc<Segments>,
}
impl Preparation {
    /// All owned completion headers are admitted before the edit can publish.
    pub(crate) fn new(
        source: &NativeDocument,
        edge: u32,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        if !(32..=1024).contains(&edge) {
            return Err("Code feedback max_edge must be 32–1024".into());
        }
        let metadata = source.metadata();
        let revision = metadata
            .revision()
            .checked_add(1)
            .ok_or("Code revision overflow")?;
        let bytes = metadata
            .id()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(8192))
            .ok_or("Code completion admission overflow")?;
        let capacity = MetadataCharge::reserve(source.budget(), bytes, check)?;
        let mut document = String::new();
        document
            .try_reserve_exact(metadata.id().len())
            .map_err(|_| "Code completion header allocation refused")?;
        document.push_str(metadata.id());
        if document.capacity() > bytes - 8192 {
            return Err("Code completion capacity exceeds admission".into());
        }
        check()?;
        Ok(Self {
            document,
            revision,
            edge,
            pool: source.budget().clone(),
            capacity,
        })
    }
    /// The receipt already means applied. Only the optional presentation may
    /// fail or be interrupted from here, including a callback panic.
    pub(crate) fn finish(
        self,
        shared: &Shared,
        reply: EditReply,
        check: &dyn Fn() -> Result<()>,
    ) -> Completion {
        let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !reply
                .native_pool()
                .is_some_and(|pool| pool.same_pool(&self.pool))
                || reply.data()["revision"].as_u64() != Some(self.revision)
            {
                return Err("Code receipt differs from its completion admission".into());
            }
            render(
                shared,
                reply
                    .native_key()
                    .ok_or("Code receipt has no native generation")?,
                &self.document,
                self.revision,
                self.edge,
                &self.pool,
                check,
            )
        }))
        .ok()
        .and_then(Result::ok);
        Completion {
            reply,
            image,
            document: self.document,
            pool: self.pool,
            _capacity: self.capacity,
        }
    }
}
impl Completion {
    pub(crate) fn data(&self) -> &Value {
        self.reply.data()
    }
    pub(crate) fn pool(&self) -> &ResidentBudget {
        &self.pool
    }
    /// The PNG remains shared/admitted across any number of status replies.
    /// Each new metadata projection gets its own admission before allocation.
    pub(crate) fn response(
        &self,
        scopes: &[Scope],
        admission: &mut ReplyAdmission,
    ) -> Result<Value> {
        if !self
            .reply
            .native_pool()
            .is_some_and(|pool| pool.same_pool(&self.pool))
        {
            return Err("Code edit applied; result admission differs".into());
        }
        #[derive(Serialize)]
        struct Applied<'a> {
            #[serde(flatten)]
            edit: &'a Value,
            document_id: &'a str,
            scopes: &'a [Scope],
            #[serde(skip_serializing_if = "Option::is_none")]
            feedback_warning: Option<&'static str>,
        }
        #[derive(Serialize)]
        struct Response<'a> {
            result: Applied<'a>,
            images: Images<'a>,
        }
        let view = Response {
            result: Applied {
                edit: self.reply.data(),
                document_id: &self.document,
                scopes,
                feedback_warning: self.image.is_none().then_some(WARNING),
            },
            images: Images(self.image.as_ref()),
        };
        admission.admit_projection(&self.pool, &view)?;
        let value = serde_json::to_value(view).map_err(|e| e.to_string())?;
        if let Some(image) = &self.image {
            admission.image_shared(0, image.bytes.clone())?;
        }
        Ok(value)
    }
}
struct Images<'a>(Option<&'a Image>);
impl Serialize for Images<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(usize::from(self.0.is_some())))?;
        if let Some(image) = self.0 {
            output.serialize_element(image)?;
        }
        output.end()
    }
}
struct Capture {
    document: NativeDocument,
    scene: Arc<SceneSnapshot>,
    session: SourceReadSession,
}
impl Capture {
    fn check(&self, shared: &Shared, check: &dyn Fn() -> Result<()>) -> Result<()> {
        check()?;
        {
            let engine = shared
                .try_lock()
                .map_err(|_| "Code feedback project is busy")?;
            engine.ensure_open()?;
            if engine.project_id != self.scene.identity().project_id
                || engine
                    .native_document()
                    .is_none_or(|current| current.key() != self.document.key())
            {
                return Err("Code feedback source changed".into());
            }
        }
        let runtime = native_dispatch::runtime_for(shared)?;
        if !self.session.uses_runtime(&runtime) {
            return Err("Code feedback source service changed".into());
        }
        self.session.check_scene(&self.scene)?;
        check()
    }
}
#[allow(clippy::too_many_arguments)]
fn render(
    shared: &Shared,
    key: DocumentKey,
    document: &str,
    revision: u64,
    edge: u32,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Image> {
    check()?;
    let (document, scene) = {
        let engine = shared.lock().unwrap();
        engine.ensure_open()?;
        let current = engine
            .native_document()
            .ok_or("Code feedback requires native authority")?;
        if current.key() != key
            || current.metadata().id() != document
            || current.metadata().revision() != revision
            || !current.budget().same_pool(pool)
        {
            return Err("Committed code feedback source changed".into());
        }
        let scene = engine.capture_scene(None)?.into_native()?;
        if scene.key() != key {
            return Err("Code feedback scene generation changed".into());
        }
        (current.clone(), Arc::new(scene))
    };
    let session = native_dispatch::begin_read_scene(shared, &scene)?;
    let capture = Capture {
        document,
        scene,
        session,
    };
    let active = || capture.check(shared, check);
    active()?;
    let request = Request::capture(
        capture.scene.clone(),
        capture.session.clone(),
        None,
        edge,
        None,
        false,
    )?;
    let frame = request.render(&mut || active())?;
    drop(request);
    let (bytes, tag) = Pixels::new(frame).encode(pool, &mut || active())?;
    active()?;
    let key = tag.key();
    let image = Image {
        mime_type: "image/png",
        data: (),
        width: key.width,
        height: key.height,
        document_rect: key.rect,
        revision,
        bytes: Arc::new(bytes),
    };
    drop(tag);
    drop(capture);
    Ok(image)
}

#[cfg(test)]
mod tests;
