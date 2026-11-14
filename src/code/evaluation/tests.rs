use super::*;

fn context(doc: Document) -> Context {
    let config = Config::from_view((&doc).into());
    Context {
        config,
        source: Box::new(ResidentPixels::new(doc, vec![])),
        patches: BTreeMap::new(),
        next: 0,
        pixels: 0,
        bytes: 0,
        commands: vec![],
    }
}
fn fixture(depth: u16) -> (Shared, Document, Job) {
    let mut doc = Document::new_depth(19, 13, depth).unwrap();
    doc.layers[0]
        .pixels
        .set16(2, 3, [12347, 23459, 34571, 61379]);
    let mut engine = Engine::from_document(doc.clone());
    let scopes = vec![Scope::layer(&doc.layers[0].id)];
    let lease = engine
        .reserve("code-agent", "Evaluate", scopes.clone())
        .unwrap();
    let job = Job::new(Info {
        id: engine::id(),
        actor: "code-agent".into(),
        document: doc.id.clone(),
        task: lease.id,
        scopes,
        label: "Evaluate".into(),
        native: None,
    });
    (Arc::new(Mutex::new(engine)), doc, job)
}

#[test]
fn evaluation_keeps_reads_frozen_emits_original_precision_and_never_publishes() {
    for depth in [8, 16] {
        let (shared, original, job) = fixture(depth);
        let layer = &original.layers[0].id;
        let script = format!(
            r#"
            let before = read_pixel("{layer}",2,3);
            edit(#{{op:"layer.update",layer:"{layer}",opacity:0.5}});
            let after = read_pixel("{layer}",2,3);
            if before != after {{ throw "Code reads must stay frozen"; }}
            let patch = begin_pixels("{layer}",[2,3,3,4]);
            write_pixel(patch,2,3,[document.channel_max,before[1],before[2],document.channel_max]);
            commit_pixels(patch);
        "#
        );
        let commands = super::commands(&shared, &job, &original, &script, Instant::now()).unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[1]["bit_depth"], depth);
        let bytes = STANDARD
            .decode(commands[1]["rgba"].as_str().unwrap())
            .unwrap();
        let native = original.layers[0].pixels.get16(2, 3);
        assert_eq!(
            super::super::replacement_pixel(&bytes, depth, SampleOrder::Little, 0),
            [65535, native[1], native[2], 65535]
        );
        let preview = Engine::preview_edits(original.clone(), &commands).unwrap();
        assert_eq!(preview.layers[0].opacity, 0.5);
        assert_eq!(
            preview.layers[0].pixels.get16(2, 3),
            [65535, native[1], native[2], 65535]
        );
        let engine = shared.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&engine.resident_state().unwrap().document).unwrap(),
            json!(original)
        );
        assert_eq!(engine.history_lengths(), [0, 0]);
    }
}

#[test]
fn command_handoff_moves_the_owned_payload_and_refuses_retained_contexts() {
    let mut c = context(Document::new(1, 1).unwrap());
    c.push(json!({"op":"source.update","layer":"layer","text":"x".repeat(65536)}))
        .unwrap();
    let command_pointer = c.commands.as_ptr();
    let text_pointer = c.commands[0]["text"].as_str().unwrap().as_ptr();
    let result = take_commands(Arc::new(Mutex::new(c))).unwrap();
    assert_eq!(result.as_ptr(), command_pointer);
    assert_eq!(result[0]["text"].as_str().unwrap().as_ptr(), text_pointer);
    let mut c = context(Document::new(1, 1).unwrap());
    c.push(json!({"op":"layer.update","layer":"layer","opacity":0.5}))
        .unwrap();
    let owner = Arc::new(Mutex::new(c));
    let retained = owner.clone();
    assert!(take_commands(owner).unwrap_err().contains("still retains"));
    assert_eq!(retained.lock().unwrap().commands.len(), 1);
}

#[test]
fn command_count_and_escaped_byte_rejections_preserve_the_valid_buffer() {
    let mut c = context(Document::new(1, 1).unwrap());
    let valid = json!({"op":"source.update","layer":"layer","text":"\0\"é\\"});
    c.push(valid.clone()).unwrap();
    assert_eq!(c.bytes, serde_json::to_vec(&valid).unwrap().len());
    let bytes = c.bytes;
    let excessive = json!({"op":"source.update","text":"\0é".repeat(MAX_BYTES/8+1)});
    assert!(c.push(excessive).unwrap_err().contains("8 MiB"));
    assert_eq!(c.bytes, bytes);
    assert_eq!(c.commands, vec![valid.clone()]);
    for _ in 1..32 {
        c.push(valid.clone()).unwrap();
    }
    let bytes = c.bytes;
    assert!(c.push(valid.clone()).unwrap_err().contains("32 commands"));
    assert_eq!(c.bytes, bytes);
    assert_eq!(c.commands.len(), 32);
}

