use super::*;
use crate::{
    code::native_source::{
        tests::{observed, project, stage},
        NativeSource,
    },
    engine::native::worker::tests::Fixture,
    engine::{authority::DocumentAuthority, native::Direction},
};

fn replacement(layer: &str, depth: u16, rect: [i32; 4]) -> (Value, [u16; 4]) {
    let words = if depth == 16 {
        [64237u16, 53141, 42043, 0]
    } else {
        [17u16, 61, 129, 0]
    };
    let bytes: Vec<_> = if depth == 16 {
        words.into_iter().flat_map(u16::to_le_bytes).collect()
    } else {
        words.into_iter().map(|word| word as u8).collect()
    };
    (
        json!({"op":"pixels.replace","layer":layer,"rect":rect,"bit_depth":depth,"rgba":STANDARD.encode(bytes)}),
        words,
    )
}
fn pixel(shared: &Shared, job: &Job, layer: &str, point: [INT; 2]) -> [u16; 4] {
    let document = observed(shared);
    NativeSource::capture(shared, job, &document, &|| Ok(()))
        .unwrap()
        .pixel(shared, job, layer, point[0], point[1], &|| Ok(()))
        .unwrap()
}

#[test]
fn code_publication_preserves_native_channels_attribution_and_one_undo_step() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, _workspace, original, job) = project(&runtime, depth);
        let source = observed(&shared);
        let layer = &original.layers[0].id;
        let before = pixel(&shared, &job, layer, [8, 2]);
        let untouched = pixel(&shared, &job, layer, [262, 252]);
        let (replace, words) = replacement(layer, depth, [8, 2, 9, 3]);
        let commands = [
            replace,
            json!({"op":"layer.update","layer":layer,"opacity":0.5}),
        ];
        let reply =
            crate::server::edit_code_shared(&shared, &job, &source, &commands, Instant::now())
                .unwrap();
        assert_eq!(reply.data()["revision"], 1);
        assert_eq!(reply.data()["applied"], 2);
        assert_eq!(pixel(&shared, &job, layer, [8, 2]), words);
        assert_eq!(pixel(&shared, &job, layer, [262, 252]), untouched);
        {
            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [1, 0]);
            let entry = engine.authority.history_entries().next().unwrap();
            assert_eq!(entry.actor(), job.actor);
            assert_eq!(entry.label(), job.label);
            assert_eq!(entry.task(), Some(job.task.as_str()));
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
        }
        for (direction, revision, expected, lengths) in [
            (Direction::Undo, 2, before, [0, 1]),
            (Direction::Redo, 3, words, [1, 0]),
        ] {
            let reply = crate::server::history_shared(
                &shared,
                "human",
                direction,
                Some(revision - 1),
                Some(&original.id),
            )
            .unwrap();
            assert_eq!(reply.data()["revision"], revision);
            assert_eq!(shared.lock().unwrap().history_lengths(), lengths);
            assert_eq!(pixel(&shared, &job, layer, [8, 2]), expected);
            assert_eq!(pixel(&shared, &job, layer, [262, 252]), untouched);
        }
    }
}

#[test]
fn declared_scopes_reject_a_later_step_and_retire_the_first_staged_native_output() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, _workspace, original, mut job) = project(&runtime, depth);
        let source = observed(&shared);
        let layer = &original.layers[0].id;
        job.info_mut().scopes = vec![Scope {
            target: Some(layer.clone()),
            rect: Some([8, 2, 9, 3]),
        }];
        let before = pixel(&shared, &job, layer, [8, 2]);
        let baseline = runtime.disk().used().unwrap();
        let commands = [
            replacement(layer, depth, [8, 2, 9, 3]).0,
            replacement(layer, depth, [9, 2, 10, 3]).0,
        ];
        let error =
            crate::server::edit_code_shared(&shared, &job, &source, &commands, Instant::now())
                .err()
                .expect("The second command escapes the declared rectangle");
        assert!(error.contains("declared"), "{error}");
        assert_eq!(observed(&shared).key(), source.key());
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(pixel(&shared, &job, layer, [8, 2]), before);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
}

