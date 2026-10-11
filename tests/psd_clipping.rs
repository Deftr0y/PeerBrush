use peerbrush::{
    engine::{Document, Engine, Scope},
    psd,
    raster::{blend, blend16},
};
use serde_json::json;

#[derive(Clone)]
struct Spec {
    name: &'static str,
    pixels: [[u16; 4]; 2],
    blend: [u8; 4],
    clipping: u8,
    opacity: u8,
    flags: u8,
    mask: Option<[u16; 2]>,
    section: Option<u32>,
    tags: Vec<([u8; 4], Vec<u8>)>,
    ranges: Vec<u8>,
}
impl Spec {
    fn paint(name: &'static str, pixels: [[u16; 4]; 2]) -> Self {
        Self {
            name,
            pixels,
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
            flags: 0,
            mask: None,
            section: None,
            tags: vec![],
            ranges: vec![],
        }
    }
    fn folder(name: &'static str, section: u32) -> Self {
        let mut spec = Self::paint(name, [[0; 4]; 2]);
        spec.section = Some(section);
        spec
    }
}
fn block(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
}
fn tag(out: &mut Vec<u8>, key: &[u8; 4], bytes: &[u8]) {
    out.extend(b"8BIM");
    out.extend(key);
    block(out, bytes);
    if bytes.len() % 2 != 0 {
        out.push(0);
    }
}
fn native(value: u16, depth: u16) -> u16 {
    if depth == 16 {
        value
    } else {
        ((value as u32 + 128) / 257) as u16 * 257
    }
}
fn sample(out: &mut Vec<u8>, value: u16, depth: u16) {
    if depth == 16 {
        out.extend(value.to_be_bytes());
    } else {
        out.push((native(value, depth) / 257) as u8);
    }
}
const SAVED: [[u16; 4]; 2] = [[12345, 23456, 34567, 65535], [54321, 32123, 11223, 65535]];

fn pattern_text(value: &str) -> Vec<u8> {
    let words = value.encode_utf16().collect::<Vec<_>>();
    let mut out = (words.len() as u32).to_be_bytes().to_vec();
    for word in words {
        out.extend(word.to_be_bytes());
    }
    out
}
fn pattern_payload(enabled: bool, identity: &str, omit: &str, scale: f64) -> Vec<u8> {
    let mut mode = vec![];
    overlay_id(&mut mode, "BlnM");
    overlay_id(&mut mode, "Nrml");
    let percent = |n: f64| [b"#Prc".to_vec(), n.to_be_bytes().to_vec()].concat();
    let mut properties = vec![
        ("enab", b"bool", vec![u8::from(enabled)]),
        ("present", b"bool", vec![1]),
        ("Md  ", b"enum", mode),
        ("Opct", b"UntF", percent(100.)),
        (
            "Angl",
            b"UntF",
            [b"#Ang".to_vec(), 0f64.to_be_bytes().to_vec()].concat(),
        ),
        ("Scl ", b"UntF", percent(scale)),
        ("Algn", b"bool", vec![0]),
        (
            "phase",
            b"Objc",
            overlay_descriptor(
                "Pnt ",
                vec![
                    ("Hrzn", b"doub", 0f64.to_be_bytes().to_vec()),
                    ("Vrtc", b"doub", 0f64.to_be_bytes().to_vec()),
                ],
            ),
        ),
        (
            "Ptrn",
            b"Objc",
            overlay_descriptor(
                "Ptrn",
                vec![
                    ("Nm  ", b"TEXT", pattern_text("Synthetic tile")),
                    ("Idnt", b"TEXT", pattern_text(identity)),
                ],
            ),
        ),
    ];
    properties.retain(|(key, _, _)| *key != omit);
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        overlay_descriptor(
            "Lefx",
            vec![
                ("masterFXSwitch", b"bool", vec![1]),
                ("Scl ", b"UntF", percent(100.)),
                (
                    "patternFill",
                    b"Objc",
                    overlay_descriptor("patternFill", properties),
                ),
            ],
        ),
    ]
    .concat()
}
const PATTERN: [[u16; 4]; 2] = [[31337, 12347, 54323, 65535], [43219, 19237, 22349, 65535]];
fn pattern_resource(depth: u16, identity: &str) -> Vec<u8> {
    let rect = [0i32, 0, 1, 2]
        .into_iter()
        .flat_map(i32::to_be_bytes)
        .collect::<Vec<_>>();
    let mut arrays = rect.clone();
    arrays.extend(3u32.to_be_bytes());
    for channel in 0..3 {
        arrays.extend(1u32.to_be_bytes());
        let mut data = u32::from(depth).to_be_bytes().to_vec();
        data.extend(&rect);
        data.extend(depth.to_be_bytes());
        data.push(0);
        for pixel in PATTERN {
            sample(&mut data, pixel[channel], depth);
        }
        block(&mut arrays, &data);
    }
    arrays.extend([0; 8]);
    let mut pattern = 1u32.to_be_bytes().to_vec();
    pattern.extend(3u32.to_be_bytes());
    pattern.extend(1i16.to_be_bytes());
    pattern.extend(2i16.to_be_bytes());
    pattern.extend(pattern_text("Synthetic tile"));
    pattern.push(identity.len() as u8);
    pattern.extend(identity.as_bytes());
    pattern.extend(3u32.to_be_bytes());
    block(&mut pattern, &arrays);
    let mut out = vec![];
    block(&mut out, &pattern);
    out.resize(out.len().div_ceil(4) * 4, 0);
    out
}
fn inject_pattern(
    mut bytes: Vec<u8>,
    depth: u16,
    resource: &[u8],
    replace_composite: bool,
) -> Vec<u8> {
    let at = 34;
    let n = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let mut section = bytes[at + 4..at + 4 + n].to_vec();
    tag(&mut section, b"Patt", resource);
    section.resize(section.len().div_ceil(4) * 4, 0);
    let composite = bytes[at + 4 + n..].to_vec();
    bytes.truncate(at);
    block(&mut bytes, &section);
    if replace_composite {
        bytes.extend(0u16.to_be_bytes());
        for channel in 0..4 {
            for pixel in PATTERN {
                sample(&mut bytes, pixel[channel], depth);
            }
        }
    } else {
        bytes.extend(composite);
    }
    bytes
}
#[test]
fn plain_photoshop_pattern_import_retains_native_tiles_sources_and_editable_saves() {
    for depth in [8, 16] {
        for enabled in [true, false] {
            let mut spec = Spec::paint("Synthetic pattern source", SAVED);
            spec.tags.push((
                *b"lfx2",
                pattern_payload(enabled, "synthetic-pattern", "", 100.),
            ));
            let bytes = inject_pattern(
                fixture(&[spec], depth),
                depth,
                &pattern_resource(depth, "synthetic-pattern"),
                enabled,
            );
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            assert_eq!(doc.layers[0].effects[0].kind, "pattern_overlay");
            assert_eq!(doc.layers[0].effects[0].enabled, enabled);
            let bitmap = peerbrush::effects::patterns::get(
                doc.layers[0].effects[0].settings["bitmap_png"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(bitmap.depth, depth);
            for x in 0..2 {
                assert_eq!(
                    doc.layers[0].pixels.get16(x, 0),
                    SAVED[x as usize].map(|n| native(n, depth))
                );
            }
            let rendered = doc.export_png().unwrap();
            let source = doc.layers[0].pixels.rgba16();
            let layer = doc.layers[0].id.clone();
            let effect = doc.layers[0].effects[0].id.clone();
            let mut engine = Engine::new();
            engine.doc = doc;
            let mut settings = engine.doc.layers[0].effects[0].settings.clone();
            settings["phase_x"] = json!(1.);
            engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":settings})],None,None,"Pattern phase").unwrap();
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
            assert_eq!(engine.undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.export_png().unwrap(), rendered);
            for large in [false, true] {
                let reopened = psd::decode(&if large {
                    psd::encode_psb(&engine.doc).unwrap()
                } else {
                    psd::encode(&engine.doc).unwrap()
                })
                .unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.export_png().unwrap(), rendered);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
            }
        }
    }
}
#[test]
fn unsupported_missing_ambiguous_unused_and_mismatched_pattern_sources_stay_protected() {
    for depth in [8, 16] {
        for (identity, omit, scale) in [
            ("missing", "", 100.),
            ("synthetic-pattern", "Ptrn", 100.),
            ("synthetic-pattern", "phase", 100.),
            ("synthetic-pattern", "Angl", 100.),
            ("synthetic-pattern", "Algn", 100.),
            ("synthetic-pattern", "Opct", 100.),
            ("synthetic-pattern", "", 50.),
        ] {
            let mut spec = Spec::paint("Synthetic protected pattern", SAVED);
            spec.tags
                .push((*b"lfx2", pattern_payload(true, identity, omit, scale)));
            let doc = psd::decode(&inject_pattern(
                fixture(&[spec], depth),
                depth,
                &pattern_resource(depth, "synthetic-pattern"),
                false,
            ))
            .unwrap();
            assert!(doc.read_only, "{identity}/{omit}/{scale}");
            assert!(doc.layers[0].effects.is_empty());
            assert_eq!(
                doc.layers[0].pixels.get16(0, 0),
                SAVED[0].map(|n| native(n, depth))
            );
        }
        let mut spec = Spec::paint("Synthetic gated pattern", SAVED);
        spec.tags.push((
            *b"lfx2",
            pattern_payload(true, "synthetic-pattern", "", 100.),
        ));
        let base = fixture(&[spec], depth);
        for resource in [
            pattern_resource(depth, "synthetic-pattern"),
            [
                pattern_resource(depth, "synthetic-pattern"),
                pattern_resource(depth, "unused-pattern"),
            ]
            .concat(),
            [
                pattern_resource(depth, "synthetic-pattern"),
                pattern_resource(depth, "synthetic-pattern"),
            ]
            .concat(),
            pattern_resource(if depth == 16 { 8 } else { 16 }, "synthetic-pattern"),
        ] {
            let doc = psd::decode(&inject_pattern(base.clone(), depth, &resource, false)).unwrap();
            assert!(doc.read_only);
            assert!(doc.layers[0].effects.is_empty());
            assert_eq!(
                doc.layers[0].pixels.get16(0, 0),
                SAVED[0].map(|n| native(n, depth))
            );
        }
    }
}

// Handwritten standard PSD records, independent of PeerBrush's encoder/private
// sources. Input order is the editor's top-to-bottom tree, including dividers.
fn fixture(specs: &[Spec], depth: u16) -> Vec<u8> {
    fixture_size(specs, depth, 2, 1)
}
fn fixture_size(specs: &[Spec], depth: u16, width: u32, height: u32) -> Vec<u8> {
    assert_eq!(width * height, 2);
    let mut records = vec![];
    let mut channels = vec![];
    for spec in specs.iter().rev() {
        let (w, h) = if spec.section.is_some() {
            (0i32, 0i32)
        } else {
            (width as i32, height as i32)
        };
        for value in [0i32, 0, h, w] {
            records.extend(value.to_be_bytes());
        }
        records.extend((if spec.mask.is_some() { 5u16 } else { 4u16 }).to_be_bytes());
        for channel in [-1i16, 0, 1, 2].into_iter().chain(spec.mask.map(|_| -2)) {
            let mut data = 0u16.to_be_bytes().to_vec();
            if channel == -2 {
                for value in spec.mask.unwrap() {
                    sample(&mut data, value, depth);
                }
            } else if w != 0 {
                for pixel in spec.pixels {
                    sample(
                        &mut data,
                        pixel[if channel == -1 { 3 } else { channel as usize }],
                        depth,
                    );
                }
            }
            records.extend(channel.to_be_bytes());
            records.extend((data.len() as u32).to_be_bytes());
            channels.extend(data);
        }
        records.extend(b"8BIM");
        records.extend(spec.blend);
        records.extend([spec.opacity, spec.clipping, spec.flags, 0]);
        let mut extra = vec![];
        let mut mask = vec![];
        if spec.mask.is_some() {
            for value in [0i32, 0, height as i32, width as i32] {
                mask.extend(value.to_be_bytes());
            }
            mask.extend([255, 0, 0, 0]);
        }
        block(&mut extra, &mask);
        block(&mut extra, &spec.ranges);
        extra.push(spec.name.len() as u8);
        extra.extend(spec.name.as_bytes());
        extra.resize((extra.len() + 3) & !3, 0);
        if let Some(section) = spec.section {
            tag(&mut extra, b"lsct", &section.to_be_bytes());
        }
        for (key, data) in &spec.tags {
            tag(&mut extra, key, data);
        }
        block(&mut records, &extra);
    }
    let mut info = (-(specs.len() as i16)).to_be_bytes().to_vec();
    info.extend(records);
    info.extend(channels);
    if info.len() % 2 != 0 {
        info.push(0);
    }
    let mut section = vec![];
    if depth == 16 {
        section.extend([0; 8]);
        tag(&mut section, b"Lr16", &info);
        section.resize((section.len() + 3) & !3, 0);
    } else {
        block(&mut section, &info);
        section.extend([0; 4]);
    }
    let mut out = b"8BPS".to_vec();
    out.extend(1u16.to_be_bytes());
    out.extend([0; 6]);
    out.extend(4u16.to_be_bytes());
    out.extend(height.to_be_bytes());
    out.extend(width.to_be_bytes());
    out.extend(depth.to_be_bytes());
    out.extend(3u16.to_be_bytes());
    out.extend([0; 8]);
    block(&mut out, &section);
    out.extend(0u16.to_be_bytes());
    for c in 0..4 {
        for pixel in SAVED {
            sample(&mut out, pixel[c], depth);
        }
    }
    out
}
fn ordinary(bytes: &[u8]) -> Vec<u8> {
    let n = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
    let mut out = bytes[..30].to_vec();
    out.extend([0; 4]);
    out.extend(&bytes[34 + n..]);
    out
}

#[test]
fn default_photoshop_blending_ranges_include_transparency_and_mask_pairs() {
    for depth in [8, 16] {
        for masked in [false, true] {
            let mut top = Spec::paint("Default ranges", SAVED);
            if masked {
                top.mask = Some([12345, 65535]);
            }
            top.ranges = [0, 0, 255, 255].repeat(if masked { 12 } else { 10 });
            let bottom = Spec::paint("Base", [[21001, 31003, 41007, 65535]; 2]);
            let mut reference = top.clone();
            reference.ranges.clear();
            let expected = psd::decode(&fixture(&[reference, bottom.clone()], depth)).unwrap();
            let doc = psd::decode(&fixture(&[top.clone(), bottom.clone()], depth)).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            assert_eq!(doc.layers.len(), 2);
            assert_eq!(rendered(&doc), rendered(&expected));
            for (a, b) in doc.layers.iter().zip(&expected.layers) {
                assert_eq!(a.pixels, b.pixels);
                assert_eq!(a.mask_value_raw(0, 0), b.mask_value_raw(0, 0));
            }
            for bytes in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.bit_depth, depth);
                assert_eq!(rendered(&reopened), rendered(&doc));
                assert_eq!(reopened.layers[0].pixels, doc.layers[0].pixels);
            }
            // A real range change, excessive channel count or incomplete pair
            // must retain protection rather than being accepted as a no-op.
            for ranges in [
                {
                    let mut r = top.ranges.clone();
                    r[3] = 254;
                    r
                },
                [0, 0, 255, 255].repeat(if masked { 14 } else { 12 }),
                {
                    let mut r = top.ranges.clone();
                    r.pop();
                    r
                },
            ] {
                let mut unsupported = top.clone();
                unsupported.ranges = ranges;
                let protected =
                    psd::decode(&fixture(&[unsupported, bottom.clone()], depth)).unwrap();
                assert!(protected.read_only);
                assert!(protected.warnings.iter().any(|w| w.contains("Blend If")));
                assert!(psd::encode(&protected).is_err());
                assert_eq!(
                    rendered(&protected),
                    SAVED
                        .concat()
                        .into_iter()
                        .map(|v| native(v, depth))
                        .collect::<Vec<_>>()
                );
            }
            top.tags.push((*b"lmfx", vec![0; 16]));
            let protected = psd::decode(&fixture(&[top, bottom], depth)).unwrap();
            assert!(protected.read_only, "Layer styles must still be protected");
            assert!(!protected.warnings.iter().any(|w| w.contains("Blend If")));
            assert!(protected.warnings.iter().any(|w| w.contains("lmfx")));
        }
    }
}