#[test]
fn unfinished_buffers_script_errors_and_cancellation_cannot_publish() {
    for failure in ["buffer", "script", "cancel"] {
        let (shared, original, job) = fixture(16);
        let layer = &original.layers[0].id;
        let script = match failure {
            "buffer" => format!(
                r#"let patch=begin_pixels("{layer}",[2,3,3,4]); edit(#{{op:"layer.update",layer:"{layer}",opacity:0.5}});"#
            ),
            "script" => format!(
                r#"edit(#{{op:"layer.update",layer:"{layer}",opacity:0.5}}); throw "Stop";"#
            ),
            "cancel" => {
                job.cancel.store(true, Ordering::Relaxed);
                "loop { let value=1+1; }".into()
            }
            _ => unreachable!(),
        };
        assert!(super::commands(&shared, &job, &original, &script, Instant::now()).is_err());
        let engine = shared.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&engine.resident_state().unwrap().document).unwrap(),
            json!(original)
        );
        assert_eq!(engine.history_lengths(), [0, 0]);
    }
}

#[test]
fn borrowed_script_metadata_matches_the_existing_resident_and_native_view_contract() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(19, 13, depth).unwrap();
        doc.layers[0].name = "Artwork · צבע".into();
        doc.layers[0].x = -7;
        doc.layers[0].y = 5;
        doc.layers[0].locked = true;
        let expected = json!({"width":doc.width,"height":doc.height,"bit_depth":doc.bit_depth,
            "channel_max":if depth==16 {65535} else {255},"layers":doc.layers.iter().map(|layer|
                json!({"id":layer.id,"name":layer.name,"kind":layer.kind,"parent":layer.parent,"x":layer.x,"y":layer.y,"locked":layer.locked})
            ).collect::<Vec<_>>()});
        let pool = crate::large_document::ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let native = crate::tree::presentation_tests::native(&doc, &pool);
        for view in [DocumentView::from(&doc), DocumentView::from(&native)] {
            let metadata = input::metadata(view).unwrap();
            let actual: Value = rhai::serde::from_dynamic(&metadata).unwrap();
            assert_eq!(actual, expected);
        }
    }
}

struct ReplyPixels {
    point: [u16; 4],
    patch: Vec<u16>,
    failure: bool,
    calls: Arc<AtomicUsize>,
}
impl Pixels for ReplyPixels {
    fn pixel(&self, _layer: &str, _x: INT, _y: INT) -> Result<[u16; 4], String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.failure {
            Err("Pixel source refused the request".into())
        } else {
            Ok(self.point)
        }
    }
    fn patch(&self, _layer: &str, _rect: [i32; 4]) -> Result<Vec<u16>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.failure {
            Err("Pixel source refused the request".into())
        } else {
            Ok(self.patch.clone())
        }
    }
}
fn reply_input(depth: u16, source: ReplyPixels) -> Input {
    Input {
        config: Config {
            width: 3,
            height: 2,
            bit_depth: depth,
        },
        metadata: rhai::serde::to_dynamic(json!({"width":3,"height":2,"bit_depth":depth,
            "channel_max":if depth==16 {65535} else {255},"layers":[]}))
        .unwrap(),
        pixels: Box::new(source),
        progress: Box::new(|_| None),
    }
}

#[test]
fn the_shared_evaluator_runs_from_metadata_and_native_pixel_replies_without_a_document() {
    for depth in [8, 16] {
        let point = if depth == 16 {
            [12347, 23459, 34571, 61379]
        } else {
            [31, 73, 149, 177]
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let input = reply_input(
            depth,
            ReplyPixels {
                point,
                patch: point.to_vec(),
                failure: false,
                calls: calls.clone(),
            },
        );
        let script = r#"
            let pixel=read_pixel("layer",0,0);
            let patch=begin_pixels("layer",[0,0,1,1]);
            write_pixel(patch,0,0,[pixel[0],pixel[1],pixel[2],document.channel_max]);
            commit_pixels(patch);
        "#;
        let commands = evaluate(input, script, Instant::now()).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["bit_depth"], depth);
        let bytes = STANDARD
            .decode(commands[0]["rgba"].as_str().unwrap())
            .unwrap();
        let expected = if depth == 16 {
            [point[0], point[1], point[2], 65535]
        } else {
            [point[0] * 257, point[1] * 257, point[2] * 257, 65535]
        };
        assert_eq!(
            super::super::replacement_pixel(&bytes, depth, SampleOrder::Little, 0),
            expected
        );
    }
}

#[test]
fn malformed_pixel_replies_and_reader_errors_never_become_zero_or_truncated_channels() {
    for failure in ["short", "point_depth", "patch_depth", "source"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let input = reply_input(
            8,
            ReplyPixels {
                point: if failure == "point_depth" {
                    [65535, 1, 2, 3]
                } else {
                    [1, 2, 3, 4]
                },
                patch: if failure == "short" {
                    vec![1, 2, 3]
                } else if failure == "patch_depth" {
                    vec![65535, 1, 2, 3]
                } else {
                    vec![1, 2, 3, 4]
                },
                failure: failure == "source",
                calls,
            },
        );
        let script = if failure == "point_depth" || failure == "source" {
            r#"read_pixel("layer",0,0); edit(#{op:"layer.update",layer:"layer",opacity:0.5});"#
        } else {
            r#"let patch=begin_pixels("layer",[0,0,1,1]); commit_pixels(patch);"#
        };
        let error = evaluate(input, script, Instant::now()).unwrap_err();
        assert!(
            error.contains(match failure {
                "short" => "incomplete",
                "source" => "refused",
                _ => "channel depth",
            }),
            "{failure}: {error}"
        );
    }
}
