use super::*;
use crate::{
    code::{
        evaluation::{Pixels, ResidentPixels},
        native_source::tests::{observed, project},
    },
    engine::native::worker::tests::Fixture,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
const SCRIPT: &str = "edit(#{op:\"layer.update\",layer:\"layer\",opacity:0.5});";

struct Transcript {
    parent: State,
    child: State,
    input: Vec<u8>,
    output: Vec<u8>,
}
impl Transcript {
    fn new(source: &NativeSource) -> Self {
        let mut body = Vec::new();
        evaluation::write_metadata(source.view(), &mut body).unwrap();
        let metadata_bytes = body.len() as u32;
        body.extend_from_slice(SCRIPT.as_bytes());
        let start = Message::Start {
            config: Config::from_view(source.view()),
            metadata_bytes,
            script_bytes: SCRIPT.len() as u32,
            build: build_label(),
        };
        let mut value = Self {
            parent: State::parent([1; 16]).unwrap(),
            child: State::child(),
            input: Vec::new(),
            output: Vec::new(),
        };
        value.reply(start, &body);
        value
    }
    fn child(&mut self, message: Message, body: &[u8]) {
        let mut encoded = Vec::new();
        self.child
            .send(&message, body, &mut encoded, &|| Ok(()))
            .unwrap();
        self.parent
            .receive(
                &mut Cursor::new(&encoded),
                &mut vec![0; body.len()],
                &|| Ok(()),
            )
            .unwrap();
        self.input.extend_from_slice(&encoded);
    }
    fn reply(&mut self, message: Message, body: &[u8]) {
        let mut encoded = Vec::new();
        self.parent
            .send(&message, body, &mut encoded, &|| Ok(()))
            .unwrap();
        self.child
            .receive(
                &mut Cursor::new(&encoded),
                &mut vec![0; body.len()],
                &|| Ok(()),
            )
            .unwrap();
        self.output.extend_from_slice(&encoded);
    }
    fn command(&mut self, index: u8, command: &Value) {
        self.child(
            Message::Command { index },
            &serde_json::to_vec(command).unwrap(),
        );
        self.reply(Message::Continue, &[]);
    }
}

#[test]
fn native_parent_serves_original_channels_and_returns_admitted_commands_without_publication() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(96 * 1024 * 1024, 64 * 1024 * 1024);
        let (shared, workspace, original, job) = project(&runtime, depth);
        let document = observed(&shared);
        let layer = &original.layers[0].id;
        let oracle = ResidentPixels::new(original.clone(), job.scopes.clone());
        let mut source = NativeSource::capture(&shared, &job, &document, &|| Ok(())).unwrap();
        let mut wire = Transcript::new(&source);
        wire.child(
            Message::Point {
                layer: layer.clone(),
                point: [8, 2],
            },
            &[],
        );
        let point: Vec<_> = oracle
            .pixel(layer, 8, 2)
            .unwrap()
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        wire.reply(Message::Words { words: 4 }, &point);
        let rect = [7, 1, 9, 3];
        wire.child(
            Message::Patch {
                layer: layer.clone(),
                rect,
            },
            &[],
        );
        let words = oracle.patch(layer, rect).unwrap();
        let bytes: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        wire.reply(
            Message::Words {
                words: words.len() as u32,
            },
            &bytes,
        );
        wire.child(Message::Progress { operations: 1024 }, &[]);
        wire.reply(Message::Continue, &[]);
        let pixels: Vec<_> = if depth == 16 {
            bytes
        } else {
            words.iter().map(|word| *word as u8).collect()
        };
        let command = json!({"op":"pixels.replace","layer":layer,"rect":rect,"bit_depth":depth,"rgba":STANDARD.encode(pixels)});
        wire.command(0, &command);
        let edit = json!({"op":"layer.update","layer":layer,"opacity":0.5});
        wire.command(1, &edit);
        wire.child(Message::Done { commands: 2 }, &[]);
        let mut output = Vec::new();
        let check = || {
            assert!(shared.try_lock().is_ok());
            assert!(workspace.try_lock().is_ok());
            Ok(())
        };
        let received = exchange(
            &shared,
            &job,
            &mut source,
            SCRIPT,
            [1; 16],
            Cursor::new(wire.input),
            &mut output,
            Instant::now(),
            &check,
        )
        .unwrap();
        assert_eq!(output, wire.output);
        assert_eq!(&**received.commands, &[command, edit]);
        assert!(received.commands.owner().uses_budget(&runtime.resident()));
        assert_eq!(observed(&shared).key(), document.key());
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        drop(source);
        let retained = received.commands.clone();
        drop(received);
        let held = runtime.resident().used();
        assert!(retained.is_unique());
        assert_eq!(retained[0]["bit_depth"], depth);
        drop(retained);
        assert!(runtime.resident().used() < held);
    }
}

#[test]
fn missing_pixel_source_is_an_explicit_error_reply_instead_of_zero_channels() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(96 * 1024 * 1024, 64 * 1024 * 1024);
    let (shared, _workspace, original, job) = project(&runtime, 16);
    let document = observed(&shared);
    let mut source = NativeSource::capture(&shared, &job, &document, &|| Ok(())).unwrap();
    let mut wire = Transcript::new(&source);
    wire.child(
        Message::Point {
            layer: "missing".into(),
            point: [0, 0],
        },
        &[],
    );
    let error = source
        .pixel(&shared, &job, "missing", 0, 0, &|| Ok(()))
        .unwrap_err();
    wire.reply(
        Message::Error {
            message: super::super::child::error_text(&error),
        },
        &[],
    );
    let command = json!({"op":"layer.update","layer":original.layers[0].id,"opacity":0.5});
    wire.command(0, &command);
    wire.child(Message::Done { commands: 1 }, &[]);
    let mut output = Vec::new();
    let received = exchange(
        &shared,
        &job,
        &mut source,
        SCRIPT,
        [1; 16],
        Cursor::new(wire.input),
        &mut output,
        Instant::now(),
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(output, wire.output);
    assert_eq!(received.commands[0], command);
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
}