#[test]
fn protocol_default_range_psd_opens_editable_and_protected_report_keeps_source_intact() {
    use peerbrush::server;
    use std::sync::{Arc, Mutex};
    let dir = std::env::temp_dir().join(format!(
        "peerbrush-default-ranges-{}",
        peerbrush::engine::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for depth in [8, 16] {
        let mut spec = Spec::paint("Standard raster", SAVED);
        spec.ranges = [0, 0, 255, 255].repeat(10);
        let source = fixture(&[spec.clone()], depth);
        let path = dir.join(format!("Source-{depth}.psd"));
        std::fs::write(&path, &source).unwrap();
        let shared = Arc::new(Mutex::new(Engine::new()));
        peerbrush::workspace::attach(&shared);
        server::dispatch(
            &shared,
            "document",
            &json!({"action":"open","path":path,"actor":"human"}),
        )
        .unwrap();
        let before = shared.lock().unwrap().doc.export_png().unwrap();
        let document = shared.lock().unwrap().doc.id.clone();
        let layer = shared.lock().unwrap().doc.layers[0].id.clone();
        assert!(!shared.lock().unwrap().doc.read_only);
        server::dispatch(&shared, "edit", &json!({"actor":"human","document_id":document,"expected_revision":0,"commands":[{"op":"paint.fill","layer":layer,"rect":[0,0,1,1],"color":[233,84,32,255]}]})).unwrap();
        assert_ne!(shared.lock().unwrap().doc.export_png().unwrap(), before);
        server::dispatch(
            &shared,
            "history",
            &json!({"action":"undo","actor":"human"}),
        )
        .unwrap();
        assert_eq!(shared.lock().unwrap().doc.export_png().unwrap(), before);
        let saved = dir.join(format!("Edited-{depth}.psd"));
        server::dispatch(&shared, "document", &json!({"action":"save","path":saved,"actor":"human","document_id":document,"expected_revision":2})).unwrap();
        let reopened = psd::decode(&std::fs::read(saved).unwrap()).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(reopened.bit_depth, depth);
        assert_eq!(reopened.export_png().unwrap(), before);
        assert_eq!(std::fs::read(&path).unwrap(), source);

        spec.tags.push((*b"lmfx", vec![0; 16]));
        let protected_source = fixture(&[spec], depth);
        let protected_path = dir.join(format!("Protected-{depth}.psd"));
        std::fs::write(&protected_path, &protected_source).unwrap();
        server::dispatch(
            &shared,
            "document",
            &json!({"action":"open","path":protected_path,"actor":"human"}),
        )
        .unwrap();
        let state = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
        for panel in ["document_info", "document_sources"] {
            server::dispatch(
                &shared,
                "capture_ui",
                &json!({"path":dir.join("report.png"),"panel":panel}),
            )
            .unwrap();
        }
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
            state
        );
        assert!(server::save(&shared, &protected_path).is_err());
        assert_eq!(std::fs::read(&protected_path).unwrap(), protected_source);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
fn saved_composite(bytes: &[u8], depth: u16) -> Vec<u16> {
    let bytes = ordinary(bytes);
    let n = u32::from_be_bytes(bytes[34..38].try_into().unwrap()) as usize;
    let plane = &bytes[40 + n..];
    assert_eq!(&bytes[38 + n..40 + n], &[0, 0]);
    let words: Vec<u16> = if depth == 16 {
        plane
            .chunks_exact(2)
            .map(|v| u16::from_be_bytes(v.try_into().unwrap()))
            .collect()
    } else {
        plane.iter().map(|v| *v as u16 * 257).collect()
    };
    (0..2)
        .flat_map(|x| (0..4).map(move |c| (x, c)))
        .map(|(x, c)| words[c * 2 + x])
        .collect()
}
fn stack() -> Vec<Spec> {
    let mut top = Spec::paint("Highlights", [[45001, 13001, 17001, 36001]; 2]);
    top.clipping = 1;
    top.blend = *b"scrn";
    top.opacity = 203;
    let mut clip = Spec::paint("Texture", [[50001, 20001, 30001, 49001]; 2]);
    clip.clipping = 1;
    clip.blend = *b"mul ";
    clip.opacity = 173;
    clip.mask = Some([35001, 65535]);
    let mut base = Spec::paint(
        "Subject",
        [[12001, 24001, 36001, 32769], [11111, 22223, 33337, 0]],
    );
    base.opacity = 192;
    base.blend = *b"over";
    base.mask = Some([40001, 57001]);
    base.tags.push((*b"clbl", vec![1, 0, 0, 0]));
    vec![
        top,
        clip,
        base,
        Spec::paint("Backdrop", [[6001, 12001, 50001, 65535]; 2]),
    ]
}
fn rendered(doc: &Document) -> Vec<u16> {
    if doc.bit_depth == 16 {
        peerbrush::depth16::render(doc).unwrap().words
    } else {
        doc.preview(None, 2, None, false)
            .unwrap()
            .2
            .into_iter()
            .map(|v| v as u16 * 257)
            .collect()
    }
}
fn expected(specs: &[Spec], depth: u16, isolated: bool) -> Vec<u16> {
    let op = |a: [u16; 4], b: [u16; 4], t: f64, mode: &str| {
        if depth == 16 {
            blend16(a, b, t, mode)
        } else {
            blend(
                a.map(|v| (v / 257) as u8),
                b.map(|v| (v / 257) as u8),
                t as f32,
                mode,
            )
            .map(|v| v as u16 * 257)
        }
    };
    let mut out = vec![];
    for x in 0..2 {
        let mut unit = specs[2].pixels[x].map(|v| native(v, depth));
        let alpha = unit[3];
        unit[3] = 65535;
        unit = op(
            unit,
            specs[1].pixels[x].map(|v| native(v, depth)),
            173. / 255. * native(specs[1].mask.unwrap()[x], depth) as f64 / 65535.,
            "multiply",
        );
        unit = op(
            unit,
            specs[0].pixels[x].map(|v| native(v, depth)),
            203. / 255.,
            "screen",
        );
        unit[3] = alpha;
        let amount = 192. / 255. * native(specs[2].mask.unwrap()[x], depth) as f64 / 65535.;
        let background = specs[3].pixels[x].map(|v| native(v, depth));
        out.extend(if isolated {
            op(
                background,
                op([0; 4], unit, amount, "overlay"),
                1.,
                "normal",
            )
        } else {
            op(background, unit, amount, "overlay")
        });
    }
    out
}

#[test]
fn standard_raster_clipping_imports_and_saves_native_layers_masks_and_grouped_blends() {
    for depth in [8, 16] {
        let specs = stack();
        let doc = psd::decode(&fixture(&specs, depth)).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(doc.layers.len(), 4);
        for i in 0..2 {
            assert_eq!(
                doc.layers[i].clip_to.as_deref(),
                Some(doc.layers[2].id.as_str())
            );
        }
        assert_eq!(rendered(&doc), expected(&specs, depth, false));
        assert_eq!(
            doc.layers[2].pixels.get16(1, 0),
            specs[2].pixels[1].map(|v| native(v, depth))
        );
        let saved = psd::encode(&doc).unwrap();
        assert_eq!(saved_composite(&saved, depth), rendered(&doc));
        assert!(saved
            .windows(16)
            .any(|v| v == b"8BIMclbl\0\0\0\x04\x01\0\0\0"));
        for bytes in [saved.clone(), ordinary(&saved)] {
            let again = psd::decode(&bytes).unwrap();
            assert!(!again.read_only, "{:?}", again.warnings);
            assert_eq!(again.layers.len(), 4);
            assert_eq!(rendered(&again), rendered(&doc));
            for i in 0..4 {
                assert_eq!(
                    again.layers[i].pixels.rgba16(),
                    doc.layers[i].pixels.rgba16()
                );
            }
            for i in [1, 2] {
                assert_eq!(
                    again.layers[i].mask_value_raw(0, 0),
                    doc.layers[i].mask_value_raw(0, 0)
                );
            }
        }
    }
}

#[test]
fn clipping_bases_stay_inside_folders_and_hidden_bases_hide_the_entire_stack() {
    for depth in [8, 16] {
        let mut specs = vec![Spec::folder("Folder", 1)];
        specs.extend(stack()[..3].iter().cloned());
        specs.push(Spec::folder("End", 3));
        specs.push(stack()[3].clone());
        for hidden in [false, true] {
            specs[3].flags = if hidden { 2 } else { 0 };
            let doc = psd::decode(&fixture(&specs, depth)).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            assert_eq!(doc.layers[1].parent, Some(doc.layers[0].id.clone()));
            assert_eq!(
                doc.layers[1].clip_to.as_deref(),
                Some(doc.layers[3].id.as_str())
            );
            if hidden {
                assert_eq!(
                    rendered(&doc),
                    specs[5]
                        .pixels
                        .concat()
                        .into_iter()
                        .map(|v| native(v, depth))
                        .collect::<Vec<_>>()
                );
            } else {
                assert_eq!(rendered(&doc), expected(&stack(), depth, true));
            }
            let again = psd::decode(&ordinary(&psd::encode(&doc).unwrap())).unwrap();
            assert!(!again.read_only);
            assert_eq!(rendered(&again), rendered(&doc));
            assert_eq!(again.layers[1].parent, Some(again.layers[0].id.clone()));
            assert_eq!(
                again.layers[1].clip_to.as_deref(),
                Some(again.layers[3].id.as_str())
            );
        }
    }
}

#[test]
fn imported_clipping_uses_engine_history_reservations_and_atomic_structure_validation() {
    let mut engine = Engine::new();
    engine.doc = psd::decode(&fixture(&stack(), 16)).unwrap();
    let before = rendered(&engine.doc);
    let top = engine.doc.layers[0].id.clone();
    let base = engine.doc.layers[2].id.clone();
    engine
        .reserve("artist", "Paint clipped texture", vec![Scope::layer(&top)])
        .unwrap();
    assert!(engine
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":top,"opacity":0.2})],
            None,
            None,
            "Blocked"
        )
        .is_err());
    assert_eq!(rendered(&engine.doc), before);
    engine.leases.clear();
    engine
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":top,"opacity":0.2})],
            None,
            None,
            "Clipped highlights",
        )
        .unwrap();
    assert_ne!(rendered(&engine.doc), before);
    assert_eq!(engine.undo.len(), 1);
    let state = serde_json::to_value(&engine.doc).unwrap();
    assert!(engine
        .edit(
            "human",
            &[json!({"op":"layer.delete","layer":base})],
            None,
            None,
            "Keep base"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&engine.doc).unwrap(), state);
    engine.undo("human").unwrap();
    assert_eq!(rendered(&engine.doc), before);
    engine.redo("human").unwrap();
    let saved = psd::encode(&engine.doc).unwrap();
    assert_eq!(saved_composite(&saved, 16), rendered(&engine.doc));
    assert_eq!(
        rendered(&psd::decode(&saved).unwrap()),
        rendered(&engine.doc)
    );
}

#[test]
fn standard_clipped_layers_keep_their_baked_effect_pixels_and_editable_mask_channels() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = psd::decode(&fixture(&stack(), depth)).unwrap();
        let layer = engine.doc.layers[1].id.clone();
        let source = engine.doc.layers[1].pixels.rgba16();
        engine
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"invert"})],
                None,
                None,
                "Clip effect",
            )
            .unwrap();
        let saved = psd::encode(&engine.doc).unwrap();
        assert_eq!(saved_composite(&saved, depth), rendered(&engine.doc));
        let standard = psd::decode(&ordinary(&saved)).unwrap();
        assert!(!standard.read_only, "{:?}", standard.warnings);
        assert_eq!(standard.layers.len(), 4);
        assert_eq!(rendered(&standard), rendered(&engine.doc));
        assert!(standard.layers[1].effects.is_empty());
        assert_ne!(standard.layers[1].pixels.rgba16(), source);
        let restored = psd::decode(&saved).unwrap();
        assert_eq!(restored.layers[1].pixels.rgba16(), source);
        assert_eq!(restored.layers[1].effects.len(), 1);
    }
}

