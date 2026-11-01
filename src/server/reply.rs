//! Native response ownership survives metadata projection and transport framing.
#[path = "reply/admission.rs"]
mod admission;
#[path = "reply/chunks.rs"]
mod chunks;
#[path = "reply/wire.rs"]
mod wire;
use crate::large_document::{Reservation, ResidentBudget};
pub(super) use chunks::{SegmentReader, SegmentWriter, Segments};
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::io::Write;

const TICKETS: usize = 256;
const SLOTS: usize = 8;
const DEPTH: usize = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part<'a> {
    End,
    Key(&'a str),
    Index(usize),
}
struct Slot {
    path: [Part<'static>; DEPTH],
    length: usize,
    data: Data,
}
enum Data {
    Png(Segments),
    JsonText(Value),
}
pub(super) struct ReplyAdmission {
    allowed: bool,
    slots: Vec<Slot>,
    tickets: Vec<Reservation>,
    pool: Option<ResidentBudget>,
    index: Option<Reservation>,
}
impl ReplyAdmission {
    pub(super) fn new(allowed: bool) -> Self {
        Self {
            allowed,
            slots: Vec::new(),
            tickets: Vec::new(),
            pool: None,
            index: None,
        }
    }
    pub(super) fn admit(&mut self, pool: &ResidentBudget, mut bytes: usize) -> Result<(), String> {
        if !self.allowed {
            return Err("Native responses require an owned transport reply".into());
        }
        if self
            .pool
            .as_ref()
            .is_some_and(|owner| !owner.same_pool(pool))
        {
            return Err("Response source uses another resident admission".into());
        }
        if self.pool.is_none() {
            let size = TICKETS * std::mem::size_of::<Reservation>()
                + SLOTS * std::mem::size_of::<Slot>()
                + 4096;
            let index = pool.reserve_bytes(size, size).map_err(|e| e.to_string())?;
            let mut tickets = Vec::new();
            tickets
                .try_reserve_exact(TICKETS)
                .map_err(|_| "Response ticket allocation refused")?;
            let mut slots = Vec::new();
            slots
                .try_reserve_exact(SLOTS)
                .map_err(|_| "Response slot allocation refused")?;
            if tickets.capacity() * std::mem::size_of::<Reservation>()
                + slots.capacity() * std::mem::size_of::<Slot>()
                > index.bytes()
            {
                return Err("Response descriptor capacity exceeds admission".into());
            }
            self.tickets = tickets;
            self.slots = slots;
            self.pool = Some(pool.clone());
            self.index = Some(index);
        }
        while bytes > 0 {
            if self.tickets.len() == TICKETS {
                return Err("Response ticket count exceeds its bound".into());
            }
            let length = bytes.min(64 * 1024 * 1024);
            self.tickets.push(
                pool.reserve_bytes(length, length)
                    .map_err(|e| e.to_string())?,
            );
            bytes -= length;
        }
        Ok(())
    }
    pub(super) fn pool(&self) -> Result<&ResidentBudget, String> {
        self.pool
            .as_ref()
            .ok_or_else(|| "Response has no native admission".into())
    }
    pub(super) fn admit_projection<T: Serialize + ?Sized>(
        &mut self,
        pool: &ResidentBudget,
        value: &T,
    ) -> Result<(), String> {
        self.admit(pool, admission::bound(value)?)
    }
    pub(super) fn image(&mut self, index: usize, bytes: Segments) -> Result<(), String> {
        if !bytes.uses_budget(self.pool()?) {
            return Err("Response image uses another admission".into());
        }
        self.slot(
            &[Part::Key("images"), Part::Index(index), Part::Key("data")],
            Data::Png(bytes),
        )
    }
    fn slot(&mut self, path: &[Part<'static>], data: Data) -> Result<(), String> {
        if self.slots.len() == SLOTS || path.len() > DEPTH || self.pool.is_none() {
            return Err("Response slot admission exceeds its bound".into());
        }
        let mut parts = [Part::End; DEPTH];
        parts[..path.len()].copy_from_slice(path);
        if self
            .slots
            .iter()
            .any(|slot| slot.length == path.len() && slot.path[..slot.length] == *path)
        {
            return Err("Response slot is already assigned".into());
        }
        self.slots.push(Slot {
            path: parts,
            length: path.len(),
            data,
        });
        Ok(())
    }
    pub(super) fn finish(self, value: Value) -> Result<OwnedReply, String> {
        for slot in &self.slots {
            if wire::at(&value, &slot.path[..slot.length]) != Some(&Value::Null) {
                return Err("Response image or text slot is not its sealed placeholder".into());
            }
        }
        Ok(OwnedReply {
            value,
            admission: self,
        })
    }
}
/// The Value is a private metadata template. Native payload slots cannot escape
/// as detached strings; Serialize supplies them from their admitted owners.
pub(crate) struct OwnedReply {
    value: Value,
    admission: ReplyAdmission,
}
impl OwnedReply {
    pub(super) fn plain(value: Value) -> Self {
        Self {
            value,
            admission: ReplyAdmission::new(false),
        }
    }
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
    pub(super) fn into_plain(self) -> Result<Value, String> {
        if self.admission.pool.is_some() || !self.admission.slots.is_empty() {
            return Err("Native responses require an owned transport reply".into());
        }
        Ok(self.value)
    }
    fn prefix(&mut self, key: &'static str) -> Result<(), String> {
        for slot in &mut self.admission.slots {
            if slot.length == DEPTH {
                return Err("Response envelope depth exceeds its bound".into());
            }
            slot.path.copy_within(0..slot.length, 1);
            slot.path[0] = Part::Key(key);
            slot.length += 1;
        }
        Ok(())
    }
    pub(super) fn envelope(
        mut self,
        key: &'static str,
        mut header: serde_json::Map<String, Value>,
    ) -> Result<Self, String> {
        if header.contains_key(key) {
            return Err("Response envelope field is already assigned".into());
        }
        self.prefix(key)?;
        header.insert(key.into(), self.value);
        self.value = Value::Object(header);
        Ok(self)
    }
    pub(super) fn update(&mut self, change: impl FnOnce(&mut Value)) {
        change(&mut self.value);
    }
    pub(super) fn tool_result(mut self) -> Result<Self, String> {
        let images = self
            .value
            .as_object_mut()
            .and_then(|value| value.remove("images"))
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let Value::Array(images) = images else {
            return Err("Response images are not an array".into());
        };
        let mut content = Vec::new();
        let mut views = Vec::new();
        content
            .try_reserve_exact(images.len() + 1)
            .map_err(|_| "Response content allocation refused")?;
        views
            .try_reserve_exact(images.len())
            .map_err(|_| "Response view allocation refused")?;
        content.push(serde_json::json!({"type":"text","text":Value::Null}));
        for (mut image, index) in images.into_iter().zip(0..) {
            let data = image
                .as_object_mut()
                .and_then(|image| image.remove("data"))
                .ok_or("Response image has no data field")?;
            views.push(image);
            content.push(serde_json::json!({"type":"image","mimeType":"image/png","data":data}));
            for slot in &mut self.admission.slots {
                if slot.length == 3
                    && slot.path[..3]
                        == [Part::Key("images"), Part::Index(index), Part::Key("data")]
                {
                    slot.path[..3].copy_from_slice(&[
                        Part::Key("content"),
                        Part::Index(index + 1),
                        Part::Key("data"),
                    ]);
                }
            }
        }
        if !views.is_empty() {
            self.value["views"] = Value::Array(views);
        }
        let metadata = self.value;
        if self.admission.pool.is_some() {
            self.admission.slot(
                &[Part::Key("content"), Part::Index(0), Part::Key("text")],
                Data::JsonText(metadata),
            )?;
        } else {
            content[0]["text"] =
                Value::String(serde_json::to_string(&metadata).map_err(|e| e.to_string())?);
        }
        self.value = serde_json::json!({"content":content,"isError":false});
        self.admission.finish(self.value)
    }
    pub(super) fn serialized(
        self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Segments, String> {
        let pool = self.admission.pool()?;
        let mut count = wire::Count::new(chunks::MAX_BYTES);
        serde_json::to_writer(&mut count, &self).map_err(|e| e.to_string())?;
        check()?;
        let mut output = SegmentWriter::new(pool, count.bytes().max(1), check)?;
        serde_json::to_writer(&mut output, &self).map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())?;
        let bytes = output.finish()?;
        if bytes.len() != count.bytes() {
            return Err("Response serialization length changed".into());
        }
        Ok(bytes)
    }
}
impl Serialize for OwnedReply {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        for slot in &self.admission.slots {
            if wire::at(&self.value, &slot.path[..slot.length]) != Some(&Value::Null) {
                return Err(serde::ser::Error::custom("Sealed response slot changed"));
            }
        }
        wire::Node {
            value: &self.value,
            slots: &self.admission.slots,
            path: [Part::End; DEPTH],
            length: 0,
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
#[path = "reply/tests.rs"]
mod tests;
