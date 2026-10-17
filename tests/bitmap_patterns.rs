use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::{
    depth16,
    effects::{self, patterns},
    engine::{Document, Engine, Scope},
    psd,
    raster::{self, Raster},
    server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn tile(depth: u16) -> (String, Vec<u16>) {
    let words = vec![
        12345, 23457, 34569, 65535, 45123, 16237, 52649, 65535, 59713, 38471, 11719, 65535, 21413,
        57377, 43691, 65535,
    ];
    if depth == 16 {
        (STANDARD.encode(raster::png16(2, 2, &words).unwrap()), words)
    } else {
        let bytes: Vec<u8> = words.into_iter().map(raster::project16).collect();
        (
            STANDARD.encode(raster::png(2, 2, &bytes).unwrap()),
            bytes.into_iter().map(|b| u16::from(b) * 257).collect(),
        )
    }
}
fn settings(png: &str) -> Value {
    json!({"pattern":"bitmap","bitmap_png":png,"opacity":1.})
}
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(12, 10, depth).unwrap();
    e.doc.layers[0].x = 3;
    e.doc.layers[0].y = 2;
    e.doc.layers[0].pixels = Raster::new_depth(6, 6, depth);
    for y in 0..6 {
        for x in 0..6 {
            e.doc.layers[0]
                .pixels
                .set16(x, y, [31913, 41417, 52721, 65535]);
        }
    }
    e
}
fn rendered(doc: &Document) -> Vec<u16> {
    if doc.bit_depth == 16 {
        depth16::render(doc).unwrap().words
    } else {
        doc.preview(None, 32, None, false)
            .unwrap()
            .2
            .into_iter()
            .map(|b| u16::from(b) * 257)
            .collect()
    }
}
fn strip(bytes: &[u8]) -> Vec<u8> {
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
fn tiled_native_samples_use_document_coordinates_and_promote_without_narrowing() {
    for depth in [8, 16] {
        let (png, words) = tile(depth);
        let mut e = fixture(8);
        let original = e.doc.layers[0].pixels.rgba16();
        let layer = e.doc.layers[0].id.clone();
        let command = json!({"op":"effect.add","layer":layer,"kind":"pattern_overlay","settings":settings(&png)});
        let preview = Engine::preview_edits(e.doc.clone(), std::slice::from_ref(&command)).unwrap();
        assert_eq!(e.doc.bit_depth, 8);
        assert!(e.undo.is_empty());
        e.edit("human", &[command], None, None, "Bitmap pattern")
            .unwrap();
        assert_eq!(e.doc.bit_depth, depth);
        assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
        assert_eq!(e.doc.layers[0].pixels.rgba16(), original);
        let output = rendered(&e.doc);
        for y in 0..10 {
            for x in 0..12 {
                let actual = &output[(y * 12 + x) * 4..(y * 12 + x) * 4 + 4];
                if (3..9).contains(&x) && (2..8).contains(&y) {
                    let at = ((y % 2) * 2 + x % 2) * 4;
                    assert_eq!(
                        actual,
                        &words[at..at + 4],
                        "native sample at {x},{y}, depth {depth}"
                    );
                } else {
                    assert_eq!(actual[3], 0);
                }
            }
        }
        assert_eq!(e.undo.len(), 1);
        if depth == 16 {
            let before = e.doc.clone();
            assert!(e
                .edit(
                    "human",
                    &[json!({"op":"document.settings","bit_depth":8})],
                    None,
                    None,
                    "Lower depth"
                )
                .is_err());
            assert_eq!(
                serde_json::to_value(&e.doc).unwrap(),
                serde_json::to_value(before).unwrap()
            );
        }
        e.undo("human").unwrap();
        assert_eq!(e.doc.bit_depth, 8);
        assert_eq!(e.doc.layers[0].pixels.rgba16(), original);
        e.redo("human").unwrap();
        assert_eq!(rendered(&e.doc), output);
    }
}
#[test]
fn rotation_scale_phase_and_wrap_have_analytic_native_sampling() {
    let (png, words) = tile(16);
    let b = patterns::get(&png).unwrap();
    let mut v = effects::normalized("pattern_overlay", &settings(&png)).unwrap();
    assert_eq!(b.sample(0., 0., &v), words[..4]);
    v["phase_x"] = json!(1.);
    v["phase_y"] = json!(-1.);
    assert_eq!(b.sample(0., 0., &v), words[12..16]);
    v["phase_x"] = json!(0.);
    v["phase_y"] = json!(0.);
    v["bitmap_angle"] = json!(90.);
    assert_eq!(b.sample(0., 0., &v), words[8..12]);
    v["bitmap_angle"] = json!(0.);
    v["bitmap_scale"] = json!(2.);
    v["sampling"] = json!("nearest");
    for y in 0..4 {
        for x in 0..4 {
            let at = ((y / 2) * 2 + x / 2) * 4;
            assert_eq!(b.sample(x as f64, y as f64, &v), words[at..at + 4]);
        }
    }
    v["bitmap_scale"] = json!(1.);
    v["sampling"] = json!("bilinear");
    v["phase_x"] = json!(0.5);
    let actual = b.sample(0., 0., &v);
    for c in 0..3 {
        assert_eq!(
            actual[c],
            ((u32::from(words[c]) + u32::from(words[4 + c])) as f64 / 2.).round() as u16
        );
    }
    assert_eq!(actual[3], 65535);
}
#[test]
fn translucent_pattern_resampling_is_premultiplied_and_preserves_layer_alpha() {
    let png = STANDARD
        .encode(raster::png16(2, 1, &[12013, 23017, 34019, 65535, 65535, 0, 0, 0]).unwrap());
    let bitmap = patterns::get(&png).unwrap();
    let mut v = effects::normalized("pattern_overlay", &settings(&png)).unwrap();
    v["phase_x"] = json!(0.5);
    assert_eq!(bitmap.sample(0., 0., &v), [12013, 23017, 34019, 32768]);
    let mut image = depth16::Image16 {
        width: 2,
        height: 1,
        words: vec![12013, 23017, 34019, 27123, 11111, 22223, 33337, 0],
    };
    depth16::color::apply(&mut image, "pattern_overlay", &v).unwrap();
    assert_eq!(
        image.words,
        [12013, 23017, 34019, 27123, 11111, 22223, 33337, 0]
    );
    let transparent = STANDARD.encode(raster::png16(1, 1, &[65535, 0, 0, 0]).unwrap());
    let before = image.words.clone();
    depth16::color::apply(&mut image, "pattern_overlay", &settings(&transparent)).unwrap();
    assert_eq!(image.words, before);
}
#[test]
fn bitmap_sources_roundtrip_with_masks_and_current_standard_psd_psb_channels() {
    for depth in [8, 16] {
        let (png, _) = tile(depth);
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        let original = e.doc.layers[0].pixels.rgba16();
        e.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"pattern_overlay","settings":settings(&png)}),json!({"op":"mask.add","layer":layer}),json!({"op":"mask.step.add","layer":layer,"kind":"fill","value":127})],None,None,"Masked pattern").unwrap();
        let expected = rendered(&e.doc);
        for bytes in [
            psd::encode(&e.doc).unwrap(),
            psd::encode_psb(&e.doc).unwrap(),
        ] {
            let restored = psd::decode(&bytes).unwrap();
            assert!(!restored.read_only);
            assert_eq!(restored.layers[0].effects[0].settings["bitmap_png"], png);
            assert_eq!(restored.layers[0].pixels.rgba16(), original);
            assert_eq!(rendered(&restored), expected);
            let standard = psd::decode(&strip(&bytes)).unwrap();
            assert!(!standard.read_only);
            assert_eq!(rendered(&standard), expected);
            use std::io::Read;
            let start = bytes.windows(4).position(|s| s == b"PBR1").unwrap() + 4;
            let mut source = Vec::new();
            flate2::read::ZlibDecoder::new(&bytes[start..])
                .read_to_end(&mut source)
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&source).unwrap()["format"],
                29
            );
        }
        e.edit(
            "human",
            &[json!({"op":"crop","rect":[2,1,10,9]})],
            None,
            None,
            "Crop patterned canvas",
        )
        .unwrap();
        let cropped = rendered(&e.doc);
        for y in 0..8 {
            for x in 0..8 {
                let at = (y * 8 + x) * 4;
                let old = ((y + 1) * 12 + x + 2) * 4;
                assert_eq!(&cropped[at..at + 4], &expected[old..old + 4]);
            }
        }
        e.undo("human").unwrap();
        assert_eq!(rendered(&e.doc), expected);
        e.edit(
            "human",
            &[json!({"op":"image.resize","width":24,"height":20})],
            None,
            None,
            "Resize pattern",
        )
        .unwrap();
        assert_eq!(e.doc.layers[0].effects[0].settings["bitmap_scale"], 2.);
        assert_eq!(e.doc.layers[0].effects[0].settings["bitmap_png"], png);
        e.undo("human").unwrap();
        assert_eq!(rendered(&e.doc), expected);
    }
}
#[test]
fn invalid_assets_and_limits_reject_atomically_including_disabled_sources() {
    let (png, _) = tile(16);
    let mut e = fixture(8);
    let layer = e.doc.layers[0].id.clone();
    let mut invalid = vec![
        json!({"pattern":"bitmap"}),
        settings("!base64"),
        settings(&STANDARD.encode(b"not png")),
        json!({"pattern":"bitmap","bitmap_png":png,"bitmap_scale":0.}),
        json!({"pattern":"bitmap","bitmap_png":png,"phase_x":32769}),
        json!({"pattern":"bitmap","bitmap_png":png,"color":[1,0,0]}),
        json!({"bitmap_png":png}),
    ];
    invalid.push(settings(
        &STANDARD.encode(raster::png(513, 1, &vec![0; 513 * 4]).unwrap()),
    ));
    invalid.push(settings(
        &"A".repeat(patterns::ENCODED_LIMIT.div_ceil(3) * 4 + 1),
    ));
    for v in invalid {
        let before = serde_json::to_value(&e.doc).unwrap();
        let history = e.undo.len();
        assert!(e
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"pattern_overlay","settings":v})],
                None,
                None,
                "Invalid pattern"
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert_eq!(e.undo.len(), history);
    }
    let native = effects::Effect {
        id: peerbrush::engine::id(),
        kind: "pattern_overlay".into(),
        enabled: false,
        weight: 0.,
        settings: effects::normalized("pattern_overlay", &settings(&png)).unwrap(),
    };
    e.doc.layers[0].effects.push(native);
    assert!(
        patterns::validate_document(&e.doc).is_err(),
        "Disabled sources cannot narrow 16-bit data"
    );
    e.doc.bit_depth = 16;
    assert!(patterns::validate_document(&e.doc).is_ok());
    let large = STANDARD.encode(raster::png16(512, 512, &vec![12345; 512 * 512 * 4]).unwrap());
    let effect = effects::Effect {
        settings: effects::normalized("pattern_overlay", &settings(&large)).unwrap(),
        ..e.doc.layers[0].effects[0].clone()
    };
    e.doc.layers[0].effects = vec![effect; 17];
    assert!(patterns::validate_document(&e.doc)
        .unwrap_err()
        .contains("32 MiB"));
}
#[test]
fn shared_protocol_returns_actual_pixels_coordinates_and_enforces_reservations() {
    let shared = Arc::new(Mutex::new(fixture(8)));
    let doc = shared.lock().unwrap().doc.clone();
    let layer = doc.layers[0].id.clone();
    let (png, _) = tile(16);
    shared
        .lock()
        .unwrap()
        .reserve(
            "other",
            "Pattern area",
            vec![Scope {
                target: Some(layer.clone()),
                rect: Some([0, 0, 2, 2]),
            }],
        )
        .unwrap();
    let request = json!({"actor":"human","expected_revision":doc.revision,"commands":[{"op":"effect.add","layer":layer,"kind":"pattern_overlay","settings":settings(&png)}],"max_edge":32});
    assert!(server::dispatch(&shared, "edit", &request).is_err());
    assert_eq!(shared.lock().unwrap().doc.bit_depth, 8);
    shared.lock().unwrap().leases.clear();
    let reply = server::dispatch(&shared, "edit", &request).unwrap();
    assert_eq!(reply["images"][0]["document_rect"], json!([0, 0, 12, 10]));
    let bytes = STANDARD
        .decode(reply["images"][0]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        image::load_from_memory(&bytes)
            .unwrap()
            .into_rgba8()
            .into_raw(),
        shared
            .lock()
            .unwrap()
            .doc
            .preview(None, 32, None, false)
            .unwrap()
            .2
    );
    let history = shared.lock().unwrap().undo.len();
    assert!(server::dispatch(&shared, "edit", &request).is_err());
    assert_eq!(shared.lock().unwrap().undo.len(), history);
}