#[test]
fn unsupported_standard_resources_cannot_be_hidden_by_valid_private_sources() {
    for depth in [8, 16] {
        let doc = psd::decode(&fixture(&stack(), depth)).unwrap();
        let saved = psd::encode(&doc).unwrap();
        let n = u32::from_be_bytes(saved[30..34].try_into().unwrap()) as usize;
        let mut resource = b"8BIM".to_vec();
        resource.extend(1039u16.to_be_bytes());
        resource.extend([0; 2]);
        block(&mut resource, b"Unmanaged ICC profile");
        if resource.len() % 2 != 0 {
            resource.push(0);
        }
        let mut modified = saved[..30].to_vec();
        modified.extend(((n + resource.len()) as u32).to_be_bytes());
        modified.extend(&saved[34..34 + n]);
        modified.extend(resource);
        modified.extend(&saved[34 + n..]);
        let loaded = psd::decode(&modified).unwrap();
        assert!(loaded.read_only);
        assert_eq!(loaded.bit_depth, depth);
        assert_eq!(loaded.layers.len(), 1);
        assert!(loaded.warnings.iter().any(|v| v.contains("color profiles")));
        assert!(!loaded
            .warnings
            .iter()
            .any(|v| v.contains("definitions were changed")));
        assert_eq!(rendered(&loaded), rendered(&doc));
    }
}

fn protected(specs: &[Spec], depth: u16, reason: &str) {
    let mut engine = Engine::new();
    engine.doc = psd::decode(&fixture(specs, depth)).unwrap();
    assert!(
        engine.doc.read_only,
        "Expected {reason}, got {:?}",
        engine.doc.warnings
    );
    assert!(
        engine.doc.warnings.iter().any(|v| v.contains(reason)),
        "{:?}",
        engine.doc.warnings
    );
    assert_eq!(engine.doc.bit_depth, depth);
    assert_eq!(
        engine.doc.layers[0].pixels.rgba16(),
        SAVED
            .concat()
            .into_iter()
            .map(|v| native(v, depth))
            .collect::<Vec<_>>()
    );
    assert!(psd::encode(&engine.doc).is_err());
    assert!(engine
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":engine.doc.layers[0].id,"opacity":0.5})],
            None,
            None,
            "Protected"
        )
        .is_err());
    assert!(engine.undo.is_empty());
}

#[test]
fn unsupported_and_malformed_blending_controls_never_become_editable() {
    let cases = [
        (*b"clbl", vec![0, 0, 0, 0], "clbl"),
        (*b"clbl", vec![1, 0, 1, 0], "clbl"),
        (*b"knko", vec![1, 0, 0, 0], "knko"),
        (*b"knko", vec![], "knko"),
        (*b"iOpa", vec![127], "iOpa"),
        (*b"iOpa", vec![255, 1, 0, 0], "iOpa"),
        (*b"lspf", 1u32.to_be_bytes().to_vec(), "lspf"),
        (*b"lspf", vec![0], "lspf"),
        (*b"infx", vec![2, 0, 0, 0], "layer flag"),
        (*b"sn2P", vec![], "layer flag"),
    ];
    for depth in [8, 16] {
        for (key, data, reason) in &cases {
            let mut specs = stack();
            specs[2].tags = vec![(*key, data.clone())];
            protected(&specs, depth, reason);
        }
        let mut specs = stack();
        specs[2].ranges = vec![0, 0, 254, 255, 0, 0, 255, 255];
        protected(&specs, depth, "Blend If");
        specs[2].ranges = vec![0, 0, 255, 255];
        protected(&specs, depth, "Blend If");
        specs[2].ranges.clear();
        specs[2].flags = 1;
        protected(&specs, depth, "protection");
        specs[2].flags = 0x18;
        protected(&specs, depth, "pixels do not represent");
    }
}

#[test]
fn valid_default_controls_and_blend_if_ranges_allow_native_raster_editing() {
    for depth in [8, 16] {
        let mut specs = stack();
        specs[2].ranges = [0, 0, 255, 255].repeat(8);
        specs[2].tags.extend([
            (*b"knko", vec![0; 4]),
            (*b"iOpa", vec![255]),
            (*b"lspf", vec![0; 4]),
            (*b"infx", vec![1, 0, 0, 0]),
            (*b"sn2P", vec![0, 0, 0, 1]),
            (*b"lyid", 42u32.to_be_bytes().to_vec()),
        ]);
        let doc = psd::decode(&fixture(&specs, depth)).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(rendered(&doc), expected(&specs, depth, false));
    }
}

#[test]
fn orphan_and_folder_clipping_do_not_use_an_unrelated_base() {
    for depth in [8, 16] {
        protected(&stack()[..2], depth, "no base");
        let specs = [
            Spec::folder("Folder", 1),
            stack()[0].clone(),
            Spec::folder("End", 3),
            stack()[3].clone(),
        ];
        protected(&specs, depth, "no base");
        let specs = [
            stack()[0].clone(),
            Spec::folder("Folder", 1),
            stack()[2].clone(),
            Spec::folder("End", 3),
            stack()[3].clone(),
        ];
        protected(&specs, depth, "involving a folder");
        let mut specs = stack();
        specs[0].clipping = 2;
        protected(&specs, depth, "clipping flag");
    }
}

