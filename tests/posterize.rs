use peerbrush::{
    depth16::{self, Image16},
    effects::{self, Image},
    engine::{Document, Engine, Scope},
    psd, server,
};
use serde_json::{json, Value};
use std::io::Read;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(8, 6, depth).unwrap();
    for y in 0..6 {
        for x in 0..8 {
            if depth == 16 {
                e.doc.layers[0].pixels.set16(
                    x,
                    y,
                    [10001 + x as u16 * 411, 30003 + y as u16 * 503, 50007, 65535],
                );
            } else {
                e.doc.layers[0]
                    .pixels
                    .set(x, y, [39 + x as u8 * 2, 117 + y as u8 * 3, 195, 255]);
            }
        }
    }
    e
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], None, None, "Posterize").unwrap();
}
fn native(doc: &Document) -> Vec<u16> {
    if doc.bit_depth == 16 {
        depth16::render(doc).unwrap().words
    } else {
        doc.preview(None, 8, None, false)
            .unwrap()
            .2
            .into_iter()
            .map(u16::from)
            .collect()
    }
}
fn strip_resources(bytes: &[u8]) -> Vec<u8> {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    [
        bytes[..at].to_vec(),
        vec![0; 4],
        bytes[at + 4 + len..].to_vec(),
    ]
    .concat()
}

#[test]
fn eight_bit_levels_quantize_rgb_at_thresholds_and_keep_every_alpha_byte() {
    let original = (0..=255u8).flat_map(|v| [v, v, v, v]).collect::<Vec<_>>();
    let mut im = Image {
        width: 256,
        height: 1,
        bytes: original.clone(),
    };
    effects::apply(&mut im, "posterize", &json!({"levels":4})).unwrap();
    for p in im.bytes.chunks_exact(4) {
        assert!([0, 85, 170, 255].contains(&p[0]));
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }
    for (old, new) in original.chunks_exact(4).zip(im.bytes.chunks_exact(4)) {
        assert_eq!(old[3], new[3]);
    }
    for (value, expected) in [
        (42, 0),
        (43, 85),
        (127, 85),
        (128, 170),
        (212, 170),
        (213, 255),
    ] {
        assert_eq!(im.get(value, 0)[0], expected);
    }
    let mut full = Image {
        width: 256,
        height: 1,
        bytes: original.clone(),
    };
    effects::apply(&mut full, "posterize", &json!({"levels":256})).unwrap();
    assert_eq!(full.bytes, original);
}

#[test]
fn native_sixteen_bit_thresholds_and_level_counts_are_processed_without_projection() {
    let values = [0, 16383, 16384, 32767, 32768, 49151, 49152, 65535];
    let mut im = Image16 {
        width: 8,
        height: 1,
        words: values
            .into_iter()
            .enumerate()
            .flat_map(|(i, v)| [v, v, v, 10001 + i as u16])
            .collect(),
    };
    depth16::color::apply(&mut im, "posterize", &json!({"levels":3})).unwrap();
    assert_eq!(
        im.words.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>(),
        [0, 0, 32768, 32768, 32768, 32768, 65535, 65535]
    );
    for (i, p) in im.words.chunks_exact(4).enumerate() {
        assert_eq!(p[3], 10001 + i as u16);
    }
    let mut full = Image16 {
        width: 65536,
        height: 1,
        words: (0..=65535u16).flat_map(|v| [v, v, v, 65535]).collect(),
    };
    depth16::color::apply(&mut full, "posterize", &json!({"levels":256})).unwrap();
    assert_eq!(
        full.words
            .chunks_exact(4)
            .map(|p| p[0])
            .collect::<HashSet<_>>()
            .len(),
        256
    );
    assert!(full
        .words
        .chunks_exact(4)
        .all(|p| p[0] % 257 == 0 && p[3] == 65535));
}

