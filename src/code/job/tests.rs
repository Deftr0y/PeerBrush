use super::*;
use crate::{
    code::native_fixture::{observed, project},
    engine::native::worker::tests::Fixture,
    server::code_feedback::Preparation,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};

fn arguments(shared: &Shared, doc: &Document, job: &Job, action: &str) -> Value {
    json!({"action":action,"actor":job.actor,"project_id":shared.lock().unwrap().project_id,
        "document_id":doc.id,"run":job.id,"expected_revision":0,"task":job.task,
        "description":job.label,"scopes":job.scopes,"script":"edit(#{op:\"selection.clear\"});"})
}

#[test]
fn native_job_clones_share_admitted_metadata_and_owned_status_replies_retain_the_png() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, _workspace, doc, mut job) = project(&runtime, depth);
        let source = observed(&shared);
        let args = arguments(&shared, &doc, &job, "status");
        job.info_mut().native = Some(native::ownership(&source, &args).unwrap());
        let used = runtime.resident().used();
        let clone = job.clone();
        assert!(Arc::ptr_eq(&clone.info, &job.info));
        assert_eq!(clone.scopes.as_ptr(), job.scopes.as_ptr());
        assert_eq!(runtime.resident().used(), used);
        let prepared = Preparation::new(&source, 64, &|| Ok(())).unwrap();
        let reply = crate::server::edit_code_shared(
            &shared,
            &job,
            &source,
            &[json!({"op":"layer.update","layer":doc.layers[0].id,"opacity":0.5})],
            Instant::now(),
        )
        .unwrap();
        let completion = Arc::new(prepared.finish(&shared, reply, &|| Ok(())));
        let retained = Arc::downgrade(&completion);
        *job.result.lock().unwrap() = Some(Ok(Outcome::Native(completion)));
        shared.lock().unwrap().code_jobs.push(job.clone());
        assert!(crate::server::dispatch(&shared, "code", &args).is_err());
        let mut foreign = args.clone();
        foreign["actor"] = json!("another-agent");
        assert!(crate::server::dispatch_owned(&shared, "code", &foreign).is_err());
        let mut packets = Vec::new();
        for _ in 0..2 {
            let packet = crate::server::dispatch_owned(&shared, "code", &args).unwrap();
            assert_eq!(packet.value()["status"], "committed");
            assert_eq!(packet.value()["result"]["revision"], 1);
            assert!(packet.value()["images"][0]["data"].is_null());
            packets.push(packet);
        }
        let state = shared.lock().unwrap().code_states().unwrap();
        assert_eq!(state[0]["status"], "Applied");
        assert_eq!(state[0]["running"], false);
        shared.lock().unwrap().code_jobs.clear();
        drop(job);
        drop(clone);
        assert!(retained.upgrade().is_none());
        let first = serde_json::to_value(&packets[0]).unwrap();
        let second = serde_json::to_value(&packets[1]).unwrap();
        assert_eq!(first["images"], second["images"]);
        let png = STANDARD
            .decode(first["images"][0]["data"].as_str().unwrap())
            .unwrap();
        assert!(image::load_from_memory(&png).is_ok());
        assert_eq!(
            first["images"][0]["document_rect"],
            json!([0, 0, doc.width, doc.height])
        );
    }
}

#[test]
fn actual_publication_survives_a_later_error_and_pending_feedback_is_already_applied() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
    let (shared, _workspace, doc, mut job) = project(&runtime, 16);
    let source = observed(&shared);
    let args = arguments(&shared, &doc, &job, "status");
    job.info_mut().native = Some(native::ownership(&source, &args).unwrap());
    crate::server::edit_code_shared(
        &shared,
        &job,
        &source,
        &[json!({"op":"layer.update","layer":doc.layers[0].id,"opacity":0.5})],
        Instant::now(),
    )
    .unwrap();
    shared.lock().unwrap().code_jobs.push(job.clone());
    assert!(shared.lock().unwrap().has_running_code());
    let pending = crate::server::dispatch_owned(&shared, "code", &args).unwrap();
    assert_eq!(pending.value()["status"], "committed");
    assert_eq!(pending.value()["result"]["revision"], 1);
    let recovered = job
        .recover_result(Err("Completion panicked after publication".into()))
        .unwrap();
    let Outcome::Published(receipt) = recovered else {
        panic!("Missing actual publication receipt")
    };
    assert_eq!(receipt.key, observed(&shared).key());
    *job.result.lock().unwrap() = Some(Ok(Outcome::Published(receipt)));
    let result = crate::server::dispatch_owned(&shared, "code", &args).unwrap();
    assert_eq!(result.value()["status"], "committed");
    assert_eq!(result.value()["result"]["applied"], 1);
    assert_eq!(result.value()["images"], json!([]));
    assert_eq!(shared.lock().unwrap().history_lengths(), [1, 0]);
    assert!(!shared.lock().unwrap().has_running_code());
}

#[test]
fn native_start_refuses_detached_transport_invalid_scopes_or_missing_service_before_enqueuing() {
    for failure in ["plain", "scopes", "unknown", "service"] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, workspace, doc, job) = project(&runtime, 16);
        let mut args = arguments(&shared, &doc, &job, "start");
        args.as_object_mut().unwrap().remove("run");
        match failure {
            "scopes" => args["scopes"] = json!(vec![json!({"target":null}); 17]),
            "unknown" => args["unknown"] = json!(true),
            "service" => workspace.lock().unwrap().source_runtime = None,
            _ => (),
        }
        let key = observed(&shared).key();
        let result = if failure == "plain" {
            crate::server::dispatch(&shared, "code", &args)
        } else {
            crate::server::dispatch_owned(&shared, "code", &args).map(|_| Value::Null)
        };
        assert!(result.is_err(), "{failure}");
        assert!(shared.lock().unwrap().code_jobs.is_empty());
        assert_eq!(observed(&shared).key(), key);
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    }
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn native_start_is_queued_and_execution_capacity_refusal_never_falls_back_to_resident_edits() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        // Enough for source/headers, deliberately below the protected child's
        // 512 MiB admission. No application child executable is needed here.
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, _workspace, doc, job) = project(&runtime, depth);
        let mut args = arguments(&shared, &doc, &job, "start");
        args.as_object_mut().unwrap().remove("run");
        args["script"] = json!(format!(
            "edit(#{{op:\"layer.update\",layer:\"{}\",opacity:0.5}});",
            doc.layers[0].id
        ));
        let source = observed(&shared);
        let queued = crate::server::dispatch_owned(&shared, "code", &args).unwrap();
        assert_eq!(queued.value()["status"], "running");
        let run = queued.value()["run"].as_str().unwrap().to_owned();
        assert_eq!(
            shared.lock().unwrap().ai_change.as_ref().unwrap().actor,
            job.actor
        );
        let mut status = arguments(&shared, &doc, &job, "status");
        status["run"] = json!(run);
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let response = crate::server::dispatch_owned(&shared, "code", &status).unwrap();
            if response.value()["status"] != "running" {
                assert_eq!(response.value()["status"], "discarded");
                break;
            }
            assert!(Instant::now() < until, "Native code result did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        loop {
            let engine = shared.lock().unwrap();
            if engine.file_operations.load(Ordering::Acquire) == 0 {
                break;
            }
            drop(engine);
            assert!(
                Instant::now() < until,
                "Native code file-operation owner did not drain"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(observed(&shared).key(), source.key());
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        assert!(!shared.lock().unwrap().has_running_code());
    }
}