#[test]
fn parent_refuses_forbidden_or_malformed_commands_trailing_output_and_changed_tasks() {
    for failure in [
        "forbidden",
        "duplicate",
        "shape",
        "depth",
        "base64",
        "trailing",
        "cancel",
        "takeover",
        "deadline",
    ] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(96 * 1024 * 1024, 64 * 1024 * 1024);
        let (shared, _workspace, original, job) = project(&runtime, 16);
        let document = observed(&shared);
        let mut source = NativeSource::capture(&shared, &job, &document, &|| Ok(())).unwrap();
        let mut wire = Transcript::new(&source);
        let mut command = json!({"op":"layer.update","layer":original.layers[0].id,"opacity":0.5});
        if matches!(failure, "shape" | "depth" | "base64") {
            command = json!({"op":"pixels.replace","layer":original.layers[0].id,"rect":[0,0,1,1],"bit_depth":16,"rgba":STANDARD.encode([0u8;8])});
        }
        match failure {
            "forbidden" => command["source_revision"] = json!(1),
            "shape" => command["rect"] = json!([0, 0, 1000, 1]),
            "depth" => command["bit_depth"] = json!(8),
            "base64" => command["rgba"] = json!("!!!!!!!!!!!!"),
            _ => (),
        }
        let bytes = if failure == "duplicate" {
            br#"{"op":"selection.clear","op":"selection.invert"}"#.to_vec()
        } else {
            serde_json::to_vec(&command).unwrap()
        };
        wire.child(Message::Command { index: 0 }, &bytes);
        wire.reply(Message::Continue, &[]);
        wire.child(Message::Done { commands: 1 }, &[]);
        if failure == "trailing" {
            wire.input.push(1);
        }
        if failure == "cancel" {
            job.cancel.store(true, Ordering::Relaxed);
        }
        if failure == "takeover" {
            shared.lock().unwrap().take_over_tasks();
        }
        let started = if failure == "deadline" {
            Instant::now() - Duration::from_secs(31)
        } else {
            Instant::now()
        };
        assert!(
            exchange(
                &shared,
                &job,
                &mut source,
                SCRIPT,
                [1; 16],
                Cursor::new(wire.input),
                Vec::new(),
                started,
                &|| Ok(())
            )
            .is_err(),
            "{failure}"
        );
        assert_eq!(observed(&shared).key(), document.key());
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    }
}

#[test]
fn returned_native_patch_totals_are_rechecked_without_trusting_child_requests() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(96 * 1024 * 1024, 64 * 1024 * 1024);
    let (shared, _workspace, original, job) = project(&runtime, 16);
    let document = observed(&shared);
    let mut source = NativeSource::capture(&shared, &job, &document, &|| Ok(())).unwrap();
    let mut wire = Transcript::new(&source);
    let command = json!({"op":"pixels.replace","layer":original.layers[0].id,
        "rect":[0,0,270,260],"bit_depth":16,"rgba":STANDARD.encode(vec![0u8;270*260*8])});
    // A forged child skips all Patch requests. Four returned replacements still
    // exceed the aggregate limit, even though every individual frame fits.
    for index in 0..4 {
        wire.command(index, &command);
    }
    wire.child(Message::Done { commands: 4 }, &[]);
    let result = exchange(
        &shared,
        &job,
        &mut source,
        SCRIPT,
        [1; 16],
        Cursor::new(wire.input),
        Vec::new(),
        Instant::now(),
        &|| Ok(()),
    );
    assert!(matches!(result,Err(error) if error.contains("262144")));
    assert_eq!(observed(&shared).key(), document.key());
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
}

#[test]
fn admission_refusal_and_cancellation_during_ack_return_no_commands_or_history() {
    struct CancelOnContinue(Arc<AtomicBool>);
    impl Write for CancelOnContinue {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.windows(8).any(|part| part == b"Continue") {
                self.0.store(true, Ordering::Relaxed);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(96 * 1024 * 1024, 64 * 1024 * 1024);
    let (shared, _workspace, original, job) = project(&runtime, 16);
    let document = observed(&shared);
    let mut source = NativeSource::capture(&shared, &job, &document, &|| Ok(())).unwrap();
    let pool = runtime.resident();
    let held = MetadataCharge::reserve(&pool, pool.limit() - pool.used() - 1024 * 1024, &|| Ok(()))
        .unwrap();
    let used = pool.used();
    let mut output = Vec::new();
    assert!(exchange(
        &shared,
        &job,
        &mut source,
        SCRIPT,
        [1; 16],
        Cursor::new([]),
        &mut output,
        Instant::now(),
        &|| Ok(())
    )
    .is_err());
    assert!(output.is_empty());
    assert_eq!(pool.used(), used);
    drop(held);
    let mut wire = Transcript::new(&source);
    wire.command(
        0,
        &json!({"op":"layer.update","layer":original.layers[0].id,"opacity":0.5}),
    );
    wire.child(Message::Done { commands: 1 }, &[]);
    assert!(exchange(
        &shared,
        &job,
        &mut source,
        SCRIPT,
        [1; 16],
        Cursor::new(wire.input),
        CancelOnContinue(job.cancel.clone()),
        Instant::now(),
        &|| Ok(())
    )
    .is_err());
    assert!(job.cancel.load(Ordering::Relaxed));
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
}
