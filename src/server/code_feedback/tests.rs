use super::*;
use crate::{
    code::native_fixture::{observed, project, stage},
    engine::{authority::DocumentAuthority, native::worker::tests::Fixture, Engine},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;
use std::cell::Cell;

#[test]
fn committed_native_feedback_matches_pixels_and_survives_repeated_owned_status_handoffs() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, workspace, original, job) = project(&runtime, depth);
        let source = observed(&shared);
        let command = json!({"op":"layer.update","layer":original.layers[0].id,"opacity":0.25});
        let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
        let (width, height, expected, rect) = expected.preview(None, 96, None, false).unwrap();
        let prepared = Preparation::new(&source, 96, &|| Ok(())).unwrap();
        let reply = crate::server::edit_code_shared(
            &shared,
            &job,
            &source,
            &[command],
            std::time::Instant::now(),
        )
        .unwrap();
        assert_eq!(reply.native_key(), Some(observed(&shared).key()));
        let check = || {
            assert!(shared.try_lock().is_ok());
            assert!(workspace.try_lock().is_ok());
            Ok(())
        };
        let completion = prepared.finish(&shared, reply, &check);
        let image = completion
            .image
            .as_ref()
            .expect("Complete current native PNG");
        assert_eq!(
            (image.width, image.height, image.document_rect),
            (width, height, rect)
        );
        assert_eq!(image.revision, 1);
        let encoded: Vec<_> = image
            .bytes
            .parts()
            .flat_map(|part| part.iter().copied())
            .collect();
        let png = image::load_from_memory(&encoded).unwrap().to_rgba8();
        assert_eq!(png.as_raw(), &expected);
        let retained = Arc::downgrade(&image.bytes);
        let mut packets = Vec::new();
        for tool in [false, true] {
            let mut admission = ReplyAdmission::new(true);
            let template = completion
                .response(job.declared_scopes(), &mut admission)
                .unwrap();
            assert!(template["images"][0]["data"].is_null());
            assert_eq!(template["result"]["revision"], 1);
            assert_eq!(template["result"]["document_id"], original.id);
            assert_eq!(template["images"][0]["revision"], 1);
            let packet = admission.finish(template).unwrap();
            packets.push((
                tool,
                if tool {
                    packet.tool_result().unwrap()
                } else {
                    packet
                },
            ));
        }
        drop(completion);
        assert!(retained.upgrade().is_some());
        for (tool, packet) in &packets {
            let response = serde_json::to_value(packet).unwrap();
            let image = if *tool {
                &response["content"][1]
            } else {
                &response["images"][0]
            };
            assert_eq!(
                STANDARD.decode(image["data"].as_str().unwrap()).unwrap(),
                encoded
            );
            if *tool {
                assert_eq!(image["mimeType"], "image/png");
                let metadata: Value =
                    serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
                assert_eq!(metadata["views"][0]["document_rect"], json!(rect));
                assert_eq!(metadata["views"][0]["revision"], 1);
                assert_eq!(metadata["result"]["document_id"], original.id);
            } else {
                assert_eq!(image["document_rect"], json!(rect));
            }
        }
        packets.pop();
        assert!(retained.upgrade().is_some());
        drop(packets);
        assert!(retained.upgrade().is_none());
        assert_eq!(shared.lock().unwrap().history_lengths(), [1, 0]);
    }
}

#[test]
fn feedback_failures_and_callback_panics_keep_the_committed_receipt_and_human_state() {
    for failure in [
        "callback", "panic", "changed", "nominal", "service", "quota", "late",
    ] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, workspace, original, job) = project(&runtime, 16);
        let source = observed(&shared);
        let prepared = Preparation::new(&source, 64, &|| Ok(())).unwrap();
        let command = json!({"op":"layer.update","layer":original.layers[0].id,"opacity":0.5});
        let reply = crate::server::edit_code_shared(
            &shared,
            &job,
            &source,
            &[command.clone()],
            std::time::Instant::now(),
        )
        .unwrap();
        assert_eq!(reply.data()["revision"], 1);
        let mut held = None;
        match failure {
            "changed" => {
                crate::server::edit_shared(
                    &shared,
                    "human",
                    &[json!({"op":"layer.update","layer":original.layers[0].id,"visible":false})],
                    Some(1),
                    None,
                    "Human visibility",
                    None,
                    Some(&original.id),
                )
                .unwrap();
            }
            "nominal" => {
                let mut replacement = Engine::preview_edits(original.clone(), &[command]).unwrap();
                replacement.revision = 1;
                let replacement = stage(&runtime, &replacement);
                assert_eq!(replacement.metadata().id(), original.id);
                assert_eq!(replacement.metadata().revision(), 1);
                assert_ne!(Some(replacement.key()), reply.native_key());
                shared.lock().unwrap().authority =
                    DocumentAuthority::from_native(replacement, 8).unwrap();
            }
            "service" => workspace.lock().unwrap().source_runtime = None,
            "quota" => {
                let pool = runtime.resident();
                held = Some(
                    MetadataCharge::reserve(&pool, pool.limit() - pool.used(), &|| Ok(())).unwrap(),
                );
            }
            _ => (),
        }
        let current = observed(&shared).key();
        let history = shared.lock().unwrap().history_lengths();
        let calls = Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            if failure == "callback" || (failure == "late" && calls.get() > 8) {
                return Err("Feedback interrupted".into());
            }
            if failure == "panic" {
                panic!("Feedback callback panic");
            }
            Ok(())
        };
        let completion = prepared.finish(&shared, reply, &check);
        drop(held);
        assert!(completion.image.is_none(), "{failure}");
        assert_eq!(completion.data()["revision"], 1);
        assert_eq!(observed(&shared).key(), current);
        assert_eq!(shared.lock().unwrap().history_lengths(), history);
        let mut admission = ReplyAdmission::new(true);
        let response = completion
            .response(job.declared_scopes(), &mut admission)
            .unwrap();
        assert_eq!(response["result"]["revision"], 1);
        assert_eq!(response["result"]["feedback_warning"], WARNING);
        assert_eq!(response["images"], json!([]));
        workspace.lock().unwrap().source_runtime = Some(runtime.clone());
    }
}

#[test]
fn code_completion_refuses_detached_or_foreign_replies_without_losing_its_image_owner() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
    let (shared, _workspace, original, job) = project(&runtime, 16);
    let source = observed(&shared);
    for edge in [0, 31, 1025, u32::MAX] {
        assert!(Preparation::new(&source, edge, &|| Ok(())).is_err());
    }
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    let prepared = Preparation::new(&source, 64, &|| Ok(())).unwrap();
    let reply = crate::server::edit_code_shared(
        &shared,
        &job,
        &source,
        &[json!({"op":"layer.update","layer":original.layers[0].id,"opacity":0.5})],
        std::time::Instant::now(),
    )
    .unwrap();
    let completion = prepared.finish(&shared, reply, &|| Ok(()));
    let bytes = Arc::downgrade(&completion.image.as_ref().unwrap().bytes);
    assert!(completion
        .response(job.declared_scopes(), &mut ReplyAdmission::new(false))
        .is_err());
    let other = ResidentBudget::new(1024 * 1024).unwrap();
    let mut admission = ReplyAdmission::new(true);
    admission.admit(&other, 4096).unwrap();
    assert!(completion
        .response(job.declared_scopes(), &mut admission)
        .is_err());
    assert!(bytes.upgrade().is_some());
    assert_eq!(shared.lock().unwrap().history_lengths(), [1, 0]);
}
