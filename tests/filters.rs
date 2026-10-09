use peerbrush::{
    depth16::{self, Image16},
    effects::{self, Image},
    engine::{Document, Engine, Scope},
    psd, server,
};
use serde_json::{json, Value};
use std::{
    io::Read,
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
                    [
                        10001 + x as u16 * 411,
                        30003 + y as u16 * 503,
                        50007,
                        12345 + x as u16 * 1003,
                    ],
                );
            } else {
                e.doc.layers[0].pixels.set(
                    x,
                    y,
                    [39 + x as u8 * 2, 117 + y as u8 * 3, 195, 48 + x as u8 * 4],
                );
            }
        }
    }
    e
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], None, None, "Whole-image filter")
        .unwrap();
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
fn full_composite_filters_keep_sources_isolation_preview_history_and_native_precision() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let target = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"effect.add","layer":target,"kind":"hsl","settings":{"saturation":-0.2}}),
        );
        let original = e.doc.clone();
        let source = native(&original);
        let source_key = e.doc.layers[0].effect_key.clone();
        let command =
            json!({"op":"filter.add","kind":"posterize","settings":{"levels":3},"weight":0.5});
        let draft = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
        let history = e.undo.len();
        edit(&mut e, command);
        assert_eq!(native(&draft), native(&e.doc));
        assert_eq!(e.undo.len(), history + 1);
        assert_eq!(e.doc.layers[0].pixels, original.layers[0].pixels);
        assert_eq!(
            e.doc.layers[0].effect_key, source_key,
            "Filter settings must reuse the layer's derived cache"
        );
        assert_eq!(
            e.doc.preview(None, 8, Some(&target), false).unwrap().2,
            original.preview(None, 8, Some(&target), false).unwrap().2
        );
        let after = if depth == 16 {
            let mut image = Image16 {
                width: 8,
                height: 6,
                words: source.clone(),
            };
            depth16::color::apply(&mut image, "posterize", &json!({"levels":3})).unwrap();
            image.words
        } else {
            let mut image = Image {
                width: 8,
                height: 6,
                bytes: source.iter().map(|v| *v as u8).collect(),
            };
            effects::apply(&mut image, "posterize", &json!({"levels":3})).unwrap();
            image.bytes.into_iter().map(u16::from).collect()
        };
        let expected = source
            .chunks_exact(4)
            .zip(after.chunks_exact(4))
            .flat_map(|(a, b)| {
                effects::weighted_pixel(a.try_into().unwrap(), b.try_into().unwrap(), 0.5)
            })
            .collect::<Vec<_>>();
        assert_eq!(native(&e.doc), expected);
        let filter = e.doc.filters[0].id.clone();
        edit(
            &mut e,
            json!({"op":"filter.update","filter":filter,"enabled":false}),
        );
        assert_eq!(native(&e.doc), source);
        e.undo("human").unwrap();
        assert_eq!(native(&e.doc), expected);
        e.undo("human").unwrap();
        assert_eq!(native(&e.doc), source);
    }
}

#[test]
fn filtered_psd_has_current_standard_pixels_protected_format_and_editable_originals() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let raw = e.doc.layers[0].pixels.clone();
        edit(
            &mut e,
            json!({"op":"filter.add","kind":"channel_clamp","settings":{"channel":"o","minimum":0.35,"maximum":0.6}}),
        );
        edit(&mut e, json!({"op":"filter.add","kind":"invert"}));
        let expected = native(&e.doc);
        let bytes = psd::encode(&e.doc).unwrap();
        let start = bytes.windows(4).position(|w| w == b"PBR1").unwrap() + 4;
        let mut source = Vec::new();
        flate2::read::ZlibDecoder::new(&bytes[start..])
            .read_to_end(&mut source)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&source).unwrap()["format"],
            15
        );
        let reopened = psd::decode(&bytes).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(reopened.filters.len(), 2);
        assert_eq!(reopened.layers[0].pixels, raw);
        assert_eq!(native(&reopened), expected);
        let standard = psd::decode(&strip_resources(&bytes)).unwrap();
        assert_eq!(standard.layers.len(), e.doc.layers.len() + 1);
        assert!(standard.layers[0].visible);
        assert!(standard.layers.iter().skip(1).all(|l| !l.visible));
        assert_eq!(standard.layers[1].pixels, raw);
        assert!(standard.filters.is_empty());
        assert_eq!(native(&standard), expected);
    }
}

