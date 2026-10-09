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

// Handwritten standard PSD records, independent of PeerBrush's encoder/private
// sources. Input order is the editor's top-to-bottom tree, including dividers.
fn fixture(specs: &[Spec], depth: u16) -> Vec<u8> {
    let mut records = vec![];
    let mut channels = vec![];
    for spec in specs.iter().rev() {
        let (w, h) = if spec.section.is_some() {
            (0i32, 0i32)
        } else {
            (2, 1)
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
            for value in [0i32, 0, 1, 2] {
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
    out.extend(1u32.to_be_bytes());
    out.extend(2u32.to_be_bytes());
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