#[test]
fn folder_and_internal_layer_clipboard_keep_embedded_native_sources() {
    let (png, words) = tile(16);
    let mut e = fixture(8);
    let child = e.doc.layers[0].id.clone();
    let original = e.doc.layers[0].pixels.rgba16();
    let folder = peerbrush::engine::Layer::new("Patterned folder", "group", 12, 10);
    let group = folder.id.clone();
    e.doc.layers[0].parent = Some(group.clone());
    e.doc.layers.insert(0, folder);
    e.edit("human",&[json!({"op":"effect.add","layer":group,"kind":"pattern_overlay","settings":settings(&png)})],None,None,"Folder bitmap").unwrap();
    assert_eq!(e.doc.bit_depth, 16);
    assert_eq!(
        e.doc
            .layers
            .iter()
            .find(|l| l.id == child)
            .unwrap()
            .pixels
            .rgba16(),
        original
    );
    let image = depth16::render(&e.doc).unwrap();
    assert_eq!(image.get(3, 2), words[4..8]);
    let copied = peerbrush::layer_clipboard::copy(&e.doc, &[group]).unwrap();
    let restored: peerbrush::layer_clipboard::Layers =
        serde_json::from_value(serde_json::to_value(copied).unwrap()).unwrap();
    let mut target = Document::new(12, 10).unwrap();
    let dest = target.layers[0].id.clone();
    let pasted = peerbrush::layer_clipboard::paste(&mut target, &restored, &dest).unwrap();
    assert_eq!(target.bit_depth, 16);
    let folder = target.layers.iter().find(|l| l.id == pasted[0]).unwrap();
    assert_eq!(folder.effects[0].settings["bitmap_png"], png);
    assert_eq!(rendered(&target), image.words);
    let decoded = psd::decode(&psd::encode(&target).unwrap()).unwrap();
    assert_eq!(rendered(&decoded), image.words);
}