#[test]
fn section_divider_blends_and_scene_groups_are_checked_instead_of_ignored() {
    for depth in [8, 16] {
        for mode in [*b"xxxx"] {
            let mut specs = vec![
                Spec::folder("Folder", 1),
                stack()[2].clone(),
                Spec::folder("End", 3),
            ];
            // A normal layer-record key must not hide an unsupported divider.
            let mut divider = 1u32.to_be_bytes().to_vec();
            divider.extend(b"8BIM");
            divider.extend(mode);
            specs[0].section = None;
            specs[0].tags.push((*b"lsct", divider));
            protected(&specs, depth, "xxxx");
        }
        let mut specs = [
            Spec::folder("Scene", 1),
            stack()[2].clone(),
            Spec::folder("End", 3),
        ];
        let mut divider = 1u32.to_be_bytes().to_vec();
        divider.extend(b"8BIMnorm");
        divider.extend(1u32.to_be_bytes());
        specs[0].section = None;
        specs[0].tags.push((*b"lsct", divider));
        protected(&specs, depth, "animation scene");
    }
}

#[test]
fn standard_pass_through_dividers_keep_clipping_native_masks_and_external_blending() {
    for depth in [8, 16] {
        for override_mode in [false, true] {
            let artwork = stack();
            let mut folder = Spec::folder("Pass Through", 1);
            folder.opacity = 173;
            folder.mask = Some([23001, 65535]);
            if override_mode {
                let mut divider = 1u32.to_be_bytes().to_vec();
                divider.extend(b"8BIMpass");
                folder.section = None;
                folder.tags.push((*b"lsct", divider));
            } else {
                folder.blend = *b"pass";
            }
            let specs = vec![
                folder,
                artwork[0].clone(),
                artwork[1].clone(),
                artwork[2].clone(),
                Spec::folder("End", 3),
                artwork[3].clone(),
            ];
            let doc = psd::decode(&fixture(&specs, depth)).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            assert_eq!(doc.layers[0].blend, "pass_through");
            let full = expected(&artwork, depth, false);
            let expected: Vec<u16> = (0..2)
                .flat_map(|x| {
                    let t = f64::from(doc.layers[0].opacity)
                        * f64::from(native(specs[0].mask.unwrap()[x], depth))
                        / 65535.;
                    (0..4)
                        .map(|c| {
                            if c == 3 {
                                return 65535;
                            }
                            let a = native(artwork[3].pixels[x][c], depth);
                            let b = full[x * 4 + c];
                            if depth == 16 {
                                (a as f64 * (1. - t) + b as f64 * t).round() as u16
                            } else {
                                ((a as f32 / 257. * (1. - t as f32) + b as f32 / 257. * t as f32)
                                    .round() as u16)
                                    * 257
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            assert_eq!(rendered(&doc), expected);
            let saved = psd::encode(&doc).unwrap();
            assert!(saved.windows(8).any(|v| v == b"8BIMpass"));
            assert_eq!(saved_composite(&saved, depth), rendered(&doc));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let again = psd::decode(&bytes).unwrap();
                assert!(!again.read_only, "{:?}", again.warnings);
                assert_eq!(again.layers.len(), 5);
                assert_eq!(again.layers[0].blend, "pass_through");
                assert_eq!(rendered(&again), rendered(&doc));
                assert_eq!(
                    again.layers[0].mask_value_raw(0, 0),
                    doc.layers[0].mask_value_raw(0, 0)
                );
                for i in 1..5 {
                    assert_eq!(
                        again.layers[i].pixels.rgba16(),
                        doc.layers[i].pixels.rgba16()
                    );
                }
            }
        }
        let mut specs = stack();
        specs[2].blend = *b"pass";
        protected(&specs, depth, "only for folders");
    }
}

// Queued with the sprint's combined QA: real standard records, no private definitions.
fn overlay_id(out: &mut Vec<u8>, name: &str) {
    out.extend(
        (if name.len() == 4 {
            0
        } else {
            name.len() as u32
        })
        .to_be_bytes(),
    );
    out.extend(name.as_bytes());
}
fn overlay_descriptor(class: &str, items: Vec<(&str, &[u8; 4], Vec<u8>)>) -> Vec<u8> {
    let mut out = 0u32.to_be_bytes().to_vec();
    overlay_id(&mut out, class);
    out.extend((items.len() as u32).to_be_bytes());
    for (key, kind, bytes) in items {
        overlay_id(&mut out, key);
        out.extend(kind);
        out.extend(bytes);
    }
    out
}
fn overlay_payload(master: bool, enabled: bool, color: [f64; 3]) -> Vec<u8> {
    let rgb = overlay_descriptor(
        "RGBC",
        vec![
            ("Rd  ", b"doub", color[0].to_be_bytes().to_vec()),
            ("Grn ", b"doub", color[1].to_be_bytes().to_vec()),
            ("Bl  ", b"doub", color[2].to_be_bytes().to_vec()),
        ],
    );
    let mut mode = vec![];
    overlay_id(&mut mode, "BlnM");
    overlay_id(&mut mode, "Nrml");
    let percent = [b"#Prc".to_vec(), 100f64.to_be_bytes().to_vec()].concat();
    let effect = overlay_descriptor(
        "SoFi",
        vec![
            ("enab", b"bool", vec![u8::from(enabled)]),
            ("present", b"bool", vec![1]),
            ("showInDialog", b"bool", vec![1]),
            ("Md  ", b"enum", mode),
            ("Opct", b"UntF", percent.clone()),
            ("Clr ", b"Objc", rgb),
        ],
    );
    let root = overlay_descriptor(
        "Lefx",
        vec![
            ("masterFXSwitch", b"bool", vec![u8::from(master)]),
            ("Scl ", b"UntF", percent),
            ("SoFi", b"Objc", effect),
        ],
    );
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        root,
    ]
    .concat()
}
fn overlay_fixture(depth: u16, master: bool, enabled: bool, replace_composite: bool) -> Vec<u8> {
    let mut spec = Spec::paint("Synthetic styled source", SAVED);
    spec.tags.push((
        *b"lfx2",
        overlay_payload(master, enabled, [111., 67., 231.]),
    ));
    let mut bytes = fixture(&[spec], depth);
    if replace_composite {
        let n = 8 * usize::from(depth / 8);
        bytes.truncate(bytes.len() - n);
        for channel in [111u16 * 257, 67 * 257, 231 * 257, 65535] {
            for _ in 0..2 {
                sample(&mut bytes, channel, depth);
            }
        }
    }
    bytes
}
#[test]
fn photoshop_color_overlay_import_preserves_native_sources_editability_and_standard_saves() {
    for depth in [8, 16] {
        for (master, enabled) in [(true, true), (true, false), (false, true)] {
            let mut doc =
                psd::decode(&overlay_fixture(depth, master, enabled, master && enabled)).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            assert_eq!(doc.layers.len(), 1);
            assert_eq!(doc.layers[0].effects.len(), 1);
            assert_eq!(doc.layers[0].effects[0].enabled, master && enabled);
            for x in 0..2 {
                assert_eq!(
                    doc.layers[0].pixels.get16(x, 0),
                    SAVED[x as usize].map(|n| native(n, depth))
                );
            }
            let before = doc.export_png().unwrap();
            let source = doc.layers[0].pixels.rgba16();
            let layer = doc.layers[0].id.clone();
            let effect = doc.layers[0].effects[0].id.clone();
            let mut engine = Engine::new();
            engine.doc = doc;
            engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"opacity":0.25}})],None,None,"Overlay strength").unwrap();
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
            assert_eq!(engine.undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.export_png().unwrap(), before);
            doc = engine.doc;
            for bytes in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                assert_eq!(reopened.export_png().unwrap(), before);
            }
        }
    }
}
#[test]
fn mismatched_or_unsupported_photoshop_color_overlays_remain_protected() {
    for depth in [8, 16] {
        let mismatch = psd::decode(&overlay_fixture(depth, true, true, false)).unwrap();
        assert!(mismatch.read_only);
        assert!(mismatch
            .warnings
            .iter()
            .any(|w| w.contains("differs from the native saved composite")));
        for variant in 0..7 {
            let mut spec = Spec::paint("Synthetic protected style", SAVED);
            let mut payload = overlay_payload(true, true, [111., 67., 231.]);
            match variant {
                0 => {
                    let at = payload.windows(4).position(|b| b == b"Nrml").unwrap();
                    payload[at..at + 4].copy_from_slice(b"????");
                }
                1 => spec.mask = Some([65535, 65535]),
                2 => spec.clipping = 1,
                3 => spec.opacity = 128,
                4 => {
                    spec.tags.push((*b"lmfx", payload.clone()));
                }
                5 => {
                    payload = overlay_payload(true, true, [256., 67., 231.]);
                }
                _ => {
                    spec.tags.push((*b"lrFX", vec![0; 16]));
                }
            }
            spec.tags.push((*b"lfx2", payload));
            let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
            assert!(doc.read_only, "variant {variant}");
            assert_eq!(doc.layers.len(), 1);
            assert!(doc.layers[0].effects.is_empty());
            for x in 0..2 {
                assert_eq!(
                    doc.layers[0].pixels.get16(x, 0),
                    SAVED[x as usize].map(|n| native(n, depth))
                );
            }
        }
    }
}

#[test]
fn photoshop_overlay_blend_enum_import_uses_exact_native_composite_gate() {
    for depth in [8, 16] {
        for (ps_mode, mode) in [
            (b"Mltp", "multiply"),
            (b"Scrn", "screen"),
            (b"Ovrl", "overlay"),
            (b"Drkn", "darken"),
            (b"Lghn", "lighten"),
        ] {
            let mut payload = overlay_payload(true, true, [111., 67., 231.]);
            let at = payload.windows(4).position(|b| b == b"Nrml").unwrap();
            payload[at..at + 4].copy_from_slice(ps_mode);
            let mut spec = Spec::paint("Synthetic blend style", SAVED);
            spec.tags.push((*b"lfx2", payload));
            let mut bytes = fixture(&[spec], depth);
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            let colors = [111., 67., 231.];
            let mut expected = [[0u16; 4]; 2];
            for x in 0..2 {
                for c in 0..3 {
                    let b = f64::from(native(SAVED[x][c], depth)) / 65535.;
                    let s = colors[c] / 255.;
                    let n = match mode {
                        "multiply" => b * s,
                        "screen" => b + s - b * s,
                        "overlay" => {
                            if b <= 0.5 {
                                2. * b * s
                            } else {
                                1. - 2. * (1. - b) * (1. - s)
                            }
                        }
                        "darken" => b.min(s),
                        _ => b.max(s),
                    };
                    expected[x][c] = native((n * 65535.).round() as u16, depth);
                }
                expected[x][3] = 65535;
            }
            for c in 0..4 {
                for p in expected {
                    sample(&mut bytes, p[c], depth);
                }
            }
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{mode}, {depth}: {:?}", doc.warnings);
            assert_eq!(doc.layers[0].effects[0].settings["blend"], mode);
            for x in 0..2 {
                assert_eq!(
                    doc.layers[0].pixels.get16(x, 0),
                    SAVED[x as usize].map(|n| native(n, depth))
                );
            }
        }
    }
}

