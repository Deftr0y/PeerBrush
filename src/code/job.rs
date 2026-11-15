//! Immutable job metadata is shared; native receipts and PNG owners never turn
//! into detached JSON inside the retained job record.
use super::*;
use crate::{
    engine::{native::DocumentKey, runtime_value::RuntimeValue},
    large_document::ResidentBudget,
    psd::memory36::MetadataCharge,
    server::{code_feedback::Completion, reply::ReplyAdmission},
};
use serde::{ser::SerializeSeq, Serialize, Serializer};
use std::ops::Deref;

pub(crate) struct Info {
    pub(super) id: String,
    pub(super) actor: String,
    pub(super) document: String,
    pub(super) task: String,
    pub(super) scopes: Vec<Scope>,
    pub(super) label: String,
    pub(super) native: Option<Ownership>,
}
pub(super) struct Ownership {
    pub(super) pool: ResidentBudget,
    pub(super) _charge: Arc<MetadataCharge>,
}
pub(super) enum Outcome {
    Resident(Value),
    Native(Arc<Completion>),
    Published(Published),
}
#[derive(Clone, Copy)]
pub(super) struct Published {
    pub(super) key: DocumentKey,
    pub(super) revision: u64,
    pub(super) applied: usize,
}
pub(super) struct Running;
impl Running {
    pub(super) fn acquire() -> Result<Self, String> {
        RUNNING
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                (count < 2).then_some(count + 1)
            })
            .map(|_| Self)
            .map_err(|_| "Two code runs are already active; wait for completion".into())
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Clone)]
pub(crate) struct Job {
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) result: Arc<Mutex<Option<Result<Outcome, String>>>>,
    published: Arc<Mutex<Option<Published>>>,
    pub(super) info: Arc<Info>,
}
impl Deref for Job {
    type Target = Info;
    fn deref(&self) -> &Info {
        &self.info
    }
}
impl Job {
    pub(super) fn new(info: Info) -> Self {
        Self {
            info: Arc::new(info),
            cancel: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
            published: Arc::new(Mutex::new(None)),
        }
    }
    pub(crate) fn attribution(&self) -> (&str, &str, &str) {
        (&self.actor, &self.task, &self.label)
    }
    pub(crate) fn declared_scopes(&self) -> &[Scope] {
        &self.scopes
    }
    pub(crate) fn check_edit(
        &self,
        engine: &mut Engine,
        source: DocumentView<'_>,
        started: Instant,
    ) -> Result<(), String> {
        if started.elapsed() >= Duration::from_secs(30) {
            return Err("Code preparation time limit exceeded; no edits committed".into());
        }
        guard(engine, self, source)
    }
    pub(crate) fn check_worker(
        &self,
        shared: &Shared,
        source: DocumentView<'_>,
        started: Instant,
    ) -> Result<(), String> {
        let mut engine = shared
            .try_lock()
            .map_err(|_| "Code source is busy; preparation deferred")?;
        self.check_edit(&mut engine, source, started)
    }
    /// Called only after actual native publication, while Engine still fences
    /// the winning state. This record never grants source or filesystem access.
    pub(crate) fn record_applied(&self, key: DocumentKey, revision: u64, applied: usize) {
        *self.published.lock().unwrap_or_else(|e| e.into_inner()) = Some(Published {
            key,
            revision,
            applied,
        });
    }
    pub(super) fn recover_result(
        &self,
        result: Result<Outcome, String>,
    ) -> Result<Outcome, String> {
        if result.is_err() {
            if let Some(receipt) = *self.published.lock().unwrap_or_else(|e| e.into_inner()) {
                return Ok(Outcome::Published(receipt));
            }
        }
        result
    }
    pub(super) fn state(&self, admission: &mut ReplyAdmission) -> Result<Value, String> {
        let result = self.result.lock().unwrap_or_else(|e| e.into_inner());
        let published = *self.published.lock().unwrap_or_else(|e| e.into_inner());
        let fallback = published
            .filter(|_| result.is_none())
            .map(|receipt| Ok(Outcome::Published(receipt)));
        let result = result.as_ref().or(fallback.as_ref());
        let status = match result {
            None => "running",
            Some(Ok(_)) => "committed",
            Some(Err(_)) => "discarded",
        };
        #[derive(Serialize)]
        struct Header<'a> {
            run: &'a str,
            actor: &'a str,
            document_id: &'a str,
            task: &'a str,
            description: &'a str,
            scopes: &'a [Scope],
            status: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            error: Option<&'a str>,
        }
        let header = Header {
            run: &self.id,
            actor: &self.actor,
            document_id: &self.document,
            task: &self.task,
            description: &self.label,
            scopes: &self.scopes,
            status,
            error: result.and_then(|r| r.as_ref().err()).map(String::as_str),
        };
        let pool = self
            .native
            .as_ref()
            .map(|owner| &owner.pool)
            .or_else(|| admission.pool_option());
        let pool = pool.cloned();
        if let Some(pool) = &pool {
            admission.admit_projection(pool, &header)?;
        }
        let mut state = serde_json::to_value(header).map_err(|e| e.to_string())?;
        match result {
            Some(Ok(Outcome::Resident(value))) => {
                if let Some(pool) = &pool {
                    admission.admit_projection(pool, value)?;
                }
                let mut value = value.clone();
                let images = value
                    .as_object_mut()
                    .ok_or("Code result must be an object")?
                    .remove("images")
                    .unwrap_or_else(|| json!([]));
                state["result"] = value;
                state["images"] = images;
            }
            Some(Ok(Outcome::Native(completion))) => {
                let projection = completion.response(&self.scopes, admission)?;
                let Value::Object(projection) = projection else {
                    return Err("Code completion must be an object".into());
                };
                state.as_object_mut().unwrap().extend(projection);
            }
            Some(Ok(Outcome::Published(receipt))) => {
                #[derive(Serialize)]
                struct Applied<'a> {
                    revision: u64,
                    applied: usize,
                    document_id: &'a str,
                    scopes: &'a [Scope],
                    feedback_warning: &'static str,
                }
                let view = Applied {
                    revision: receipt.revision,
                    applied: receipt.applied,
                    document_id: &self.document,
                    scopes: &self.scopes,
                    feedback_warning:
                        "Edit applied; canvas feedback is unavailable. Observe the current project.",
                };
                if let Some(pool) = &pool {
                    admission.admit_projection(pool, &view)?;
                }
                state["result"] = serde_json::to_value(view).map_err(|e| e.to_string())?;
                state["images"] = json!([]);
            }
            _ => (),
        }
        Ok(state)
    }
    #[cfg(test)]
    pub(super) fn info_mut(&mut self) -> &mut Info {
        Arc::get_mut(&mut self.info).expect("Unshared fixture metadata")
    }
}
#[cfg(test)]
mod tests;
pub(super) struct Summaries<'a>(pub(super) &'a [Job]);
impl Serialize for Summaries<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Summary<'a> {
            run: &'a str,
            actor: &'a str,
            document_id: &'a str,
            description: &'a str,
            scopes: &'a [Scope],
            running: bool,
            status: &'static str,
            error: Option<&'a str>,
        }
        let mut output = serializer.serialize_seq(Some(self.0.len()))?;
        for job in self.0 {
            let result = job.result.lock().unwrap_or_else(|e| e.into_inner());
            let published = job
                .published
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            output.serialize_element(&Summary {
                run: &job.id,
                actor: &job.actor,
                document_id: &job.document,
                description: &job.label,
                scopes: &job.scopes,
                running: result.is_none() && !published,
                status: if published {
                    "Applied"
                } else {
                    match result.as_ref() {
                        None => "Working",
                        Some(Ok(_)) => "Applied",
                        Some(Err(_)) => "Discarded",
                    }
                },
                error: result
                    .as_ref()
                    .and_then(|r| r.as_ref().err())
                    .map(String::as_str),
            })?;
        }
        output.end()
    }
}
impl Engine {
    pub(crate) fn has_running_code(&self) -> bool {
        self.code_jobs.iter().any(|job| {
            job.result
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        })
    }
    pub(crate) fn code_states(&self) -> Result<RuntimeValue<Value>, String> {
        let summaries = Summaries(&self.code_jobs);
        let pool = self
            .native_document()
            .map(|document| document.budget())
            .or_else(|| {
                self.code_jobs
                    .iter()
                    .find_map(|job| job.native.as_ref().map(|owner| &owner.pool))
            });
        match pool {
            Some(pool) => RuntimeValue::project(&summaries, pool, &|| Ok(())),
            None => serde_json::to_value(summaries)
                .map(Into::into)
                .map_err(|e| e.to_string()),
        }
    }
    pub(crate) fn cancel_code(&self, id: &str) {
        if let Some(job) = self.code_jobs.iter().find(|job| job.id == id) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}