#[test]
fn bitmap_icc_uses_the_shared_native_depth_importer_and_rejects_bad_profiles() {
    let (_, words) = tile(16);
    let png = STANDARD.encode(
        raster::png16_with_profile(2, 2, &words, Some(peerbrush::color_profile::srgb_profile()))
            .unwrap(),
    );
    let b = patterns::get(&png).unwrap();
    assert_eq!(b.depth, 16);
    let v = effects::normalized("pattern_overlay", &settings(&png)).unwrap();
    assert_eq!(b.sample(0., 0., &v), words[..4]);
    assert_eq!(v["bitmap_png"], png);
    let invalid = STANDARD
        .encode(raster::png16_with_profile(2, 2, &words, Some(b"unsupported profile")).unwrap());
    assert!(patterns::get(&invalid).is_err());
}

#[test]
fn bitmap_texture_uses_native_luminance_height_and_transparency_is_neutral() {
    for depth in [8, 16] {
        let samples = [0u16, 16385, 32769, 49153];
        let native: Vec<u16> = samples
            .into_iter()
            .map(|n| {
                if depth == 8 {
                    u16::from(raster::project16(n)) * 257
                } else {
                    n
                }
            })
            .collect();
        let words: Vec<u16> = native.iter().flat_map(|n| [*n, *n, *n, 65535]).collect();
        let png = if depth == 16 {
            raster::png16(4, 1, &words).unwrap()
        } else {
            raster::png(
                4,
                1,
                &words
                    .iter()
                    .copied()
                    .map(raster::project16)
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let v = json!({"pattern":"bitmap","bitmap_png":STANDARD.encode(png),"sampling":"nearest","angle":0.,"depth":1.,"opacity":1.,"color":[0.,0.,0.],"color2":[1.,1.,1.]});
        let original = [12345, 23457, 34569, 41317];
        let mut pixels: Vec<u16> = (0..4).flat_map(|_| original).collect();
        effects::styles::apply(&mut pixels, 4, 1, "texture", &v).unwrap();
        for x in 0..4 {
            let delta = i32::from(native[(x + 1) % 4]) - i32::from(native[(x + 3) % 4]);
            let c = if delta > 0 { 65535 } else { 0 };
            let mut expected = raster::blend16(
                [original[0], original[1], original[2], 65535],
                [c, c, c, delta.unsigned_abs() as u16],
                1.,
                "normal",
            );
            expected[3] = original[3];
            assert_eq!(&pixels[x * 4..x * 4 + 4], &expected);
        }
        let transparent: Vec<u16> = words
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2], 0])
            .collect();
        let transparent = STANDARD.encode(raster::png16(4, 1, &transparent).unwrap());
        let mut v = v.clone();
        v["bitmap_png"] = json!(transparent);
        let mut pixels: Vec<u16> = (0..4).flat_map(|_| original).collect();
        effects::styles::apply(&mut pixels, 4, 1, "texture", &v).unwrap();
        assert_eq!(pixels, (0..4).flat_map(|_| original).collect::<Vec<_>>());
    }
}
#[test]
fn bitmap_texture_shared_commands_keep_native_sources_geometry_history_and_format19() {
    let (png, _) = tile(16);
    let mut e = fixture(8);
    let before = e.doc.export_png().unwrap();
    let source = e.doc.layers[0].pixels.rgba16();
    let layer = e.doc.layers[0].id.clone();
    let command = json!({"op":"effect.add","layer":layer,"kind":"texture","settings":{"pattern":"bitmap","bitmap_png":png,"bitmap_scale":1.3,"bitmap_angle":17.,"phase_x":2.,"phase_y":-1.,"angle":61.,"color":[0.1,0.2,0.3],"color2":[0.7,0.8,0.9]}});
    let preview = Engine::preview_edits(e.doc.clone(), std::slice::from_ref(&command)).unwrap();
    assert_eq!(e.doc.bit_depth, 8);
    e.edit("human", &[command], None, None, "Bitmap Texture")
        .unwrap();
    assert_eq!(e.doc.bit_depth, 16);
    assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
    assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
    assert_ne!(e.doc.export_png().unwrap(), before);
    assert_eq!(e.undo.len(), 1);
    let current = e.doc.export_png().unwrap();
    for bytes in [
        psd::encode(&e.doc).unwrap(),
        psd::encode_psb(&e.doc).unwrap(),
    ] {
        let d = psd::decode(&bytes).unwrap();
        assert_eq!(d.layers[0].effects[0].settings["bitmap_png"], png);
        assert_eq!(d.layers[0].pixels.rgba16(), source);
        assert_eq!(d.export_png().unwrap(), current);
    }
    let mut invalid = e.doc.layers[0].effects[0].settings.clone();
    invalid["invert"] = json!(true);
    assert!(effects::validate("texture", &invalid).is_err());
    e.undo("human").unwrap();
    assert_eq!(e.doc.bit_depth, 8);
    assert_eq!(e.doc.export_png().unwrap(), before);
    e.redo("human").unwrap();
    assert_eq!(e.doc.export_png().unwrap(), current);
}