fn gradient_payload(reverse: bool, omit: Option<&str>, smoothness: f64) -> Vec<u8> {
    let desc = |class: &str, fields: Vec<(&str, &[u8; 4], Vec<u8>)>| {
        overlay_descriptor(
            class,
            fields
                .into_iter()
                .filter(|(key, _, _)| Some(*key) != omit)
                .collect(),
        )
    };
    let enum_bytes = |class: &str, key: &str| {
        let mut out = vec![];
        overlay_id(&mut out, class);
        overlay_id(&mut out, key);
        out
    };
    let unit = |unit: &str, n: f64| [unit.as_bytes().to_vec(), n.to_be_bytes().to_vec()].concat();
    let mut colors = 2u32.to_be_bytes().to_vec();
    let mut alphas = 2u32.to_be_bytes().to_vec();
    for (position, value) in [(0i32, 0f64), (4096, 255.)] {
        let rgb = desc(
            "RGBC",
            vec![
                ("Rd  ", b"doub", value.to_be_bytes().to_vec()),
                ("Grn ", b"doub", value.to_be_bytes().to_vec()),
                ("Bl  ", b"doub", value.to_be_bytes().to_vec()),
            ],
        );
        colors.extend(b"Objc");
        colors.extend(desc(
            "Clrt",
            vec![
                ("Clr ", b"Objc", rgb),
                ("Type", b"enum", enum_bytes("Clry", "UsrS")),
                ("Lctn", b"long", position.to_be_bytes().to_vec()),
                ("Mdpn", b"long", 50i32.to_be_bytes().to_vec()),
            ],
        ));
        alphas.extend(b"Objc");
        alphas.extend(desc(
            "TrnS",
            vec![
                ("Opct", b"UntF", unit("#Prc", 100.)),
                ("Lctn", b"long", position.to_be_bytes().to_vec()),
                ("Mdpn", b"long", 50i32.to_be_bytes().to_vec()),
            ],
        ));
    }
    let gradient = desc(
        "Grdn",
        vec![
            ("GrdF", b"enum", enum_bytes("GrdF", "CstS")),
            ("Intr", b"doub", smoothness.to_be_bytes().to_vec()),
            ("Clrs", b"VlLs", colors),
            ("Trns", b"VlLs", alphas),
        ],
    );
    let effect = desc(
        "GrFl",
        vec![
            ("enab", b"bool", vec![1]),
            ("present", b"bool", vec![1]),
            ("showInDialog", b"bool", vec![1]),
            ("Md  ", b"enum", enum_bytes("BlnM", "Nrml")),
            ("Opct", b"UntF", unit("#Prc", 100.)),
            ("Grad", b"Objc", gradient),
            ("Angl", b"UntF", unit("#Ang", 0.)),
            ("Type", b"enum", enum_bytes("GrdT", "Lnr ")),
            ("Rvrs", b"bool", vec![u8::from(reverse)]),
            ("Dthr", b"bool", vec![0]),
            ("Algn", b"bool", vec![0]),
            ("Scl ", b"UntF", unit("#Prc", 100.)),
        ],
    );
    let root = desc(
        "Lefx",
        vec![
            ("masterFXSwitch", b"bool", vec![1]),
            ("Scl ", b"UntF", unit("#Prc", 100.)),
            ("GrFl", b"Objc", effect),
        ],
    );
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        root,
    ]
    .concat()
}
#[test]
fn plain_photoshop_linear_gradient_import_retains_native_source_and_reversible_stops() {
    for depth in [8, 16] {
        for reverse in [false, true] {
            let mut spec = Spec::paint("Synthetic linear gradient", SAVED);
            spec.tags
                .push((*b"lfx2", gradient_payload(reverse, None, 0.)));
            let mut bytes = fixture(&[spec], depth);
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for c in 0..4 {
                for x in 0..2 {
                    let word = if c == 3 {
                        65535
                    } else if (x == 0) != reverse {
                        16384
                    } else {
                        49151
                    };
                    sample(&mut bytes, word, depth);
                }
            }
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            assert_eq!(doc.layers[0].effects[0].kind, "gradient_overlay");
            assert_eq!(doc.layers[0].effects[0].settings["reverse"], reverse);
            assert_eq!(
                doc.layers[0].effects[0].settings["color_stops"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            for x in 0..2 {
                assert_eq!(
                    doc.layers[0].pixels.get16(x, 0),
                    SAVED[x as usize].map(|n| native(n, depth))
                );
            }
            let current = doc.export_png().unwrap();
            for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                assert_eq!(psd::decode(&output).unwrap().export_png().unwrap(), current);
            }
        }
    }
}
#[test]
fn missing_or_unsupported_photoshop_gradient_fields_protect_without_panics() {
    for depth in [8, 16] {
        for omit in [
            Some("Angl"),
            Some("Scl "),
            Some("Grad"),
            Some("Dthr"),
            Some("Lctn"),
            Some("Clr "),
            Some("Clrs"),
            Some("Trns"),
            Some("Md  "),
            Some("Intr"),
            Some("Type"),
            None,
        ] {
            let mut spec = Spec::paint("Synthetic protected gradient", SAVED);
            spec.tags.push((
                *b"lfx2",
                gradient_payload(false, omit, if omit.is_none() { 4096. } else { 0. }),
            ));
            let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
            assert!(doc.read_only, "missing {omit:?}");
            assert!(doc.layers[0].effects.is_empty());
            for x in 0..2 {
                assert_eq!(
                    doc.layers[0].pixels.get16(x, 0),
                    SAVED[x as usize].map(|n| native(n, depth))
                );
            }
        }
    }
}

// Independent descriptor and composite fixtures: do not build the source PSD
// using the importer or native style renderer under test.
fn mixed_overlay_payload(enabled: [bool; 3], master: bool) -> Vec<u8> {
    let sources = [
        ("SoFi", overlay_payload(true, enabled[0], [111., 67., 231.])),
        (
            "patternFill",
            pattern_payload(enabled[1], "synthetic-pattern", "", 100.),
        ),
        ("GrFl", gradient_payload(false, None, 0.)),
    ];
    let mut entries = vec![("masterFXSwitch", b"bool", vec![u8::from(master)])];
    // Deliberately serialize in reverse family order. Property order must not
    // become native execution order.
    for (key, bytes) in sources.into_iter().rev() {
        let needle = [key.as_bytes(), b"Objc"].concat();
        let at = bytes
            .windows(needle.len())
            .position(|v| v == needle)
            .unwrap();
        let mut effect = bytes[at + needle.len()..].to_vec();
        if key == "GrFl" {
            let at = effect.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
            effect[at] = u8::from(enabled[2]);
        }
        if key != "SoFi" {
            let at = effect
                .windows(12)
                .position(|v| v == b"OpctUntF#Prc")
                .unwrap()
                + 12;
            effect[at..at + 8].copy_from_slice(&50f64.to_be_bytes());
        }
        entries.push((key, b"Objc", effect));
    }
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        overlay_descriptor("Lefx", entries),
    ]
    .concat()
}
fn mixed_overlay_fixture(depth: u16, enabled: [bool; 3], master: bool, mismatch: bool) -> Vec<u8> {
    let mut spec = Spec::paint("Synthetic mixed overlays", SAVED);
    spec.tags
        .push((*b"lmfx", mixed_overlay_payload(enabled, master)));
    let mut bytes = inject_pattern(
        fixture(&[spec], depth),
        depth,
        &pattern_resource(depth, "synthetic-pattern"),
        false,
    );
    let mut merged = SAVED.map(|p| p.map(|n| native(n, depth)));
    if master {
        for (x, pixel) in merged.iter_mut().enumerate() {
            if enabled[0] {
                pixel[..3].copy_from_slice(&[111 * 257, 67 * 257, 231 * 257]);
            }
            for (active, tint) in [
                (
                    enabled[1],
                    [
                        native(PATTERN[x][0], depth),
                        native(PATTERN[x][1], depth),
                        native(PATTERN[x][2], depth),
                    ],
                ),
                (enabled[2], [if x == 0 { 16384 } else { 49151 }; 3]),
            ] {
                if active {
                    for c in 0..3 {
                        let value = ((f64::from(pixel[c]) * 32767. + f64::from(tint[c]) * 32768.)
                            / 65535.)
                            .round() as u16;
                        pixel[c] = native(value, depth);
                    }
                }
            }
        }
    }
    if mismatch {
        merged[0][0] = merged[0][0].saturating_add(if depth == 16 { 1 } else { 257 });
    }
    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
    for channel in 0..4 {
        for pixel in merged {
            sample(&mut bytes, pixel[channel], depth);
        }
    }
    bytes
}
#[test]
fn mixed_photoshop_overlays_retain_order_disabled_sources_undo_and_native_saves() {
    for depth in [8, 16] {
        for flags in 0u8..8 {
            let enabled = [flags & 1 != 0, flags & 2 != 0, flags & 4 != 0];
            for master in [false, true] {
                let doc =
                    psd::decode(&mixed_overlay_fixture(depth, enabled, master, false)).unwrap();
                assert!(
                    !doc.read_only,
                    "{depth} {flags} {master}: {:?}",
                    doc.warnings
                );
                let effects = &doc.layers[0].effects;
                assert_eq!(
                    effects.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
                    ["color_overlay", "pattern_overlay", "gradient_overlay"]
                );
                assert_eq!(
                    effects.iter().map(|e| e.enabled).collect::<Vec<_>>(),
                    enabled.map(|v| v && master)
                );
                let source = doc.layers[0].pixels.rgba16();
                assert_eq!(
                    source,
                    SAVED
                        .into_iter()
                        .flatten()
                        .map(|n| native(n, depth))
                        .collect::<Vec<_>>()
                );
                let current = doc.export_png().unwrap();
                let mut engine = Engine::new();
                engine.doc = doc;
                let layer = engine.doc.layers[0].id.clone();
                let effect = engine.doc.layers[0].effects[1].id.clone();
                let mut settings = engine.doc.layers[0].effects[1].settings.clone();
                settings["phase_x"] = json!(1.);
                engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":settings})], None, None, "Mixed pattern phase").unwrap();
                assert_eq!(engine.undo.len(), 1);
                assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.export_png().unwrap(), current);
                for bytes in [
                    psd::encode(&engine.doc).unwrap(),
                    psd::encode_psb(&engine.doc).unwrap(),
                ] {
                    let reopened = psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                    assert_eq!(reopened.export_png().unwrap(), current);
                    assert_eq!(reopened.layers[0].effects.len(), 3);
                }
            }
        }
    }
}
#[test]
fn mixed_photoshop_overlays_protect_mismatches_and_unknown_even_disabled_styles() {
    for depth in [8, 16] {
        let mismatched = psd::decode(&mixed_overlay_fixture(depth, [true; 3], true, true)).unwrap();
        assert!(mismatched.read_only);
        for master in [false, true] {
            let mut payload = mixed_overlay_payload([false; 3], master);
            let at = payload.windows(4).position(|v| v == b"SoFi").unwrap();
            payload[at..at + 4].copy_from_slice(b"DrSh");
            let mut spec = Spec::paint("Synthetic unsupported mixed styles", SAVED);
            spec.tags.push((*b"lfx2", payload));
            let bytes = inject_pattern(
                fixture(&[spec], depth),
                depth,
                &pattern_resource(depth, "synthetic-pattern"),
                false,
            );
            let doc = psd::decode(&bytes).unwrap();
            assert!(doc.read_only);
            assert_eq!(doc.bit_depth, depth);
            assert!(doc
                .warnings
                .iter()
                .any(|v| v.contains("Unsupported Photoshop style")));
        }
    }
}

