//! Cancellable per-output publication. Completed exports are receipts; originals stay editable.
use super::{Capture, Control, Output, Settings};
use crate::server::Shared;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Existing {
    #[default]
    Error,
    Skip,
    Replace,
}
#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    pub output: Output,
    pub path: PathBuf,
    pub status: String,
    pub bytes: Option<usize>,
    pub message: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct State {
    pub run: String,
    pub actor: String,
    pub project_id: String,
    pub document_id: String,
    pub revision: u64,
    pub status: String,
    pub stage: String,
    pub outputs: Vec<Receipt>,
    pub error: Option<String>,
}
#[derive(Clone)]
pub(crate) struct Job {
    pub id: String,
    actor: String,
    control: Control,
    pub state: Arc<Mutex<State>>,
}
pub fn states(library: &Arc<Mutex<super::library::Library>>) -> Vec<State> {
    let jobs = library.lock().unwrap().jobs.clone();
    jobs.iter()
        .map(|j| j.state.lock().unwrap().clone())
        .collect()
}
pub fn cancel_all(library: &Arc<Mutex<super::library::Library>>) {
    for job in &library.lock().unwrap().jobs {
        job.control.cancel();
    }
}
pub fn state(library: &Arc<Mutex<super::library::Library>>, id: &str) -> Result<State, String> {
    let job = library
        .lock()
        .unwrap()
        .jobs
        .iter()
        .find(|j| j.id == id)
        .cloned()
        .ok_or("Export run is missing")?;
    let state = job.state.lock().unwrap().clone();
    Ok(state)
}

pub fn cancel(
    library: &Arc<Mutex<super::library::Library>>,
    id: &str,
    actor: &str,
) -> Result<State, String> {
    let job = library
        .lock()
        .unwrap()
        .jobs
        .iter()
        .find(|j| j.id == id)
        .cloned()
        .ok_or("Export run is missing")?;
    if actor != "human" && actor != job.actor {
        return Err("Only the export owner or human input may cancel this run".into());
    }
    job.control.cancel();
    state(library, id)
}

fn launch<F: FnOnce() + Send + 'static>(
    library: &Arc<Mutex<super::library::Library>>,
    job: Job,
    worker: F,
    spawn: impl FnOnce(F) -> std::io::Result<std::thread::JoinHandle<()>>,
) -> Result<(), String> {
    let mut library = library.lock().unwrap();
    if library
        .jobs
        .iter()
        .any(|j| j.state.lock().unwrap().status == "running")
    {
        return Err("Finish or cancel the current export batch first".into());
    }
    // Keep the slot exclusive while launching. A refused launch must not
    // publish a running job or evict a completed receipt.
    drop(spawn(worker).map_err(|error| format!("Could not start export batch: {error}"))?);
    while library.jobs.len() >= 8 {
        library.jobs.remove(0);
    }
    library.jobs.push(job);
    Ok(())
}

pub fn start(
    shared: &Shared,
    request: &Value,
    settings: Settings,
    targets: Vec<String>,
    directory: PathBuf,
    existing: Existing,
    actor: &str,
) -> Result<State, String> {
    start_guarded(
        shared,
        |engine| {
            crate::workspace::validate_target(engine, request)?;
            let document = request["document_id"]
                .as_str()
                .ok_or("Export requires the observed document_id")?;
            let revision = request["expected_revision"]
                .as_u64()
                .ok_or("Export requires the observed expected_revision")?;
            crate::workspace::guard(engine, document, revision)
        },
        settings,
        targets,
        directory,
        existing,
        actor,
    )
}