#[test]
fn invalid_filters_locks_full_document_reservations_and_stale_targets_are_atomic() {
    let e = fixture(16);
    let target = e.doc.layers[0].id.clone();
    let document = e.doc.id.clone();
    let baseline = native(&e.doc);
    let shared = Arc::new(Mutex::new(e));
    let request = |command| json!({"actor":"human","document_id":document,"expected_revision":0,"commands":[command]});
    for command in [
        json!({"op":"filter.add","kind":"unknown"}),
        json!({"op":"filter.add","kind":"liquify"}),
        json!({"op":"filter.add","kind":"posterize","settings":{"levels":2.5}}),
        json!({"op":"filter.add","kind":"invert","weight":2}),
        json!({"op":"filter.add","kind":"invert","layer":target}),
        json!({"op":"filter.add","kind":"invert","rect":[0,0,4,4]}),
    ] {
        assert!(server::dispatch(&shared, "edit", &request(command)).is_err());
        let e = shared.lock().unwrap();
        assert!(e.doc.filters.is_empty());
        assert!(e.undo.is_empty());
        assert_eq!(native(&e.doc), baseline);
    }
    let command = json!({"op":"filter.add","kind":"invert"});
    shared.lock().unwrap().doc.layers[0].locked = true;
    assert!(server::dispatch(&shared, "edit", &request(command.clone())).is_err());
    {
        let mut e = shared.lock().unwrap();
        e.doc.layers[0].locked = false;
        e.reserve(
            "agent",
            "Keep one pixel",
            vec![Scope {
                target: Some(target),
                rect: Some([0, 0, 1, 1]),
            }],
        )
        .unwrap();
    }
    assert!(server::dispatch(&shared, "edit", &request(command.clone())).is_err());
    shared.lock().unwrap().leases.clear();
    server::dispatch(&shared, "edit", &request(command.clone())).unwrap();
    assert!(server::dispatch(&shared, "edit", &request(command)).is_err());
    let e = shared.lock().unwrap();
    assert_eq!(e.undo.len(), 1);
    assert_eq!(
        e.undo[0].scopes,
        vec![Scope {
            target: None,
            rect: None
        }]
    );
}

#[test]
fn filtered_standard_psd_retains_hidden_native_source_masks_and_off_canvas_rasters() {
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    edit(&mut e, json!({"op":"mask.add","layer":layer,"value":255}));
    edit(
        &mut e,
        json!({"op":"mask.step.add","layer":layer,"kind":"paint"}),
    );
    let l = &mut e.doc.layers[0];
    l.x = -2;
    l.y = 1;
    let step = l.mask.as_mut().unwrap().steps.last_mut().unwrap();
    for y in 0..6 {
        for x in 0..8 {
            step.pixels
                .set16(x, y, [12001 + x as u16 * 3701 + y as u16 * 301; 4]);
        }
    }
    let source = e.doc.clone();
    edit(
        &mut e,
        json!({"op":"filter.add","kind":"invert","weight":0.7}),
    );
    let standard = psd::decode(&strip_resources(&psd::encode(&e.doc).unwrap())).unwrap();
    assert!(!standard.read_only);
    assert_eq!(native(&standard), native(&e.doc));
    let hidden = standard
        .layers
        .iter()
        .find(|l| l.name == source.layers[0].name)
        .unwrap();
    assert!(!hidden.visible);
    assert_eq!((hidden.x, hidden.y), (-2, 1));
    assert_eq!(hidden.pixels, source.layers[0].pixels);
    let masks = depth16::prepare_masks(&standard).unwrap();
    let index = standard
        .layers
        .iter()
        .position(|l| l.id == hidden.id)
        .unwrap();
    let source_masks = depth16::prepare_masks(&source).unwrap();
    for y in 0..6 {
        for x in 0..8 {
            assert_eq!(
                (depth16::mask_value_prepared(hidden, x, y, masks[index].as_deref(), false)
                    * 65535.)
                    .round() as u16,
                (depth16::mask_value_prepared(
                    &source.layers[0],
                    x,
                    y,
                    source_masks[0].as_deref(),
                    false
                ) * 65535.)
                    .round() as u16
            );
        }
    }
}