fn blended_overlay_fixture(depth: u16, pattern: bool, ps_mode: &[u8; 4], opacity: f64) -> Vec<u8> {
    let mut payload = if pattern {
        pattern_payload(true, "synthetic-pattern", "", 100.)
    } else {
        gradient_payload(false, None, 0.)
    };
    let at = payload.windows(4).position(|v| v == b"Nrml").unwrap();
    payload[at..at + 4].copy_from_slice(ps_mode);
    let at = payload
        .windows(12)
        .position(|v| v == b"OpctUntF#Prc")
        .unwrap()
        + 12;
    payload[at..at + 8].copy_from_slice(&opacity.to_be_bytes());
    let mut spec = Spec::paint("Synthetic blended overlay", SAVED);
    spec.tags.push((*b"lfx2", payload));
    let mut bytes = fixture(&[spec], depth);
    if pattern {
        bytes = inject_pattern(
            bytes,
            depth,
            &pattern_resource(depth, "synthetic-pattern"),
            false,
        );
    }
    let coverage = (opacity / 100. * 65535.).round() / 65535.;
    let mut merged = SAVED.map(|p| p.map(|n| native(n, depth)));
    for (x, pixel) in merged.iter_mut().enumerate() {
        for c in 0..3 {
            let b = f64::from(pixel[c]) / 65535.;
            let s = f64::from(if pattern {
                native(PATTERN[x][c], depth)
            } else if x == 0 {
                16384
            } else {
                49151
            }) / 65535.;
            let n = match ps_mode {
                b"Mltp" => b * s,
                b"Scrn" => 1. - (1. - b) * (1. - s),
                b"Ovrl" => {
                    if b < 0.5 {
                        2. * b * s
                    } else {
                        1. - 2. * (1. - b) * (1. - s)
                    }
                }
                b"Drkn" => b.min(s),
                b"Lghn" => b.max(s),
                _ => s,
            };
            pixel[c] = native(
                ((n * coverage + b * (1. - coverage)) * 65535.).round() as u16,
                depth,
            );
        }
    }
    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
    for channel in 0..4 {
        for pixel in merged {
            sample(&mut bytes, pixel[channel], depth);
        }
    }
    bytes
}
#[test]
fn photoshop_pattern_and_gradient_blends_retain_native_sources_and_editable_saves() {
    for depth in [8, 16] {
        for pattern in [false, true] {
            for (ps_mode, mode) in [
                (b"Nrml", "normal"),
                (b"Mltp", "multiply"),
                (b"Scrn", "screen"),
                (b"Ovrl", "overlay"),
                (b"Drkn", "darken"),
                (b"Lghn", "lighten"),
            ] {
                for opacity in [50., 100.] {
                    let doc =
                        psd::decode(&blended_overlay_fixture(depth, pattern, ps_mode, opacity))
                            .unwrap();
                    assert!(
                        !doc.read_only,
                        "{depth} {pattern} {mode} {opacity}: {:?}",
                        doc.warnings
                    );
                    assert_eq!(doc.layers[0].effects[0].settings["blend"], mode);
                    assert_eq!(doc.layers[0].effects[0].settings["opacity"], opacity / 100.);
                    let original = doc.layers[0].pixels.rgba16();
                    assert_eq!(
                        original,
                        SAVED
                            .into_iter()
                            .flatten()
                            .map(|n| native(n, depth))
                            .collect::<Vec<_>>()
                    );
                    let current = doc.export_png().unwrap();
                    for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                        let reopened = psd::decode(&output).unwrap();
                        assert!(!reopened.read_only);
                        assert_eq!(reopened.layers[0].pixels.rgba16(), original);
                        assert_eq!(reopened.export_png().unwrap(), current);
                    }
                }
            }
        }
    }
}
#[test]
fn unsupported_or_mismatched_pattern_and_gradient_blends_keep_sources_protected() {
    for depth in [8, 16] {
        for pattern in [false, true] {
            for mode in [
                b"Nrml", b"Mltp", b"Scrn", b"Ovrl", b"Drkn", b"Lghn", b"HrdL",
            ] {
                let mut bytes = blended_overlay_fixture(depth, pattern, mode, 50.);
                // One native-word difference must never unlock a styled source.
                let at = bytes.len() - 8 * usize::from(depth / 8);
                if mode != b"HrdL" {
                    bytes[at] ^= 1;
                }
                let doc = psd::decode(&bytes).unwrap();
                assert!(doc.read_only, "{depth} {pattern} {mode:?}");
                assert_eq!(doc.bit_depth, depth);
            }
        }
    }
}

fn multi_overlay_payload(
    enabled: [bool; 4],
    master: bool,
    color_count: usize,
    gradient_count: usize,
    include_pattern: bool,
) -> Vec<u8> {
    let mut color = (color_count as u32).to_be_bytes().to_vec();
    let mut gradient = (gradient_count as u32).to_be_bytes().to_vec();
    for (list, key, count, offset) in [
        (&mut color, "SoFi", color_count, 0),
        (&mut gradient, "GrFl", gradient_count, 2),
    ] {
        for index in 0..count {
            let payload = if offset == 0 {
                overlay_payload(
                    true,
                    enabled[index % 2],
                    if index % 2 == 0 {
                        [111., 67., 231.]
                    } else {
                        [31., 197., 83.]
                    },
                )
            } else {
                gradient_payload(index % 2 != 0, None, 0.)
            };
            let needle = [key.as_bytes(), b"Objc"].concat();
            let at = payload
                .windows(needle.len())
                .position(|v| v == needle)
                .unwrap()
                + needle.len();
            let mut effect = payload[at..].to_vec();
            let at = effect.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
            effect[at] = u8::from(enabled[offset + index % 2]);
            let at = effect
                .windows(12)
                .position(|v| v == b"OpctUntF#Prc")
                .unwrap()
                + 12;
            effect[at..at + 8].copy_from_slice(&50f64.to_be_bytes());
            list.extend(b"Objc");
            list.extend(effect);
        }
    }
    let mut entries = vec![
        ("masterFXSwitch", b"bool", vec![u8::from(master)]),
        ("gradientFillMulti", b"VlLs", gradient),
        ("solidFillMulti", b"VlLs", color),
    ];
    if include_pattern {
        let payload = pattern_payload(false, "synthetic-pattern", "", 100.);
        let at = payload
            .windows(15)
            .position(|v| v == b"patternFillObjc")
            .unwrap()
            + 15;
        entries.push(("patternFill", b"Objc", payload[at..].to_vec()));
    }
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        overlay_descriptor("Lefx", entries),
    ]
    .concat()
}
#[test]
fn multiple_photoshop_overlays_preserve_file_order_disabled_entries_and_native_sources() {
    for depth in [8, 16] {
        for flags in 0u8..16 {
            let enabled = [
                flags & 1 != 0,
                flags & 2 != 0,
                flags & 4 != 0,
                flags & 8 != 0,
            ];
            for master in [false, true] {
                let mut spec = Spec::paint("Synthetic multiple overlays", SAVED);
                spec.tags.push((
                    *b"lmfx",
                    multi_overlay_payload(enabled, master, 2, 2, false),
                ));
                let mut bytes = fixture(&[spec], depth);
                let mut merged = SAVED.map(|p| p.map(|n| native(n, depth)));
                if master {
                    for (x, pixel) in merged.iter_mut().enumerate() {
                        let colors = [
                            [111 * 257, 67 * 257, 231 * 257],
                            [31 * 257, 197 * 257, 83 * 257],
                            [if x == 0 { 16384 } else { 49151 }; 3],
                            [if x == 0 { 49151 } else { 16384 }; 3],
                        ];
                        for (active, tint) in enabled.into_iter().zip(colors) {
                            if active {
                                for c in 0..3 {
                                    pixel[c] = native(
                                        ((f64::from(pixel[c]) * 32767.
                                            + f64::from(tint[c]) * 32768.)
                                            / 65535.)
                                            .round() as u16,
                                        depth,
                                    );
                                }
                            }
                        }
                    }
                }
                bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                for channel in 0..4 {
                    for pixel in merged {
                        sample(&mut bytes, pixel[channel], depth);
                    }
                }
                let doc = psd::decode(&bytes).unwrap();
                assert!(
                    !doc.read_only,
                    "{depth} {flags} {master}: {:?}",
                    doc.warnings
                );
                assert_eq!(
                    doc.layers[0]
                        .effects
                        .iter()
                        .map(|e| e.kind.as_str())
                        .collect::<Vec<_>>(),
                    [
                        "color_overlay",
                        "color_overlay",
                        "gradient_overlay",
                        "gradient_overlay"
                    ]
                );
                assert_eq!(
                    doc.layers[0]
                        .effects
                        .iter()
                        .map(|e| e.enabled)
                        .collect::<Vec<_>>(),
                    enabled.map(|v| v && master)
                );
                assert_eq!(
                    doc.layers[0].effects[0].settings["color"],
                    json!([111. / 255., 67. / 255., 231. / 255.])
                );
                assert_eq!(
                    doc.layers[0].effects[1].settings["color"],
                    json!([31. / 255., 197. / 255., 83. / 255.])
                );
                assert_eq!(doc.layers[0].effects[2].settings["reverse"], false);
                assert_eq!(doc.layers[0].effects[3].settings["reverse"], true);
                let source = doc.layers[0].pixels.rgba16();
                assert_eq!(
                    source,
                    SAVED
                        .into_iter()
                        .flatten()
                        .map(|n| native(n, depth))
                        .collect::<Vec<_>>()
                );
                let current = doc.export_png().unwrap();
                let mut engine = Engine::new();
                engine.doc = doc;
                let layer = engine.doc.layers[0].id.clone();
                let effect = engine.doc.layers[0].effects[0].id.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"effect.reorder","layer":layer,"effect":effect,"index":3})],
                        None,
                        None,
                        "Reorder imported overlays",
                    )
                    .unwrap();
                assert_eq!(engine.undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.export_png().unwrap(), current);
                for output in [
                    psd::encode(&engine.doc).unwrap(),
                    psd::encode_psb(&engine.doc).unwrap(),
                ] {
                    let reopened = psd::decode(&output).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].effects.len(), 4);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                    assert_eq!(reopened.export_png().unwrap(), current);
                }
            }
        }
    }
}
#[test]
fn multiple_overlay_limits_aliases_and_unsupported_disabled_entries_protect_atomically() {
    for depth in [8, 16] {
        for (colors, gradients, pattern, protected) in [
            (16, 16, false, false),
            (16, 16, true, true),
            (17, 1, false, true),
            (0, 1, false, true),
        ] {
            let mut spec = Spec::paint("Synthetic bounded overlays", SAVED);
            spec.tags.push((
                *b"lmfx",
                multi_overlay_payload([false; 4], false, colors, gradients, pattern),
            ));
            let mut bytes = fixture(&[spec], depth);
            if pattern {
                bytes = inject_pattern(
                    bytes,
                    depth,
                    &pattern_resource(depth, "synthetic-pattern"),
                    false,
                );
            }
            let doc = psd::decode(&bytes).unwrap();
            assert_eq!(
                doc.read_only, protected,
                "{depth} {colors} {gradients} {pattern}: {:?}",
                doc.warnings
            );
            if !protected {
                assert_eq!(doc.layers[0].effects.len(), 32);
            }
        }
        for invalid in ["alias", "unsupported"] {
            let mut payload = multi_overlay_payload([false; 4], false, 2, 2, false);
            if invalid == "unsupported" {
                let at = payload.windows(4).position(|v| v == b"Nrml").unwrap();
                payload[at..at + 4].copy_from_slice(b"HrdL");
            } else {
                let at = payload.windows(4).position(|v| v == b"Lefx").unwrap() + 4;
                payload[at..at + 4].copy_from_slice(&4u32.to_be_bytes());
                overlay_id(&mut payload, "SoFi");
                payload.extend(b"Objc");
                payload.extend(overlay_descriptor("SoFi", vec![]));
            }
            let mut spec = Spec::paint("Synthetic invalid multi overlays", SAVED);
            spec.tags.push((*b"lfx2", payload));
            let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
            assert!(doc.read_only);
        }
    }
}

