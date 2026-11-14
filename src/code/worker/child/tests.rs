use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::io::Cursor;

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn metadata(depth: u16) -> Value {
    json!({"width":3,"height":2,"bit_depth":depth,"channel_max":if depth==16 {65535} else {255},
        "layers":[{"id":"layer","name":"Paint 1","kind":"paint","parent":null,"x":0,"y":0,"locked":false}]})
}
fn start(depth: u16, metadata: &Value, script: &str) -> (Message, Vec<u8>) {
    let mut body = serde_json::to_vec(metadata).unwrap();
    let bytes = body.len();
    body.extend_from_slice(script.as_bytes());
    (
        Message::Start {
            config: Config {
                width: 3,
                height: 2,
                bit_depth: depth,
            },
            metadata_bytes: bytes as u32,
            script_bytes: script.len() as u32,
            build: [7; 32],
        },
        body,
    )
}
fn step(
    sender: &mut State,
    receiver: &mut State,
    message: Message,
    body: &[u8],
    save: Option<&mut Vec<u8>>,
) {
    let mut encoded = Vec::new();
    sender
        .send(&message, body, &mut encoded, &|| Ok(()))
        .unwrap();
    let mut storage = vec![0; body.len()];
    receiver
        .receive(&mut Cursor::new(&encoded), &mut storage, &|| Ok(()))
        .unwrap();
    if let Some(save) = save {
        save.extend_from_slice(&encoded);
    }
}

#[test]
fn child_evaluation_uses_only_metadata_and_ordered_native_word_replies() {
    for depth in [8, 16] {
        let script = r#"let old=read_pixel("layer",0,0);let patch=begin_pixels("layer",[0,0,1,1]);
            write_pixel(patch,0,0,[old[0],old[1],old[2],document.channel_max]);commit_pixels(patch);"#;
        let (start, body) = start(depth, &metadata(depth), script);
        let mut parent = State::parent([1; 16]).unwrap();
        let mut model = State::child();
        let mut input = Vec::new();
        step(
            &mut parent,
            &mut model,
            start.clone(),
            &body,
            Some(&mut input),
        );
        let point = Message::Point {
            layer: "layer".into(),
            point: [0, 0],
        };
        let patch = Message::Patch {
            layer: "layer".into(),
            rect: [0, 0, 1, 1],
        };
        let words = if depth == 16 {
            [12347u16, 23459, 34571, 61379]
        } else {
            [31u16, 73, 149, 177]
        };
        let word_bytes: Vec<_> = words.into_iter().flat_map(u16::to_le_bytes).collect();
        step(&mut model, &mut parent, point.clone(), &[], None);
        step(
            &mut parent,
            &mut model,
            Message::Words { words: 4 },
            &word_bytes,
            Some(&mut input),
        );
        step(&mut model, &mut parent, patch.clone(), &[], None);
        step(
            &mut parent,
            &mut model,
            Message::Words { words: 4 },
            &word_bytes,
            Some(&mut input),
        );
        let mut pixels = words;
        pixels[3] = if depth == 16 { 65535 } else { 255 };
        let pixels: Vec<_> = if depth == 16 {
            pixels.into_iter().flat_map(u16::to_le_bytes).collect()
        } else {
            pixels.into_iter().map(|word| word as u8).collect()
        };
        let command = json!({"op":"pixels.replace","layer":"layer","rect":[0,0,1,1],"bit_depth":depth,"rgba":STANDARD.encode(pixels)});
        step(
            &mut model,
            &mut parent,
            Message::Command { index: 0 },
            &serde_json::to_vec(&command).unwrap(),
            None,
        );
        step(
            &mut parent,
            &mut model,
            Message::Continue,
            &[],
            Some(&mut input),
        );
        let output = Output::default();
        serve(
            Cursor::new(input),
            output.clone(),
            [7; 32],
            &Permit::for_test(),
        )
        .unwrap();
        let mut parent = State::parent([1; 16]).unwrap();
        parent
            .send(&start, &body, &mut Vec::new(), &|| Ok(()))
            .unwrap();
        let mut reader = Cursor::new(output.0.lock().unwrap().clone());
        let mut storage = vec![0; MAX_BYTES];
        assert_eq!(
            parent
                .receive(&mut reader, &mut storage, &|| Ok(()))
                .unwrap()
                .message,
            point
        );
        parent
            .send(
                &Message::Words { words: 4 },
                &word_bytes,
                &mut Vec::new(),
                &|| Ok(()),
            )
            .unwrap();
        assert_eq!(
            parent
                .receive(&mut reader, &mut storage, &|| Ok(()))
                .unwrap()
                .message,
            patch
        );
        parent
            .send(
                &Message::Words { words: 4 },
                &word_bytes,
                &mut Vec::new(),
                &|| Ok(()),
            )
            .unwrap();
        let frame = parent
            .receive(&mut reader, &mut storage, &|| Ok(()))
            .unwrap();
        assert_eq!(frame.message, Message::Command { index: 0 });
        assert_eq!(
            serde_json::from_slice::<Value>(frame.body).unwrap(),
            command
        );
        parent
            .send(&Message::Continue, &[], &mut Vec::new(), &|| Ok(()))
            .unwrap();
        assert_eq!(
            parent
                .receive(&mut reader, &mut storage, &|| Ok(()))
                .unwrap()
                .message,
            Message::Done { commands: 1 }
        );
        assert!(!parent.successful());
        parent.exact_eof(&mut reader, &|| Ok(())).unwrap();
        assert!(parent.successful());
    }
}

