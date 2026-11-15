//! Ordered Alt sampling and completed strokes. Samples with dependent strokes
//! are never superseded; publication receipts advance the source fence.
use super::*;
use crate::{
    engine::native::{copy_admitted, DocumentKey},
    large_document::ResidentBudget,
    psd::memory36::{Admitted, MetadataCharge},
    render::sample::Color,
};
use std::{collections::VecDeque, sync::Weak};

const EVENTS: usize = 100;
const BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Id(egui::Id, u64);

struct Source {
    engine: Weak<std::sync::Mutex<crate::engine::Engine>>,
    project: String,
    document: String,
    revision: u64,
    native: Option<DocumentKey>,
}
impl Source {
    fn identity(&self, shared: &Shared, project: &str, doc: DocumentView<'_>) -> bool {
        self.engine.as_ptr() == Arc::as_ptr(shared)
            && self.project == project
            && self.document == doc.id()
            && self.native.is_some() == doc.native().is_some()
    }
    fn matches(&self, shared: &Shared, project: &str, doc: DocumentView<'_>) -> bool {
        self.identity(shared, project, doc)
            && self.revision == doc.revision()
            && self.native == doc.native().map(|doc| doc.key())
    }
}
struct Entry {
    id: Id,
    event: Event,
    bytes: usize,
    _charge: Arc<MetadataCharge>,
}
enum Event {
    Sample {
        point: [i32; 2],
        layer: String,
        mask: bool,
        step: Option<String>,
    },
    Stroke {
        command: Admitted<Value>,
        label: String,
        sample: Id,
        use_sample: bool,
        endpoint: ([f32; 2], f32),
    },
}
pub(super) enum Action<'a> {
    Sample {
        point: [i32; 2],
        layer: &'a str,
        mask: bool,
        step: Option<&'a str>,
    },
    Stroke {
        command: Admitted<Value>,
        label: &'a str,
        endpoint: ([f32; 2], f32),
    },
}
pub(super) struct Ready<'a> {
    pub id: Id,
    pub document: &'a str,
    pub revision: u64,
    pub action: Action<'a>,
}
pub(super) struct Enqueued {
    pub id: Id,
    pub superseded: Option<Id>,
}
pub(super) type PreviewStamp = (u64, Option<Id>, Option<Id>, Option<Id>);
pub(super) struct Preview {
    pub commands: Vec<Admitted<Value>>,
    _charge: Arc<MetadataCharge>,
}
pub(super) struct Timeline {
    source: Source,
    events: VecDeque<Entry>,
    next: u64,
    nonce: egui::Id,
    flight: Option<Id>,
    last_sample: Option<Id>,
    resolved: Option<(Id, Color)>,
    failure: bool,
    bytes: usize,
    pool: ResidentBudget,
    _charge: Arc<MetadataCharge>,
}
impl Timeline {
    pub(super) fn new(
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<Self, String> {
        let pool = match doc.native() {
            Some(doc) => doc.budget().clone(),
            None => ResidentBudget::new(BYTES).map_err(|e| e.to_string())?,
        };
        let bytes = project
            .len()
            .checked_add(doc.id().len())
            .and_then(|n| n.checked_add(EVENTS * std::mem::size_of::<Entry>() * 2 + 4096))
            .filter(|n| *n <= BYTES)
            .ok_or("Sample input metadata exceeds its bound")?;
        let charge = MetadataCharge::reserve(&pool, bytes, &|| Ok(()))?;
        let mut events = VecDeque::new();
        events
            .try_reserve_exact(EVENTS)
            .map_err(|_| "Sample input queue allocation refused")?;
        Ok(Self {
            source: Source {
                engine: Arc::downgrade(shared),
                project: project.into(),
                document: doc.id().into(),
                revision: doc.revision(),
                native: doc.native().map(|doc| doc.key()),
            },
            events,
            next: 0,
            nonce: egui::Id::new(crate::engine::id()),
            flight: None,
            last_sample: None,
            resolved: None,
            failure: false,
            bytes,
            pool,
            _charge: charge,
        })
    }
    fn validate(
        &self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<(), String> {
        if self.failure {
            return Err("Sample input stopped; queued input preserved".into());
        }
        if !self.source.matches(shared, project, doc) {
            return Err("Image changed while sampling; queued input preserved".into());
        }
        if let Some(doc) = doc.native() {
            if !self._charge.uses_budget(doc.budget()) {
                return Err("Sample input belongs to another budget".into());
            }
        }
        Ok(())
    }
    fn admit(&self, bytes: usize, replacing: usize) -> Result<Arc<MetadataCharge>, String> {
        if self
            .bytes
            .checked_sub(replacing)
            .and_then(|used| used.checked_add(bytes))
            .is_none_or(|used| used > BYTES)
        {
            return Err("Sample input queue is full; existing input preserved".into());
        }
        MetadataCharge::reserve(&self.pool, bytes, &|| Ok(()))
    }
    fn id(&mut self) -> Result<Id, String> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or("Sample input identifier exhausted")?;
        Ok(Id(self.nonce, self.next))
    }
    pub(super) fn sample(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
        point: [f32; 2],
        layer: &str,
        mask: bool,
        step: Option<&str>,
    ) -> Result<Enqueued, String> {
        self.validate(shared, project, doc)?;
        let point = crate::render::sample::point([doc.width(), doc.height()], point)?;
        if mask && doc.find_layer(layer).is_none() {
            return Err("Sample mask layer no longer exists".into());
        }
        let previous = self
            .events
            .back()
            .filter(|entry| matches!(entry.event, Event::Sample { .. }));
        if let Some(entry) = previous {
            if let Event::Sample {
                point: old,
                layer: target,
                mask: channel,
                step: paint,
            } = &entry.event
            {
                if *old == point && target == layer && *channel == mask && paint.as_deref() == step
                {
                    return Ok(Enqueued {
                        id: entry.id,
                        superseded: None,
                    });
                }
            }
        }
        let replacing = previous.map_or(0, |entry| entry.bytes);
        if replacing == 0 && self.events.len() == EVENTS {
            return Err("Sample input queue is full; existing input preserved".into());
        }
        let bytes = layer
            .len()
            .checked_add(step.map_or(0, str::len))
            .and_then(|n| n.checked_add(1024))
            .ok_or("Sample target metadata overflow")?;
        let charge = self.admit(bytes, replacing)?;
        let id = self.id()?;
        let entry = Entry {
            id,
            event: Event::Sample {
                point,
                layer: layer.into(),
                mask,
                step: step.map(str::to_owned),
            },
            bytes,
            _charge: charge,
        };
        let superseded = if replacing != 0 {
            let old = self.events.pop_back().unwrap();
            self.bytes -= old.bytes;
            if self.flight == Some(old.id) {
                self.flight = None;
            }
            Some(old.id)
        } else {
            None
        };
        self.bytes += bytes;
        self.events.push_back(entry);
        self.last_sample = Some(id);
        Ok(Enqueued { id, superseded })
    }
    pub(super) fn stroke(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
        command: &Value,
        label: &str,
        use_sample: bool,
    ) -> Result<Id, String> {
        self.validate(shared, project, doc)?;
        if self.events.len() == EVENTS {
            return Err("Sample input queue is full; existing input preserved".into());
        }
        let sample = self
            .last_sample
            .ok_or("A sampled stroke needs a preceding sample")?;
        if command["op"] != "paint"
            || command["document_id"].as_str() != Some(self.source.document.as_str())
            || command["source_revision"].as_u64() != Some(doc.revision())
            || command["layer"]
                .as_str()
                .and_then(|id| doc.find_layer(id))
                .is_none()
            || command["mask"].as_bool().is_none()
            || !command["color"].as_array().is_some_and(|color| {
                color.len() == 4 && color.iter().all(|v| v.as_u64().is_some_and(|v| v <= 255))
            })
        {
            return Err("Sampled stroke requires an observed paint command and RGBA color".into());
        }
        let scratch = self
            .pool
            .reserve_bytes(512, BYTES)
            .map_err(|error| error.to_string())?;
        let endpoint = crate::brush::endpoint_from_command(command)?;
        drop(scratch);
        let bytes = label
            .len()
            .checked_add(1024)
            .ok_or("Stroke label metadata overflow")?;
        let charge = self.admit(bytes, 0)?;
        let command: Admitted<Value> = copy_admitted(command, &self.pool, true, &|| Ok(()))?;
        let bytes = bytes
            .checked_add(command.owner().bytes())
            .filter(|bytes| self.bytes.checked_add(*bytes).is_some_and(|n| n <= BYTES))
            .ok_or("Sample input queue is full; existing input preserved")?;
        let id = self.id()?;
        self.events.push_back(Entry {
            id,
            event: Event::Stroke {
                command,
                label: label.into(),
                sample,
                use_sample,
                endpoint,
            },
            bytes,
            _charge: charge,
        });
        self.bytes += bytes;
        Ok(id)
    }
    pub(super) fn start(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<Option<Ready<'_>>, String> {
        self.validate(shared, project, doc)?;
        if self.flight.is_some() {
            return Ok(None);
        }
        let Some(entry) = self.events.front() else {
            return Ok(None);
        };
        let action = match &entry.event {
            Event::Sample {
                point,
                layer,
                mask,
                step,
            } => Action::Sample {
                point: *point,
                layer,
                mask: *mask,
                step: step.as_deref(),
            },
            Event::Stroke {
                command,
                label,
                sample,
                use_sample,
                endpoint,
            } => {
                let (resolved, pixel) = self
                    .resolved
                    .ok_or("Sampled stroke has no completed color")?;
                if resolved != *sample {
                    return Err("Sampled stroke dependency changed".into());
                };
                let copy = self.materialize(command, *use_sample, pixel)?;
                Action::Stroke {
                    command: copy,
                    label,
                    endpoint: *endpoint,
                }
            }
        };
        self.flight = Some(entry.id);
        Ok(Some(Ready {
            id: entry.id,
            document: &self.source.document,
            revision: self.source.revision,
            action,
        }))
    }
    fn materialize(
        &self,
        command: &Admitted<Value>,
        use_sample: bool,
        sample: Color,
    ) -> Result<Admitted<Value>, String> {
        // The sampled RGBA entry can add an array and grow the command map.
        let mask = command["mask"] == true;
        let words = if use_sample {
            let mut words = sample.words();
            if mask {
                // A mask sample changes coverage color. The brush owns stroke alpha.
                let alpha = crate::engine::raster_color(command, 16)?[3];
                let gray = words.map_or_else(|| u16::from(sample.display()[0]) * 257, |rgba| rgba[0]);
                words = (words.is_some() || alpha % 257 != 0).then_some([gray, gray, gray, alpha]);
            }
            words
        } else {
            None
        };
        let _scratch = words
            .map(|_| self.pool.reserve_bytes(1024, BYTES).map_err(|error| error.to_string()))
            .transpose()?;
        let copy: Admitted<Value> = copy_admitted(&**command, &self.pool, true, &|| Ok(()))?;
        copy.rehome_unique(|mut value, owner| {
            value["source_revision"] = Value::from(self.source.revision);
            if use_sample {
                let pixel = sample.display();
                if let Some(words) = words {
                    value["color16"] = json!(words);
                } else if let Some(fields) = value.as_object_mut() {
                    fields.remove("color16");
                }
                for index in 0..if mask { 3 } else { 4 } {
                    value["color"][index] = Value::from(if mask { pixel[0] } else { pixel[index] });
                }
            }
            let owner = if words.is_some() {
                crate::engine::native::metadata_working_charge(&value, &self.pool, &|| Ok(()))?
            } else {
                owner.clone()
            };
            Ok((value, owner))
        })
    }
    pub(super) fn preview_stamp(
        &self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<PreviewStamp, String> {
        self.validate(shared, project, doc)?;
        Ok((
            self.source.revision,
            self.events.front().map(|entry| entry.id),
            self.events.back().map(|entry| entry.id),
            self.resolved.map(|(id, _)| id),
        ))
    }
    pub(super) fn preview(
        &self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<Preview, String> {
        self.validate(shared, project, doc)?;
        let count = self
            .events
            .iter()
            .take_while(|entry| matches!(entry.event, Event::Stroke { .. }))
            .count();
        let charge = MetadataCharge::reserve(
            &self.pool,
            512 + count * std::mem::size_of::<Admitted<Value>>() * 2,
            &|| Ok(()),
        )?;
        let mut commands = Vec::new();
        commands
            .try_reserve_exact(count)
            .map_err(|_| "Sampled preview allocation refused")?;
        for entry in self.events.iter().take(count) {
            let Event::Stroke {
                command,
                sample,
                use_sample,
                ..
            } = &entry.event
            else {
                unreachable!();
            };
            let (resolved, pixel) = self
                .resolved
                .ok_or("Sampled preview needs a completed color")?;
            if resolved != *sample {
                return Err("Sampled preview dependency changed".into());
            }
            commands.push(self.materialize(command, *use_sample, pixel)?);
        }
        Ok(Preview {
            commands,
            _charge: charge,
        })
    }
    fn pop(&mut self) {
        let entry = self.events.pop_front().unwrap();
        self.bytes -= entry.bytes;
        self.flight = None;
    }
    #[cfg(test)]
    pub(super) fn sampled(
        &mut self,
        id: Id,
        pixel: Pixel,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<bool, String> {
        self.sampled_color(id, Color::Byte(pixel), shared, project, doc)
    }

    pub(super) fn sampled_color(
        &mut self,
        id: Id,
        pixel: Color,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<bool, String> {
        if self.flight != Some(id)
            || !self
                .events
                .front()
                .is_some_and(|entry| entry.id == id && matches!(entry.event, Event::Sample { .. }))
        {
            return Ok(false);
        }
        if let Err(error) = self.validate(shared, project, doc) {
            self.failed(id);
            return Err(error);
        }
        self.resolved = Some((id, pixel));
        self.pop();
        Ok(true)
    }
    pub(super) fn committed(
        &mut self,
        id: Id,
        revision: u64,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<bool, String> {
        if self.flight != Some(id)
            || !self
                .events
                .front()
                .is_some_and(|entry| entry.id == id && matches!(entry.event, Event::Stroke { .. }))
        {
            return Ok(false);
        }
        if self.source.revision.checked_add(1) != Some(revision) {
            self.failed(id);
            return Err("Stroke receipt revision differs; queued input preserved".into());
        }
        // This stroke has committed even if a later edit changed the source before
        // UI received its receipt. Never offer the published stroke for replay.
        self.pop();
        if revision != doc.revision()
            || !self.source.identity(shared, project, doc)
            || doc
                .native()
                .is_some_and(|doc| !self._charge.uses_budget(doc.budget()))
        {
            self.failure = true;
            return Err("Image changed after stroke publication; remaining input preserved".into());
        }
        self.source.revision = revision;
        self.source.native = doc.native().map(|doc| doc.key());
        Ok(true)
    }
    pub(super) fn failed(&mut self, id: Id) -> bool {
        if self.flight != Some(id) {
            return false;
        }
        self.failure = true;
        self.flight = None;
        true
    }
    pub(super) fn last_endpoint(
        &self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
        layer: &str,
        mask: bool,
        step: Option<&str>,
    ) -> Result<Option<([f32; 2], f32)>, String> {
        self.validate(shared, project, doc)?;
        Ok(self
            .events
            .iter()
            .rev()
            .find_map(|entry| match &entry.event {
                Event::Stroke {
                    command, endpoint, ..
                } => Some((command, endpoint)),
                _ => None,
            })
            .filter(|(command, _)| {
                command["layer"].as_str() == Some(layer)
                    && command["mask"].as_bool() == Some(mask)
                    && command["step"].as_str() == step
                    && !command["erase"].as_bool().unwrap_or(false)
            })
            .map(|(_, endpoint)| *endpoint))
    }
    pub(super) fn awaiting_publication(
        &self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> bool {
        self.source.identity(shared, project, doc)
            && self.source.revision.checked_add(1) == Some(doc.revision())
            && self.events.front().is_some_and(|entry| {
                self.flight == Some(entry.id) && matches!(entry.event, Event::Stroke { .. })
            })
    }
    pub(super) fn revision(&self) -> u64 {
        self.source.revision
    }
    pub(super) fn document(&self) -> &str {
        &self.source.document
    }
    pub(super) fn has_strokes(&self) -> bool {
        self.events
            .iter()
            .any(|entry| matches!(entry.event, Event::Stroke { .. }))
    }
    pub(super) fn is_failed(&self) -> bool {
        self.failure
    }
    pub(super) fn pending(&self) -> bool {
        !self.events.is_empty()
    }
    pub(super) fn current_color(
        &self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
    ) -> Result<Option<Pixel>, String> {
        self.validate(shared, project, doc)?;
        Ok(self
            .resolved
            .filter(|(id, _)| Some(*id) == self.last_sample)
            .map(|(_, pixel)| pixel.display()))
    }
}

#[cfg(test)]
#[path = "sample_input_tests.rs"]
mod tests;