pub(super) fn start_guarded(
    shared: &Shared,
    validate_source: impl FnOnce(&crate::engine::Engine) -> Result<(), String>,
    settings: Settings,
    targets: Vec<String>,
    directory: PathBuf,
    existing: Existing,
    actor: &str,
) -> Result<State, String> {
    let operation = crate::server::FileOperation::begin(shared)?;
    settings.validate()?;
    if actor.trim().is_empty() || actor.len() > 64 || actor.chars().any(char::is_control) {
        return Err("Use a readable export actor of up to 64 characters".into());
    }
    let directory = std::fs::canonicalize(directory)
        .map_err(|e| format!("Choose an existing export directory: {e}"))?;
    if !directory.is_dir() {
        return Err("Choose an existing export directory".into());
    }
    let (capture, project, library) = {
        let engine = shared.lock().unwrap();
        validate_source(&engine)?;
        (
            Capture::new(&engine)?,
            engine.project_id.clone(),
            engine.export_library.clone(),
        )
    };
    let control = Control::default();
    let id = format!("export-{}", crate::engine::id());
    let doc = capture.view();
    let state = State {
        run: id.clone(),
        actor: actor.into(),
        project_id: project,
        document_id: doc.id().to_owned(),
        revision: doc.revision(),
        status: "running".into(),
        stage: "Planning native output".into(),
        outputs: vec![],
        error: None,
    };
    let job = Job {
        id: id.clone(),
        actor: actor.into(),
        control: control.clone(),
        state: Arc::new(Mutex::new(state.clone())),
    };
    let registered = job.clone();
    let shared = shared.clone();
    let worker = move || {
        let _operation = operation;
        let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||->Result<(),String> {
            let plan=capture.plan(&shared,&targets,&settings,&control)?;
            capture.validate(&shared.lock().unwrap())?;
            {
                let mut state=job.state.lock().unwrap();
                state.outputs=plan.outputs.iter().map(|output|Receipt {output:output.clone(),path:directory.join(&output.filename),status:"not_written".into(),bytes:None,message:None}).collect();
            }
            for (index,output) in plan.outputs.iter().enumerate() {
                control.check()?;
                job.state.lock().unwrap().stage=format!("Rendering {} ({}/{})",output.filename,index+1,plan.outputs.len());
                let path=directory.join(&output.filename);
                let published=crate::server::export_planned_to_guarded(
                    &shared,&path,&json!({"document_id":plan.document_id,"expected_revision":plan.revision,"overwrite":existing==Existing::Replace,"skip_existing":existing==Existing::Skip}),
                    &control,output,&settings,capture.seed(),
                );
                let published=match published {
                    Ok(published)=>published,
                    Err(error)=>{let mut state=job.state.lock().unwrap();state.outputs[index].status="failed".into();state.outputs[index].message=Some(error.clone());return Err(error);}
                };
                let mut state=job.state.lock().unwrap();let receipt=&mut state.outputs[index];
                receipt.status=if published.is_some() {"written"} else {"skipped"}.into();receipt.bytes=published;
                if published.is_none() {receipt.message=Some("Existing destination retained".into());}
            }
            Ok(())
        })).unwrap_or_else(|_|Err("Export could not complete; source remains unchanged".into()));
        let mut state = job.state.lock().unwrap();
        match result {
            Ok(()) => {
                state.status = "completed".into();
                state.stage = "Export complete".into();
            }
            Err(error) => {
                state.status = if control.check().is_err() {
                    "canceled"
                } else {
                    "failed"
                }
                .into();
                state.stage = "Export stopped".into();
                state.error = Some(error.clone());
                for receipt in &mut state.outputs {
                    if receipt.status == "not_written" {
                        receipt.message = Some("Not attempted after export stopped".into());
                    }
                }
            }
        }
    };
    launch(&library, registered, worker, |worker| {
        std::thread::Builder::new()
            .name("peerbrush-export-batch".into())
            .spawn(worker)
    })?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn job(id: String, status: &str) -> Job {
        Job {
            id: id.clone(),
            actor: "human".into(),
            control: Control::default(),
            state: Arc::new(Mutex::new(State {
                run: id,
                actor: "human".into(),
                project_id: "project".into(),
                document_id: "document".into(),
                revision: 0,
                status: status.into(),
                stage: String::new(),
                outputs: vec![],
                error: None,
            })),
        }
    }

    #[test]
    fn refused_worker_keeps_receipts_and_releases_the_batch_slot_and_file_operation() {
        let library = Arc::new(Mutex::new(super::super::library::Library::default()));
        library.lock().unwrap().jobs = (0..8)
            .map(|index| job(index.to_string(), "completed"))
            .collect();
        let counter = Arc::new(AtomicUsize::new(0));
        let operation = crate::server::FileOperation::track(counter.clone());
        let refused = launch(
            &library,
            job("refused".into(), "running"),
            move || drop(operation),
            |_| Err(std::io::Error::other("Worker creation refused")),
        );
        assert!(refused.unwrap_err().contains("Worker creation refused"));
        assert_eq!(counter.load(Ordering::Acquire), 0);
        let runs = || {
            states(&library)
                .into_iter()
                .map(|state| state.run)
                .collect::<Vec<_>>()
        };
        assert_eq!(runs(), (0..8).map(|index| index.to_string()).collect::<Vec<_>>());

        launch(&library, job("retry".into(), "running"), || {}, |worker| {
            std::thread::Builder::new().spawn(worker)
        })
        .unwrap();
        assert_eq!(runs(), ["1", "2", "3", "4", "5", "6", "7", "retry"]);
        let overlapping = launch(&library, job("overlap".into(), "running"), || {}, |_| {
            panic!("A competing batch must not start a worker");
        });
        assert!(overlapping.unwrap_err().contains("current export batch"));
    }
}