#[test]
fn supported_photoshop_overlays_retain_raster_masks_and_layer_opacity_at_native_depth() {
    for depth in [8, 16] {
        for masked in [false, true] {
            for opacity in [0u8, 64, 128, 255] {
                for family in 0..3 {
                    let pattern = family == 1;
                    let gradient = family == 2;
                    let mut spec = Spec::paint("Synthetic masked overlay", SAVED);
                    spec.opacity = opacity;
                    let mask_values = [12347u16, 54321];
                    if masked {
                        spec.mask = Some(mask_values);
                    }
                    spec.tags.push((
                        *b"lfx2",
                        if pattern {
                            pattern_payload(true, "synthetic-pattern", "", 100.)
                        } else if gradient {
                            gradient_payload(false, None, 0.)
                        } else {
                            overlay_payload(true, true, [111., 67., 231.])
                        },
                    ));
                    let mut bytes = fixture(&[spec], depth);
                    if pattern {
                        bytes = inject_pattern(
                            bytes,
                            depth,
                            &pattern_resource(depth, "synthetic-pattern"),
                            false,
                        );
                    }
                    let mut merged = [[0u16; 4]; 2];
                    for (x, pixel) in merged.iter_mut().enumerate() {
                        let mask = if masked {
                            native(mask_values[x], depth)
                        } else {
                            65535
                        };
                        pixel[3] = native(
                            (f64::from(mask) * f64::from(opacity) / 255.).round() as u16,
                            depth,
                        );
                        if pixel[3] != 0 {
                            pixel[..3].copy_from_slice(&if pattern {
                                [
                                    native(PATTERN[x][0], depth),
                                    native(PATTERN[x][1], depth),
                                    native(PATTERN[x][2], depth),
                                ]
                            } else if gradient {
                                [native(if x == 0 { 16384 } else { 49151 }, depth); 3]
                            } else {
                                [111 * 257, 67 * 257, 231 * 257]
                            });
                        }
                    }
                    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                    for channel in 0..4 {
                        for pixel in merged {
                            sample(&mut bytes, pixel[channel], depth);
                        }
                    }
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(
                        !doc.read_only,
                        "{depth} {masked} {opacity} {pattern}: {:?}",
                        doc.warnings
                    );
                    assert_eq!(doc.layers[0].opacity, f64::from(opacity) / 255.);
                    assert_eq!(doc.layers[0].mask.is_some(), masked);
                    if let Some(mask) = &doc.layers[0].mask {
                        for x in 0..2 {
                            let value = native(mask_values[x], depth);
                            assert_eq!(
                                mask.steps[1].pixels.get16(x as i32, 0),
                                [value, value, value, 65535]
                            );
                        }
                    }
                    let source = doc.layers[0].pixels.rgba16();
                    assert_eq!(
                        source,
                        SAVED
                            .into_iter()
                            .flatten()
                            .map(|n| native(n, depth))
                            .collect::<Vec<_>>()
                    );
                    let mask = doc.layers[0]
                        .mask
                        .as_ref()
                        .map(|m| m.steps[1].pixels.rgba16());
                    let current = doc.export_png().unwrap();
                    let mut engine = Engine::new();
                    engine.doc = doc;
                    let layer = engine.doc.layers[0].id.clone();
                    let effect = engine.doc.layers[0].effects[0].id.clone();
                    engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":false})], None, None, "Bypass masked overlay").unwrap();
                    assert_eq!(engine.undo.len(), 1);
                    assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                    assert_eq!(
                        engine.doc.layers[0]
                            .mask
                            .as_ref()
                            .map(|m| m.steps[1].pixels.rgba16()),
                        mask
                    );
                    engine.undo("human").unwrap();
                    assert_eq!(engine.doc.export_png().unwrap(), current);
                    for output in [
                        psd::encode(&engine.doc).unwrap(),
                        psd::encode_psb(&engine.doc).unwrap(),
                    ] {
                        let reopened = psd::decode(&output).unwrap();
                        assert!(!reopened.read_only);
                        assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                        assert_eq!(
                            reopened.layers[0]
                                .mask
                                .as_ref()
                                .map(|m| m.steps[1].pixels.rgba16()),
                            mask
                        );
                        assert_eq!(reopened.layers[0].opacity, f64::from(opacity) / 255.);
                        assert_eq!(reopened.export_png().unwrap(), current);
                    }
                }
            }
        }
    }
}

