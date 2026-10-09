use peerbrush::{
    depth16::{self, Image16},
    effects::{self, Image},
    engine::{Document, Engine, Scope},
    preview::Cache,
    psd, server,
};
use serde_json::{json, Value};
use std::{
    io::Read,
    sync::{Arc, Mutex},
};

fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], None, None, "Channel clamp").unwrap();
}
fn native(doc: &Document) -> Vec<u16> {
    if doc.bit_depth == 16 {
        depth16::render(doc).unwrap().words
    } else {
        doc.preview(None, doc.width.max(doc.height), None, false)
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
fn each_channel_clamps_all_native_values_and_leaves_every_other_sample_exact() {
    for (channel, name) in ["r", "g", "b", "o"].into_iter().enumerate() {
        let source = (0..=255u8)
            .flat_map(|v| [v, 255 - v, v.wrapping_add(17), v.wrapping_add(53)])
            .collect::<Vec<_>>();
        let mut image = Image {
            width: 256,
            height: 1,
            bytes: source.clone(),
        };
        effects::apply(
            &mut image,
            "channel_clamp",
            &json!({"channel":name,"minimum":63.0/255.0,"maximum":193.0/255.0}),
        )
        .unwrap();
        for (old, new) in source.chunks_exact(4).zip(image.bytes.chunks_exact(4)) {
            for c in 0..4 {
                assert_eq!(
                    new[c],
                    if c == channel {
                        old[c].clamp(63, 193)
                    } else {
                        old[c]
                    }
                );
            }
        }
        let source = (0..=65535u16)
            .flat_map(|v| [v, 65535 - v, v.wrapping_add(17), v.wrapping_add(53)])
            .collect::<Vec<_>>();
        let mut image = Image16 {
            width: 65536,
            height: 1,
            words: source.clone(),
        };
        depth16::color::apply(
            &mut image,
            "channel_clamp",
            &json!({"channel":name,"minimum":10001.0/65535.0,"maximum":50003.0/65535.0}),
        )
        .unwrap();
        for (old, new) in source.chunks_exact(4).zip(image.words.chunks_exact(4)) {
            for c in 0..4 {
                assert_eq!(
                    new[c],
                    if c == channel {
                        old[c].clamp(10001, 50003)
                    } else {
                        old[c]
                    }
                );
            }
        }
        depth16::color::apply(
            &mut image,
            "channel_clamp",
            &json!({"channel":name,"minimum":0.5,"maximum":0.5}),
        )
        .unwrap();
        assert!(image.words.chunks_exact(4).all(|p| p[channel] == 32768));
    }
}

#[test]
fn alpha_minimum_reaches_empty_tiles_and_folder_frame_with_exact_history_and_psd_sources() {
    for depth in [8, 16] {
        for group in [false, true] {
            let mut e = Engine::new();
            e.doc = Document::new_depth(271, 133, depth).unwrap();
            let layer = e.doc.layers[0].id.clone();
            if depth == 16 {
                e.doc.layers[0]
                    .pixels
                    .set16(1, 1, [10001, 30003, 50007, 12345]);
            } else {
                e.doc.layers[0].pixels.set(1, 1, [39, 117, 195, 48]);
            }
            let raw = e.doc.layers[0].pixels.rgba16();
            if group {
                edit(
                    &mut e,
                    json!({"op":"group.create_selected","layers":[layer]}),
                );
            }
            let target = e
                .doc
                .layers
                .iter()
                .find(|l| {
                    if group {
                        l.kind == "group"
                    } else {
                        l.id == layer
                    }
                })
                .unwrap()
                .id
                .clone();
            let before = native(&e.doc);
            let add = json!({"op":"effect.add","layer":target,"kind":"channel_clamp","settings":{"channel":"o","minimum":0.25,"maximum":0.75},"weight":0.5});
            let draft = Engine::preview_edits(e.doc.clone(), &[add.clone()]).unwrap();
            let history = e.undo.len();
            edit(&mut e, add);
            let applied = native(&e.doc);
            assert_eq!(applied, native(&draft));
            assert_eq!(e.undo.len(), history + 1);
            let max = if depth == 16 { 65535.0 } else { 255.0 };
            let expected = ((max * 0.25_f64).round() * 0.5).round() as u16;
            assert_eq!(
                &applied[applied.len() - 4..],
                &[0, 0, 0, expected],
                "depth {depth} group {group}"
            );
            let bytes = psd::encode(&e.doc).unwrap();
            let start = bytes.windows(4).position(|w| w == b"PBR1").unwrap() + 4;
            let mut embedded = Vec::new();
            flate2::read::ZlibDecoder::new(&bytes[start..])
                .read_to_end(&mut embedded)
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&embedded).unwrap()["format"],
                14
            );
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
            assert_eq!(native(&e.doc), before);
            e.redo("human").unwrap();
            assert_eq!(native(&e.doc), applied);
            let effect = e
                .doc
                .layers
                .iter()
                .find(|l| l.id == target)
                .unwrap()
                .effects[0]
                .id
                .clone();
            edit(
                &mut e,
                json!({"op":"effect.update","layer":target,"effect":effect,"enabled":false}),
            );
            assert_eq!(native(&e.doc), before);
            e.undo("human").unwrap();
            edit(
                &mut e,
                json!({"op":"effect.delete","layer":target,"effect":effect}),
            );
            assert_eq!(native(&e.doc), before);
        }
    }
}