#[test]
fn layer_and_folder_previews_strength_bypass_undo_and_standard_psd_reopening_keep_sources() {
    for depth in [8, 16] {
        for group in [false, true] {
            let mut e = fixture(depth);
            let layer = e.doc.layers[0].id.clone();
            let raw = e.doc.layers[0].pixels.rgba16();
            if group {
                edit(
                    &mut e,
                    json!({"op":"group.create_selected","layers":[layer]}),
                );
            }
            let target = if group {
                e.doc
                    .layers
                    .iter()
                    .find(|l| l.kind == "group")
                    .unwrap()
                    .id
                    .clone()
            } else {
                layer.clone()
            };
            edit(
                &mut e,
                json!({"op":"effect.add","layer":target,"kind":"posterize","settings":{"levels":3},"weight":0.5}),
            );
            let index = e.doc.layers.iter().position(|l| l.id == target).unwrap();
            let effect = e.doc.layers[index].effects[0].id.clone();
            let initial = native(&e.doc);
            if depth == 16 {
                assert_eq!(initial[0], 5001);
                assert_ne!(initial[0] % 257, 0);
            }
            let history = e.undo.len();
            let revision = e.doc.revision;
            let command = json!({"op":"effect.update","layer":target,"effect":effect,"settings":{"levels":8}});
            let draft = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
            assert_eq!(e.undo.len(), history);
            assert_eq!(e.doc.revision, revision);
            edit(&mut e, command);
            let applied = native(&e.doc);
            assert_eq!(applied, native(&draft));
            assert_ne!(applied, initial);
            let source = e.doc.layers.iter().find(|l| l.id == layer).unwrap();
            assert_eq!(source.pixels.rgba16(), raw);
            let bytes = psd::encode(&e.doc).unwrap();
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(native(&reopened), applied);
            assert_eq!(
                reopened
                    .layers
                    .iter()
                    .find(|l| l.id == layer)
                    .unwrap()
                    .pixels
                    .rgba16(),
                raw
            );
            assert_eq!(
                native(&psd::decode(&strip_resources(&bytes)).unwrap()),
                applied
            );
            e.undo("human").unwrap();
            assert_eq!(native(&e.doc), initial);
            e.redo("human").unwrap();
            assert_eq!(native(&e.doc), applied);
            edit(
                &mut e,
                json!({"op":"effect.update","layer":target,"effect":effect,"enabled":false}),
            );
            assert_eq!(native(&e.doc), native(&fixture(depth).doc));
        }
    }
}

#[test]
fn protocol_invalid_integer_settings_stale_sources_locks_and_reservations_are_atomic() {
    let e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    let document = e.doc.id.clone();
    let shared = Arc::new(Mutex::new(e));
    let request = |levels| {
        json!({"actor":"human","document_id":document,"expected_revision":0,
        "commands":[{"op":"effect.add","layer":layer,"kind":"posterize","settings":{"levels":levels}}]})
    };
    for value in [
        json!(1),
        json!(257),
        json!(2.5),
        json!("4"),
        json!(null),
        json!(true),
    ] {
        assert!(server::dispatch(&shared, "edit", &request(value)).is_err());
        let e = shared.lock().unwrap();
        assert_eq!(e.doc.revision, 0);
        assert!(e.undo.is_empty());
        assert!(e.doc.layers[0].effects.is_empty());
    }
    assert!(server::dispatch(
        &shared,
        "edit",
        &json!({"actor":"human","commands":[
            {"op":"mask.add","layer":layer},{"op":"mask.step.add","layer":layer,"kind":"posterize"}
        ]})
    )
    .is_err());
    assert!(shared.lock().unwrap().doc.layers[0].mask.is_none());
    shared.lock().unwrap().doc.layers[0].locked = true;
    assert!(server::dispatch(&shared, "edit", &request(json!(4))).is_err());
    {
        let mut e = shared.lock().unwrap();
        e.doc.layers[0].locked = false;
        e.reserve("agent", "Preserve native work", vec![Scope::layer(&layer)])
            .unwrap();
    }
    assert!(server::dispatch(&shared, "edit", &request(json!(4))).is_err());
    shared.lock().unwrap().leases.clear();
    server::dispatch(&shared, "edit", &request(json!(4))).unwrap();
    assert!(server::dispatch(&shared, "edit", &request(json!(8))).is_err());
    let e = shared.lock().unwrap();
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.doc.layers[0].effects[0].settings["levels"], 4);
}

#[test]
fn posterize_adjustments_use_protected_source_format_thirteen_at_both_depths() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        let raw = e.doc.layers[0].pixels.rgba16();
        edit(
            &mut e,
            json!({"op":"adjustment.add","layers":[layer],"kind":"posterize","settings":{"levels":4}}),
        );
        assert_eq!(e.doc.layers[0].kind, "adjustment");
        let bytes = psd::encode(&e.doc).unwrap();
        let start = bytes.windows(4).position(|w| w == b"PBR1").unwrap() + 4;
        let mut source = Vec::new();
        flate2::read::ZlibDecoder::new(&bytes[start..])
            .read_to_end(&mut source)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&source).unwrap()["format"],
            13
        );
        let reopened = psd::decode(&bytes).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(native(&reopened), native(&e.doc));
        assert_eq!(
            reopened
                .layers
                .iter()
                .find(|l| l.id == layer)
                .unwrap()
                .pixels
                .rgba16(),
            raw
        );
        assert_eq!(
            native(&psd::decode(&strip_resources(&bytes)).unwrap()),
            native(&e.doc)
        );
    }
}