fn axial_gradient_payload(degrees: f64, reflected: bool, reverse: bool) -> Vec<u8> {
    let mut payload = gradient_payload(reverse, None, 0.);
    let at = payload
        .windows(12)
        .position(|v| v == b"AnglUntF#Ang")
        .unwrap()
        + 12;
    payload[at..at + 8].copy_from_slice(&degrees.to_be_bytes());
    if reflected {
        let at = payload.windows(4).position(|v| v == b"Lnr ").unwrap();
        payload[at..at + 4].copy_from_slice(b"Rflc");
    }
    payload
}
#[test]
fn photoshop_cardinal_and_reflected_gradients_keep_native_orientation_and_sources() {
    for depth in [8, 16] {
        for (width, height) in [(2u32, 1u32), (1, 2)] {
            for degrees in [-180f64, -90., 0., 90., 180.] {
                for reflected in [false, true] {
                    for reverse in [false, true] {
                        let mut spec = Spec::paint("Synthetic axial gradient", SAVED);
                        spec.tags.push((
                            *b"lfx2",
                            axial_gradient_payload(degrees, reflected, reverse),
                        ));
                        let mut bytes = fixture_size(&[spec], depth, width, height);
                        let merged = std::array::from_fn::<_, 2, _>(|index| {
                            // Analytic cardinal coordinates, without trigonometry or
                            // the native gradient sampler/renderer under test.
                            let x = if width == 2 {
                                if index == 0 {
                                    -0.25
                                } else {
                                    0.25
                                }
                            } else {
                                0.
                            };
                            let y = if height == 2 {
                                if index == 0 {
                                    -0.25
                                } else {
                                    0.25
                                }
                            } else {
                                0.
                            };
                            let position: f64 = if degrees == 90. {
                                -y
                            } else if degrees == -90. {
                                y
                            } else if degrees.abs() == 180. {
                                -x
                            } else {
                                x
                            };
                            let mut t = if reflected {
                                2. * position.abs()
                            } else {
                                0.5 + position
                            };
                            if reverse {
                                t = 1. - t;
                            }
                            let value = native((t * 65535.).round() as u16, depth);
                            [value, value, value, 65535]
                        });
                        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                        for channel in 0..4 {
                            for pixel in merged {
                                sample(&mut bytes, pixel[channel], depth);
                            }
                        }
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{depth} {width}x{height} {degrees} {reflected} {reverse}: {:?}",
                            doc.warnings
                        );
                        assert_eq!(doc.layers[0].effects[0].settings["angle"], degrees);
                        assert_eq!(
                            doc.layers[0].effects[0].settings["style"],
                            if reflected { "reflected" } else { "linear" }
                        );
                        assert_eq!(doc.layers[0].effects[0].settings["reverse"], reverse);
                        let source = doc.layers[0].pixels.rgba16();
                        assert_eq!(
                            source,
                            SAVED
                                .into_iter()
                                .flatten()
                                .map(|n| native(n, depth))
                                .collect::<Vec<_>>()
                        );
                        let current = doc.export_png().unwrap();
                        for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.export_png().unwrap(), current);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn arbitrary_gradient_rotation_and_shape_remain_protected_even_when_disabled() {
    for depth in [8, 16] {
        for degrees in [-181., -89., 45., 89., 181.] {
            let mut payload = axial_gradient_payload(degrees, true, false);
            let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
            payload[at] = 0;
            let mut spec = Spec::paint("Synthetic unsupported rotated gradient", SAVED);
            spec.tags.push((*b"lmfx", payload));
            assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
        }
        for shape in [b"Rdl ", b"Angl", b"Dmnd", b"????"] {
            let mut payload = axial_gradient_payload(90., true, false);
            let at = payload.windows(4).position(|v| v == b"Rflc").unwrap();
            payload[at..at + 4].copy_from_slice(shape);
            let mut spec = Spec::paint("Synthetic unsupported gradient shape", SAVED);
            spec.tags.push((*b"lfx2", payload));
            assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
        }
    }
}

#[test]
fn supported_photoshop_overlays_keep_raster_clipping_sources_and_standard_saves() {
    for depth in [8, 16] {
        for family in 0..3 {
            let base_pixels = [[22349, 31237, 54321, 12347], [54323, 32349, 13337, 54321]];
            let base = Spec::paint("Synthetic clipping base", base_pixels);
            let mut clip = Spec::paint("Synthetic clipped overlay", SAVED);
            clip.clipping = 1;
            clip.tags.push((
                *b"lfx2",
                match family {
                    0 => overlay_payload(true, true, [111., 67., 231.]),
                    1 => pattern_payload(true, "synthetic-pattern", "", 100.),
                    _ => gradient_payload(false, None, 0.),
                },
            ));
            let mut bytes = fixture(&[clip, base], depth);
            if family == 1 {
                bytes = inject_pattern(
                    bytes,
                    depth,
                    &pattern_resource(depth, "synthetic-pattern"),
                    false,
                );
            }
            let merged = std::array::from_fn::<_, 2, _>(|x| {
                let color = match family {
                    0 => [111 * 257, 67 * 257, 231 * 257],
                    1 => [
                        native(PATTERN[x][0], depth),
                        native(PATTERN[x][1], depth),
                        native(PATTERN[x][2], depth),
                    ],
                    _ => [native(if x == 0 { 16384 } else { 49151 }, depth); 3],
                };
                [
                    color[0],
                    color[1],
                    color[2],
                    native(base_pixels[x][3], depth),
                ]
            });
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for channel in 0..4 {
                for pixel in merged {
                    sample(&mut bytes, pixel[channel], depth);
                }
            }
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{depth} {family}: {:?}", doc.warnings);
            assert_eq!(doc.layers[0].clip_to.as_ref(), Some(&doc.layers[1].id));
            let source = doc.layers[0].pixels.rgba16();
            let base = doc.layers[1].pixels.rgba16();
            assert_eq!(
                source,
                SAVED
                    .into_iter()
                    .flatten()
                    .map(|n| native(n, depth))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                base,
                base_pixels
                    .into_iter()
                    .flatten()
                    .map(|n| native(n, depth))
                    .collect::<Vec<_>>()
            );
            let current = doc.export_png().unwrap();
            let mut engine = Engine::new();
            engine.doc = doc;
            let layer = engine.doc.layers[0].id.clone();
            let effect = engine.doc.layers[0].effects[0].id.clone();
            engine
                .edit(
                    "human",
                    &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":false})],
                    None,
                    None,
                    "Bypass clipped overlay",
                )
                .unwrap();
            assert_eq!(engine.undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.export_png().unwrap(), current);
            for output in [
                psd::encode(&engine.doc).unwrap(),
                psd::encode_psb(&engine.doc).unwrap(),
            ] {
                let reopened = psd::decode(&output).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(
                    reopened.layers[0].clip_to.as_ref(),
                    Some(&reopened.layers[1].id)
                );
                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base);
                assert_eq!(reopened.export_png().unwrap(), current);
            }
            let standard = psd::decode(&ordinary(&psd::encode(&engine.doc).unwrap())).unwrap();
            assert!(!standard.read_only);
            assert!(standard.layers[0].effects.is_empty());
            assert_eq!(
                standard.layers[0].clip_to.as_ref(),
                Some(&standard.layers[1].id)
            );
            assert_eq!(standard.export_png().unwrap(), current);
        }
    }
}
#[test]
fn styled_clipped_rasters_without_a_compatible_base_keep_native_composites_protected() {
    for depth in [8, 16] {
        let mut clip = Spec::paint("Synthetic missing clipping base", SAVED);
        clip.clipping = 1;
        clip.tags
            .push((*b"lfx2", overlay_payload(false, false, [111., 67., 231.])));
        let doc = psd::decode(&fixture(&[clip], depth)).unwrap();
        assert!(doc.read_only);
        assert!(doc
            .warnings
            .iter()
            .any(|v| v.to_ascii_lowercase().contains("clip")));
        assert_eq!(doc.bit_depth, depth);
    }
}

fn hard_shadow_payload(
    kind: &str,
    enabled: bool,
    master: bool,
    angle: f64,
    opacity: f64,
    variant: &str,
) -> Vec<u8> {
    let unit = |unit: &[u8; 4], n: f64| [unit.to_vec(), n.to_be_bytes().to_vec()].concat();
    let mut mode = vec![];
    overlay_id(&mut mode, "BlnM");
    overlay_id(&mut mode, if variant == "blend" { "Mltp" } else { "Nrml" });
    let mut curve = 2u32.to_be_bytes().to_vec();
    for endpoint in [0f64, 255.] {
        curve.extend(b"Objc");
        curve.extend(overlay_descriptor(
            "CrPt",
            vec![
                ("Hrzn", b"doub", endpoint.to_be_bytes().to_vec()),
                (
                    "Vrtc",
                    b"doub",
                    (if variant == "contour" && endpoint == 255. {
                        254.
                    } else {
                        endpoint
                    })
                    .to_be_bytes()
                    .to_vec(),
                ),
            ],
        ));
    }
    let mut properties = vec![
        ("enab", b"bool", vec![u8::from(enabled)]),
        ("present", b"bool", vec![1]),
        ("showInDialog", b"bool", vec![1]),
        ("Md  ", b"enum", mode),
        ("Opct", b"UntF", unit(b"#Prc", opacity)),
        (
            "Clr ",
            b"Objc",
            overlay_descriptor(
                "RGBC",
                vec![
                    ("Rd  ", b"doub", 111f64.to_be_bytes().to_vec()),
                    ("Grn ", b"doub", 67f64.to_be_bytes().to_vec()),
                    ("Bl  ", b"doub", 231f64.to_be_bytes().to_vec()),
                ],
            ),
        ),
        ("uglg", b"bool", vec![u8::from(variant == "global")]),
        ("AntA", b"bool", vec![u8::from(variant == "antialias")]),
        (
            "lagl",
            b"UntF",
            unit(b"#Ang", if variant == "angle" { 45. } else { angle }),
        ),
        (
            "Dstn",
            b"UntF",
            unit(
                b"#Pxl",
                if variant == "fractional" {
                    0.5
                } else if variant == "distance" {
                    129.
                } else {
                    1.
                },
            ),
        ),
        (
            "Ckmt",
            b"UntF",
            unit(b"#Pxl", if variant == "choke" { 1. } else { 0. }),
        ),
        (
            "blur",
            b"UntF",
            unit(b"#Pxl", if variant == "size" { 1. } else { 0. }),
        ),
        (
            "Nose",
            b"UntF",
            unit(b"#Prc", if variant == "noise" { 1. } else { 0. }),
        ),
        (
            "TrnS",
            b"Objc",
            overlay_descriptor(
                "ShpC",
                vec![
                    ("Nm  ", b"TEXT", pattern_text("Synthetic linear contour")),
                    ("Crv ", b"VlLs", curve),
                ],
            ),
        ),
    ];
    if kind == "DrSh" {
        properties.push((
            "layerConceals",
            b"bool",
            vec![u8::from(variant != "knockout")],
        ));
    }
    if variant == "unknown" {
        properties.push(("unsupportedShadowField", b"bool", vec![0]));
    }
    if variant == "missing" {
        properties.retain(|(key, _, _)| *key != "TrnS");
    }
    let mut entries = vec![
        ("masterFXSwitch", b"bool", vec![u8::from(master)]),
        ("Scl ", b"UntF", unit(b"#Prc", 100.)),
        (kind, b"Objc", overlay_descriptor(kind, properties)),
    ];
    if variant == "mixed" {
        entries.push(("SoFi", b"Objc", overlay_descriptor("SoFi", vec![])));
    }
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        overlay_descriptor("Lefx", entries),
    ]
    .concat()
}
fn shadow_fixture(
    depth: u16,
    kind: &str,
    enabled: bool,
    master: bool,
    vertical: bool,
    positive: bool,
    opacity: f64,
    variant: &str,
) -> (Vec<u8>, Vec<u16>) {
    let angle = if vertical {
        if positive {
            90.
        } else {
            -90.
        }
    } else if positive {
        180.
    } else {
        0.
    };
    let source = if kind == "DrSh" {
        [[12347, 23459, 34571, 65535], [54321, 32123, 11223, 0]]
    } else {
        SAVED
    };
    let mut spec = Spec::paint("Synthetic hard shadow", source);
    if variant == "mask" {
        spec.mask = Some([65535; 2]);
    }
    if variant == "opacity" {
        spec.opacity = 128;
    }
    if variant == "partial" {
        spec.pixels[0][3] = 12347;
    }
    if variant == "clip" {
        spec.clipping = 1;
    }
    spec.tags.push((
        *b"lfx2",
        hard_shadow_payload(kind, enabled, master, angle, opacity, variant),
    ));
    let mut merged = source.map(|p| {
        if p[3] == 0 {
            [0; 4]
        } else {
            p.map(|n| native(n, depth))
        }
    });
    let tint = [111 * 257, 67 * 257, 231 * 257];
    if master && enabled {
        let alpha = (65535. * opacity / 100.).round() as u16;
        if kind == "DrSh" {
            if positive {
                merged[1] = [tint[0], tint[1], tint[2], native(alpha, depth)];
            }
        } else {
            let index = if positive { 0 } else { 1 };
            for c in 0..3 {
                merged[index][c] = native(
                    ((f64::from(merged[index][c]) * f64::from(65535 - alpha)
                        + f64::from(tint[c]) * f64::from(alpha))
                        / 65535.)
                        .round() as u16,
                    depth,
                );
            }
        }
    }
    let mut bytes = fixture_size(
        &[spec],
        depth,
        if vertical { 1 } else { 2 },
        if vertical { 2 } else { 1 },
    );
    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
    for c in 0..4 {
        for p in merged {
            sample(&mut bytes, p[c], depth);
        }
    }
    (bytes, merged.into_iter().flatten().collect())
}
#[test]
fn bounded_hard_photoshop_shadows_retain_native_sources_orientation_bypass_undo_and_saves() {
    for depth in [8, 16] {
        for kind in ["DrSh", "IrSh"] {
            for vertical in [false, true] {
                for positive in [false, true] {
                    for opacity in [50., 100.] {
                        for flags in 0u8..4 {
                            let (bytes, expected) = shadow_fixture(
                                depth,
                                kind,
                                flags & 1 != 0,
                                flags & 2 != 0,
                                vertical,
                                positive,
                                opacity,
                                "",
                            );
                            let doc = psd::decode(&bytes).unwrap();
                            assert!(
                                !doc.read_only,
                                "{depth} {kind} {vertical} {positive} {opacity} {flags}: {:?}",
                                doc.warnings
                            );
                            assert_eq!(rendered(&doc), expected);
                            assert_eq!(doc.layers[0].effects.len(), 1);
                            assert_eq!(
                                doc.layers[0].effects[0].kind,
                                if kind == "DrSh" {
                                    "drop_shadow"
                                } else {
                                    "inner_shadow"
                                }
                            );
                            assert_eq!(doc.layers[0].effects[0].enabled, flags == 3);
                            let source = doc.layers[0].pixels.rgba16();
                            let current = doc.export_png().unwrap();
                            let mut engine = Engine::new();
                            engine.doc = doc;
                            let layer = engine.doc.layers[0].id.clone();
                            let effect = engine.doc.layers[0].effects[0].id.clone();
                            engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})],None,None,"Hard shadow bypass").unwrap();
                            assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                            assert_eq!(engine.undo.len(), 1);
                            engine.undo("human").unwrap();
                            assert_eq!(engine.doc.export_png().unwrap(), current);
                            for bytes in [
                                psd::encode(&engine.doc).unwrap(),
                                psd::encode_psb(&engine.doc).unwrap(),
                            ] {
                                let reopened = psd::decode(&bytes).unwrap();
                                assert!(!reopened.read_only);
                                assert_eq!(reopened.export_png().unwrap(), current);
                                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                                assert_eq!(
                                    reopened.layers[0].effects[0].settings,
                                    engine.doc.layers[0].effects[0].settings
                                );
                            }
                            let standard =
                                psd::decode(&ordinary(&psd::encode(&engine.doc).unwrap())).unwrap();
                            assert!(!standard.read_only);
                            assert_eq!(standard.export_png().unwrap(), current);
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn unsupported_hard_shadow_definitions_stay_protected_even_when_disabled() {
    for depth in [8, 16] {
        for kind in ["DrSh", "IrSh"] {
            for enabled in [false, true] {
                for variant in [
                    "global",
                    "antialias",
                    "angle",
                    "fractional",
                    "distance",
                    "choke",
                    "size",
                    "noise",
                    "contour",
                    "unknown",
                    "missing",
                    "mixed",
                    "mask",
                    "opacity",
                    "partial",
                    "clip",
                ] {
                    let (bytes, expected) =
                        shadow_fixture(depth, kind, enabled, true, false, true, 100., variant);
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(doc.read_only, "{depth} {kind} {enabled} {variant}");
                    assert_eq!(rendered(&doc), expected);
                    assert_eq!(doc.bit_depth, depth);
                }
                if kind == "DrSh" {
                    for variant in ["blend", "knockout"] {
                        let (bytes, expected) =
                            shadow_fixture(depth, kind, enabled, true, false, true, 100., variant);
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(doc.read_only, "{variant}");
                        assert_eq!(rendered(&doc), expected);
                    }
                }
                let (mut bytes, mut expected) =
                    shadow_fixture(depth, kind, enabled, true, false, true, 100., "");
                let index = bytes.len() - 8 * usize::from(depth / 8);
                let next = expected[0] + if depth == 16 { 1 } else { 257 };
                if depth == 16 {
                    bytes[index..index + 2].copy_from_slice(&next.to_be_bytes());
                } else {
                    bytes[index] = (next / 257) as u8;
                }
                expected[0] = next;
                let doc = psd::decode(&bytes).unwrap();
                assert!(doc.read_only);
                assert_eq!(rendered(&doc), expected);
            }
        }
    }
}