#[test]
fn invalid_bounds_channel_locks_reservations_and_stale_protocol_sources_preserve_work() {
    let mut e = Engine::new();
    e.doc = Document::new_depth(8, 8, 16).unwrap();
    e.doc.layers[0]
        .pixels
        .set16(1, 1, [10001, 30003, 50007, 12345]);
    let layer = e.doc.layers[0].id.clone();
    let document = e.doc.id.clone();
    let source = native(&e.doc);
    let shared = Arc::new(Mutex::new(e));
    let request = |settings| json!({"actor":"human","document_id":document,"expected_revision":0,"commands":[{"op":"effect.add","layer":layer,"kind":"channel_clamp","settings":settings}]});
    for settings in [
        json!({"channel":"a"}),
        json!({"channel":null}),
        json!({"channel":0}),
        json!({"minimum":-0.1}),
        json!({"maximum":1.1}),
        json!({"minimum":0.75,"maximum":0.25}),
        json!({"minimum":"0"}),
        json!({"maximum":false}),
        json!({"minimum":null}),
    ] {
        assert!(server::dispatch(&shared, "edit", &request(settings)).is_err());
        let e = shared.lock().unwrap();
        assert_eq!(e.doc.revision, 0);
        assert!(e.undo.is_empty());
        assert_eq!(native(&e.doc), source);
    }
    assert!(server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[{"op":"mask.add","layer":layer},{"op":"mask.step.add","layer":layer,"kind":"channel_clamp"}]})).is_err());
    assert!(shared.lock().unwrap().doc.layers[0].mask.is_none());
    let settings = json!({"channel":"r","minimum":0.5,"maximum":0.75});
    shared.lock().unwrap().doc.layers[0].locked = true;
    assert!(server::dispatch(&shared, "edit", &request(settings.clone())).is_err());
    {
        let mut e = shared.lock().unwrap();
        e.doc.layers[0].locked = false;
        e.reserve("agent", "Preserve native work", vec![Scope::layer(&layer)])
            .unwrap();
    }
    assert!(server::dispatch(&shared, "edit", &request(settings.clone())).is_err());
    shared.lock().unwrap().leases.clear();
    server::dispatch(&shared, "edit", &request(settings.clone())).unwrap();
    assert!(server::dispatch(&shared, "edit", &request(settings)).is_err());
    assert_eq!(shared.lock().unwrap().undo.len(), 1);
}

#[test]
fn regional_paint_previews_match_full_clamped_native_render_and_preserve_cached_baseline() {
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.doc = Document::new_depth(271, 133, depth).unwrap();
        let target = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"effect.add","layer":target,"kind":"channel_clamp","settings":{"channel":"o","minimum":0.25,"maximum":0.75}}),
        );
        edit(
            &mut e,
            json!({"op":"effect.add","layer":target,"kind":"channel_clamp","settings":{"channel":"r","minimum":0.2,"maximum":0.7}}),
        );
        let source = e.doc.clone();
        let baseline = native(&source);
        let mut cache = Cache::default();
        cache
            .render(&source, "clamp", None, 271, None, false)
            .unwrap();
        for points in [
            vec![[257., 32.]],
            vec![[257., 32.], [261., 42.]],
            vec![[257., 32.]],
        ] {
            let command = json!({"op":"paint","layer":target,"points":points,"radius":3,"color":[200,90,40,210]});
            let doc = cache.edit(source.clone(), &[command], "clamp").unwrap();
            let copied = cache
                .render(&doc, "clamp", Some([252, 27, 266, 47]), 271, None, false)
                .unwrap();
            assert!(copied.dirty.is_some());
            assert_eq!(copied.bytes, doc.preview(None, 271, None, false).unwrap().2);
        }
        assert_eq!(native(&source), baseline);
        assert!(source.layers[0].pixels.tile_bounds().is_none());
    }
}

#[test]
fn adjustment_targets_reject_clamps_atomically_in_engine_preview_and_psd_validation() {
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.doc = Document::new_depth(8, 8, depth).unwrap();
        let layer = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"adjustment.add","layers":[layer],"kind":"levels"}),
        );
        let target = e.doc.layers[0].id.clone();
        let before = serde_json::to_value(&e.doc).unwrap();
        let history = e.undo.len();
        let command = json!({"op":"effect.add","layer":target,"kind":"channel_clamp","settings":{"channel":"o","minimum":0.25}});
        assert!(Engine::preview_edits(e.doc.clone(), &[command.clone()]).is_err());
        assert!(e.edit("human", &[command], None, None, "Clamp").is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert_eq!(e.undo.len(), history);
        e.doc.layers[0].effects.push(effects::Effect {
            id: peerbrush::engine::id(),
            kind: "channel_clamp".into(),
            enabled: false,
            weight: 0.0,
            settings: effects::defaults("channel_clamp"),
        });
        assert!(psd::validate(&e.doc).is_err());
    }
}
