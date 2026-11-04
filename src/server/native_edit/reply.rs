//! Native edit results retain their actual response admission after commit.
use super::*;
use crate::server::reply::ReplyAdmission;
use serde::{ser::SerializeSeq, Serialize, Serializer};

#[derive(Serialize)]
#[serde(transparent)]
pub(crate) struct EditReply {
    pub(super) value: Value,
    #[serde(skip)]
    admission: Option<ReplyAdmission>,
}
impl EditReply {
    pub(crate) fn data(&self) -> &Value {
        &self.value
    }
    pub(super) fn resident(value: Value) -> Self {
        Self {
            value,
            admission: None,
        }
    }
    pub(in crate::server) fn into_response(
        self,
        target: &mut ReplyAdmission,
    ) -> Result<Value, String> {
        let Self { value, admission } = self;
        if let Some(mut admission) = admission {
            if let Err(error) = target.absorb_projection(&mut admission) {
                drop(value);
                drop(admission);
                return Err(error);
            }
        }
        Ok(value)
    }
    pub(super) fn native(
        prepared: &native::worker::PreparedNative,
        applied: usize,
    ) -> Result<Self, String> {
        let view = NativeReplyView {
            revision: prepared.preview().metadata().revision(),
            applied,
            created: Created {
                prepared,
                roots: false,
            },
            created_roots: Created {
                prepared,
                roots: true,
            },
            layers: LayerNames(prepared.preview()),
        };
        let mut admission = ReplyAdmission::new(true);
        admission.admit_projection(prepared.preview().budget(), &view)?;
        admission.admit(prepared.preview().budget(), 4096)?;
        let value =
            serde_json::to_value(view).map_err(|_| "Native edit response projection failed")?;
        Ok(Self {
            value,
            admission: Some(admission),
        })
    }
}
#[derive(Serialize)]
struct NativeReplyView<'a> {
    revision: u64,
    applied: usize,
    created: Created<'a>,
    created_roots: Created<'a>,
    layers: LayerNames<'a>,
}
struct Created<'a> {
    prepared: &'a native::worker::PreparedNative,
    roots: bool,
}
impl Serialize for Created<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let base = self.prepared.base().metadata();
        let candidate = self.prepared.preview().metadata();
        let mut out = serializer.serialize_seq(None)?;
        for layer in candidate.layers() {
            if base.layers().any(|old| old.id() == layer.id()) {
                continue;
            }
            if self.roots
                && layer
                    .parent()
                    .is_some_and(|parent| !base.layers().any(|old| old.id() == parent))
            {
                continue;
            }
            out.serialize_element(layer.id())?;
        }
        out.end()
    }
}
struct LayerNames<'a>(&'a native::NativeDocument);
impl Serialize for LayerNames<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Layer<'a> {
            id: &'a str,
            name: &'a str,
        }
        let metadata = self.0.metadata();
        let mut out = serializer.serialize_seq(Some(metadata.layers().len()))?;
        for layer in metadata.layers() {
            out.serialize_element(&Layer {
                id: layer.id(),
                name: layer.name(),
            })?;
        }
        out.end()
    }
}