#[test]
fn task_filter_compensation_preserves_later_human_pixels_and_independent_settings() {
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    edit(&mut e, json!({"op":"filter.add","kind":"adjust"}));
    let filter = e.doc.filters[0].id.clone();
    let task = e.reserve("agent", "Color correction", vec![]).unwrap().id;
    e.edit("agent",&[json!({"op":"filter.update","filter":filter,"settings":{"brightness":0.2,"contrast":1.0,"saturation":1.0}})],Some(e.doc.revision),Some(&task),"Correction").unwrap();
    edit(
        &mut e,
        json!({"op":"filter.update","filter":filter,"weight":0.4}),
    );
    edit(
        &mut e,
        json!({"op":"paint","layer":layer,"points":[[2,2]],"radius":1,"color":[200,40,20,255]}),
    );
    let human = e.doc.layers[0].pixels.clone();
    e.undo_task("human", "agent", &task).unwrap();
    assert_eq!(e.doc.layers[0].pixels, human);
    assert_eq!(e.doc.filters[0].weight, 0.4);
    assert_eq!(e.doc.filters[0].settings["brightness"], 0.0);
    e.undo("human").unwrap();
    assert_eq!(e.doc.filters[0].settings["brightness"], 0.2);
    edit(
        &mut e,
        json!({"op":"layer.update","layer":layer,"locked":true}),
    );
    assert!(e.undo_task("human", "agent", &task).is_err());
    assert_eq!(e.doc.layers[0].pixels, human);
}

#[test]
fn document_resize_maps_spatial_filter_sources_and_rejects_invalid_bounds_atomically() {
    let mut e = fixture(16);
    edit(
        &mut e,
        json!({"op":"filter.add","kind":"blur","settings":{"radius":2}}),
    );
    let original = e.doc.clone();
    edit(&mut e, json!({"op":"image.resize","width":16,"height":12}));
    assert_eq!(e.doc.filters[0].settings["radius"], 4.0);
    let changed = e.doc.clone();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"image.resize","width":20,"height":12})],
            None,
            None,
            "Nonproportional"
        )
        .is_err());
    assert_eq!(native(&e.doc), native(&changed));
    e.undo("human").unwrap();
    assert_eq!(native(&e.doc), native(&original));
    assert_eq!(e.doc.filters[0].settings["radius"], 2);
}

#[test]
fn derived_cache_tracks_cow_sources_uncommitted_settings_and_composite_alpha() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        edit(
            &mut e,
            json!({"op":"filter.add","kind":"channel_clamp","settings":{"channel":"o","minimum":0.25,"maximum":0.8}}),
        );
        let original = e.doc.clone();
        let baseline = native(&original);
        let mut draft = original.clone();
        if depth == 16 {
            draft.layers[0]
                .pixels
                .set16(7, 5, [12347, 45679, 23459, 51123]);
        } else {
            draft.layers[0].pixels.set(7, 5, [37, 211, 97, 199]);
        }
        // No revision/key change: retained COW identity still has to detect actual pixels.
        let changed = native(&draft);
        assert_ne!(changed, baseline);
        assert_eq!(native(&original), baseline);
        draft.filters[0].settings["maximum"] = json!(0.5);
        assert_ne!(native(&draft), changed);
        assert_eq!(native(&original), baseline);
    }
}

#[test]
fn merging_native_layers_keeps_the_global_filter_editable_and_applies_it_once() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let first = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"layer.add","kind":"paint","name":"Second"}),
        );
        let second = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"paint.fill","layer":second,"color":[200,100,50,80],"rect":[1,1,4,4]}),
        );
        edit(&mut e, json!({"op":"filter.add","kind":"invert"}));
        let before = native(&e.doc);
        let filters = serde_json::to_value(&e.doc.filters).unwrap();
        edit(&mut e, json!({"op":"layer.merge","layers":[second,first]}));
        assert_eq!(e.doc.layers.len(), 1);
        assert_eq!(native(&e.doc), before);
        assert_eq!(serde_json::to_value(&e.doc.filters).unwrap(), filters);
    }
}