#[test]
fn publication_refuses_cancellation_takeover_deadline_service_loss_and_nominal_source_replacement()
{
    for failure in [
        "cancel", "takeover", "deadline", "service", "source", "task", "locked",
    ] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, workspace, mut original, mut job) = project(&runtime, 16);
        let mut source = observed(&shared);
        let layer = original.layers[0].id.clone();
        match failure {
            "cancel" => job.cancel.store(true, Ordering::Relaxed),
            "takeover" => shared.lock().unwrap().take_over_tasks(),
            "service" => workspace.lock().unwrap().source_runtime = None,
            "source" => {
                let replacement = stage(&runtime, &original);
                assert_eq!(replacement.metadata().id(), source.metadata().id());
                assert_eq!(
                    replacement.metadata().revision(),
                    source.metadata().revision()
                );
                assert_ne!(replacement.key(), source.key());
                shared.lock().unwrap().authority =
                    DocumentAuthority::from_native(replacement, 8).unwrap();
            }
            "task" => job.info_mut().task = "not-the-owned-task".into(),
            "locked" => {
                original.layers[0].locked = true;
                source = stage(&runtime, &original);
                shared.lock().unwrap().authority =
                    DocumentAuthority::from_native(source.clone(), 8).unwrap();
            }
            _ => (),
        }
        let key = observed(&shared).key();
        let started = if failure == "deadline" {
            Instant::now() - Duration::from_secs(31)
        } else {
            Instant::now()
        };
        assert!(
            crate::server::edit_code_shared(
                &shared,
                &job,
                &source,
                &[replacement(&layer, 16, [8, 2, 9, 3]).0],
                started
            )
            .is_err(),
            "{failure}"
        );
        assert_eq!(observed(&shared).key(), key);
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        workspace.lock().unwrap().source_runtime = Some(runtime.clone());
    }
}

#[test]
fn a_human_edit_survives_refusal_of_the_frozen_code_source() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
    let (shared, _workspace, original, job) = project(&runtime, 16);
    let source = observed(&shared);
    let layer = &original.layers[0].id;
    crate::server::edit_shared(
        &shared,
        "human",
        &[json!({"op":"layer.update","layer":layer,"visible":false})],
        Some(0),
        None,
        "Human visibility",
        None,
        Some(&original.id),
    )
    .unwrap();
    let human = observed(&shared);
    assert!(crate::server::edit_code_shared(
        &shared,
        &job,
        &source,
        &[replacement(layer, 16, [8, 2, 9, 3]).0],
        Instant::now()
    )
    .is_err());
    assert_eq!(observed(&shared).key(), human.key());
    let engine = shared.lock().unwrap();
    assert_eq!(engine.history_lengths(), [1, 0]);
    assert!(!view::current(&engine).find_layer(layer).unwrap().visible());
}

#[test]
fn each_command_checks_scopes_after_prior_selection_changes() {
    for initially_selected in [false, true] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let (shared, _workspace, mut original, mut job) = project(&runtime, 16);
        let area = [8, 2, 9, 3];
        if initially_selected {
            original.selection = Some(area);
        }
        let source = stage(&runtime, &original);
        let layer = original.layers[0].id.clone();
        {
            let mut engine = shared.lock().unwrap();
            engine.authority = DocumentAuthority::from_native(source.clone(), 8).unwrap();
            engine
                .leases
                .iter_mut()
                .find(|lease| lease.id == job.task)
                .unwrap()
                .scopes = vec![Scope {
                target: None,
                rect: None,
            }];
        }
        job.info_mut().scopes = vec![
            Scope {
                target: Some(layer.clone()),
                rect: Some(area),
            },
            Scope {
                target: Some("@selection".into()),
                rect: None,
            },
        ];
        let selection = if initially_selected {
            json!({"op":"selection.clear"})
        } else {
            json!({"op":"selection","rect":area})
        };
        let commands = [
            selection,
            json!({"op":"fill","layer":layer,"color":[37,83,127,193]}),
        ];
        let baseline = runtime.disk().used().unwrap();
        let result =
            crate::server::edit_code_shared(&shared, &job, &source, &commands, Instant::now());
        if initially_selected {
            // Clearing the selection makes the following fill global. The old
            // selected footprint cannot authorize that later command.
            assert!(matches!(result,Err(error) if error.contains("declared")));
            assert_eq!(observed(&shared).key(), source.key());
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        } else {
            // A newly restricted draft must allow the fill at its actual narrow
            // footprint, although the original document had no selection.
            let reply = result.unwrap();
            assert_eq!(reply.data()["applied"], 2);
            let expected = Engine::preview_edits(original, &commands).unwrap();
            let reader = evaluation::ResidentPixels::new(expected, vec![]);
            use evaluation::Pixels;
            for point in [[8, 2], [9, 2], [262, 252]] {
                assert_eq!(
                    pixel(&shared, &job, &layer, point),
                    reader.pixel(&layer, point[0], point[1]).unwrap()
                );
            }
            assert_eq!(shared.lock().unwrap().history_lengths(), [1, 0]);
        }
    }
}