#[test]
fn parent_abort_cannot_be_caught_to_emit_or_publish_commands() {
    let script = r#"try {read_pixel("layer",0,0);} catch(error) {} edit(#{op:"layer.update",layer:"layer",opacity:0.5});"#;
    let (start, body) = start(16, &metadata(16), script);
    let mut parent = State::parent([1; 16]).unwrap();
    let mut model = State::child();
    let mut input = Vec::new();
    step(&mut parent, &mut model, start, &body, Some(&mut input));
    step(
        &mut model,
        &mut parent,
        Message::Point {
            layer: "layer".into(),
            point: [0, 0],
        },
        &[],
        None,
    );
    step(
        &mut parent,
        &mut model,
        Message::Abort {
            message: "Task ended".into(),
        },
        &[],
        Some(&mut input),
    );
    let output = Output::default();
    assert!(serve(
        Cursor::new(input),
        output.clone(),
        [7; 32],
        &Permit::for_test()
    )
    .is_err());
    let bytes = output.0.lock().unwrap();
    assert!(!bytes
        .windows(b"Command".len())
        .any(|part| part == b"Command"));
}

#[test]
fn invalid_metadata_or_build_labels_never_reach_the_interpreter() {
    for failure in ["build", "depth", "channel", "duplicate", "parent"] {
        let mut metadata = metadata(16);
        match failure {
            "depth" => metadata["bit_depth"] = json!(8),
            "channel" => metadata["channel_max"] = json!(255),
            "duplicate" => {
                let layer = metadata["layers"][0].clone();
                metadata["layers"].as_array_mut().unwrap().push(layer);
            }
            "parent" => metadata["layers"][0]["parent"] = json!("missing"),
            _ => (),
        }
        let (start, body) = start(16, &metadata, "loop {}");
        let mut input = Vec::new();
        State::parent([1; 16])
            .unwrap()
            .send(&start, &body, &mut input, &|| Ok(()))
            .unwrap();
        let output = Output::default();
        assert!(
            serve(
                Cursor::new(input),
                output.clone(),
                if failure == "build" { [8; 32] } else { [7; 32] },
                &Permit::for_test()
            )
            .is_err(),
            "{failure}"
        );
        assert!(output.0.lock().unwrap().is_empty(), "{failure}");
    }
}

#[test]
fn interpreter_failure_is_terminal_and_error_text_is_bounded_plain_utf8() {
    let (start, body) = start(16, &metadata(16), "throw \"Stop\";");
    let mut input = Vec::new();
    let mut parent = State::parent([1; 16]).unwrap();
    parent.send(&start, &body, &mut input, &|| Ok(())).unwrap();
    let output = Output::default();
    serve(
        Cursor::new(input),
        output.clone(),
        [7; 32],
        &Permit::for_test(),
    )
    .unwrap();
    let frame = parent
        .receive(
            &mut Cursor::new(output.0.lock().unwrap().clone()),
            &mut [],
            &|| Ok(()),
        )
        .unwrap();
    assert!(matches!(frame.message, Message::Failed { .. }));
    assert!(!parent.successful());
    let text = error_text(&"é\0\n\u{1b}".repeat(1000));
    assert!(text.len() <= 512);
    assert!(!text.chars().any(char::is_control));
}
