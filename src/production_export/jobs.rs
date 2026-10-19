//! Cancellable per-output publication. Completed PNGs are receipts; originals stay editable.
use super::{Control, Output, Settings};
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
pub fn start(
    shared: &Shared,
    request: &Value,
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
    let (doc, project, library) = {
        let engine = shared.lock().unwrap();
        crate::workspace::validate_target(&engine, request)?;
        let document = request["document_id"]
            .as_str()
            .ok_or("Export requires the observed document_id")?;
        let revision = request["expected_revision"]
            .as_u64()
            .ok_or("Export requires the observed expected_revision")?;
        crate::workspace::guard(&engine, document, revision)?;
        (
            engine.doc.clone(),
            engine.project_id.clone(),
            engine.export_library.clone(),
        )
    };
    let control = Control::default();
    let id = format!("export-{}", crate::engine::id());
    let state = State {
        run: id.clone(),
        actor: actor.into(),
        project_id: project,
        document_id: doc.id.clone(),
        revision: doc.revision,
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
    {
        let mut library = library.lock().unwrap();
        if library
            .jobs
            .iter()
            .any(|j| j.state.lock().unwrap().status == "running")
        {
            return Err("Finish or cancel the current export batch first".into());
        }
        while library.jobs.len() >= 8 {
            library.jobs.remove(0);
        }
        library.jobs.push(job.clone());
    }
    let shared = shared.clone();
    std::thread::spawn(move || {
        let _operation = operation;
        let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||->Result<(),String> {
            let plan=super::plan(&doc,&targets,&settings,&control)?;
            {
                let mut state=job.state.lock().unwrap();
                state.outputs=plan.outputs.iter().map(|output|Receipt {output:output.clone(),path:directory.join(&output.filename),status:"not_written".into(),bytes:None,message:None}).collect();
            }
            for (index,output) in plan.outputs.iter().enumerate() {
                control.check()?;
                job.state.lock().unwrap().stage=format!("Rendering {} ({}/{})",output.filename,index+1,plan.outputs.len());
                let path=directory.join(&output.filename);
                let mut byte_count=0;
                let published=crate::server::export_derived_guarded(&shared,&path,&json!({"document_id":doc.id,"expected_revision":doc.revision,"overwrite":existing==Existing::Replace,"skip_existing":existing==Existing::Skip}),&control, |source| {
                    let bytes=super::render(source,output,&settings,&control)?;byte_count=bytes.len();Ok(bytes)
                });
                let published=match published {
                    Ok(published)=>published,
                    Err(error)=>{let mut state=job.state.lock().unwrap();state.outputs[index].status="failed".into();state.outputs[index].message=Some(error.clone());return Err(error);}
                };
                let mut state=job.state.lock().unwrap();let receipt=&mut state.outputs[index];
                receipt.status=if published {"written"} else {"skipped"}.into();receipt.bytes=published.then_some(byte_count);
                if !published {receipt.message=Some("Existing destination retained".into());}
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
    });
    Ok(state)
}
