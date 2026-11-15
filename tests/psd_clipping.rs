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
    empty_mask: bool,
    section: Option<u32>,
    no_color_channels: bool,
    empty_raster: bool,
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
            empty_mask: false,
            section: None,
            no_color_channels: false,
            empty_raster: false,
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
fn composite_sample(out: &mut Vec<u8>, pixel: [u16; 4], channel: usize, depth: u16) {
    let max = if depth == 16 { 65535u64 } else { 255 };
    let divisor = if depth == 16 { 1u64 } else { 257 };
    let pixel = pixel.map(|v| u64::from(native(v, depth)) / divisor);
    let value = if channel == 3 {
        pixel[3]
    } else {
        (pixel[channel] * pixel[3] + max * (max - pixel[3]) + max / 2) / max
    };
    sample(out, (value * divisor) as u16, depth);
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
    pattern_resource_compression(depth, identity, 0)
}
fn pattern_resource_compression(depth: u16, identity: &str, compression: u8) -> Vec<u8> {
    pattern_resource_pixels(depth, identity, compression, PATTERN, false)
}
fn pattern_resource_pixels(
    depth: u16,
    identity: &str,
    compression: u8,
    pixels: [[u16; 4]; 2],
    alpha: bool,
) -> Vec<u8> {
    pattern_resource_pixels_bounds(depth, identity, compression, pixels, alpha, [0, 0, 2, 1])
}
fn pattern_resource_pixels_bounds(
    depth: u16,
    identity: &str,
    compression: u8,
    pixels: [[u16; 4]; 2],
    alpha: bool,
    bounds: [i32; 4],
) -> Vec<u8> {
    let rect = [bounds[1], bounds[0], bounds[3], bounds[2]]
        .into_iter()
        .flat_map(i32::to_be_bytes)
        .collect::<Vec<_>>();
    let mut arrays = rect.clone();
    arrays.extend(3u32.to_be_bytes());
    for slot in 0..5 {
        if slot == 3 || (slot == 4 && !alpha) {
            arrays.extend(0u32.to_be_bytes());
            continue;
        }
        let channel = if slot == 4 { 3 } else { slot };
        arrays.extend(1u32.to_be_bytes());
        let mut data = u32::from(depth).to_be_bytes().to_vec();
        data.extend(&rect);
        data.extend(depth.to_be_bytes());
        data.push(compression);
        let mut row = Vec::new();
        for (i, pixel) in pixels.iter().enumerate() {
            let n = native(pixel[channel], depth);
            if compression == 3 {
                let prev = if i == 0 {
                    0
                } else {
                    native(pixels[i - 1][channel], depth)
                };
                if depth == 16 {
                    row.extend(n.wrapping_sub(prev).to_be_bytes());
                } else {
                    row.push(
                        peerbrush::raster::project16(n)
                            .wrapping_sub(peerbrush::raster::project16(prev)),
                    );
                }
            } else {
                sample(&mut row, n, depth);
            }
        }
        if compression >= 2 {
            use std::io::Write;
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(&row).unwrap();
            data.extend(z.finish().unwrap());
        } else if compression == 1 {
            data.extend(((row.len() + 1) as u16).to_be_bytes());
            data.push(row.len() as u8 - 1);
            data.extend(row);
        } else {
            data.extend(row);
        }
        block(&mut arrays, &data);
    }
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
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
    } else {
        bytes.extend(composite);
    }
    bytes
}
#[test]
fn photoshop_pattern_overlay_placement_requires_exact_native_composite() {
    for depth in [8, 16] {
        for (phase, angle, indices) in [
            ([1f64, -1.], 0f64, [1, 0]),
            ([0., 0.], 90., [0, 0]),
            ([0., 0.], -90., [1, 1]),
            ([-32768., 32768.], 180., [1, 0]),
        ] {
            let mut payload = pattern_payload(true, "synthetic-pattern", "", 100.);
            for (field, value) in [
                (b"Hrzndoub".as_slice(), phase[0]),
                (b"Vrtcdoub".as_slice(), phase[1]),
                (b"AnglUntF#Ang".as_slice(), angle),
            ] {
                let at = payload.windows(field.len()).position(|v| v == field).unwrap() + field.len();
                payload[at..at + 8].copy_from_slice(&value.to_be_bytes());
            }
            let mut spec = Spec::paint("Synthetic placed pattern", SAVED);
            spec.tags.push((*b"lfx2", payload));
            let mut bytes = inject_pattern(
                fixture(&[spec], depth), depth, &pattern_resource(depth, "synthetic-pattern"), false,
            );
            let samples = indices.map(|index| PATTERN[index]);
            let mut merged = Vec::new();
            for channel in 0..4 {
                for pixel in samples {
                    composite_sample(&mut merged, pixel, channel, depth);
                }
            }
            let at = bytes.len() - merged.len();
            bytes[at..].copy_from_slice(&merged);
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{depth} {phase:?} {angle}: {:?}", doc.warnings);
            let settings = &doc.layers[0].effects[0].settings;
            assert_eq!(settings["phase_x"], phase[0]);
            assert_eq!(settings["phase_y"], phase[1]);
            assert_eq!(settings["bitmap_angle"], angle);
            assert_eq!(rendered(&doc), samples.into_iter().flatten().map(|word| native(word, depth)).collect::<Vec<_>>());
            assert_eq!(doc.layers[0].pixels.rgba16(), SAVED.into_iter().flatten().map(|word| native(word, depth)).collect::<Vec<_>>());
            bytes[at] ^= 1;
            assert!(psd::decode(&bytes).unwrap().read_only);
        }
    }
}

#[test]
fn plain_photoshop_pattern_import_retains_native_tiles_sources_and_editable_saves() {
    for depth in [8, 16] {
        for compression in [0, 2, 3] {
            for enabled in [true, false] {
                let mut spec = Spec::paint("Synthetic pattern source", SAVED);
                spec.tags.push((
                    *b"lfx2",
                    pattern_payload(enabled, "synthetic-pattern", "", 100.),
                ));
                let bytes = inject_pattern(
                    fixture(&[spec], depth),
                    depth,
                    &pattern_resource_compression(depth, "synthetic-pattern", compression),
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
                engine.resident_state_mut().expect("resident fixture").document = doc;
                let mut settings = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings.as_ref().clone();
                settings["phase_x"] = json!(1.);
                engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":settings})],None,None,"Pattern phase").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), rendered);
                for large in [false, true] {
                    let reopened = psd::decode(&if large {
                        psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap()
                    } else {
                        psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap()
                    })
                    .unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.export_png().unwrap(), rendered);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                }
            }
        }
    }
}
#[test]
fn transparent_photoshop_patterns_retain_native_sources_blends_guards_and_working_saves() {
    for depth in [8, 16] {
        for compression in [0, 1, 2, 3] {
            for alpha in [[0, 41317], [65535, 1]] {
                let mut pixels = PATTERN;
                for x in 0..2 {
                    pixels[x][3] = alpha[x];
                }
                for enabled in [true, false] {
                    for (mode, opacity) in [(b"Nrml", 100f64), (b"Mltp", 37f64)] {
                        let mut payload = pattern_payload(enabled, "synthetic-pattern", "", 100.);
                        let at = payload.windows(4).position(|v| v == b"Nrml").unwrap();
                        payload[at..at + 4].copy_from_slice(mode);
                        let at = payload
                            .windows(12)
                            .position(|v| v == b"OpctUntF#Prc")
                            .unwrap()
                            + 12;
                        payload[at..at + 8].copy_from_slice(&opacity.to_be_bytes());
                        let mut spec = Spec::paint("Synthetic transparent pattern", SAVED);
                        spec.tags.push((*b"lfx2", payload));
                        let mut bytes = inject_pattern(
                            fixture(&[spec], depth),
                            depth,
                            &pattern_resource_pixels(
                                depth,
                                "synthetic-pattern",
                                compression,
                                pixels,
                                true,
                            ),
                            false,
                        );
                        let mut merged = SAVED.map(|p| p.map(|n| native(n, depth)));
                        if enabled {
                            for x in 0..2 {
                                let coverage =
                                    (f64::from(native(alpha[x], depth)) * opacity / 100.).round()
                                        / 65535.;
                                for c in 0..3 {
                                    let base = f64::from(merged[x][c]) / 65535.;
                                    let tile = f64::from(native(pixels[x][c], depth)) / 65535.;
                                    let color = if mode == b"Mltp" { base * tile } else { tile };
                                    merged[x][c] = native(
                                        ((base * (1. - coverage) + color * coverage) * 65535.)
                                            .round() as u16,
                                        depth,
                                    );
                                }
                            }
                        }
                        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                        for c in 0..4 {
                            for pixel in merged {
                                composite_sample(&mut bytes, pixel, c, depth);
                            }
                        }
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(!doc.read_only, "{:?}", doc.warnings);
                        assert_eq!(
                            rendered(&doc),
                            merged.into_iter().flatten().collect::<Vec<_>>()
                        );
                        let source = doc.layers[0].pixels.rgba16();
                        let rendered = doc.export_png().unwrap();
                        let bitmap = doc.layers[0].effects[0].settings["bitmap_png"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                        use base64::Engine as _;
                        let png = base64::engine::general_purpose::STANDARD
                            .decode(&bitmap)
                            .unwrap();
                        assert_eq!(
                            image::load_from_memory(&png)
                                .unwrap()
                                .into_rgba16()
                                .into_raw(),
                            pixels
                                .into_iter()
                                .flatten()
                                .map(|v| native(v, depth))
                                .collect::<Vec<_>>()
                        );
                        let mut e = Engine::new();
                        e.resident_state_mut().expect("resident fixture").document = doc;
                        let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                        let effect = e.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                        let mut settings = e.resident_state().expect("resident fixture").document.layers[0].effects[0].settings.as_ref().clone();
                        settings["phase_x"] = json!(1.);
                        e.edit("human",&[json!({"op":"effect.update","layer":id,"effect":effect,"settings":settings})],None,None,"Transparent phase").unwrap();
                        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                        assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                        e.undo("human").unwrap();
                        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), rendered);
                        for large in [false, true] {
                            let saved = if large {
                                psd::encode_psb(&e.resident_state().expect("resident fixture").document).unwrap()
                            } else {
                                psd::encode(&e.resident_state().expect("resident fixture").document).unwrap()
                            };
                            let reopened = psd::decode(&saved).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.export_png().unwrap(), rendered);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(
                                reopened.layers[0].effects[0].settings["bitmap_png"],
                                bitmap
                            );
                        }
                        // Even one encoded composite sample disagreement protects the whole source.
                        let at = bytes.len() - 8 * usize::from(depth / 8);
                        bytes[at] ^= 1;
                        assert!(psd::decode(&bytes).unwrap().read_only);
                    }
                }
            }
        }
    }
}
#[test]
fn eight_bit_photoshop_pattern_sources_remain_eight_bit_inside_native_sixteen_bit_documents() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    for compression in [0, 1, 2, 3] {
        for enabled in [false, true] {
            for (mode, blend, opacity, alpha) in [
                (b"Nrml", "normal", 100f64, false),
                (b"Mltp", "multiply", 100., false),
                (b"Scrn", "screen", 100., false),
                (b"Ovrl", "overlay", 100., false),
                (b"Drkn", "darken", 100., false),
                (b"Lghn", "lighten", 100., false),
                (b"Nrml", "normal", 37., true),
                (b"Mltp", "multiply", 37., true),
            ] {
                let mut pixels = PATTERN;
                if alpha {
                    pixels[0][3] = 0;
                    pixels[1][3] = 41317;
                }
                let tile = pixels.map(|p| p.map(|v| native(v, 8)));
                let mut payload = pattern_payload(enabled, "synthetic-pattern", "", 100.);
                let at = payload.windows(4).position(|v| v == b"Nrml").unwrap();
                payload[at..at + 4].copy_from_slice(mode);
                let at = payload
                    .windows(12)
                    .position(|v| v == b"OpctUntF#Prc")
                    .unwrap()
                    + 12;
                payload[at..at + 8].copy_from_slice(&opacity.to_be_bytes());
                let mut spec = Spec::paint("Synthetic mixed pattern precision", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let mut bytes = inject_pattern(
                    fixture(&[spec], 16),
                    16,
                    &pattern_resource_pixels(8, "synthetic-pattern", compression, pixels, alpha),
                    false,
                );
                // Reference uses the asset's byte samples and the document's
                // original low bits, independently of the native effect renderer.
                let mut merged = SAVED;
                if enabled {
                    for x in 0..2 {
                        let coverage = (f64::from(tile[x][3]) * opacity / 100.).round() / 65535.;
                        for c in 0..3 {
                            let base = f64::from(SAVED[x][c]) / 65535.;
                            let tint = f64::from(tile[x][c]) / 65535.;
                            let color = match blend {
                                "multiply" => base * tint,
                                "screen" => base + tint - base * tint,
                                "overlay" if base <= 0.5 => 2. * base * tint,
                                "overlay" => 1. - 2. * (1. - base) * (1. - tint),
                                "darken" => base.min(tint),
                                "lighten" => base.max(tint),
                                _ => tint,
                            };
                            merged[x][c] = ((base * (1. - coverage) + color * coverage) * 65535.)
                                .round() as u16;
                        }
                    }
                }
                bytes.truncate(bytes.len() - 16);
                for c in 0..4 {
                    for pixel in merged {
                        composite_sample(&mut bytes, pixel, c, 16);
                    }
                }
                let doc = psd::decode(&bytes).unwrap();
                assert!(
                    !doc.read_only,
                    "{compression} {enabled} {blend} {alpha}: {:?}",
                    doc.warnings
                );
                assert_eq!(doc.bit_depth, 16);
                assert_eq!(doc.layers[0].pixels.rgba16(), SAVED.concat());
                assert_eq!(rendered(&doc), merged.concat());
                let bitmap = doc.layers[0].effects[0].settings["bitmap_png"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let png = STANDARD.decode(&bitmap).unwrap();
                assert_eq!(png[24], 8, "retain the original PNG8 channel depth");
                assert_eq!(
                    image::load_from_memory(&png)
                        .unwrap()
                        .into_rgba16()
                        .into_raw(),
                    tile.concat()
                );
                assert_eq!(peerbrush::effects::patterns::get(&bitmap).unwrap().depth, 8);
                let current = doc.export_png().unwrap();
                let mut engine = Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = doc;
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Mixed precision pattern bypass").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, 16);
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), SAVED.concat());
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                for output in [
                    psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                ] {
                    assert_eq!(saved_composite(&output, 16), merged.concat());
                    let reopened = psd::decode(&output).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.bit_depth, 16);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), SAVED.concat());
                    assert_eq!(reopened.layers[0].effects[0].settings["bitmap_png"], bitmap);
                    assert_eq!(reopened.export_png().unwrap(), current);
                    let standard = psd::decode(&ordinary(&output)).unwrap();
                    assert!(!standard.read_only);
                    assert_eq!(standard.bit_depth, 16);
                    assert_eq!(standard.export_png().unwrap(), current);
                }
                // One native-word disagreement still protects the complete source.
                let at = bytes.len() - 15;
                bytes[at] ^= 1;
                let protected = psd::decode(&bytes).unwrap();
                assert!(protected.read_only);
                assert!(protected.layers[0].effects.is_empty());
                assert_eq!(protected.bit_depth, 16);
            }
        }
    }
}

#[test]
fn sixteen_bit_photoshop_patterns_never_become_editable_eight_bit_sources() {
    for compression in [0, 1, 2, 3] {
        for enabled in [false, true] {
            let mut spec = Spec::paint("Synthetic protected precision", SAVED);
            spec.tags.push((
                *b"lfx2",
                pattern_payload(enabled, "synthetic-pattern", "", 100.),
            ));
            let bytes = inject_pattern(
                fixture(&[spec], 8),
                8,
                &pattern_resource_compression(16, "synthetic-pattern", compression),
                enabled,
            );
            let doc = psd::decode(&bytes).unwrap();
            assert!(doc.read_only, "{compression} {enabled}");
            assert_eq!(doc.bit_depth, 8);
            assert!(doc.layers[0].effects.is_empty());
            assert!(doc.warnings.iter().any(|w| w.contains("Pattern precision")));
            assert!(psd::encode(&doc).is_err());
            assert!(psd::encode_psb(&doc).is_err());
        }
    }
}

#[test]
fn omitted_pattern_angle_imports_as_unrotated_with_exact_native_source_and_save_guards() {
    for depth in [8, 16] {
        for enabled in [false, true] {
            for compression in [0, 2, 3] {
                let build = |omit| {
                    let mut spec = Spec::paint("Synthetic optional angle", SAVED);
                    spec.tags.push((
                        *b"lfx2",
                        pattern_payload(enabled, "synthetic-pattern", omit, 100.),
                    ));
                    inject_pattern(
                        fixture(&[spec], depth),
                        depth,
                        &pattern_resource_compression(depth, "synthetic-pattern", compression),
                        enabled,
                    )
                };
                let doc = psd::decode(&build("Angl")).unwrap();
                assert!(
                    !doc.read_only,
                    "{depth} {enabled} {compression}: {:?}",
                    doc.warnings
                );
                let explicit = psd::decode(&build("")).unwrap();
                assert_eq!(doc.layers[0].effects.len(), 1);
                assert_eq!(
                    doc.layers[0].effects[0].settings,
                    explicit.layers[0].effects[0].settings
                );
                assert_eq!(doc.layers[0].effects[0].enabled, enabled);
                assert_eq!(doc.layers[0].effects[0].settings["bitmap_angle"], 0.);
                assert_eq!(
                    rendered(&doc),
                    if enabled { PATTERN } else { SAVED }
                        .into_iter()
                        .flat_map(|pixel| pixel.map(|n| native(n, depth)))
                        .collect::<Vec<_>>()
                );
                let source = doc.layers[0].pixels.rgba16();
                let current = doc.export_png().unwrap();
                let mut engine = Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = doc;
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})],None,None,"Optional-angle bypass").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                for bytes in [
                    psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                ] {
                    let reopened = psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.bit_depth, depth);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                    assert_eq!(
                        reopened.layers[0].effects[0].settings["bitmap_png"],
                        engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["bitmap_png"]
                    );
                    assert_eq!(reopened.export_png().unwrap(), current);
                    assert_eq!(
                        psd::decode(&ordinary(&bytes))
                            .unwrap()
                            .export_png()
                            .unwrap(),
                        current
                    );
                }
                let mut mismatch = build("Angl");
                let at = mismatch.len() - 8 * usize::from(depth / 8);
                mismatch[at] ^= 1;
                assert!(psd::decode(&mismatch).unwrap().read_only);
            }
        }
    }
}

#[test]
fn explicit_unsupported_pattern_angles_never_default_to_zero() {
    for depth in [8, 16] {
        for enabled in [false, true] {
            for (unit, angle) in [
                (b"#Prc", 0f64),
                (b"#Ang", 1.),
                (b"#Ang", f64::NAN),
                (b"#Ang", f64::INFINITY),
            ] {
                let mut payload = pattern_payload(enabled, "synthetic-pattern", "", 100.);
                let at = payload
                    .windows(12)
                    .position(|bytes| bytes == b"AnglUntF#Ang")
                    .unwrap();
                payload[at + 8..at + 12].copy_from_slice(unit);
                payload[at + 12..at + 20].copy_from_slice(&angle.to_be_bytes());
                let mut spec = Spec::paint("Synthetic guarded angle", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let bytes = inject_pattern(
                    fixture(&[spec], depth),
                    depth,
                    &pattern_resource(depth, "synthetic-pattern"),
                    enabled,
                );
                let doc = psd::decode(&bytes).unwrap();
                assert!(doc.read_only, "{depth} {enabled} {unit:?} {angle}");
                assert_eq!(doc.bit_depth, depth);
                assert_eq!(
                    rendered(&doc),
                    if enabled { PATTERN } else { SAVED }
                        .into_iter()
                        .flat_map(|pixel| pixel.map(|n| native(n, depth)))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
}

#[test]
fn photoshop_pattern_storage_origins_preserve_local_tiles_native_sources_and_saves() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    for (depth, asset_depth) in [(8, 8), (16, 16), (16, 8)] {
        for origin in [[-17, 29], [38, -23], [-32768, 32767], [32766, -32768]] {
            let bounds = [origin[0], origin[1], origin[0] + 2, origin[1] + 1];
            for compression in [0, 3] {
                for enabled in [false, true] {
                    let mut pixels = PATTERN;
                    pixels[0][3] = 0;
                    pixels[1][3] = 41317;
                    let tile = pixels.map(|pixel| pixel.map(|n| native(n, asset_depth)));
                    let mut payload = pattern_payload(enabled, "synthetic-origin", "", 100.);
                    let at = payload
                        .windows(12)
                        .position(|v| v == b"OpctUntF#Prc")
                        .unwrap()
                        + 12;
                    payload[at..at + 8].copy_from_slice(&37f64.to_be_bytes());
                    let mut spec = Spec::paint("Synthetic pattern storage frame", SAVED);
                    spec.tags.push((*b"lfx2", payload));
                    let resource = pattern_resource_pixels_bounds(
                        asset_depth,
                        "synthetic-origin",
                        compression,
                        pixels,
                        true,
                        bounds,
                    );
                    let mut bytes =
                        inject_pattern(fixture(&[spec], depth), depth, &resource, false);
                    // Storage coordinates are absent from the independent local
                    // tile and source-over calculation, unlike document phase.
                    let source = SAVED.map(|pixel| pixel.map(|n| native(n, depth)));
                    let mut merged = source;
                    if enabled {
                        for x in 0..2 {
                            let weight =
                                (f64::from(tile[x][3]) / 65535. * 0.37 * 65535.).round() / 65535.;
                            for c in 0..3 {
                                merged[x][c] = native(
                                    (((1. - weight) * (f64::from(source[x][c]) / 65535.)
                                        + weight * (f64::from(tile[x][c]) / 65535.))
                                        * 65535.)
                                        .round() as u16,
                                    depth,
                                );
                            }
                        }
                    }
                    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                    for c in 0..4 {
                        for pixel in merged {
                            composite_sample(&mut bytes, pixel, c, depth);
                        }
                    }
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(
                        !doc.read_only,
                        "{depth}/{asset_depth} {bounds:?} {compression} {enabled}: {:?}",
                        doc.warnings
                    );
                    assert_eq!(doc.bit_depth, depth);
                    assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
                    assert_eq!(rendered(&doc), merged.concat());
                    let effect = &doc.layers[0].effects[0];
                    assert_eq!(effect.enabled, enabled);
                    assert_eq!(effect.settings["phase_x"], 0.);
                    assert_eq!(effect.settings["phase_y"], 0.);
                    let bitmap = effect.settings["bitmap_png"].as_str().unwrap().to_owned();
                    let png = STANDARD.decode(&bitmap).unwrap();
                    assert_eq!(u16::from(png[24]), asset_depth);
                    assert_eq!(
                        image::load_from_memory(&png)
                            .unwrap()
                            .into_rgba16()
                            .into_raw(),
                        tile.concat()
                    );
                    let current = doc.export_png().unwrap();
                    let mut engine = Engine::new();
                    let layer = doc.layers[0].id.clone();
                    let effect = effect.id.clone();
                    engine.resident_state_mut().expect("resident fixture").document = doc;
                    engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Pattern storage-frame bypass").unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                    engine.undo("human").unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source.concat());
                    assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                    for output in [
                        psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    ] {
                        assert_eq!(saved_composite(&output, depth), merged.concat());
                        let reopened = psd::decode(&output).unwrap();
                        assert!(!reopened.read_only);
                        assert_eq!(reopened.bit_depth, depth);
                        assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                        assert_eq!(reopened.layers[0].effects[0].settings["bitmap_png"], bitmap);
                        assert_eq!(reopened.export_png().unwrap(), current);
                        assert_eq!(
                            psd::decode(&ordinary(&output))
                                .unwrap()
                                .export_png()
                                .unwrap(),
                            current
                        );
                    }
                    let at = bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth / 8) - 1;
                    bytes[at] ^= 1;
                    assert!(
                        psd::decode(&bytes).unwrap().read_only,
                        "storage-origin composite mismatch was accepted"
                    );
                }
            }
        }
    }
}
#[test]
fn photoshop_pattern_storage_origins_do_not_authorize_invalid_frames_or_placement() {
    for depth in [8, 16] {
        for enabled in [false, true] {
            for bounds in [
                [-17, 29, -17, 30],
                [-17, 29, -18, 30],
                [-17, 29, -15, 29],
                [32767, 29, 32769, 30],
                [-32769, 29, -32767, 30],
            ] {
                let mut spec = Spec::paint("Synthetic invalid pattern frame", SAVED);
                spec.tags.push((
                    *b"lfx2",
                    pattern_payload(enabled, "synthetic-origin", "", 100.),
                ));
                let resource = pattern_resource_pixels_bounds(
                    depth,
                    "synthetic-origin",
                    0,
                    PATTERN,
                    true,
                    bounds,
                );
                assert!(
                    psd::decode(&inject_pattern(
                        fixture(&[spec], depth),
                        depth,
                        &resource,
                        false
                    ))
                    .unwrap()
                    .read_only
                );
            }
            let bounds: [i32; 4] = [-17, 29, -15, 30];
            for variant in ["channel", "phase", "aligned"] {
                let mut payload = pattern_payload(enabled, "synthetic-origin", "", 100.);
                let mut resource = pattern_resource_pixels_bounds(
                    depth,
                    "synthetic-origin",
                    0,
                    PATTERN,
                    true,
                    bounds,
                );
                match variant {
                    "channel" => {
                        let rect = [bounds[1], bounds[0], bounds[3], bounds[2]]
                            .into_iter()
                            .flat_map(i32::to_be_bytes)
                            .collect::<Vec<_>>();
                        let matches = resource
                            .windows(16)
                            .enumerate()
                            .filter_map(|(at, v)| (v == rect).then_some(at))
                            .collect::<Vec<_>>();
                        assert_eq!(matches.len(), 5);
                        // Same dimensions, but one channel has a different frame.
                        let shifted = [bounds[1] + 1, bounds[0], bounds[3] + 1, bounds[2]]
                            .into_iter()
                            .flat_map(i32::to_be_bytes)
                            .collect::<Vec<_>>();
                        let at = matches[1];
                        resource[at..at + 16].copy_from_slice(&shifted);
                    }
                    "phase" => {
                        let at = payload.windows(8).position(|v| v == b"Hrzndoub").unwrap() + 8;
                        payload[at..at + 8].copy_from_slice(&1.25f64.to_be_bytes());
                    }
                    _ => {
                        let at = payload.windows(8).position(|v| v == b"Algnbool").unwrap() + 8;
                        payload[at] = 1;
                    }
                }
                let mut spec =
                    Spec::paint("Synthetic unsupported pattern storage/placement", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let doc = psd::decode(&inject_pattern(
                    fixture(&[spec], depth),
                    depth,
                    &resource,
                    false,
                ))
                .unwrap();
                assert!(doc.read_only, "{depth} {enabled} {variant}");
                assert!(doc.layers[0].effects.is_empty());
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
        let (w, h) = if spec.section.is_some() || spec.empty_raster {
            (0i32, 0i32)
        } else {
            (width as i32, height as i32)
        };
        for value in [0i32, 0, h, w] {
            records.extend(value.to_be_bytes());
        }
        let color_channels = if spec.no_color_channels { &[][..] } else { &[-1i16, 0, 1, 2] };
        records.extend((color_channels.len() as u16 + u16::from(spec.mask.is_some())).to_be_bytes());
        for channel in color_channels.iter().copied().chain(spec.mask.map(|_| -2)) {
            let mut data = 0u16.to_be_bytes().to_vec();
            if channel == -2 {
                if !spec.empty_mask {
                    for value in spec.mask.unwrap() {
                        sample(&mut data, value, depth);
                    }
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
            for value in if spec.empty_mask {
                [0; 4]
            } else {
                [0i32, 0, height as i32, width as i32]
            } {
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
fn folder_blend_if_uses_isolated_children_and_outside_backdrop_without_color_channels() {
    for depth in [8, 16] {
        for side in [0, 4] {
            let mut folder = Spec::folder("Folder", 1);
            let mut closing = Spec::folder("</Folder>", 3);
            folder.no_color_channels = true;
            closing.no_color_channels = true;
            closing.ranges = [0, 0, 255, 255].repeat(10);
            folder.ranges = closing.ranges.clone();
            // Red above 100 is rejected on the chosen side of the folder merge.
            folder.ranges[8 + side + 2..8 + side + 4].fill(100);
            let upper = Spec::paint("Upper", [[0; 4], [60000, 25001, 30001, 65535]]);
            let lower = Spec::paint("Lower", [SAVED[0]; 2]);
            let backdrop = Spec::paint("Backdrop", [[1234, 5678, 9876, 65535], SAVED[1]]);
            let mut specs = [folder, upper, lower, closing, backdrop];
            let bytes = fixture(&specs, depth);
            let expected = SAVED.into_iter().flatten().map(|v| native(v, depth)).collect::<Vec<_>>();
            for doc in [
                psd::decode(&bytes).unwrap(),
                psd::decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
            ] {
                assert!(!doc.read_only, "{:?}", doc.warnings);
                assert_eq!(doc.layers[0].kind, "group");
                assert_eq!(doc.layers[1].parent.as_deref(), Some(doc.layers[0].id.as_str()));
                assert_eq!(rendered(&doc), expected);
                for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    for bytes in [encoded.clone(), ordinary(&encoded)] {
                        let reopened = psd::decode(&bytes).unwrap();
                        assert!(!reopened.read_only, "{:?}", reopened.warnings);
                        assert_eq!(reopened.layers[0].psd_metadata, doc.layers[0].psd_metadata);
                        assert_eq!(reopened.layers[1].pixels, doc.layers[1].pixels);
                        assert_eq!(rendered(&reopened), expected);
                    }
                }
            }
            specs[3].ranges[..2].fill(1);
            let protected = psd::decode(&fixture(&specs, depth)).unwrap();
            assert!(protected.read_only);
            assert!(protected.warnings.iter().any(|warning| warning.contains("Blend If on a closing folder")));
        }
    }
}

#[test]
fn pass_through_folder_blend_if_stops_child_adjustments_at_the_folder_boundary() {
    for depth in [8, 16] {
        let mut outer = Spec::folder("Outer", 1);
        outer.blend = *b"pass";
        outer.ranges = [0, 0, 255, 255].repeat(10);
        outer.ranges[..2].fill(1);
        let mut inner = Spec::folder("Inner", 1);
        inner.blend = *b"pass";
        let mut invert = Spec::paint("Invert", [[0; 4]; 2]);
        invert.empty_raster = true;
        invert.flags = 24;
        invert.tags.push((*b"nvrt", vec![]));
        let mut ink = SAVED[0];
        for channel in &mut ink[..3] {
            *channel = 65535 - *channel;
        }
        let child = Spec::paint("Ink", [ink, [0; 4]]);
        let backdrop = Spec::paint("Outside", [SAVED[1]; 2]);
        let bytes = fixture(&[outer, inner, invert, child, Spec::folder("</Inner>", 3),
            Spec::folder("</Outer>", 3), backdrop], depth);
        let doc = psd::decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(doc.layers[0].blend, "pass_through");
        let expected = SAVED.into_iter().flatten().map(|v| native(v, depth)).collect::<Vec<_>>();
        assert_eq!(rendered(&doc), expected);
        for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            for bytes in [encoded.clone(), ordinary(&encoded)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].blend, "pass_through");
                assert_eq!(rendered(&reopened), expected);
            }
        }
        let target = doc.layers[0].id.clone();
        let cleared = Engine::preview_edits(doc, &[json!({
            "op":"layer.update","layer":target,"blend_if":null,
        })]).unwrap();
        let mut adjusted_outside = SAVED[1];
        for channel in &mut adjusted_outside[..3] {
            *channel = 65535 - *channel;
        }
        assert_eq!(&rendered(&cleared)[4..], &adjusted_outside.map(|v| native(v, depth)));
    }
}

#[test]
fn clipped_blend_if_reads_effective_base_coverage_without_expanding_the_shape() {
    for depth in [8, 16] {
        for adjustment in [false, true] {
            let mut base = Spec::paint("Base", [[16448, 16448, 16448, 65535], [0; 4]]);
            base.opacity = 128;
            let mut member = Spec::paint("Clipped", [[49344, 49344, 49344, 65535]; 2]);
            member.clipping = 1;
            member.ranges = [0, 0, 255, 255].repeat(10);
            member.ranges[14..16].fill(30);
            if adjustment {
                member.empty_raster = true;
                member.flags = 24;
                member.tags.push((*b"nvrt", vec![]));
            }
            // The red test rejects the painted part of the base. Only the
            // uncovered 127/255 contributes to the member's blend strength.
            let rgb = match (depth, adjustment) {
                (8, false) => 128 * 257,
                (8, true) => 127 * 257,
                (16, false) => 32831,
                _ => 32704,
            };
            let expected = [[rgb, rgb, rgb, 32896], [0; 4]];
            let mut bytes = fixture(&[member, base], depth);
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for channel in 0..4 {
                for pixel in expected {
                    composite_sample(&mut bytes, pixel, channel, depth);
                }
            }
            for doc in [
                psd::decode(&bytes).unwrap(),
                psd::decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
            ] {
                assert!(!doc.read_only, "{:?}", doc.warnings);
                assert_eq!(doc.layers[0].clip_to.as_deref(), Some(doc.layers[1].id.as_str()));
                assert_eq!(rendered(&doc), expected.concat());
                for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    let reopened = psd::decode(&ordinary(&encoded)).unwrap();
                    assert!(!reopened.read_only, "{:?}", reopened.warnings);
                    assert_eq!(reopened.layers[0].psd_metadata, doc.layers[0].psd_metadata);
                    assert_eq!(reopened.layers[1].pixels, doc.layers[1].pixels);
                    assert_eq!(rendered(&reopened), expected.concat());
                }
            }
        }
    }
}

#[test]
fn photoshop_blend_channels_preserve_backdrop_contribution_and_source_raster() {
    for depth in [8, 16] {
        let original = [51399, 51143, 13011, 65535];
        let mut top = Spec::paint("Restricted", [original; 2]);
        let excluded = [0u32, 2].into_iter().flat_map(u32::to_be_bytes).collect();
        top.tags.push((*b"brst", excluded));
        let bottom = Spec::paint("Backdrop", [[25700, 17990, 15420, 32896], [25700, 17990, 15420, 0]]);
        let (red, blue) = if depth == 8 { (12850, 7710) } else { (12900, 7740) };
        let expected = [[red, 51143, blue, 65535], [0, 51143, 0, 65535]];
        let mut bytes = fixture(&[top.clone(), bottom.clone()], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in expected {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        let doc = psd::decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(rendered(&doc), expected.concat());
        assert_eq!(doc.layers[0].pixels.get16(0, 0), original.map(|v| native(v, depth)));
        for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            for bytes in [encoded.clone(), ordinary(&encoded)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].psd_metadata, doc.layers[0].psd_metadata);
                assert_eq!(reopened.layers[0].pixels, doc.layers[0].pixels);
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
        let target = doc.layers[0].id.clone();
        let hidden = Engine::preview_edits(doc, &[json!({
            "op":"layer.update","layer":target,"blend_channels":[false,false,false],
        })]).unwrap();
        assert_eq!(rendered(&hidden), [[25700, 17990, 15420, 32896], [0; 4]].concat());
        top.tags[0].1 = 3u32.to_be_bytes().to_vec();
        let protected = psd::decode(&fixture(&[top, bottom], depth)).unwrap();
        assert!(protected.read_only);
        assert!(protected.warnings.iter().any(|reason| reason.contains("brst")));
    }
}

#[test]
fn pass_through_channel_exclusions_reach_nested_child_adjustment_inputs() {
    for depth in [8, 16] {
        let mut outer = Spec::folder("Keep red", 1);
        outer.blend = *b"pass";
        outer.tags.push((*b"brst", 0u32.to_be_bytes().to_vec()));
        let mut inner = Spec::folder("Keep blue", 1);
        inner.blend = *b"pass";
        inner.tags.push((*b"brst", 2u32.to_be_bytes().to_vec()));
        let mut threshold = Spec::paint("Threshold", [[0; 4]; 2]);
        threshold.flags = 24;
        threshold.empty_raster = true;
        threshold.tags.push((*b"thrs", vec![0, 128, 0, 0]));
        threshold.ranges = [0, 0, 255, 255].repeat(10);
        threshold.ranges[14..16].fill(250);
        let child = Spec::paint("Bright red", [[62000, 25700, 45000, 65535], [0; 4]]);
        let outside = [[12345, 20201, 30003, 65535], [65535, 32123, 11223, 65535]];
        let expected = [[outside[0][0], 0, outside[0][2], 65535], outside[1]];
        let mut bytes = fixture(&[outer, inner, threshold, child, Spec::folder("</Inner>", 3),
            Spec::folder("</Outer>", 3), Spec::paint("Backdrop", outside)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in expected {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        let doc = psd::decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let expected = expected.concat().into_iter().map(|v| native(v, depth)).collect::<Vec<_>>();
        assert_eq!(rendered(&doc), expected);
        for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            for bytes in [encoded.clone(), ordinary(&encoded)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].blend, "pass_through");
                assert_eq!(reopened.layers[1].blend, "pass_through");
                assert_eq!(rendered(&reopened), expected);
            }
        }
        let outer = doc.layers[0].id.clone();
        let cleared = Engine::preview_edits(doc, &[json!({
            "op":"layer.update","layer":outer,"blend_channels":[true,true,true],
        })]).unwrap();
        assert_eq!(rendered(&cleared)[1], 65535);
    }
}

#[test]
fn clipped_blend_channels_keep_inside_and_outside_backdrops_separate() {
    for depth in [8, 16] {
        let outside = [8345, 16007, 24011, 65535];
        let inside = [34567, 38789, 41411, 65535];
        let ink = [50001, 50503, 52007, 65535];
        let mut base = Spec::paint("Base", [inside, [0; 4]]);
        base.tags.push((*b"brst", 0u32.to_be_bytes().to_vec()));
        let mut member = Spec::paint("Member", [ink; 2]);
        member.clipping = 1;
        member.tags.push((*b"brst", 1u32.to_be_bytes().to_vec()));
        let expected = [[outside[0], inside[1], ink[2], 65535], outside];
        let mut bytes = fixture(&[member, base, Spec::paint("Backdrop", [outside; 2])], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in expected {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        let doc = psd::decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let expected = expected.concat().into_iter().map(|v| native(v, depth)).collect::<Vec<_>>();
        assert_eq!(rendered(&doc), expected);
        for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            for bytes in [encoded.clone(), ordinary(&encoded)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(rendered(&reopened), expected);
                for (after, before) in reopened.layers.iter().zip(&doc.layers) {
                    assert_eq!(after.psd_metadata, before.psd_metadata);
                    assert_eq!(after.pixels, before.pixels);
                }
            }
        }
        let target = doc.layers[1].id.clone();
        let hidden = Engine::preview_edits(doc, &[json!({
            "op":"layer.update","layer":target,"blend_channels":[false,false,false],
        })]).unwrap();
        assert_eq!(rendered(&hidden), [outside; 2].concat().into_iter()
            .map(|v| native(v, depth)).collect::<Vec<_>>());
    }
}

#[test]
fn ranged_clipping_base_keeps_shape_available_to_a_partial_member() {
    for depth in [8, 16] {
        let mut base = Spec::paint("Hidden base", [[50000, 25001, 30001, 32896]; 2]);
        base.ranges = [0, 0, 255, 255].repeat(10);
        base.ranges[10..12].fill(100);
        let mut member = Spec::paint("Painted member", [[49344, 49344, 49344, 32896], [0; 4]]);
        member.clipping = 1;
        // Red rejects the base. The member draws into black at its own
        // coverage, bounded by the base's original 128/255 clipping shape.
        let (rgb, alpha) = if depth == 8 { (96 * 257, 64 * 257) } else { (24769, 16513) };
        let expected = [[rgb, rgb, rgb, alpha], [0; 4]];
        let mut bytes = fixture(&[member, base], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in expected {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        let doc = psd::decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(rendered(&doc), expected.concat());
        for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            for bytes in [encoded.clone(), ordinary(&encoded)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[1].psd_metadata, doc.layers[1].psd_metadata);
                assert_eq!(reopened.layers[1].pixels, doc.layers[1].pixels);
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
    }
}

#[test]
fn adjustment_blend_if_tests_adjusted_and_original_backdrop_at_native_depth() {
    for depth in [8, 16] {
        for source_gate in [true, false] {
            let mut invert = Spec::paint("Invert", [[0; 4]; 2]);
            invert.empty_raster = true;
            invert.no_color_channels = true;
            invert.flags = 24;
            invert.tags.push((*b"nvrt", vec![]));
            invert.ranges = [0, 0, 255, 255].repeat(10);
            if source_gate {
                // The adjusted red values straddle 46. Both input reds exceed it.
                invert.ranges[8..10].fill(46);
            } else {
                // The input reds straddle 209. Both adjusted reds are below it.
                invert.ranges[14..16].fill(209);
            }
            let mut original = SAVED;
            for channel in &mut original[0][..3] {
                *channel = 65535 - *channel;
            }
            let bytes = fixture(&[invert, Spec::paint("Base", original)], depth);
            let expected = SAVED.into_iter().flatten().map(|v| native(v, depth)).collect::<Vec<_>>();
            for doc in [
                psd::decode(&bytes).unwrap(),
                psd::decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
            ] {
                assert!(!doc.read_only, "{:?}", doc.warnings);
                assert_eq!(doc.layers[0].kind, "adjustment");
                assert_eq!(rendered(&doc), expected);
                for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    for bytes in [encoded.clone(), ordinary(&encoded)] {
                        let reopened = psd::decode(&bytes).unwrap();
                        assert!(!reopened.read_only, "{:?}", reopened.warnings);
                        assert_eq!(reopened.layers[0].psd_metadata, doc.layers[0].psd_metadata);
                        assert_eq!(reopened.layers[1].pixels, doc.layers[1].pixels);
                        assert_eq!(rendered(&reopened), expected);
                    }
                }
            }
        }
    }
}

#[test]
fn photoshop_invert_imports_empty_and_canvas_records_and_saves_editable_native_layers() {
    use peerbrush::compatibility::{Representation, StandardLayout};
    for depth in [8, 16] {
        for empty in [false, true] {
            for mask_kind in ["none", "paint", "empty"] {
                let mut invert = Spec::paint("Invert", [[12001, 23003, 34007, 0]; 2]);
                invert.empty_raster = empty;
                invert.flags = 24;
                invert.tags.push((*b"nvrt", vec![]));
                if mask_kind != "none" {
                    invert.mask = Some([0, 65535]);
                    invert.empty_mask = mask_kind == "empty";
                }
                let mut original = SAVED;
                for (i, pixel) in original.iter_mut().enumerate() {
                    if mask_kind != "paint" || i == 1 {
                        for c in &mut pixel[..3] {
                            *c = 65535 - *c;
                        }
                    }
                }
                let base = Spec::paint("Base", original);
                let bytes = fixture(&[invert, base], depth);
                for loaded in [
                    psd::decode(&bytes).unwrap(),
                    psd::decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default())
                        .unwrap(),
                ] {
                    assert!(!loaded.read_only, "{:?}", loaded.warnings);
                    assert_eq!(loaded.layers.len(), 2);
                    let top = &loaded.layers[0];
                    assert_eq!(top.kind, "adjustment");
                    assert_eq!(top.effects[0].kind, "invert");
                    assert_eq!((top.pixels.width, top.pixels.height), (2, 1));
                    if !empty {
                        assert_eq!(
                            top.pixels.get16(0, 0),
                            [12001, 23003, 34007, 0].map(|v| native(v, depth))
                        );
                    }
                    if mask_kind != "none" {
                        assert_eq!(
                            top.mask_value_raw(0, 0),
                            if mask_kind == "paint" { 0. } else { 1. }
                        );
                        assert_eq!(top.mask_value_raw(1, 0), 1.);
                    }
                    let report = peerbrush::compatibility::report(&loaded).unwrap();
                    assert_eq!(report.standard_layout, StandardLayout::LayerTree);
                    assert_eq!(report.layers[0].standard, Representation::Adjustment);
                    assert_eq!(report.standard_layer_count, 2);
                    for saved in [
                        psd::encode(&loaded).unwrap(),
                        psd::encode_psb(&loaded).unwrap(),
                    ] {
                        for without_private in [false, true] {
                            let saved = if without_private {
                                ordinary(&saved)
                            } else {
                                saved.clone()
                            };
                            let reopened = psd::decode(&saved).unwrap();
                            assert!(!reopened.read_only, "{:?}", reopened.warnings);
                            assert_eq!(reopened.bit_depth, depth);
                            assert_eq!(reopened.layers.len(), 2);
                            assert_eq!(reopened.layers[0].kind, "adjustment");
                            assert_eq!(
                                reopened.layers[0].pixels.rgba16(),
                                loaded.layers[0].pixels.rgba16()
                            );
                            assert_eq!(
                                reopened.layers[1].pixels.rgba16(),
                                loaded.layers[1].pixels.rgba16()
                            );
                            assert_eq!(rendered(&reopened), rendered(&loaded));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn malformed_unsupported_and_mismatched_invert_adjustments_remain_protected() {
    for depth in [8, 16] {
        for variant in [
            "payload",
            "duplicate",
            "blend",
            "clipped",
            "folder",
            "composite",
        ] {
            let mut invert = Spec::paint("Protected Invert", [[0; 4]; 2]);
            invert.flags = 24;
            invert.tags.push((*b"nvrt", vec![]));
            match variant {
                "payload" => invert.tags[0].1.push(1),
                "duplicate" => invert.tags.push((*b"nvrt", vec![])),
                "blend" => invert.blend = *b"mul ",
                "clipped" => invert.clipping = 1,
                "folder" => invert.section = Some(1),
                _ => {}
            }
            let loaded =
                psd::decode(&fixture(&[invert, Spec::paint("Base", SAVED)], depth)).unwrap();
            assert!(loaded.read_only, "{depth}/{variant}");
            assert!(loaded.layers[0].effects.is_empty());
            assert_eq!(
                loaded.layers[0].pixels.get16(0, 0),
                SAVED[0].map(|v| native(v, depth))
            );
            assert!(psd::encode(&loaded).is_err());
        }
    }
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
            // Active non-color ranges, excessive channel count and incomplete
            // pairs still require protection.
            for ranges in [
                {
                    let mut r = top.ranges.clone();
                    r[35] = 254;
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
        let before = shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap();
        let document = shared.lock().unwrap().resident_state().expect("resident fixture").document.id.clone();
        let layer = shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].id.clone();
        assert!(!shared.lock().unwrap().resident_state().expect("resident fixture").document.read_only);
        server::dispatch(&shared, "edit", &json!({"actor":"human","document_id":document,"expected_revision":0,"commands":[{"op":"paint.fill","layer":layer,"rect":[0,0,1,1],"color":[233,84,32,255]}]})).unwrap();
        assert_ne!(shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(), before);
        server::dispatch(
            &shared,
            "history",
            &json!({"action":"undo","actor":"human"}),
        )
        .unwrap();
        assert_eq!(shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(), before);
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
        let state = serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
        for panel in ["document_info", "document_sources"] {
            server::dispatch(
                &shared,
                "capture_ui",
                &json!({"path":dir.join("report.png"),"panel":panel}),
            )
            .unwrap();
        }
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
            state
        );
        assert!(server::save(&shared, &protected_path).is_err());
        assert_eq!(std::fs::read(&protected_path).unwrap(), protected_source);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
fn saved_composite(bytes: &[u8], depth: u16) -> Vec<u16> {
    let bytes = ordinary(bytes);
    let (n, header) = if bytes[4..6] == 2u16.to_be_bytes() {
        (
            u64::from_be_bytes(bytes[34..42].try_into().unwrap()) as usize,
            42,
        )
    } else {
        (
            u32::from_be_bytes(bytes[34..38].try_into().unwrap()) as usize,
            38,
        )
    };
    let plane = &bytes[header + n + 2..];
    assert_eq!(&bytes[header + n..header + n + 2], &[0, 0]);
    let words: Vec<u16> = if depth == 16 {
        plane
            .chunks_exact(2)
            .map(|v| u16::from_be_bytes(v.try_into().unwrap()))
            .collect()
    } else {
        plane.iter().map(|v| *v as u16 * 257).collect()
    };
    let count = u32::from_be_bytes(bytes[14..18].try_into().unwrap()) as usize
        * u32::from_be_bytes(bytes[18..22].try_into().unwrap()) as usize;
    assert_eq!(words.len(), count * 4);
    (0..count)
        .flat_map(|x| (0..4).map(move |c| (x, c)))
        .map(|(x, c)| words[c * count + x])
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
        doc.preview(None, doc.width.max(doc.height), None, false)
            .unwrap()
            .2
            .into_iter()
            .map(|v| v as u16 * 257)
            .collect()
    }
}

#[test]
fn photoshop_rgb8_masked_multiply_between_inverts_requires_an_exact_saved_composite() {
    let gray = |v: u16| [v * 257, v * 257, v * 257, 65535];
    let mut invert = Spec::paint("Invert", [[0; 4]; 2]);
    invert.empty_raster = true;
    invert.flags = 24;
    invert.tags.push((*b"nvrt", vec![]));
    let mut multiply = Spec::paint("Masked Multiply", [gray(197); 2]);
    multiply.blend = *b"mul ";
    multiply.opacity = 89;
    multiply.mask = Some([199 * 257, 190 * 257]);
    let base = Spec::paint("Backdrop", [gray(100), gray(129)]);
    let specs = [invert.clone(), multiply, invert, base];
    let make = |expected: [[u16; 4]; 2]| {
        let mut bytes = fixture(&specs, 8);
        bytes.truncate(bytes.len() - 8);
        for channel in 0..4 {
            for pixel in expected {
                composite_sample(&mut bytes, pixel, channel, 8);
            }
        }
        bytes
    };
    let expected = [gray(109), gray(137)];
    let doc = psd::decode(&make(expected)).unwrap();
    assert!(!doc.read_only, "{:?}", doc.warnings);
    assert!(doc.psd_rgb8_multiply);
    assert_eq!(rendered(&doc), expected.concat());
    let report = peerbrush::compatibility::report(&doc).unwrap();
    assert!(report.layers[1].standard_mask);
    assert!(report.losses.is_empty(), "{:?}", report.losses);
    let mut custom_default = doc.clone();
    custom_default.layers[1].mask.as_mut().unwrap().steps[0].value = 200.;
    let report = peerbrush::compatibility::report(&custom_default).unwrap();
    assert!(report.losses.iter().any(|loss|
        loss.code == peerbrush::compatibility::LossCode::MaskStackBaked));
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        for bytes in [ordinary(&saved), saved] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert!(reopened.psd_rgb8_multiply);
            assert_eq!(reopened.layers[1].blend, "multiply");
            assert!(reopened.layers[1].mask.is_some());
            assert_eq!(reopened.layers[1].pixels.rgba16(), [gray(197); 2].concat());
            assert_eq!(rendered(&reopened), expected.concat());
        }
    }
    let legacy = [gray(110), gray(136)];
    let reopened = psd::decode(&make(legacy)).unwrap();
    assert!(!reopened.read_only, "{:?}", reopened.warnings);
    assert!(!reopened.psd_rgb8_multiply);
    assert_eq!(rendered(&reopened), legacy.concat());

    let mut changed = make(expected);
    let offset = changed.len() - 8;
    changed[offset] += 1;
    let protected = psd::decode(&changed).unwrap();
    assert!(protected.read_only);
    assert!(!protected.psd_rgb8_multiply);
    assert_eq!(protected.layers.len(), 1);
}

#[test]
fn photoshop_legacy_brightness_round_trips_rgb8_with_original_samples_and_mean() {
    let base = [[70, 100, 125, 255], [15, 180, 240, 255]].map(|pixel| pixel.map(|v| v * 257));
    let expected = [[33, 93, 143, 255], [0, 253, 255, 255]].map(|pixel| pixel.map(|v| v * 257));
    let source = |depth, lab, mismatch| {
        let mut tone = Spec::paint("Legacy Brightness/Contrast", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"brit", vec![0, 10, 0, 50, 0, 141, lab, 0]));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        let mut merged = expected;
        if mismatch { merged[0][0] += 257; }
        for channel in 0..4 {
            for pixel in merged {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        psd::decode(&bytes).unwrap()
    };
    let doc = source(8, 0, false);
    assert!(!doc.read_only, "{:?}", doc.warnings);
    assert_eq!(doc.layers[0].effects[0].kind, "legacy_brightness");
    assert_eq!(*doc.layers[0].effects[0].settings, json!({"brightness":10,"contrast":50,"mean":141}));
    assert_eq!(doc.layers[1].pixels.rgba16(), base.concat());
    assert_eq!(rendered(&doc), expected.concat());
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        for bytes in [saved.clone(), ordinary(&saved)] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
            assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
            assert_eq!(rendered(&reopened), expected.concat());
        }
    }
    assert!(source(8, 0, true).read_only);
    assert!(source(8, 1, false).read_only);
    assert!(source(16, 0, false).read_only);
}

#[test]
fn photoshop_brightness_descriptor_selects_mode_and_preserves_original_rgb8() {
    let base = [[0, 20, 64, 255], [128, 192, 255, 255]].map(|pixel| pixel.map(|v| v * 257));
    let expected = [[0, 20, 138, 255], [243, 254, 255, 255]].map(|pixel| pixel.map(|v| v * 257));
    let legacy = [[10, 30, 74, 255], [138, 202, 255, 255]].map(|pixel| pixel.map(|v| v * 257));
    let descriptor = |brightness: i32, contrast: i32, legacy, auto| {
        let mut bytes = 16u32.to_be_bytes().to_vec();
        bytes.extend(overlay_descriptor("null", vec![
            ("Vrsn", b"long", 1i32.to_be_bytes().to_vec()),
            ("Brgh", b"long", brightness.to_be_bytes().to_vec()),
            ("Cntr", b"long", contrast.to_be_bytes().to_vec()),
            ("means", b"long", 141i32.to_be_bytes().to_vec()),
            ("Lab ", b"bool", vec![0]),
            ("useLegacy", b"bool", vec![legacy]),
            ("Auto", b"bool", vec![auto]),
        ]));
        bytes
    };
    let source = |depth, tags, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Brightness/Contrast", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags = tags;
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        psd::decode(&bytes).unwrap()
    };
    let zero = (*b"brit", vec![0; 8]);
    let matching = (*b"brit", vec![0, 117, 0, 100, 0, 141, 0, 0]);
    let modern = (*b"CgEd", descriptor(117, 100, 0, 0));
    let doc = source(8, vec![zero.clone(), modern.clone()], expected);
    assert!(!doc.read_only, "{:?}", doc.warnings);
    assert_eq!(doc.layers[0].effects[0].kind, "brightness_contrast");
    assert_eq!(*doc.layers[0].effects[0].settings, json!({"brightness":117,"contrast":100,"mean":141}));
    assert_eq!(doc.layers[1].pixels.rgba16(), base.concat());
    assert_eq!(rendered(&doc), expected.concat());
    let reversed = source(8, vec![modern.clone(), matching], expected);
    assert!(!reversed.read_only, "{:?}", reversed.warnings);
    let explicit_legacy = source(8, vec![zero.clone(), (*b"CgEd", descriptor(10, 0, 1, 0))], legacy);
    assert!(!explicit_legacy.read_only, "{:?}", explicit_legacy.warnings);
    assert_eq!(explicit_legacy.layers[0].effects[0].kind, "legacy_brightness");
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(saved.windows(8).any(|bytes| bytes == b"8BIMCgEd"));
        assert!(saved.windows(20).any(|bytes| bytes == b"8BIMbrit\0\0\0\x08\0\0\0\0\0\0\0\0"));
        for bytes in [saved.clone(), ordinary(&saved)] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(reopened.layers[0].effects[0].kind, "brightness_contrast");
            assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
            assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
            assert_eq!(rendered(&reopened), expected.concat());
        }
    }
    let conflict = (*b"brit", vec![0, 116, 0, 100, 0, 141, 0, 0]);
    assert!(source(8, vec![conflict, modern.clone()], expected).read_only);
    assert!(source(8, vec![zero.clone(), (*b"CgEd", descriptor(117, 100, 0, 1))], expected).read_only);
    assert!(source(8, vec![zero.clone(), modern.clone()], base).read_only);
    assert!(source(16, vec![zero, modern], expected).read_only);
}

#[test]
fn native16_legacy_brightness_keeps_low_words_in_editable_sources_and_composite() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"legacy_brightness","settings":{"brightness":7,"contrast":0}
    })]).unwrap();
    let expected = [original[0] + 7 * 257, original[1] + 7 * 257, original[2] + 7 * 257, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMbrit"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].kind, "legacy_brightness");
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn native16_modern_brightness_keeps_low_words_in_editable_sources_and_composite() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"brightness_contrast","settings":{"brightness":-117,"contrast":39}
    })]).unwrap();
    let expected = [4472, 8846, 17529, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMCgEd"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].kind, "brightness_contrast");
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}
#[test]
fn photoshop_exposure_preserves_binary_settings_and_original_rgb8() {
    let base = [[0, 20, 64, 255], [128, 192, 255, 255]].map(|pixel| pixel.map(|v| v * 257));
    let expected = [[0, 0, 84, 255], [168, 243, 255, 255]].map(|pixel| pixel.map(|v| v * 257));
    let offset = f32::from_bits((-0.015625f32).to_bits() + 1);
    let mut record = [0, 1].to_vec();
    for value in [0.75f32, offset, 1.125] {
        record.extend(value.to_be_bytes());
    }
    let source = |depth, payload: &[u8], merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Exposure", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"expA", payload.to_vec()));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        psd::decode(&bytes).unwrap()
    };
    let doc = source(8, &record, expected);
    assert!(!doc.read_only, "{:?}", doc.warnings);
    assert_eq!(doc.layers[0].effects[0].kind, "exposure");
    let settings = &doc.layers[0].effects[0].settings;
    assert_eq!((settings["offset"].as_f64().unwrap() as f32).to_bits(), offset.to_bits());
    assert_eq!(doc.layers[1].pixels.rgba16(), base.concat());
    assert_eq!(rendered(&doc), expected.concat());
    record.extend([0; 2]);
    let mut standard_tag = b"8BIMexpA\0\0\0\x10".to_vec();
    standard_tag.extend(&record);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(saved.windows(standard_tag.len()).any(|bytes| bytes == standard_tag));
        for bytes in [saved.clone(), ordinary(&saved)] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            let effect = &reopened.layers[0].effects[0];
            assert_eq!(effect.kind, "exposure");
            assert_eq!(effect.settings["exposure"], json!(0.75));
            assert_eq!(effect.settings["gamma"], json!(1.125));
            assert_eq!((effect.settings["offset"].as_f64().unwrap() as f32).to_bits(), offset.to_bits());
            assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
            assert_eq!(rendered(&reopened), expected.concat());
        }
    }
    assert!(source(8, &record, base).read_only);
    assert!(source(16, &record, expected).read_only);
    let mut malformed = record.clone();
    malformed[1] = 2;
    assert!(source(8, &malformed, expected).read_only);
    malformed[1] = 1;
    malformed[15] = 1;
    assert!(source(8, &malformed, expected).read_only);
    for gamma in [0.0f32, f32::NAN] {
        malformed[15] = 0;
        malformed[10..14].copy_from_slice(&gamma.to_be_bytes());
        assert!(source(8, &malformed, expected).read_only);
    }
}

#[test]
fn native16_exposure_preserves_editable_sources_and_native_composite() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"exposure","settings":{"exposure":0.75,"offset":-0.015625,"gamma":1.125}
    })]).unwrap();
    let expected = [15258, 30669, 53043, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMexpA"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].kind, "exposure");
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn photoshop_tone_preset_companions_require_matching_adjustments_and_round_trip() {
    let mut levels = vec![0, 2];
    for index in 0..29 {
        let record = if index < 27 { [0u16, 255, 0, 255, 100] } else { [0; 5] };
        levels.extend(record.into_iter().flat_map(u16::to_be_bytes));
    }
    let curves = b"\0\0\x01\0\0\0\x01\0\x02\0\0\0\0\0\xff\0\xff".to_vec();
    let exposure = [b"\0\x01".as_slice(), &[0; 8], &1f32.to_be_bytes(), &[0; 2]].concat();
    let name = "Épreuve\0";
    let descriptor = |curves| {
        let (kind, file) = if curves {
            ("curvesPresetKind", "curvesPresetFileName")
        } else {
            ("presetKind", "presetFileName")
        };
        let mut bytes = 16u32.to_be_bytes().to_vec();
        bytes.extend(overlay_descriptor("null", vec![
            ("Vrsn", b"long", 1i32.to_be_bytes().to_vec()),
            (kind, b"long", 2i32.to_be_bytes().to_vec()),
            (file, b"TEXT", pattern_text(name)),
        ]));
        bytes
    };
    let source = |depth, tags, mismatch| {
        let base = if depth == 8 {
            [[23, 56, 75, 255], [153, 14, 211, 255]].map(|pixel| pixel.map(|value| value * 257))
        } else {
            SAVED
        };
        let mut tone = Spec::paint("Tone adjustment", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags = tags;
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        let mut merged = base;
        if mismatch { merged[0][0] += if depth == 8 { 257 } else { 1 }; }
        for channel in 0..4 {
            for pixel in merged { composite_sample(&mut bytes, pixel, channel, depth); }
        }
        (psd::decode(&bytes).unwrap(), base)
    };
    for (kind, tag, record, depths) in [
        ("levels", *b"levl", levels, [8, 16].as_slice()),
        ("curves", *b"curv", curves, [8, 16].as_slice()),
        ("exposure", *b"expA", exposure, [8].as_slice()),
    ] {
        let preset = descriptor(kind == "curves");
        for &depth in depths {
            for companion_first in [false, true] {
                let mut tags = vec![(tag, record.clone()), (*b"CgEd", preset.clone())];
                if companion_first { tags.reverse(); }
                let (doc, base) = source(depth, tags.clone(), false);
                assert!(!doc.read_only, "{:?}", doc.warnings);
                assert_eq!(doc.layers[0].effects[0].kind, kind);
                assert_eq!(doc.layers[0].effects[0].settings["preset"], json!({"kind":2,"name":name}));
                assert_eq!(rendered(&doc), base.concat());
                assert!(source(depth, tags, true).0.read_only);
                for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    for bytes in [saved.clone(), ordinary(&saved)] {
                        let reopened = psd::decode(&bytes).unwrap();
                        assert!(!reopened.read_only, "{:?}", reopened.warnings);
                        assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
                        assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                        assert_eq!(rendered(&reopened), base.concat());
                    }
                }
            }
            let wrong = descriptor(kind != "curves");
            assert!(source(depth, vec![(tag, record.clone()), (*b"CgEd", wrong)], false).0.read_only);
            assert!(source(depth, vec![(*b"CgEd", preset.clone())], false).0.read_only);
            let mut malformed = preset.clone();
            malformed.push(1);
            assert!(source(depth, vec![(tag, record.clone()), (*b"CgEd", malformed)], false).0.read_only);
        }
    }
}

#[test]
fn photoshop_channel_mixer_keeps_fixed_point_color_and_monochrome_settings() {
    let base = [[153, 64, 197, 255], [31, 200, 80, 255]].map(|pixel| pixel.map(|v| v * 257));
    let rgb = [[30, 28, 173, 255], [6, 255, 25, 255]].map(|pixel| pixel.map(|v| v * 257));
    let mono = [[156, 156, 156, 255], [121, 121, 121, 255]].map(|pixel| pixel.map(|v| v * 257));
    let red = [20i16, 0, 0, 0];
    let green = [-71, 140, 15, 7];
    let blue = [50, 0, 75, -20];
    let gray = [33, 33, 34, 7];
    let record = |monochrome| {
        let mut bytes = [0, 1, 0, u8::from(monochrome)].to_vec();
        let rows = if monochrome { [gray, [0; 4], [0; 4]] } else { [red, green, blue] };
        for [r, g, b, bias] in rows {
            for field in [r, g, b, 0, bias] {
                bytes.extend(field.to_be_bytes());
            }
        }
        bytes.extend([0; 10]);
        bytes
    };
    let preset = "Épreuve\0";
    let mut descriptor = 16u32.to_be_bytes().to_vec();
    descriptor.extend(overlay_descriptor("null", vec![
        ("Vrsn", b"long", 1i32.to_be_bytes().to_vec()),
        ("mixerPresetKind", b"long", 2i32.to_be_bytes().to_vec()),
        ("mixerPresetFileName", b"TEXT", pattern_text(preset)),
    ]));
    let source = |depth, tags, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Channel Mixer", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags = tags;
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        psd::decode(&bytes).unwrap()
    };
    for (monochrome, expected) in [(false, rgb), (true, mono)] {
        let mixer = (*b"mixr", record(monochrome));
        let companion = (*b"CgEd", descriptor.clone());
        let tags = if monochrome { vec![mixer, companion] } else { vec![companion, mixer] };
        let doc = source(8, tags, expected);
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let effect = &doc.layers[0].effects[0];
        assert_eq!(effect.kind, "channel_mixer");
        assert_eq!(effect.settings["monochrome"], json!(monochrome));
        assert_eq!(effect.settings["preset"], json!({"kind":2,"name":preset}));
        if monochrome {
            assert_eq!(effect.settings["gray"], json!(gray));
        } else {
            assert_eq!(effect.settings["red"], json!(red));
            assert_eq!(effect.settings["green"], json!(green));
            assert_eq!(effect.settings["blue"], json!(blue));
        }
        assert_eq!(rendered(&doc), expected.concat());
        let mut tag = b"8BIMmixr\0\0\0\x2c".to_vec();
        tag.extend(record(monochrome));
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert!(saved.windows(tag.len()).any(|bytes| bytes == tag));
            assert!(saved.windows(8).any(|bytes| bytes == b"8BIMCgEd"));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
    }
    assert!(source(8, vec![(*b"CgEd", descriptor)], base).read_only);
    assert!(source(8, vec![(*b"mixr", record(false))], base).read_only);
    assert!(source(16, vec![(*b"mixr", record(true))], mono).read_only);
    let mut invalid = record(false);
    invalid[11] = 1;
    assert!(source(8, vec![(*b"mixr", invalid)], rgb).read_only);
    let mut invalid = record(true);
    invalid[15] = 1;
    assert!(source(8, vec![(*b"mixr", invalid)], mono).read_only);
}

#[test]
fn native16_channel_mixer_retains_native_channels_and_inactive_mix_settings() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"channel_mixer","settings":{
            "red":[20,0,0,0],"green":[-71,140,15,7],"blue":[50,0,75,-20],"gray":[33,33,34,7]
        }
    })]).unwrap();
    let expected = [2460, 34158, 24072, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMmixr"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].settings["gray"], json!([33,33,34,7]));
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn photoshop_black_white_preserves_weights_tint_and_preset() {
    let base = [[153, 64, 197, 255], [31, 200, 80, 255]].map(|pixel| pixel.map(|v| v * 257));
    let gray = [[144, 144, 144, 255], [108, 108, 108, 255]].map(|pixel| pixel.map(|v| v * 257));
    let tinted = [[255, 103, 65, 255], [232, 62, 20, 255]].map(|pixel| pixel.map(|v| v * 257));
    let tint = [240.0000003f64, 70., 28.];
    let preset = "Épreuve\0";
    let descriptor = |use_tint| {
        let rgb = overlay_descriptor("RGBC", ["Rd  ", "Grn ", "Bl  "].into_iter().zip(tint)
            .map(|(key, value)| (key, b"doub", value.to_be_bytes().to_vec())).collect());
        let mut properties: Vec<_> = ["Rd  ", "Yllw", "Grn ", "Cyn ", "Bl  ", "Mgnt"]
            .into_iter().zip([40i32, 60, 40, 60, 20, 80])
            .map(|(key, value)| (key, b"long", value.to_be_bytes().to_vec())).collect();
        properties.extend([
            ("useTint", b"bool", vec![u8::from(use_tint)]),
            ("tintColor", b"Objc", rgb),
            ("bwPresetKind", b"long", 2i32.to_be_bytes().to_vec()),
            ("blackAndWhitePresetFileName", b"TEXT", pattern_text(preset)),
        ]);
        let mut payload = 16u32.to_be_bytes().to_vec();
        payload.extend(overlay_descriptor("null", properties));
        payload
    };
    let source = |depth, payload, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Black & White", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"blwh", payload));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        psd::decode(&bytes).unwrap()
    };
    for (use_tint, expected) in [(false, gray), (true, tinted)] {
        let doc = source(8, descriptor(use_tint), expected);
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let effect = &doc.layers[0].effects[0];
        assert_eq!(effect.kind, "black_white");
        assert_eq!(effect.settings["reds"], json!(40));
        assert_eq!(effect.settings["magentas"], json!(80));
        assert_eq!(effect.settings["use_tint"], json!(use_tint));
        assert_eq!(effect.settings["tint_color"], json!(tint));
        assert_eq!(effect.settings["preset"], json!({"kind":2,"name":preset}));
        assert_eq!(rendered(&doc), expected.concat());
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert!(saved.windows(8).any(|bytes| bytes == b"8BIMblwh"));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
    }
    let mut invalid = descriptor(true);
    let class = invalid.windows(4).position(|key| key == b"RGBC").unwrap();
    invalid[class..class + 4].copy_from_slice(b"CMYC");
    assert!(source(8, invalid, tinted).read_only);
    assert!(source(8, descriptor(true), gray).read_only);
    assert!(source(16, descriptor(false), gray).read_only);
}

#[test]
fn native16_black_white_tint_preserves_editable_sources_and_native_composite() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"black_white","settings":{"use_tint":true,"tint_color":[240.0000003,70,28]}
    })]).unwrap();
    let expected = [53734, 10645, 0, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMblwh"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn photoshop_selective_color_round_trips_both_methods_and_all_nine_ranges() {
    let base = [[192, 200, 149, 255], [71, 41, 105, 255]].map(|pixel| pixel.map(|v| v * 257));
    let relative = [[193, 210, 109, 255], [66, 69, 91, 255]].map(|pixel| pixel.map(|v| v * 257));
    let absolute = [[177, 233, 55, 255], [64, 76, 82, 255]].map(|pixel| pixel.map(|v| v * 257));
    let names = ["reds", "yellows", "greens", "cyans", "blues", "magentas", "whites", "neutrals", "blacks"];
    let corrections = [
        [-25i16, 15, -5, 12], [10, -25, 50, 8], [25, -50, 15, 9],
        [-45, 30, -10, -11], [35, 20, 7, -8], [-15, 32, -17, 12],
        [-75, 10, 30, 15], [15, -25, 35, -5], [-15, 18, -20, -17],
    ];
    let record = |method| {
        let mut payload = [0, 1, 0, method].to_vec();
        payload.extend([0; 8]);
        for value in corrections.into_iter().flatten() {
            payload.extend(value.to_be_bytes());
        }
        payload
    };
    let source = |depth, payload, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Selective Color", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"selc", payload));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        psd::decode(&bytes).unwrap()
    };
    for (method, name, expected) in [(0, "relative", relative), (1, "absolute", absolute)] {
        let doc = source(8, record(method), expected);
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let effect = &doc.layers[0].effects[0];
        assert_eq!(effect.kind, "selective_color");
        assert_eq!(effect.settings["method"], json!(name));
        for (range, row) in names.into_iter().zip(corrections) {
            assert_eq!(effect.settings[range], json!(row));
        }
        assert_eq!(rendered(&doc), expected.concat());
        let mut tag = b"8BIMselc\0\0\0\x54".to_vec();
        tag.extend(record(method));
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert!(saved.windows(tag.len()).any(|bytes| bytes == tag));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
    }
    assert!(source(8, record(0), absolute).read_only);
    assert!(source(8, record(2), relative).read_only);
    assert!(source(16, record(1), absolute).read_only);
    let mut invalid = record(0);
    invalid[11] = 1;
    assert!(source(8, invalid, relative).read_only);
    let mut invalid = record(0);
    invalid[12..14].copy_from_slice(&101i16.to_be_bytes());
    assert!(source(8, invalid, relative).read_only);
}

#[test]
fn native16_selective_color_preserves_source_words_and_changes_pure_black() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    base.layers[0].pixels.set16(1, 0, [0, 0, 0, 65535]);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"selective_color","settings":{
            "method":"absolute","cyans":[-45,30,-10,-11],"blues":[35,20,7,-8],
            "neutrals":[15,-25,35,-5],"blacks":[-15,18,-20,-17]
        }
    })]).unwrap();
    let expected = [10970, 29970, 33339, 65535, 19300, 1350, 22020, 65535];
    assert_eq!(rendered(&doc), expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMselc"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(rendered(&reopened), expected);
        assert_eq!(rendered(&psd::decode(&ordinary(&saved)).unwrap()), expected);
    }
}

#[test]
fn photoshop_legacy_vibrance_round_trips_and_warns_about_receiving_render() {
    let base = [[192, 200, 149, 255], [71, 41, 105, 255]].map(|pixel| pixel.map(|v| v * 257));
    let boosted = [[193, 200, 154, 255], [68, 39, 100, 255]].map(|pixel| pixel.map(|v| v * 257));
    let saturation = [[193, 200, 158, 255], [68, 43, 99, 255]].map(|pixel| pixel.map(|v| v * 257));
    let descriptor = |vibrance: i32, legacy, temperature| {
        let mut properties = vec![
            ("vibrance", b"long", vibrance.to_be_bytes().to_vec()),
            ("Strt", b"long", (-15i32).to_be_bytes().to_vec()),
        ];
        if let Some(legacy) = legacy {
            properties.push(("useLegacy", b"bool", vec![legacy]));
        }
        if temperature {
            properties.push(("temperature", b"long", 1i32.to_be_bytes().to_vec()));
        }
        let mut payload = 16u32.to_be_bytes().to_vec();
        payload.extend(overlay_descriptor("null", properties));
        payload
    };
    let source = |depth, payload, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Legacy Vibrance", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"vibA", payload));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        psd::decode(&bytes).unwrap()
    };
    for (vibrance, expected) in [(0, saturation), (25, boosted)] {
        let doc = source(8, descriptor(vibrance, Some(1), false), expected);
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let effect = &doc.layers[0].effects[0];
        assert_eq!(effect.kind, "vibrance");
        assert_eq!(*effect.settings, json!({"vibrance":vibrance,"saturation":-15}));
        assert_eq!(rendered(&doc), expected.concat());
        let report = peerbrush::compatibility::report(&doc).unwrap();
        assert_eq!(report.losses.iter().any(|loss| matches!(loss.code,
            peerbrush::compatibility::LossCode::AdjustmentRenderingApproximate)), vibrance != 0);
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert!(saved.windows(8).any(|bytes| bytes == b"8BIMvibA"));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
    }
    for payload in [descriptor(25, Some(0), false), descriptor(25, None, true)] {
        let doc = source(8, payload, boosted);
        assert!(doc.read_only);
        assert!(doc.warnings.iter().any(|warning| warning.contains("newer rendering model")));
    }
    assert!(source(8, descriptor(101, None, false), boosted).read_only);
    assert!(source(8, descriptor(25, None, false), base).read_only);
    assert!(source(16, descriptor(25, None, false), boosted).read_only);
}

#[test]
fn native16_legacy_vibrance_preserves_editable_sources_and_native_composite() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"vibrance","settings":{"vibrance":-75,"saturation":39}
    })]).unwrap();
    let expected = [21354, 28571, 44412, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMvibA"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn photoshop_photo_filter_preserves_lab_rgb_and_xyz_wire_colors() {
    let base = [[192, 200, 149, 255], [71, 41, 105, 255]].map(|pixel| pixel.map(|v| v * 257));
    let source = |depth, payload, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Photo Filter", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"phfl", payload));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        psd::decode(&bytes).unwrap()
    };
    for (space, color, density, preserve, merged) in [
        ("lab", [6500i32, -1700, -3400], 43u32, true, [[182, 201, 168, 255], [61, 45, 106, 255]]),
        ("rgb", [0x1234, 0x9abc, 0xfedc], 63, false, [[131, 155, 152, 255], [31, 32, 102, 255]]),
        ("xyz", [12288, 14336, 4096], 37, true, [[191, 202, 143, 255], [71, 43, 93, 255]]),
    ] {
        let mut payload = if space == "xyz" { vec![0, 3] }
            else { vec![0, 2, 0, if space == "lab" { 7 } else { 0 }] };
        for channel in color {
            if space == "xyz" {
                payload.extend((channel as u32).to_be_bytes());
            } else {
                payload.extend((channel as u16).to_be_bytes());
            }
        }
        if space != "xyz" {
            payload.extend([0; 2]);
        }
        payload.extend(density.to_be_bytes());
        payload.push(u8::from(preserve));
        let expected = merged.map(|pixel| pixel.map(|v| v * 257));
        let doc = source(8, payload.clone(), expected);
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let effect = &doc.layers[0].effects[0];
        assert_eq!(effect.kind, "photo_filter");
        assert_eq!(*effect.settings, json!({"color_space":space,"color":color,"density":density,"preserve_luminosity":preserve}));
        assert_eq!(rendered(&doc), expected.concat());
        let report = peerbrush::compatibility::report(&doc).unwrap();
        assert_eq!(report.losses.iter().any(|loss| matches!(loss.code,
            peerbrush::compatibility::LossCode::AdjustmentRenderingApproximate)), preserve);
        let mut standard_tag = b"8BIMphfl\0\0\0\x14".to_vec();
        payload.resize(20, 0);
        standard_tag.extend(&payload);
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert!(saved.windows(standard_tag.len()).any(|bytes| bytes == standard_tag));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
        assert!(source(8, payload.clone(), base).read_only);
        assert!(source(16, payload.clone(), expected).read_only);
        let mut invalid = payload.clone();
        invalid[19] = 1;
        assert!(source(8, invalid, expected).read_only);
        let mut invalid = payload;
        invalid[if space == "xyz" { 18 } else { 16 }] = 2;
        assert!(source(8, invalid, expected).read_only);
    }
}

#[test]
fn native16_photo_filter_preserves_editable_sources_and_native_composite() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"photo_filter","settings":{
            "color_space":"xyz","color":[12288,14336,4096],"density":37,"preserve_luminosity":true
        }
    })]).unwrap();
    let expected = [14736, 22668, 36641, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMphfl"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn photoshop_gradient_map_round_trips_methods_and_preserves_stored_opacity() {
    let base = [[64, 64, 64, 255], [192, 192, 192, 255]].map(|pixel| pixel.map(|v| v * 257));
    let name = "Dégradé";
    let record = |method: Option<&[u8; 4]>, reverse: bool| {
        let mut bytes = if method.is_some() { 3u16 } else { 1u16 }.to_be_bytes().to_vec();
        bytes.extend([u8::from(reverse), 0]);
        if let Some(method) = method { bytes.extend(method); }
        bytes.extend(pattern_text(&format!("{name}\0")));
        bytes.extend(2u16.to_be_bytes());
        for (position, midpoint, sample) in [(0u32, 33u32, 0u16), (4096, 70, 65535)] {
            bytes.extend(position.to_be_bytes());
            bytes.extend(midpoint.to_be_bytes());
            bytes.extend([0; 2]);
            for _ in 0..3 { bytes.extend(sample.to_be_bytes()); }
            bytes.extend([0; 4]);
        }
        bytes.extend(2u16.to_be_bytes());
        for (position, midpoint, opacity) in [(0u32, 50u32, 31u16), (4096, 80, 172)] {
            bytes.extend(position.to_be_bytes());
            bytes.extend(midpoint.to_be_bytes());
            bytes.extend(opacity.to_be_bytes());
        }
        bytes.extend(2u16.to_be_bytes());
        bytes.extend(4096u16.to_be_bytes());
        bytes.extend([0, 32, 0, 0]);
        bytes.extend([0; 32]);
        bytes
    };
    let source = |depth, payload, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Gradient Map", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"grdm", payload));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged { composite_sample(&mut bytes, pixel, channel, depth); }
        }
        psd::decode(&bytes).unwrap()
    };
    for (method, key, reverse, result) in [
        ("classic", None, true, [153u16, 33]),
        ("linear", Some(b"Lnr "), false, [102, 205]),
        ("perceptual", Some(b"Perc"), true, [129, 7]),
    ] {
        let expected = result.map(|value| [value * 257, value * 257, value * 257, 65535]);
        let payload = record(key, reverse);
        let doc = source(8, payload.clone(), expected);
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let effect = &doc.layers[0].effects[0];
        assert_eq!(effect.kind, "gradient_map");
        assert_eq!(effect.settings["name"], json!(name));
        assert_eq!(effect.settings["method"], json!(method));
        assert_eq!(effect.settings["reverse"], json!(reverse));
        assert_eq!(effect.settings["color_stops"][0]["midpoint"], json!(0.33));
        assert_eq!(effect.settings["color_stops"][1]["midpoint"], json!(0.70));
        assert_eq!(effect.settings["alpha_stops"][0]["opacity"], json!(31. / 255.));
        assert_eq!(effect.settings["alpha_stops"][1]["opacity"], json!(172. / 255.));
        assert_eq!(rendered(&doc), expected.concat());
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert!(saved.windows(8).any(|bytes| bytes == b"8BIMgrdm"));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
        assert!(source(8, payload.clone(), base).read_only);
        assert!(source(16, payload.clone(), expected).read_only);
        let mut noise = payload.clone();
        let mode = noise.len() - 33;
        noise[mode] = 1;
        assert!(source(8, noise, expected).read_only);
        let mut dithered = payload;
        dithered[3] = 1;
        assert!(source(8, dithered, expected).read_only);
    }
}

#[test]
fn native16_gradient_map_uses_continuous_luma_and_native_stop_words() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [12347, 23019, 41273, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"gradient_map","settings":{"smoothness":0.,"color_stops":[
            {"position":0.,"color":[1000./65535.,2000./65535.,3000./65535.]},
            {"position":1.,"color":[60000./65535.,51000./65535.,42000./65535.]}
        ]}
    })]).unwrap();
    let expected = [20649, 18319, 15988, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMgrdm"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn photoshop_color_balance_round_trips_signed_bands_and_both_luminosity_modes() {
    let base = [[5, 132, 103, 255], [180, 220, 15, 255]].map(|pixel| pixel.map(|v| v * 257));
    let plain = [[40, 36, 124, 255], [236, 194, 14, 255]].map(|pixel| pixel.map(|v| v * 257));
    let preserved = [[23, 1, 54, 255], [236, 213, 0, 255]].map(|pixel| pixel.map(|v| v * 257));
    let bands = [[75i16, -51, -9], [100, -100, 52], [31, 13, -13]];
    let record = |preserve| {
        let mut bytes = Vec::new();
        for value in bands.into_iter().flatten() { bytes.extend(value.to_be_bytes()); }
        bytes.push(u8::from(preserve));
        bytes
    };
    let source = |depth, payload, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Photoshop Color Balance", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"blnc", payload));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged { composite_sample(&mut bytes, pixel, channel, depth); }
        }
        psd::decode(&bytes).unwrap()
    };
    for (preserve, expected) in [(false, plain), (true, preserved)] {
        let doc = source(8, record(preserve), expected);
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let effect = &doc.layers[0].effects[0];
        assert_eq!(effect.kind, "photoshop_color_balance");
        assert_eq!(*effect.settings, json!({"shadows":bands[0],"midtones":bands[1],
            "highlights":bands[2],"preserve_luminosity":preserve}));
        assert_eq!(rendered(&doc), expected.concat());
        let mut tag = b"8BIMblnc\0\0\0\x14".to_vec();
        tag.extend(record(preserve));
        tag.push(0);
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert!(saved.windows(tag.len()).any(|bytes| bytes == tag));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
    }
    assert!(source(8, record(true), plain).read_only);
    assert!(source(16, record(true), preserved).read_only);
    let mut bad_axis = record(true);
    bad_axis[..2].copy_from_slice(&101i16.to_be_bytes());
    assert!(source(8, bad_axis, preserved).read_only);
    let mut bad_flag = record(true);
    bad_flag[18] = 2;
    assert!(source(8, bad_flag, preserved).read_only);
    let mut bad_pad = record(true);
    bad_pad.push(1);
    assert!(source(8, bad_pad, preserved).read_only);
}

#[test]
fn native16_photoshop_color_balance_preserves_original_low_words() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [1375, 45653, 24671, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"photoshop_color_balance","settings":{
            "shadows":[75,-51,-9],"midtones":[100,-100,52],"highlights":[31,13,-13],
            "preserve_luminosity":true
        }
    })]).unwrap();
    let expected = [6317, 16472, 10214, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMblnc"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn photoshop_hue_saturation_round_trips_ranges_colorize_and_trailer() {
    let base = [[220, 20, 20, 255], [20, 20, 220, 255]].map(|pixel| pixel.map(|v| v * 257));
    let banded = [[20, 220, 220, 255], [220, 20, 20, 255]].map(|pixel| pixel.map(|v| v * 257));
    let colorized = [0, 240, 0, 255].map(|v| v * 257);
    let record = |colorize: bool, trailer: bool| {
        let mut bytes = vec![0, 2, u8::from(colorize), 0];
        for value in [120i16, 100, 0, 120, 0, 0] { bytes.extend(value.to_be_bytes()); }
        for band in [
            [315i16, 345, 15, 45, 60, 0, 0], [15, 45, 75, 105, 0, 0, 0],
            [75, 105, 135, 165, 0, 0, 0], [135, 165, 195, 225, 0, 0, 0],
            [195, 225, 255, 285, 0, 0, 0], [255, 285, 315, 345, 0, 0, 0],
        ] {
            for value in band { bytes.extend(value.to_be_bytes()); }
        }
        if trailer {
            for band in 0i16..6 {
                for value in [band * 60, 100, 50] { bytes.extend(value.to_be_bytes()); }
            }
        }
        bytes
    };
    let source = |depth, payload, merged: [[u16; 4]; 2]| {
        let mut tone = Spec::paint("Photoshop Hue/Saturation", [[0; 4]; 2]);
        tone.flags = 24;
        tone.empty_raster = true;
        tone.tags.push((*b"hue2", payload));
        let mut bytes = fixture(&[tone, Spec::paint("Original", base)], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in merged { composite_sample(&mut bytes, pixel, channel, depth); }
        }
        psd::decode(&bytes).unwrap()
    };
    for (colorize, trailer, expected) in [(false, true, banded), (true, false, [colorized; 2])] {
        let record = record(colorize, trailer);
        let doc = source(8, record.clone(), expected);
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let effect = &doc.layers[0].effects[0];
        assert_eq!(effect.kind, "photoshop_hue_saturation");
        assert_eq!(effect.settings["colorize"], colorize);
        assert_eq!(effect.settings["bands"][0]["change"], json!([60, 0, 0]));
        assert_eq!(effect.settings["trailer"].as_array().unwrap().len(), record.len() - 100);
        assert_eq!(rendered(&doc), expected.concat());
        let mut tag = b"8BIMhue2".to_vec();
        tag.extend((record.len() as u32).to_be_bytes());
        tag.extend(record);
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert!(saved.windows(tag.len()).any(|bytes| bytes == tag));
            for bytes in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert_eq!(reopened.layers[1].pixels.rgba16(), base.concat());
                assert_eq!(rendered(&reopened), expected.concat());
            }
        }
    }
    let mut wrong_composite = banded;
    wrong_composite[0][0] += 257;
    assert!(source(8, record(false, true), wrong_composite).read_only);
    assert!(source(16, record(false, true), banded).read_only);
    let mut bad_range = record(false, true);
    bad_range[16..18].copy_from_slice(&361i16.to_be_bytes());
    assert!(source(8, bad_range, banded).read_only);
    let mut bad_flag = record(false, true);
    bad_flag[2] = 2;
    assert!(source(8, bad_flag, banded).read_only);
}

#[test]
fn native16_photoshop_hue_saturation_keeps_low_words_in_rotated_color() {
    let mut base = Document::new_depth(2, 1, 16).unwrap();
    let original = [45653, 24671, 1375, 65535];
    base.layers[0].pixels.set16(0, 0, original);
    let source = base.layers[0].id.clone();
    let doc = Engine::preview_edits(base, &[json!({
        "op":"adjustment.add","kind":"photoshop_hue_saturation","settings":{"master":[120,0,0]}
    })]).unwrap();
    let expected = [1375, 45653, 24671, 65535];
    assert_eq!(&rendered(&doc)[..4], &expected);
    for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
        assert!(!saved.windows(8).any(|bytes| bytes == b"8BIMhue2"));
        let reopened = psd::decode(&saved).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.bit_depth, 16);
        assert_eq!(reopened.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
        assert_eq!(reopened.layers.iter().find(|layer| layer.id == source).unwrap().pixels.get16(0, 0), original);
        assert_eq!(&rendered(&reopened)[..4], &expected);
        assert_eq!(&rendered(&psd::decode(&ordinary(&saved)).unwrap())[..4], &expected);
    }
}

#[test]
fn supplementary_sources_cannot_override_changed_standard_canvas_dimensions() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(2, 4, depth).unwrap();
        doc.layers[0]
            .pixels
            .set16(0, 0, [12347, 23459, 34571, 65535]);
        doc.layers[0]
            .pixels
            .set16(1, 3, [54321, 32123, 11223, 65535]);
        let original = doc.layers[0].pixels.rgba16();
        for mut bytes in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let editable = psd::decode(&bytes).unwrap();
            assert!(!editable.read_only);
            assert_eq!(editable.layers[0].pixels.rgba16(), original);
            // Same number of samples: neither planar hashes nor unchanged layer
            // records can identify this externally changed standard header.
            bytes[14..18].copy_from_slice(&2u32.to_be_bytes());
            bytes[18..22].copy_from_slice(&4u32.to_be_bytes());
            let protected = psd::decode(&bytes).unwrap();
            assert!(protected.read_only);
            assert_eq!(
                (protected.width, protected.height, protected.bit_depth),
                (4, 2, depth)
            );
            assert!(protected
                .warnings
                .iter()
                .any(|warning| warning.starts_with("PeerBrush definitions were changed")));
            assert!(psd::encode(&protected).is_err());
            assert!(psd::encode_psb(&protected).is_err());
        }
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
    engine.resident_state_mut().expect("resident fixture").document = psd::decode(&fixture(&stack(), 16)).unwrap();
    let before = rendered(&engine.resident_state().expect("resident fixture").document);
    let top = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
    let base = engine.resident_state().expect("resident fixture").document.layers[2].id.clone();
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
    assert_eq!(rendered(&engine.resident_state().expect("resident fixture").document), before);
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
    assert_ne!(rendered(&engine.resident_state().expect("resident fixture").document), before);
    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
    let state = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
    assert!(engine
        .edit(
            "human",
            &[json!({"op":"layer.delete","layer":base})],
            None,
            None,
            "Keep base"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), state);
    engine.undo("human").unwrap();
    assert_eq!(rendered(&engine.resident_state().expect("resident fixture").document), before);
    engine.redo("human").unwrap();
    let saved = psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap();
    assert_eq!(saved_composite(&saved, 16), rendered(&engine.resident_state().expect("resident fixture").document));
    assert_eq!(
        rendered(&psd::decode(&saved).unwrap()),
        rendered(&engine.resident_state().expect("resident fixture").document)
    );
}

#[test]
fn standard_clipped_layers_keep_their_baked_effect_pixels_and_editable_mask_channels() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = psd::decode(&fixture(&stack(), depth)).unwrap();
        let layer = engine.resident_state().expect("resident fixture").document.layers[1].id.clone();
        let source = engine.resident_state().expect("resident fixture").document.layers[1].pixels.rgba16();
        engine
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"invert"})],
                None,
                None,
                "Clip effect",
            )
            .unwrap();
        let saved = psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap();
        assert_eq!(saved_composite(&saved, depth), rendered(&engine.resident_state().expect("resident fixture").document));
        let standard = psd::decode(&ordinary(&saved)).unwrap();
        assert!(!standard.read_only, "{:?}", standard.warnings);
        assert_eq!(standard.layers.len(), 4);
        assert_eq!(rendered(&standard), rendered(&engine.resident_state().expect("resident fixture").document));
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
    engine.resident_state_mut().expect("resident fixture").document = psd::decode(&fixture(specs, depth)).unwrap();
    assert!(
        engine.resident_state().expect("resident fixture").document.read_only,
        "Expected {reason}, got {:?}",
        engine.resident_state().expect("resident fixture").document.warnings
    );
    assert!(
        engine.resident_state().expect("resident fixture").document.warnings.iter().any(|v| v.contains(reason)),
        "{:?}",
        engine.resident_state().expect("resident fixture").document.warnings
    );
    assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
    assert_eq!(
        engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
        SAVED
            .concat()
            .into_iter()
            .map(|v| native(v, depth))
            .collect::<Vec<_>>()
    );
    assert!(psd::encode(&engine.resident_state().expect("resident fixture").document).is_err());
    assert!(engine
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":engine.resident_state().expect("resident fixture").document.layers[0].id,"opacity":0.5})],
            None,
            None,
            "Protected"
        )
        .is_err());
    assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
}

#[test]
fn unsupported_and_malformed_blending_controls_never_become_editable() {
    let cases = [
        (*b"clbl", vec![1, 0, 1, 0], "clbl"),
        (*b"knko", vec![1, 0, 0, 0], "knko"),
        (*b"knko", vec![], "knko"),
        (*b"iOpa", vec![127, 0], "iOpa"),
        (*b"iOpa", vec![255, 1, 0, 0], "iOpa"),
        (*b"lspf", 1u32.to_be_bytes().to_vec(), "lspf"),
        (*b"lspf", vec![0], "lspf"),
        (*b"infx", vec![2, 0, 0, 0], "layer flag"),
        (*b"sn2P", vec![], "layer flag"),
        (*b"sn2P", vec![0, 0, 0, 2], "layer flag"),
    ];
    for depth in [8, 16] {
        for (key, data, reason) in &cases {
            let mut specs = stack();
            specs[2].tags = vec![(*key, data.clone())];
            protected(&specs, depth, reason);
        }
        let mut specs = stack();
        specs[2].tags = vec![(*b"clbl", vec![0; 4])];
        protected(&specs, depth, "Blend Clipped Layers As Group");
        let mut specs = stack();
        specs[2].ranges = vec![0, 0, 254, 255, 0, 0, 255, 255];
        let mut unsupported = specs.clone();
        unsupported[2].blend = *b"lddg";
        unsupported[2].tags.push((*b"iOpa", vec![128, 0, 0, 0]));
        protected(&unsupported, depth, "Blend If");
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
fn disabled_clipping_flag_round_trips_on_a_plain_normal_base() {
    for depth in [8, 16] {
        let mut top = Spec::paint("Clipped screen", [[65535; 4]; 2]);
        top.clipping = 1;
        top.blend = *b"scrn";
        let mut base = Spec::paint("Normal base", [[0, 0, 0, 65535], [0; 4]]);
        base.opacity = 128;
        base.tags.push((*b"clbl", vec![0; 4]));
        let backdrop = Spec::paint("Backdrop", [[0, 0, 0, 65535]; 2]);
        let mut bytes = fixture(&[top, base, backdrop], depth);
        let expected = [[128 * 257, 128 * 257, 128 * 257, 65535], [0, 0, 0, 65535]];
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for c in 0..4 {
            for pixel in expected {
                sample(&mut bytes, pixel[c], depth);
            }
        }
        let doc = psd::decode(&bytes).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(rendered(&doc), expected.concat());
        let flag = psd::LayerMetadata { key: "clbl".into(), data: vec![0; 4] };
        assert!(doc.layers[1].psd_metadata.contains(&flag));
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let wire = b"8BIMclbl\0\0\0\x04\0\0\0\0";
            assert_eq!(saved.windows(wire.len()).filter(|bytes| *bytes == wire).count(), 1);
            for candidate in [saved.clone(), ordinary(&saved)] {
                let reopened = psd::decode(&candidate).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(rendered(&reopened), expected.concat());
                assert!(reopened.layers[1].psd_metadata.contains(&flag));
            }
        }
        let id = doc.layers[1].id.clone();
        let mut engine = Engine::new();
        engine.resident_state_mut().unwrap().document = doc;
        assert!(engine.edit("human", &[json!({"op":"layer.update","layer":id,"blend":"multiply"})],
            None, None, "Change base blend").is_err());
        engine.edit("human", &[json!({"op":"layer.update","layer":id,
            "blend_clipped_as_group":true,"blend":"multiply"})], None, None, "Group clipping").unwrap();
        engine.undo("human").unwrap();
        assert!(engine.resident_state().unwrap().document.layers[1].psd_metadata.contains(&flag));
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
        specs[3].tags.push((*b"sn2P", vec![0; 4]));
        let doc = psd::decode(&fixture(&specs, depth)).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(rendered(&doc), expected(&specs, depth, false));

        specs[2].tags.iter_mut().find(|(key, _)| key == b"infx").unwrap().1[0] = 0;
        let doc = psd::decode(&fixture(&specs, depth)).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            for bytes in [ordinary(&saved), saved] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert!(reopened.layers[2].psd_metadata.iter().any(|entry| entry.key == "infx" && entry.data == [0; 4]));
                for (index, value) in [(2, 1), (3, 0)] {
                    assert!(reopened.layers[index].psd_metadata.iter()
                        .any(|entry| entry.key == "sn2P" && entry.data == [0, 0, 0, value]));
                }
                assert_eq!(rendered(&reopened), expected(&specs, depth, false));
            }
        }
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
fn color_overlay_descriptor(enabled: bool, color: [f64; 3]) -> Vec<u8> {
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
    overlay_descriptor(
        "SoFi",
        vec![
            ("enab", b"bool", vec![u8::from(enabled)]),
            ("present", b"bool", vec![1]),
            ("showInDialog", b"bool", vec![1]),
            ("Md  ", b"enum", mode),
            ("Opct", b"UntF", percent),
            ("Clr ", b"Objc", rgb),
        ],
    )
}
fn overlay_payload(master: bool, enabled: bool, color: [f64; 3]) -> Vec<u8> {
    let root = overlay_descriptor(
        "Lefx",
        vec![
            ("masterFXSwitch", b"bool", vec![u8::from(master)]),
            ("Scl ", b"UntF", [b"#Prc".to_vec(), 100f64.to_be_bytes().to_vec()].concat()),
            ("SoFi", b"Objc", color_overlay_descriptor(enabled, color)),
        ],
    );
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        root,
    ]
    .concat()
}
const FLOAT_COLOR: [f64; 3] = [0.13719, 0.41937, 0.73351];
const FLOAT_COLOR2: [f64; 3] = [0.95431, 0.61273, 0.16789];
fn normalized_style_fixture(
    depth: u16,
    family: &str,
    flags: u8,
    opacity: f64,
    variant: &str,
) -> (Vec<u8>, Vec<u16>, Vec<u16>) {
    style_color_fixture(
        depth,
        family,
        flags,
        opacity,
        [FLOAT_COLOR, FLOAT_COLOR2],
        None,
        variant,
    )
}
fn grayscale_style_fixture(
    depth: u16,
    family: &str,
    flags: u8,
    opacity: f64,
    gray: [f64; 2],
    variant: &str,
) -> (Vec<u8>, Vec<u16>, Vec<u16>) {
    let colors = gray.map(|black| [(100. - black) / 100.; 3]);
    style_color_fixture(depth, family, flags, opacity, colors, Some(gray), variant)
}
fn style_color_fixture(
    depth: u16,
    family: &str,
    flags: u8,
    opacity: f64,
    colors: [[f64; 3]; 2],
    gray: Option<[f64; 2]>,
    variant: &str,
) -> (Vec<u8>, Vec<u16>, Vec<u16>) {
    let mut payload = match family {
        "color" => overlay_payload(true, true, [111., 67., 231.]),
        "gradient" => gradient_payload(false, None, 0.),
        "drop" => hard_shadow_payload("DrSh", true, true, 180., opacity, ""),
        _ => hard_shadow_payload("IrSh", true, true, 180., opacity, ""),
    };
    // Substitute complete independently encoded descriptors, including the
    // variable-length channel IDs; no parser or native renderer builds fixtures.
    for (index, (old, color)) in (if family == "gradient" {
        vec![([0f64; 3], colors[0]), ([255f64; 3], colors[1])]
    } else {
        vec![([111f64, 67., 231.], colors[0])]
    })
    .into_iter()
    .enumerate()
    {
        let needle = overlay_descriptor(
            "RGBC",
            ["Rd  ", "Grn ", "Bl  "]
                .into_iter()
                .zip(old)
                .map(|(key, n)| (key, b"doub", n.to_be_bytes().to_vec()))
                .collect(),
        );
        let mut fields = if let Some(gray) = gray {
            vec![("Gry ", b"doub", gray[index].to_be_bytes().to_vec())]
        } else {
            ["redFloat", "greenFloat", "blueFloat"]
                .into_iter()
                .zip(color)
                .map(|(key, n)| (key, b"doub", n.to_be_bytes().to_vec()))
                .collect::<Vec<_>>()
        };
        match variant {
            "gray-integer" => {
                fields[0].1 = b"long";
                fields[0].2 = (gray.unwrap()[index] as i32).to_be_bytes().to_vec();
            }
            "gray-high" => fields[0].2 = 100.00001f64.to_be_bytes().to_vec(),
            "extra-channel" => fields.push(("Rd  ", b"doub", 13f64.to_be_bytes().to_vec())),
            "mixed" => fields[1].0 = "Grn ",
            "incomplete" => {
                fields.pop();
            }
            "both" => fields.extend(
                ["Rd  ", "Grn ", "Bl  "]
                    .into_iter()
                    .zip(old)
                    .map(|(key, n)| (key, b"doub", n.to_be_bytes().to_vec())),
            ),
            "unit" => {
                fields[0].1 = b"UntF";
                fields[0].2 = [b"#Prc".to_vec(), color[0].to_be_bytes().to_vec()].concat();
            }
            "boolean" => {
                fields[0].1 = b"bool";
                fields[0].2 = vec![0];
            }
            "negative" => fields[0].2 = (-0.00001f64).to_be_bytes().to_vec(),
            "hdr" => fields[0].2 = 1.00001f64.to_be_bytes().to_vec(),
            "nan" => fields[0].2 = f64::NAN.to_be_bytes().to_vec(),
            "infinity" => fields[0].2 = f64::INFINITY.to_be_bytes().to_vec(),
            "book" => fields.push(("bookID", b"long", 123i32.to_be_bytes().to_vec())),
            _ => {}
        }
        let replacement = overlay_descriptor(
            if variant == "class" {
                "HSBl"
            } else if gray.is_some() {
                "Grsc"
            } else {
                "RGBC"
            },
            fields,
        );
        let at = payload
            .windows(needle.len())
            .position(|v| v == needle)
            .unwrap();
        payload.splice(at..at + needle.len(), replacement);
    }
    color_style_document(depth, family, flags, opacity, payload, colors)
}
fn color_style_document(
    depth: u16,
    family: &str,
    flags: u8,
    opacity: f64,
    mut payload: Vec<u8>,
    colors: [[f64; 3]; 2],
) -> (Vec<u8>, Vec<u16>, Vec<u16>) {
    let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
    payload[at] = u8::from(flags & 1 != 0);
    let at = payload
        .windows(18)
        .position(|v| v == b"masterFXSwitchbool")
        .unwrap()
        + 18;
    payload[at] = u8::from(flags & 2 != 0);
    let at = payload
        .windows(12)
        .position(|v| v == b"OpctUntF#Prc")
        .unwrap()
        + 12;
    payload[at..at + 8].copy_from_slice(&opacity.to_be_bytes());
    let mut source = SAVED;
    if family == "drop" {
        source[1][3] = 0;
    }
    let mut spec = Spec::paint("Synthetic Photoshop style color", source);
    spec.tags.push((*b"lfx2", payload));
    let mut merged = source.map(|p| {
        if p[3] == 0 {
            [0; 4]
        } else {
            p.map(|v| native(v, depth))
        }
    });
    if flags == 3 {
        let coverage = (opacity * 65535. / 100.).round() / 65535.;
        for x in 0..2 {
            let tint = if family == "gradient" {
                let t = if x == 0 { 0.25 } else { 0.75 };
                std::array::from_fn(|c| {
                    ((colors[0][c] * (1. - t) + colors[1][c] * t) * 65535.).round() as u16
                })
            } else {
                colors[0].map(|c| (c * 65535.).round() as u16)
            };
            if family == "drop" {
                if x == 1 {
                    merged[x] = [
                        tint[0],
                        tint[1],
                        tint[2],
                        (coverage * 65535.).round() as u16,
                    ]
                    .map(|v| native(v, depth));
                }
            } else if family != "inner" || x == 0 {
                for c in 0..3 {
                    merged[x][c] = native(
                        (f64::from(merged[x][c]) * (1. - coverage) + f64::from(tint[c]) * coverage)
                            .round() as u16,
                        depth,
                    );
                }
            }
        }
    }
    let mut bytes = fixture(&[spec], depth);
    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
    for c in 0..4 {
        for pixel in merged {
            composite_sample(&mut bytes, pixel, c, depth);
        }
    }
    (
        bytes,
        merged.concat(),
        source.map(|p| p.map(|v| native(v, depth))).concat(),
    )
}

#[test]
fn hsb_photoshop_style_colors_preserve_native_values_sources_undo_and_standard_saves() {
    let cases: [([[f64; 3]; 2], [[f64; 3]; 2]); 6] = [
        (
            [[0., 100., 100.], [60., 100., 100.]],
            [[1., 0., 0.], [1., 1., 0.]],
        ),
        (
            [[120., 100., 100.], [180., 100., 100.]],
            [[0., 1., 0.], [0., 1., 1.]],
        ),
        (
            [[240., 100., 100.], [300., 100., 100.]],
            [[0., 0., 1.], [1., 0., 1.]],
        ),
        (
            [[360., 100., 100.], [0., 0., 37.5]],
            [[1., 0., 0.], [0.375; 3]],
        ),
        (
            [[30., 37.5, 62.5], [270., 62.5, 37.5]],
            [[0.625, 0.5078125, 0.390625], [0.2578125, 0.140625, 0.375]],
        ),
        (
            [[330., 100., 100.], [360., 100., 0.]],
            [[1., 0., 0.5], [0.; 3]],
        ),
    ];
    for depth in [8, 16] {
        for family in ["color", "gradient", "inner", "drop"] {
            for (hsb, colors) in cases {
                for (units, variant) in [(0, ""), (1, ""), (7, ""), (0, "integer")] {
                    if variant == "integer" && hsb.iter().flatten().any(|n| n.fract() != 0.) {
                        continue;
                    }
                    for flags in [0, 3] {
                        let (mut bytes, merged, source) =
                            hsb_style_fixture(depth, family, flags, hsb, colors, units, variant);
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{depth} {family} {hsb:?} {units} {flags}: {:?}",
                            doc.warnings
                        );
                        assert_eq!(doc.bit_depth, depth);
                        assert_eq!(doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(rendered(&doc), merged);
                        for (index, color) in colors
                            .into_iter()
                            .enumerate()
                            .take(if family == "gradient" { 2 } else { 1 })
                        {
                            let imported = if family == "gradient" {
                                &doc.layers[0].effects[0].settings["color_stops"][index]["color"]
                            } else {
                                &doc.layers[0].effects[0].settings["color"]
                            };
                            assert_eq!(imported, &json!(color));
                        }
                        let current = doc.export_png().unwrap();
                        for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.bit_depth, depth);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(
                                reopened.layers[0].effects[0].settings,
                                doc.layers[0].effects[0].settings
                            );
                            assert_eq!(reopened.export_png().unwrap(), current);
                            assert_eq!(
                                psd::decode(&ordinary(&output))
                                    .unwrap()
                                    .export_png()
                                    .unwrap(),
                                current
                            );
                            let matte = merged
                                .chunks_exact(4)
                                .flat_map(|p| {
                                    let a = f64::from(p[3]) / 65535.;
                                    (0..4).map(move |c| {
                                        if c == 3 {
                                            p[c]
                                        } else {
                                            native(
                                                (f64::from(p[c]) * a + 65535. * (1. - a)).round()
                                                    as u16,
                                                depth,
                                            )
                                        }
                                    })
                                })
                                .collect::<Vec<_>>();
                            assert_eq!(saved_composite(&output, depth), matte);
                        }
                        let mut engine = Engine::new();
                        let layer = doc.layers[0].id.clone();
                        let effect = doc.layers[0].effects[0].id.clone();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})], None, None, "HSB style bypass").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        let at =
                            bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth / 8) - 1;
                        bytes[at] ^= 1;
                        assert!(
                            psd::decode(&bytes).unwrap().read_only,
                            "HSB composite mismatch was accepted"
                        );
                    }
                }
            }
        }
    }
}
fn hsb_style_fixture(
    depth: u16,
    family: &str,
    flags: u8,
    hsb: [[f64; 3]; 2],
    colors: [[f64; 3]; 2],
    units: u8,
    variant: &str,
) -> (Vec<u8>, Vec<u16>, Vec<u16>) {
    let mut payload = match family {
        "color" => overlay_payload(true, true, [111., 67., 231.]),
        "gradient" => gradient_payload(false, None, 0.),
        "drop" => hard_shadow_payload("DrSh", true, true, 180., 37., ""),
        _ => hard_shadow_payload("IrSh", true, true, 180., 37., ""),
    };
    let olds = if family == "gradient" {
        vec![[0f64; 3], [255f64; 3]]
    } else {
        vec![[111f64, 67., 231.]]
    };
    for (index, old) in olds.into_iter().enumerate() {
        let needle = overlay_descriptor(
            "RGBC",
            ["Rd  ", "Grn ", "Bl  "]
                .into_iter()
                .zip(old)
                .map(|(key, n)| (key, b"doub", n.to_be_bytes().to_vec()))
                .collect(),
        );
        let mut fields = ["H   ", "Strt", "Brgh"]
            .into_iter()
            .enumerate()
            .map(|(c, key)| {
                if units & (1 << c) != 0 {
                    (
                        key,
                        b"UntF",
                        [
                            if c == 0 {
                                b"#Ang".to_vec()
                            } else {
                                b"#Prc".to_vec()
                            },
                            hsb[index][c].to_be_bytes().to_vec(),
                        ]
                        .concat(),
                    )
                } else {
                    (key, b"doub", hsb[index][c].to_be_bytes().to_vec())
                }
            })
            .collect::<Vec<_>>();
        let set = |field: &mut (&str, &[u8; 4], Vec<u8>), n: f64| {
            let prefix = if field.1 == b"UntF" {
                field.2[..4].to_vec()
            } else {
                vec![]
            };
            field.2 = [prefix, n.to_be_bytes().to_vec()].concat();
        };
        match variant {
            "hue-low" => set(&mut fields[0], -0.01),
            "hue-high" => set(&mut fields[0], 360.01),
            "saturation-low" => set(&mut fields[1], -0.01),
            "saturation-high" => set(&mut fields[1], 100.01),
            "brightness-low" => set(&mut fields[2], -0.01),
            "brightness-high" => set(&mut fields[2], 100.01),
            "nan" => set(&mut fields[0], f64::NAN),
            "infinity" => set(&mut fields[2], f64::INFINITY),
            "hue-unit" => fields[0].2[..4].copy_from_slice(b"#Prc"),
            "percent-unit" => fields[1].2[..4].copy_from_slice(b"#Ang"),
            "boolean" => {
                fields[0].1 = b"bool";
                fields[0].2 = vec![0];
            }
            "integer" => {
                for (c, field) in fields.iter_mut().enumerate() {
                    field.1 = b"long";
                    field.2 = (hsb[index][c] as i32).to_be_bytes().to_vec();
                }
            }
            "missing" => {
                fields.pop();
            }
            "extra" => fields.push(("redFloat", b"doub", 0.5f64.to_be_bytes().to_vec())),
            "book" => fields.push(("bookID", b"long", 1i32.to_be_bytes().to_vec())),
            _ => {}
        }
        let replacement =
            overlay_descriptor(if variant == "class" { "HSBl" } else { "HSBC" }, fields);
        let at = payload
            .windows(needle.len())
            .position(|v| v == needle)
            .unwrap();
        payload.splice(at..at + needle.len(), replacement);
    }
    color_style_document(depth, family, flags, 37., payload, colors)
}
#[test]
fn malformed_hsb_photoshop_style_colors_remain_protected_even_when_disabled() {
    for depth in [8, 16] {
        for family in ["color", "gradient", "inner", "drop"] {
            for flags in [0, 3] {
                for variant in [
                    "hue-low",
                    "hue-high",
                    "saturation-low",
                    "saturation-high",
                    "brightness-low",
                    "brightness-high",
                    "nan",
                    "infinity",
                    "hue-unit",
                    "percent-unit",
                    "boolean",
                    "missing",
                    "extra",
                    "book",
                    "class",
                ] {
                    let (bytes, _, _) = hsb_style_fixture(
                        depth,
                        family,
                        flags,
                        [[30., 37.5, 62.5], [270., 62.5, 37.5]],
                        [[0.625, 0.5078125, 0.390625], [0.2578125, 0.140625, 0.375]],
                        7,
                        variant,
                    );
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(doc.read_only, "{depth} {family} {flags} {variant}");
                    assert_eq!(doc.bit_depth, depth);
                    assert!(doc.layers[0].effects.is_empty());
                }
            }
        }
    }
}
#[test]
fn grayscale_photoshop_style_colors_retain_native_precision_sources_history_and_saves() {
    for depth in [8, 16] {
        for family in ["color", "gradient", "inner", "drop"] {
            for (grays, variant) in [
                ([0f64, 100.], ""),
                ([41.937, 73.351], ""),
                ([37., 73.], "gray-integer"),
            ] {
                for flags in 0..4 {
                    for opacity in [37f64, 100.] {
                        let (mut bytes, merged, source) =
                            grayscale_style_fixture(depth, family, flags, opacity, grays, variant);
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{depth} {family} {grays:?} {flags} {opacity}: {:?}",
                            doc.warnings
                        );
                        assert_eq!(doc.bit_depth, depth);
                        assert_eq!(doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(rendered(&doc), merged);
                        assert_eq!(doc.layers[0].effects[0].enabled, flags == 3);
                        for (index, black) in grays
                            .into_iter()
                            .enumerate()
                            .take(if family == "gradient" { 2 } else { 1 })
                        {
                            let imported = if family == "gradient" {
                                &doc.layers[0].effects[0].settings["color_stops"][index]["color"]
                            } else {
                                &doc.layers[0].effects[0].settings["color"]
                            };
                            let expected_color = [(100. - black) / 100.; 3];
                            assert_eq!(imported, &json!(expected_color));
                        }
                        let current = doc.export_png().unwrap();
                        for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.bit_depth, depth);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(
                                reopened.layers[0].effects[0].settings,
                                doc.layers[0].effects[0].settings
                            );
                            assert_eq!(reopened.export_png().unwrap(), current);
                            assert_eq!(
                                psd::decode(&ordinary(&output))
                                    .unwrap()
                                    .export_png()
                                    .unwrap(),
                                current
                            );
                            let matte = merged
                                .chunks_exact(4)
                                .flat_map(|p| {
                                    let alpha = f64::from(p[3]) / 65535.;
                                    (0..4).map(move |c| {
                                        if c == 3 {
                                            p[c]
                                        } else {
                                            native(
                                                (f64::from(p[c]) * alpha + 65535. * (1. - alpha))
                                                    .round()
                                                    as u16,
                                                depth,
                                            )
                                        }
                                    })
                                })
                                .collect::<Vec<_>>();
                            assert_eq!(saved_composite(&output, depth), matte);
                        }
                        let mut engine = Engine::new();
                        let layer = doc.layers[0].id.clone();
                        let effect = doc.layers[0].effects[0].id.clone();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})], None, None, "Grayscale style bypass").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        let at =
                            bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth / 8) - 1;
                        bytes[at] ^= 1;
                        assert!(
                            psd::decode(&bytes).unwrap().read_only,
                            "grayscale composite mismatch was accepted"
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn malformed_grayscale_photoshop_style_colors_stay_protected_even_when_disabled() {
    for depth in [8, 16] {
        for family in ["color", "gradient", "inner", "drop"] {
            for flags in [0, 3] {
                for variant in [
                    "incomplete",
                    "both",
                    "extra-channel",
                    "unit",
                    "boolean",
                    "negative",
                    "gray-high",
                    "nan",
                    "infinity",
                    "book",
                    "class",
                ] {
                    let (bytes, _, _) = grayscale_style_fixture(
                        depth,
                        family,
                        flags,
                        100.,
                        [41.937, 73.351],
                        variant,
                    );
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(doc.read_only, "{depth} {family} {flags} {variant}");
                    assert_eq!(doc.bit_depth, depth);
                    assert!(doc.layers[0].effects.is_empty());
                }
            }
        }
    }
}
#[test]
fn normalized_photoshop_rgb_style_colors_retain_native_samples_definitions_history_and_saves() {
    for depth in [8, 16] {
        for family in ["color", "gradient", "inner", "drop"] {
            for flags in 0..4 {
                for opacity in [37., 100.] {
                    let (mut bytes, expected, source) =
                        normalized_style_fixture(depth, family, flags, opacity, "");
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(
                        !doc.read_only,
                        "{depth} {family} {flags} {opacity}: {:?}",
                        doc.warnings
                    );
                    assert_eq!(rendered(&doc), expected);
                    assert_eq!(doc.bit_depth, depth);
                    assert_eq!(doc.layers[0].pixels.rgba16(), source);
                    let settings = doc.layers[0].effects[0].settings.as_ref();
                    assert_eq!(doc.layers[0].effects[0].enabled, flags == 3);
                    for (index, color) in if family == "gradient" {
                        vec![(0, FLOAT_COLOR), (1, FLOAT_COLOR2)]
                    } else {
                        vec![(0, FLOAT_COLOR)]
                    } {
                        for c in 0..3 {
                            let imported = if family == "gradient" {
                                &settings["color_stops"][index]["color"][c]
                            } else {
                                &settings["color"][c]
                            };
                            assert!((imported.as_f64().unwrap() - color[c]).abs() < 1e-12);
                        }
                    }
                    let current = doc.export_png().unwrap();
                    let mut engine = Engine::new();
                    engine.resident_state_mut().expect("resident fixture").document = doc;
                    let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                    engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})], None, None, "Normalized RGB bypass").unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                    engine.undo("human").unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                    for output in [
                        psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    ] {
                        let reopened = psd::decode(&output).unwrap();
                        assert!(!reopened.read_only);
                        assert_eq!(reopened.bit_depth, depth);
                        assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                        assert_eq!(reopened.export_png().unwrap(), current);
                        let standard = psd::decode(&ordinary(&output)).unwrap();
                        assert!(!standard.read_only);
                        assert_eq!(standard.export_png().unwrap(), current);
                        let matte: Vec<u16> = expected
                            .chunks_exact(4)
                            .flat_map(|p| {
                                let max = if depth == 16 { 65535u64 } else { 255 };
                                let divisor = if depth == 16 { 1u64 } else { 257 };
                                let alpha = u64::from(p[3]) / divisor;
                                (0..4).map(move |c| {
                                    if c == 3 {
                                        p[3]
                                    } else {
                                        (((u64::from(p[c]) / divisor * alpha
                                            + max * (max - alpha)
                                            + max / 2)
                                            / max)
                                            * divisor)
                                            as u16
                                    }
                                })
                            })
                            .collect();
                        assert_eq!(saved_composite(&output, depth), matte);
                    }
                    let at = bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth == 16);
                    bytes[at] ^= 1;
                    let protected = psd::decode(&bytes).unwrap();
                    assert!(protected.read_only);
                    assert!(protected.layers[0].effects.is_empty());
                }
            }
        }
    }
}

#[test]
fn malformed_normalized_photoshop_rgb_styles_remain_protected_even_when_disabled() {
    for depth in [8, 16] {
        for family in ["color", "gradient", "inner", "drop"] {
            for variant in [
                "mixed",
                "incomplete",
                "both",
                "unit",
                "boolean",
                "negative",
                "hdr",
                "nan",
                "infinity",
                "book",
                "class",
            ] {
                let (bytes, _, _) = normalized_style_fixture(depth, family, 0, 100., variant);
                let doc = psd::decode(&bytes).unwrap();
                assert!(doc.read_only, "{depth} {family} {variant}");
                assert!(doc.layers[0].effects.is_empty());
                assert_eq!(doc.bit_depth, depth);
                assert!(psd::encode(&doc).is_err());
                assert!(psd::encode_psb(&doc).is_err());
            }
        }
    }
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

fn legacy_overlay_payload(color: Option<[u16; 3]>, opacity: u8, enabled: bool) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 1 + u8::from(color.is_some())];
    bytes.extend(b"8BIMcmnS");
    block(&mut bytes, &[0, 0, 0, 0, 1, 0, 0]);
    if let Some(color) = color {
        let mut rgb = vec![0, 0];
        for word in color {
            rgb.extend(word.to_be_bytes());
        }
        rgb.extend([0, 0]);
        let mut effect = 2u32.to_be_bytes().to_vec();
        effect.extend(b"8BIMnorm");
        effect.extend(&rgb);
        effect.extend([opacity, u8::from(enabled)]);
        effect.extend(rgb);
        bytes.extend(b"8BIMsofi");
        block(&mut bytes, &effect);
    }
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes
}

#[test]
fn legacy_color_overlay_import_and_modern_companion_preserve_exact_samples() {
    let color = [64515u16, 17221, 54321];
    for depth in [8, 16] {
        for (modern, enabled, legacy_first) in [(false, true, true), (false, false, true), (true, true, true), (true, true, false)] {
            let opacity = if modern { 64 } else { 25 };
            let mut legacy = (*b"lrFX", legacy_overlay_payload(Some(color), opacity, enabled));
            let mut spec = Spec::paint("Synthetic legacy overlay", SAVED);
            if modern {
                let mut descriptor = overlay_payload(true, enabled, color.map(|word| f64::from(word) / 257.));
                let at = descriptor.windows(12).position(|v| v == b"OpctUntF#Prc").unwrap() + 12;
                descriptor[at..at + 8].copy_from_slice(&25f64.to_be_bytes());
                spec.tags.push((*b"lfx2", descriptor));
            }
            let legacy_index = if legacy_first { 0 } else { spec.tags.len() };
            spec.tags.insert(legacy_index, legacy.clone());
            let expected = |spec: &Spec| {
                let mut bytes = fixture(std::slice::from_ref(spec), depth);
                let mut merged = Vec::new();
                let alpha = if !enabled { 0 } else if modern { 16384 } else { u32::from(opacity) * 257 };
                for channel in 0..4 {
                    for pixel in SAVED {
                        let mut pixel = pixel.map(|word| native(word, depth));
                        if channel < 3 {
                            let sum = u32::from(pixel[channel]) * (65535 - alpha) + u32::from(color[channel]) * alpha;
                            pixel[channel] = ((sum + 32767) / 65535) as u16;
                        }
                        composite_sample(&mut merged, pixel, channel, depth);
                    }
                }
                let at = bytes.len() - merged.len();
                bytes[at..].copy_from_slice(&merged);
                bytes
            };
            let mut bytes = expected(&spec);
            for doc in [psd::decode(&bytes).unwrap(), psd::decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap()] {
                assert!(!doc.read_only, "{depth} {modern} {enabled}: {:?}", doc.warnings);
                let layer = &doc.layers[0];
                assert_eq!(layer.effects.len(), 1);
                let effect = &layer.effects[0];
                assert_eq!(effect.kind, "color_overlay");
                assert_eq!(effect.enabled, enabled);
                let normalized = if modern { 0.25 } else { (f64::from(opacity) * 100. / 255.) / 100. };
                assert_eq!(effect.settings["opacity"], normalized);
                for (index, word) in color.into_iter().enumerate() {
                    assert_eq!((effect.settings["color"][index].as_f64().unwrap() * 65535.).round() as u16, word);
                }
                assert_eq!(layer.pixels.rgba16(), SAVED.into_iter().flatten().map(|word| native(word, depth)).collect::<Vec<_>>());
                for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    let restored = psd::decode(&ordinary(&output)).unwrap();
                    assert!(!restored.read_only, "{:?}", restored.warnings);
                    assert_eq!(restored.layers[0].effects[0].settings, effect.settings);
                    assert_eq!(restored.layers[0].pixels.rgba16(), layer.pixels.rgba16());
                    assert_eq!(restored.export_png().unwrap(), doc.export_png().unwrap());
                }
            }
            if modern {
                legacy.1 = legacy_overlay_payload(Some(color), opacity - 1, enabled);
                spec.tags[legacy_index] = legacy;
                assert!(psd::decode(&expected(&spec)).unwrap().read_only);
            } else {
                let at = bytes.len() - 8 * usize::from(depth / 8);
                bytes[at] ^= 1;
                assert!(psd::decode(&bytes).unwrap().read_only);
            }
        }
        let mut spec = Spec::paint("Synthetic legacy common state", SAVED);
        spec.tags.push((*b"lrFX", legacy_overlay_payload(None, 0, false)));
        let doc = psd::decode(&fixture(&[spec.clone()], depth)).unwrap();
        assert!(!doc.read_only);
        assert!(doc.layers[0].effects.is_empty());
        spec.tags[0].1[22] = 1;
        assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
    }
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
            engine.resident_state_mut().expect("resident fixture").document = doc;
            engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"opacity":0.25}})],None,None,"Overlay strength").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), before);
            doc = engine.resident_state().expect("resident fixture").document.clone();
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
fn photoshop_partial_fill_retains_disabled_styles_at_native_depth() {
    for depth in [8, 16] {
        let original = [[12347, 33559, 51237, 65535], [33117, 22303, 10013, 65535]];
        let expected = original.map(|[r, g, b, _]| [r, g, b, 128 * 257].map(|word| native(word, depth)));
        let source = |enabled| {
            let mut spec = Spec::paint("Partial Fill with a retained overlay", original);
            spec.blend = *b"mul ";
            spec.tags.extend([
                (*b"iOpa", vec![128, 0, 0, 0]),
                (*b"lfx2", overlay_payload(true, enabled, [111., 67., 231.])),
            ]);
            if enabled {
                spec.tags.push((*b"infx", vec![1, 0, 0, 0]));
            }
            let mut bytes = fixture(&[spec], depth);
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for channel in 0..4 {
                for pixel in expected {
                    composite_sample(&mut bytes, pixel, channel, depth);
                }
            }
            bytes
        };
        let doc = psd::decode(&source(false)).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let top = &doc.layers[0];
        assert_eq!(top.blend, "multiply");
        assert_eq!(top.pixels.rgba16(), original.map(|pixel| pixel.map(|v| native(v, depth))).concat());
        assert_eq!(top.effects[0].kind, "color_overlay");
        assert!(!top.effects[0].enabled);
        assert_eq!(rendered(&doc), expected.concat());
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let reopened = psd::decode(&saved).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(serde_json::to_value(&reopened.layers[0].effects).unwrap(),
                serde_json::to_value(&top.effects).unwrap());
            let standard = psd::decode(&ordinary(&saved)).unwrap();
            assert!(!standard.read_only, "{:?}", standard.warnings);
            assert_eq!(standard.layers[0].psd_metadata, top.psd_metadata);
            assert_eq!(standard.layers[0].pixels, top.pixels);
            assert_eq!(rendered(&standard), expected.concat());
        }
        let enable = json!({"op":"effect.update", "layer":top.id, "effect":top.effects[0].id, "enabled":true});
        assert!(Engine::preview_edits(doc.clone(), &[enable.clone()]).unwrap_err().contains("Partial Fill"));
        let enabled = Engine::preview_edits(doc.clone(), &[
            json!({"op":"layer.update", "layer":top.id, "fill_opacity":255}), enable,
        ]).unwrap();
        assert!(enabled.layers[0].effects[0].enabled);
        let protected = psd::decode(&source(true)).unwrap();
        assert!(protected.read_only);
        assert!(protected.warnings.iter().any(|warning| warning.contains("Partial Fill")));
    }
}

#[test]
fn photoshop_blend_if_retains_disabled_styles_and_refuses_to_activate_them() {
    for depth in [8, 16] {
        let original = [[51399, 51143, 13011, 65535], [10001, 20003, 30007, 65535]];
        let backdrop = [[12347, 33559, 51237, 65535]; 2];
        let expected = [original[0], backdrop[1]].map(|pixel| pixel.map(|v| native(v, depth)));
        let source = |enabled| {
            let mut top = Spec::paint("Blend If with a retained overlay", original);
            top.ranges = [
                [0, 0, 255, 255, 0, 0, 255, 255],
                [128, 128, 255, 255, 0, 0, 255, 255],
            ].concat();
            top.tags.push((*b"lfx2", overlay_payload(true, enabled, [111., 67., 231.])));
            let mut bytes = fixture(&[top, Spec::paint("Backdrop", backdrop)], depth);
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for channel in 0..4 {
                for pixel in expected {
                    composite_sample(&mut bytes, pixel, channel, depth);
                }
            }
            bytes
        };
        let doc = psd::decode(&source(false)).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        let top = &doc.layers[0];
        assert_eq!(top.pixels.rgba16(), original.map(|pixel| pixel.map(|v| native(v, depth))).concat());
        assert_eq!(top.effects[0].kind, "color_overlay");
        assert!(!top.effects[0].enabled);
        assert_eq!(rendered(&doc), expected.concat());
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let reopened = psd::decode(&saved).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(serde_json::to_value(&reopened.layers[0].effects).unwrap(),
                serde_json::to_value(&top.effects).unwrap());
            let standard = psd::decode(&ordinary(&saved)).unwrap();
            assert!(!standard.read_only, "{:?}", standard.warnings);
            assert_eq!(standard.layers[0].psd_metadata, top.psd_metadata);
            assert_eq!(standard.layers[0].pixels, top.pixels);
            assert_eq!(rendered(&standard), expected.concat());
        }
        let enable = json!({"op":"effect.update", "layer":top.id, "effect":top.effects[0].id, "enabled":true});
        assert!(Engine::preview_edits(doc.clone(), &[enable.clone()]).unwrap_err().contains("Blend If"));
        let enabled = Engine::preview_edits(doc.clone(), &[
            json!({"op":"layer.update", "layer":top.id, "blend_if":null}), enable,
        ]).unwrap();
        assert!(enabled.layers[0].effects[0].enabled);
        let protected = psd::decode(&source(true)).unwrap();
        assert!(protected.read_only);
        assert!(protected.warnings.iter().any(|warning| warning.contains("Blend If")));
    }
}

#[test]
fn photoshop_overlays_with_channel_exclusions_keep_editable_native_sources() {
    for depth in [8, 16] {
        let original = [51399, 51143, 13011, 65535];
        let backdrop = [[25700, 17990, 15420, 32896], [0; 4]];
        let red = if depth == 8 { 12850 } else { 12900 };
        for (kind, payload, overlay) in [
            (
                "color_overlay",
                overlay_payload(true, true, [111., 67., 231.]),
                [[111 * 257, 67 * 257, 231 * 257, 65535]; 2],
            ),
            (
                "gradient_overlay",
                gradient_payload(false, None, 0.),
                [[16384, 16384, 16384, 65535], [49151, 49151, 49151, 65535]],
            ),
            (
                "pattern_overlay",
                pattern_payload(true, "synthetic-pattern", "", 100.),
                PATTERN,
            ),
        ] {
            let mut top = Spec::paint("Restricted overlay", [original; 2]);
            top.tags.extend([(*b"lfx2", payload), (*b"brst", 0u32.to_be_bytes().to_vec())]);
            let mut bytes = fixture(&[top, Spec::paint("Backdrop", backdrop)], depth);
            if kind == "pattern_overlay" {
                bytes = inject_pattern(bytes, depth, &pattern_resource(depth, "synthetic-pattern"), false);
            }
            let baked = overlay.map(|pixel| pixel.map(|v| native(v, depth)));
            let mut expected = baked;
            expected[0][0] = red;
            expected[1][0] = 0;
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for channel in 0..4 {
                for pixel in expected {
                    composite_sample(&mut bytes, pixel, channel, depth);
                }
            }
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{depth} {kind}: {:?}", doc.warnings);
            let layer = &doc.layers[0];
            assert_eq!(layer.effects[0].kind, kind);
            assert_eq!(layer.pixels.get16(0, 0), original.map(|v| native(v, depth)));
            assert_eq!(rendered(&doc), expected.concat());
            for bytes in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].pixels, layer.pixels);
                assert_eq!(reopened.layers[0].effects[0].settings, layer.effects[0].settings);
                assert_eq!(reopened.layers[0].psd_metadata, layer.psd_metadata);
                assert_eq!(rendered(&reopened), expected.concat());

                let standard = psd::decode(&ordinary(&bytes)).unwrap();
                assert!(!standard.read_only, "{:?}", standard.warnings);
                assert_eq!(standard.layers[0].pixels.get16(0, 0), baked[0]);
                assert_eq!(standard.layers[0].psd_metadata, layer.psd_metadata);
                assert_eq!(rendered(&standard), expected.concat());
            }
            let hidden = Engine::preview_edits(doc.clone(), &[json!({
                "op":"layer.update", "layer":layer.id, "blend_channels":[false,false,false],
            })]).unwrap();
            assert_eq!(rendered(&hidden), backdrop.concat());
            let clipped = Engine::preview_edits(doc.clone(), &[json!({
                "op":"layer.clip", "layer":layer.id, "base":doc.layers[1].id,
            })]).unwrap();
            assert_eq!(rendered(&clipped),
                [backdrop[0][0], baked[0][1], baked[0][2], backdrop[0][3], 0, 0, 0, 0]);
        }
    }
}

#[test]
fn photoshop_grouped_clipping_keeps_overlays_and_channel_exclusions_editable() {
    for depth in [8, 16] {
        let backdrop = [12701, 21707, 31709, 65535];
        let raw_base = [49001, 39203, 22407, 65535];
        let raw_member = [30103, 44107, 52709, 65535];
        let base_overlay = [37.0, 139.0, 211.0];
        let member_overlay = [239.0, 17.0, 89.0];
        let mut base = Spec::paint("Base", [raw_base, [0; 4]]);
        base.tags.extend([
            (*b"lfx2", overlay_payload(true, true, base_overlay)),
            (*b"brst", 0u32.to_be_bytes().to_vec()),
        ]);
        let mut member = Spec::paint("Member", [raw_member; 2]);
        member.clipping = 1;
        member.tags.extend([
            (*b"lfx2", overlay_payload(true, true, member_overlay)),
            (*b"brst", 1u32.to_be_bytes().to_vec()),
        ]);
        let expected = [
            [backdrop[0], 139 * 257, 89 * 257, 65535],
            backdrop,
        ].map(|pixel| pixel.map(|v| native(v, depth)));
        let mut bytes = fixture(&[member, base, Spec::paint("Backdrop", [backdrop; 2])], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        for channel in 0..4 {
            for pixel in expected {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        let doc = psd::decode(&bytes).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(rendered(&doc), expected.concat());
        assert_eq!(doc.layers[0].clip_to.as_deref(), Some(doc.layers[1].id.as_str()));
        for (layer, raw) in doc.layers.iter().zip([raw_member, raw_base]) {
            assert_eq!(layer.pixels.get16(0, 0), raw.map(|v| native(v, depth)));
            assert_eq!(layer.effects[0].kind, "color_overlay");
        }
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let reopened = psd::decode(&saved).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(rendered(&reopened), expected.concat());
            for (after, before) in reopened.layers.iter().zip(&doc.layers) {
                assert_eq!(after.pixels, before.pixels);
                assert_eq!(after.psd_metadata, before.psd_metadata);
                assert_eq!(serde_json::to_value(&after.effects).unwrap(),
                    serde_json::to_value(&before.effects).unwrap());
            }
            let standard = psd::decode(&ordinary(&saved)).unwrap();
            assert!(!standard.read_only, "{:?}", standard.warnings);
            assert_eq!(rendered(&standard), expected.concat());
            assert!(standard.layers.iter().all(|layer| layer.effects.is_empty()));
            assert_eq!(standard.layers[0].clip_to.as_deref(), Some(standard.layers[1].id.as_str()));
        }
        for edit in [
            json!({"op":"layer.update", "layer":doc.layers[1].id, "blend_clipped_as_group":false}),
            json!({"op":"effect.add", "layer":doc.layers[1].id, "kind":"blur", "settings":{"radius":1.0}}),
        ] {
            let reason = Engine::preview_edits(doc.clone(), &[edit]).unwrap_err();
            assert!(reason.contains("(brst)"), "{reason}");
        }
        let merged = bytes.len() - 8 * usize::from(depth / 8);
        bytes[merged] ^= 1;
        assert!(psd::decode(&bytes).unwrap().read_only);
    }
}

#[test]
fn grouped_photoshop_interior_overlays_preserve_layer_blends_and_original_samples() {
    let mut foreground = SAVED;
    foreground[1][3] = 0;
    let background = [[41231, 35177, 29123, 65535], [12347, 50231, 31453, 65535]];
    let color = [111., 67., 231.];
    for depth in [8, 16] {
        let divisor = if depth == 8 { 257u64 } else { 1 };
        let max = 65535 / divisor;
        let overlay_color = [111 * 257, 67 * 257, 231 * 257, 65535];
        for (blend, key) in [("multiply", *b"mul "), ("screen", *b"scrn")] {
            for (kind, style, overlay) in [
                ("color_overlay", overlay_payload(true, true, color), overlay_color),
                (
                    "gradient_overlay",
                    custom_gradient_payload(
                        false, None, 0., 100., None,
                        &[(0, 50, color), (4096, 50, color)],
                        Some(&[(0, 50, 100.), (4096, 50, 100.)]),
                    ),
                    overlay_color,
                ),
                ("pattern_overlay", pattern_payload(true, "synthetic-pattern", "", 100.), PATTERN[0]),
            ] {
                let mut expected = background.map(|p| p.map(|v| native(v, depth)));
                for (sample, overlay) in expected[0][..3].iter_mut().zip(overlay) {
                    let b = u64::from(*sample) / divisor;
                    let f = u64::from(native(overlay, depth)) / divisor;
                    let blended = if blend == "multiply" {
                        (b * f + max / 2) / max
                    } else {
                        max - ((max - b) * (max - f) + max / 2) / max
                    };
                    *sample = (blended * divisor) as u16;
                }
                let expected_words = expected.concat();
                let mut styled = Spec::paint("Grouped interior overlay", foreground);
                styled.blend = key;
                styled.tags.push((*b"lfx2", style));
                let backdrop = Spec::paint("Backdrop", background);
                let build = |specs: &[Spec]| {
                    let mut bytes = fixture(specs, depth);
                    if kind == "pattern_overlay" {
                        bytes = inject_pattern(bytes, depth, &pattern_resource(depth, "synthetic-pattern"), false);
                    }
                    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                    for channel in 0..4 {
                        for pixel in expected {
                            composite_sample(&mut bytes, pixel, channel, depth);
                        }
                    }
                    bytes
                };

                // Even an identical saved image cannot substitute for the explicit flag.
                for flag in [None, Some([0; 4]), Some([1, 0, 0, 1])] {
                    let mut protected = styled.clone();
                    protected.tags.extend(flag.map(|value| (*b"infx", value.to_vec())));
                    let opened = psd::decode(&build(&[protected, backdrop.clone()])).unwrap();
                    assert!(opened.read_only);
                    assert_eq!(opened.layers[0].pixels.rgba16(), expected_words);
                }

                let flag_index = usize::from(kind == "color_overlay");
                styled.tags.insert(flag_index, (*b"infx", vec![1, 0, 0, 0]));
                let mut duplicate = styled.clone();
                duplicate.tags.push((*b"infx", vec![1, 0, 0, 0]));
                let rejected = psd::decode(&build(&[duplicate, backdrop.clone()])).unwrap();
                assert!(rejected.read_only);
                assert!(rejected.warnings.iter().any(|warning| warning.contains("Duplicate Photoshop Blend Interior")));
                let bytes = build(&[styled.clone(), backdrop.clone()]);
                let mut reader = std::io::Cursor::new(&bytes);
                let doc = psd::decode_reader(&mut reader, &Default::default()).unwrap();
                assert!(!doc.read_only, "{depth} {blend} {kind}: {:?}", doc.warnings);
                let original = foreground.map(|p| p.map(|v| native(v, depth))).concat();
                assert_eq!(doc.layers[0].pixels.rgba16(), original);
                assert_eq!(doc.layers[0].blend, blend);
                assert_eq!(doc.layers[0].effects[0].kind, kind);
                assert_eq!(rendered(&doc), expected_words);
                let flag = doc.layers[0].psd_metadata.iter().find(|entry| entry.key == "infx").unwrap();
                assert_eq!(flag.data, [1, 0, 0, 0]);
                for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    let reopened = psd::decode(&saved).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), original);
                    assert_eq!(reopened.layers[0].effects[0].kind, kind);
                    assert!(reopened.layers[0].psd_metadata.contains(flag));
                    assert_eq!(saved_composite(&saved, depth), expected_words);
                    let standard = psd::decode(&ordinary(&saved)).unwrap();
                    assert!(standard.layers[0].psd_metadata.contains(flag));
                    assert_eq!(rendered(&standard), expected_words);
                }

                let mut mismatch = bytes;
                let first = mismatch.len() - 8 * usize::from(depth / 8);
                mismatch[first] ^= 1;
                assert!(psd::decode(&mismatch).unwrap().read_only);
                let mut follower = Spec::paint("Transparent clipping follower", [[0; 4]; 2]);
                follower.clipping = 1;
                let protected = psd::decode(&build(&[follower, styled, backdrop])).unwrap();
                assert!(protected.read_only);
                assert!(protected.warnings.iter().any(|warning| {
                    warning.contains("styled clipping bases")
                }));
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
                    composite_sample(&mut bytes, p, c, depth);
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
    positioned_gradient_payload(reverse, omit, smoothness, 100., None)
}

#[test]
fn photoshop_gradient_integer_grid_import_requires_exact_native_composite() {
    for depth in [8, 16] {
        for reverse in [false, true] {
            let mut spec = Spec::paint("Synthetic integer-center gradient", SAVED);
            spec.tags
                .push((*b"lfx2", gradient_payload(reverse, None, 0.)));
            let mut bytes = fixture(&[spec], depth);
            // At integer x=0,1, center=1 and radius=1, the independent
            // linear ramp samples 0 and 1/2 (1 and 1/2 when reversed).
            let expected = [
                [
                    if reverse { 65535 } else { 0 },
                    if reverse { 65535 } else { 0 },
                    if reverse { 65535 } else { 0 },
                    65535,
                ],
                [native(32768, depth); 4],
            ];
            let mut expected = expected;
            expected[1][3] = 65535;
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for channel in 0..4 {
                for pixel in expected {
                    composite_sample(&mut bytes, pixel, channel, depth);
                }
            }
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{depth} {reverse}: {:?}", doc.warnings);
            assert_eq!(doc.layers[0].effects[0].settings["pixel_grid"], true);
            let source = SAVED.map(|p| p.map(|n| native(n, depth))).concat();
            assert_eq!(doc.layers[0].pixels.rgba16(), source);
            assert_eq!(rendered(&doc), expected.concat());
            let current = doc.export_png().unwrap();
            for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                assert_eq!(saved_composite(&output, depth), expected.concat());
                let reopened = psd::decode(&output).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                assert_eq!(
                    reopened.layers[0].effects[0].settings,
                    doc.layers[0].effects[0].settings
                );
                assert_eq!(reopened.export_png().unwrap(), current);
                assert_eq!(
                    psd::decode(&ordinary(&output))
                        .unwrap()
                        .export_png()
                        .unwrap(),
                    current
                );
            }
            let mut engine = Engine::new();
            let layer = doc.layers[0].id.clone();
            let effect = doc.layers[0].effects[0].id.clone();
            engine.resident_state_mut().expect("resident fixture").document = doc;
            engine
                .edit(
                    "human",
                    &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":false})],
                    None,
                    None,
                    "Grid gradient bypass",
                )
                .unwrap();
            assert_eq!(rendered(&engine.resident_state().expect("resident fixture").document), source);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
            // A one-word saved-composite discrepancy cannot authorize editing.
            let at = bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth / 8) - 1;
            bytes[at] ^= 1;
            let protected = psd::decode(&bytes).unwrap();
            assert!(protected.read_only);
            assert!(protected.layers[0].effects.is_empty());
        }
    }
}
fn positioned_gradient_payload(
    reverse: bool,
    omit: Option<&str>,
    smoothness: f64,
    scale: f64,
    offset: Option<(f64, f64)>,
) -> Vec<u8> {
    custom_gradient_payload(
        reverse,
        omit,
        smoothness,
        scale,
        offset,
        &[(0, 50, [0.; 3]), (4096, 50, [255.; 3])],
        Some(&[(0, 50, 100.), (4096, 50, 100.)]),
    )
}

#[test]
fn photoshop_layer_aligned_gradient_preserves_transparent_sources_and_exact_saves() {
    for depth in [8, 16] {
        for enabled in [false, true] {
            let source = [[12347, 23459, 34571, 0], [54321, 32123, 11223, 65535]]
                .map(|p| p.map(|n| native(n, depth)));
            let mut payload = gradient_payload(false, None, 0.);
            for (key, value) in [(b"Algnbool", true), (b"enabbool", enabled)] {
                let at = payload.windows(key.len()).position(|v| v == key).unwrap() + key.len();
                payload[at] = u8::from(value);
            }
            let mut spec = Spec::paint("Synthetic layer-aligned gradient", source);
            spec.tags.push((*b"lfx2", payload));
            let mut bytes = fixture(&[spec], depth);
            let visible = if enabled {
                [
                    native(32768, depth),
                    native(32768, depth),
                    native(32768, depth),
                    65535,
                ]
            } else {
                source[1]
            };
            let expected = [[0; 4], visible];
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for channel in 0..4 {
                for pixel in expected {
                    composite_sample(&mut bytes, pixel, channel, depth);
                }
            }
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{depth} {enabled}: {:?}", doc.warnings);
            assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
            assert_eq!(doc.layers[0].effects[0].settings["align_layer"], true);
            assert_eq!(doc.layers[0].effects[0].settings["pixel_grid"], true);
            assert_eq!(doc.layers[0].effects[0].enabled, enabled);
            assert_eq!(rendered(&doc), expected.concat());
            let current = doc.export_png().unwrap();
            let mut standard = expected;
            // Standard PSD merged RGB is white-matted; the rendered/exported
            // transparent pixel has zero RGB, while the encoded RGB is white.
            standard[0] = [65535, 65535, 65535, 0];
            for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                let reopened = psd::decode(&output).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                assert_eq!(
                    reopened.layers[0].effects[0].settings,
                    doc.layers[0].effects[0].settings
                );
                assert_eq!(reopened.export_png().unwrap(), current);
                assert_eq!(saved_composite(&output, depth), standard.concat());
            }
            let mut engine = Engine::new();
            let layer = doc.layers[0].id.clone();
            let effect = doc.layers[0].effects[0].id.clone();
            engine.resident_state_mut().expect("resident fixture").document = doc;
            engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Aligned gradient bypass").unwrap();
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source.concat());
            assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
            let at = bytes.len() - 7 * usize::from(depth / 8) + usize::from(depth / 8) - 1;
            bytes[at] ^= 1;
            assert!(psd::decode(&bytes).unwrap().read_only);
        }
        for value in [2, 255] {
            let mut payload = gradient_payload(false, None, 0.);
            let at = payload.windows(8).position(|v| v == b"Algnbool").unwrap() + 8;
            payload[at] = value;
            let mut spec = Spec::paint("Synthetic invalid alignment", SAVED);
            spec.tags.push((*b"lfx2", payload));
            assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
        }
    }
}
fn custom_gradient_payload(
    reverse: bool,
    omit: Option<&str>,
    smoothness: f64,
    scale: f64,
    offset: Option<(f64, f64)>,
    color_stops: &[(i32, i32, [f64; 3])],
    alpha_stops: Option<&[(i32, i32, f64)]>,
) -> Vec<u8> {
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
    let mut colors = (color_stops.len() as u32).to_be_bytes().to_vec();
    for &(position, midpoint, value) in color_stops {
        let rgb = desc(
            "RGBC",
            vec![
                ("Rd  ", b"doub", value[0].to_be_bytes().to_vec()),
                ("Grn ", b"doub", value[1].to_be_bytes().to_vec()),
                ("Bl  ", b"doub", value[2].to_be_bytes().to_vec()),
            ],
        );
        colors.extend(b"Objc");
        colors.extend(desc(
            "Clrt",
            vec![
                ("Clr ", b"Objc", rgb),
                ("Type", b"enum", enum_bytes("Clry", "UsrS")),
                ("Lctn", b"long", position.to_be_bytes().to_vec()),
                ("Mdpn", b"long", midpoint.to_be_bytes().to_vec()),
            ],
        ));
    }
    let mut gradient_fields = vec![
        ("GrdF", b"enum", enum_bytes("GrdF", "CstS")),
        ("Intr", b"doub", smoothness.to_be_bytes().to_vec()),
        ("Clrs", b"VlLs", colors),
    ];
    if let Some(alpha_stops) = alpha_stops {
        let mut alphas = (alpha_stops.len() as u32).to_be_bytes().to_vec();
        for &(position, midpoint, value) in alpha_stops {
            alphas.extend(b"Objc");
            alphas.extend(desc(
                "TrnS",
                vec![
                    ("Opct", b"UntF", unit("#Prc", value)),
                    ("Lctn", b"long", position.to_be_bytes().to_vec()),
                    ("Mdpn", b"long", midpoint.to_be_bytes().to_vec()),
                ],
            ));
        }
        gradient_fields.push(("Trns", b"VlLs", alphas));
    }
    let gradient = desc("Grdn", gradient_fields);
    let mut effect_fields = vec![
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
        ("Scl ", b"UntF", unit("#Prc", scale)),
    ];
    if let Some((x, y)) = offset {
        effect_fields.push((
            "Ofst",
            b"Objc",
            desc(
                "Pnt ",
                vec![
                    ("Hrzn", b"UntF", unit("#Prc", x)),
                    ("Vrtc", b"UntF", unit("#Prc", y)),
                ],
            ),
        ));
    }
    let effect = desc("GrFl", effect_fields);
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
fn photoshop_gradient_clamped_stops_preserve_native_sources_controls_and_standard_saves() {
    let a = [37.153, 112.347, 221.789];
    let b = [211.237, 53.193, 9.413];
    let mut cases = vec![
        vec![(0, 31, a)],
        vec![(2048, 87, a)],
        vec![(4096, 1, b)],
        vec![(1536, 31, a), (2560, 87, b)],
        vec![(512, 31, a), (3584, 87, b)],
        vec![(512, 31, a), (4096, 87, b)],
        vec![(0, 31, a), (3584, 87, b)],
    ];
    cases.push(
        (1..=14)
            .map(|i| (i * 256, 50, if i % 2 == 0 { a } else { b }))
            .collect(),
    );
    // The reference operates on the original Photoshop knots: outside their
    // range the nearest value is constant, without synthetic native endpoints.
    let reference = |stops: &[(i32, i32, f64)], position: f64| -> f64 {
        let coordinate = position * 4096.;
        if coordinate <= f64::from(stops[0].0) {
            return stops[0].2;
        }
        for pair in stops.windows(2) {
            if coordinate <= f64::from(pair[1].0) {
                let ratio = (coordinate - f64::from(pair[0].0)) / f64::from(pair[1].0 - pair[0].0);
                let midpoint = f64::from(pair[1].1) / 100.;
                let weight = if ratio <= midpoint {
                    ratio / (2. * midpoint)
                } else {
                    0.5 + (ratio - midpoint) / (2. * (1. - midpoint))
                };
                return pair[0].2 * (1. - weight) + pair[1].2 * weight;
            }
        }
        stops.last().unwrap().2
    };
    for depth in [8, 16] {
        for (case, colors) in cases.iter().enumerate() {
            let alpha = match case % 4 {
                0 => None,
                1 => Some(vec![(0, 99, 0.)]),
                2 => Some(vec![(2048, 1, 41.937)]),
                _ => Some(vec![(1536, 75, 12.319), (2560, 25, 84.731)]),
            };
            for reverse in [false, true] {
                for offset in [-25f64, 0., 25.] {
                    for enabled in [false, true] {
                        let mut payload = custom_gradient_payload(
                            reverse,
                            None,
                            0.,
                            100.,
                            Some((offset, 0.)),
                            colors,
                            alpha.as_deref(),
                        );
                        let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                        payload[at] = u8::from(enabled);
                        let at = payload
                            .windows(12)
                            .position(|v| v == b"OpctUntF#Prc")
                            .unwrap()
                            + 12;
                        payload[at..at + 8].copy_from_slice(&37f64.to_be_bytes());
                        let merged = std::array::from_fn::<_, 2, _>(|x| {
                            let source = SAVED[x].map(|n| native(n, depth));
                            if !enabled {
                                return source;
                            }
                            let mut position =
                                ((x as f64 + 0.5) / 2. - offset / 100.).clamp(0., 1.);
                            if reverse {
                                position = 1. - position;
                            }
                            let coverage = alpha
                                .as_ref()
                                .map_or(1., |stops| reference(stops, position) / 100.);
                            let weight = (coverage * (37. / 100.) * 65535.).round() / 65535.;
                            std::array::from_fn(|channel| {
                                if channel == 3 {
                                    return 65535;
                                }
                                let knots = colors
                                    .iter()
                                    .map(|&(position, midpoint, color)| {
                                        (position, midpoint, color[channel] / 255.)
                                    })
                                    .collect::<Vec<_>>();
                                let tint = (reference(&knots, position) * 65535.).round() / 65535.;
                                native(
                                    ((f64::from(source[channel]) / 65535. * (1. - weight)
                                        + tint * weight)
                                        * 65535.)
                                        .round() as u16,
                                    depth,
                                )
                            })
                        });
                        let mut spec = Spec::paint("Synthetic clamped gradient", SAVED);
                        spec.tags.push((*b"lfx2", payload));
                        let mut bytes = fixture(&[spec], depth);
                        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                        for channel in 0..4 {
                            for pixel in merged {
                                composite_sample(&mut bytes, pixel, channel, depth);
                            }
                        }
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{depth} {case} {reverse} {offset} {enabled}: {:?}",
                            doc.warnings
                        );
                        let source = SAVED.map(|p| p.map(|v| native(v, depth))).concat();
                        assert_eq!(doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(rendered(&doc), merged.concat());
                        let settings = doc.layers[0].effects[0].settings.as_ref().clone();
                        let color_stops = settings["color_stops"].as_array().unwrap();
                        assert_eq!(
                            color_stops.len(),
                            colors.len()
                                + usize::from(colors[0].0 != 0)
                                + usize::from(colors.last().unwrap().0 != 4096)
                        );
                        for (index, &(position, _, color)) in colors.iter().enumerate() {
                            let stop = color_stops
                                .iter()
                                .find(|s| s["position"] == f64::from(position) / 4096.)
                                .unwrap();
                            assert_eq!(
                                stop["midpoint"],
                                colors
                                    .get(index + 1)
                                    .map_or(0.5, |stop| f64::from(stop.1) / 100.)
                            );
                            assert_eq!(stop["color"], json!(color.map(|v| v / 255.)));
                        }
                        let alpha_stops = settings["alpha_stops"].as_array().unwrap();
                        if let Some(alpha) = &alpha {
                            assert_eq!(
                                alpha_stops.len(),
                                alpha.len()
                                    + usize::from(alpha[0].0 != 0)
                                    + usize::from(alpha.last().unwrap().0 != 4096)
                            );
                            for (index, &(position, _, opacity)) in alpha.iter().enumerate() {
                                let stop = alpha_stops
                                    .iter()
                                    .find(|s| s["position"] == f64::from(position) / 4096.)
                                    .unwrap();
                                assert_eq!(
                                    stop["midpoint"],
                                    alpha
                                        .get(index + 1)
                                        .map_or(0.5, |stop| f64::from(stop.1) / 100.)
                                );
                                assert_eq!(stop["opacity"], opacity / 100.);
                            }
                        } else {
                            assert!(alpha_stops.is_empty());
                        }
                        let current = doc.export_png().unwrap();
                        for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                            assert_eq!(saved_composite(&output, depth), merged.concat());
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(*reopened.layers[0].effects[0].settings, settings);
                            assert_eq!(reopened.export_png().unwrap(), current);
                            let standard = psd::decode(&ordinary(&output)).unwrap();
                            assert!(!standard.read_only);
                            assert_eq!(standard.layers[0].pixels.rgba16(), merged.concat());
                            assert_eq!(standard.export_png().unwrap(), current);
                        }
                        let mut engine = Engine::new();
                        let layer = doc.layers[0].id.clone();
                        let effect = doc.layers[0].effects[0].id.clone();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Clamped gradient bypass").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        let at =
                            bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth == 16);
                        bytes[at] ^= 1;
                        let protected = psd::decode(&bytes).unwrap();
                        assert!(protected.read_only);
                        assert!(protected.layers[0].effects.is_empty());
                        assert_eq!(protected.bit_depth, depth);
                        assert!(psd::encode(&protected).is_err());
                        assert!(psd::encode_psb(&protected).is_err());
                    }
                }
            }
        }
    }
}

#[test]
fn photoshop_gradient_clamped_stops_reject_malformed_or_excessive_source_lists() {
    let supported = vec![(0, 50, [31.; 3]), (4096, 50, [211.; 3])];
    for depth in [8, 16] {
        for alpha in [false, true] {
            for variant in 0..10 {
                let mut stops = vec![(1024, 50, [31.; 3]), (3072, 50, [83.; 3])];
                match variant {
                    0 => stops.clear(),
                    1 => stops[0].0 = -1,
                    2 => stops[1].0 = 4097,
                    3 => stops[1].0 = stops[0].0,
                    4 => stops.reverse(),
                    5 => stops[0].1 = 0,
                    6 => stops[0].1 = 100,
                    7 => stops = (0..17).map(|i| (i * 256, 50, [31.; 3])).collect(),
                    8 => stops = (1..=16).map(|i| (i * 240, 50, [31.; 3])).collect(),
                    _ => stops = (0..16).map(|i| (i * 256, 50, [31.; 3])).collect(),
                }
                let alphas = stops
                    .iter()
                    .map(|&(position, midpoint, values)| (position, midpoint, values[0]))
                    .collect::<Vec<_>>();
                let mut payload = custom_gradient_payload(
                    false,
                    None,
                    0.,
                    100.,
                    None,
                    if alpha { &supported } else { &stops },
                    if alpha { Some(&alphas) } else { None },
                );
                let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                payload[at] = 0;
                let mut spec = Spec::paint("Synthetic protected clamped gradient", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
                assert!(doc.read_only, "{depth} {alpha} {variant}");
                assert_eq!(doc.bit_depth, depth);
                assert!(doc.layers[0].effects.is_empty());
                assert_eq!(
                    doc.layers[0].pixels.rgba16(),
                    SAVED.map(|p| p.map(|v| native(v, depth))).concat()
                );
                assert!(psd::encode(&doc).is_err());
                assert!(psd::encode_psb(&doc).is_err());
            }
        }
    }
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
fn midpoint_gradient_payload(reverse: bool, color: i32, alpha: i32) -> Vec<u8> {
    let mut payload = gradient_payload(reverse, None, 0.);
    let midpoints = payload
        .windows(8)
        .enumerate()
        .filter_map(|(at, bytes)| (bytes == b"Mdpnlong").then_some(at + 8))
        .collect::<Vec<_>>();
    assert_eq!(midpoints.len(), 4);
    for (at, value) in midpoints.into_iter().zip([50, color, 50, alpha]) {
        payload[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }
    // Independent transparency ramp, separate from the RGB midpoint.
    let at = payload
        .windows(12)
        .enumerate()
        .filter_map(|(at, v)| (v == b"OpctUntF#Prc").then_some(at + 12))
        .nth(1)
        .unwrap();
    payload[at..at + 8].copy_from_slice(&0f64.to_be_bytes());
    payload
}
#[test]
fn photoshop_gradient_midpoints_retain_native_sources_independent_coverage_and_saves() {
    // Expected values come from two line segments meeting at (midpoint, 0.5),
    // independently of the shared gradient sampler and PSD descriptor reader.
    let ramp = |position: f64, midpoint: i32| {
        let midpoint = f64::from(midpoint) / 100.;
        if position <= midpoint {
            position / midpoint / 2.
        } else {
            0.5 + (position - midpoint) / (1. - midpoint) / 2.
        }
    };
    for depth in [8, 16] {
        for (color, alpha) in [(1, 99), (25, 75), (75, 25), (99, 1)] {
            for reverse in [false, true] {
                for flags in [
                    [true; 3],
                    [false, true, true],
                    [true, false, true],
                    [true, true, false],
                ] {
                    for opacity in [37f64, 100.] {
                        let mut payload = midpoint_gradient_payload(reverse, color, alpha);
                        for (key, enabled) in [
                            (b"masterFXSwitchbool".as_slice(), flags[0]),
                            (b"enabbool".as_slice(), flags[1]),
                            (b"presentbool".as_slice(), flags[2]),
                        ] {
                            let at = payload.windows(key.len()).position(|v| v == key).unwrap()
                                + key.len();
                            payload[at] = u8::from(enabled);
                        }
                        let at = payload
                            .windows(12)
                            .position(|v| v == b"OpctUntF#Prc")
                            .unwrap()
                            + 12;
                        payload[at..at + 8].copy_from_slice(&opacity.to_be_bytes());
                        let enabled = flags.into_iter().all(|flag| flag);
                        let expected = std::array::from_fn::<_, 2, _>(|x| {
                            let source = SAVED[x].map(|n| native(n, depth));
                            if !enabled {
                                return source;
                            }
                            let position = if reverse {
                                1. - (x as f64 + 0.5) / 2.
                            } else {
                                (x as f64 + 0.5) / 2.
                            };
                            let tint = (ramp(position, color) * 65535.).round();
                            // Coverage enters blending as a native alpha sample.
                            let weight = (ramp(position, alpha) * (opacity / 100.) * 65535.)
                                .round()
                                / 65535.;
                            std::array::from_fn(|c| {
                                if c == 3 {
                                    65535
                                } else {
                                    native(
                                        (((1. - weight) * (f64::from(source[c]) / 65535.)
                                            + weight * (tint / 65535.))
                                            * 65535.)
                                            .round() as u16,
                                        depth,
                                    )
                                }
                            })
                        });
                        let mut spec = Spec::paint("Synthetic midpoint gradient", SAVED);
                        spec.tags.push((*b"lfx2", payload));
                        let mut bytes = fixture(&[spec], depth);
                        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                        for c in 0..4 {
                            for pixel in expected {
                                composite_sample(&mut bytes, pixel, c, depth);
                            }
                        }
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{depth} {color} {alpha} {reverse} {flags:?} {opacity}: {:?}",
                            doc.warnings
                        );
                        let settings = doc.layers[0].effects[0].settings.as_ref();
                        assert_eq!(
                            settings["color_stops"][0]["midpoint"],
                            f64::from(color) / 100.
                        );
                        assert_eq!(
                            settings["alpha_stops"][0]["midpoint"],
                            f64::from(alpha) / 100.
                        );
                        assert_eq!(settings["alpha_stops"][0]["opacity"], 0.);
                        assert_eq!(doc.layers[0].effects[0].enabled, enabled);
                        let source = SAVED.map(|p| p.map(|n| native(n, depth))).concat();
                        assert_eq!(doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(rendered(&doc), expected.concat());
                        let current = doc.export_png().unwrap();
                        for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                            assert_eq!(saved_composite(&output, depth), expected.concat());
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(*reopened.layers[0].effects[0].settings, *settings);
                            assert_eq!(reopened.export_png().unwrap(), current);
                            assert_eq!(
                                psd::decode(&ordinary(&output))
                                    .unwrap()
                                    .export_png()
                                    .unwrap(),
                                current
                            );
                        }
                        let mut engine = Engine::new();
                        let layer = doc.layers[0].id.clone();
                        let effect = doc.layers[0].effects[0].id.clone();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Gradient bypass").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        let at =
                            bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth / 8) - 1;
                        bytes[at] ^= 1;
                        assert!(
                            psd::decode(&bytes).unwrap().read_only,
                            "midpoint composite mismatch was accepted"
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn photoshop_gradient_midpoints_reject_invalid_values_and_types_even_when_disabled() {
    for depth in [8, 16] {
        for disabled in [false, true] {
            for alpha in [false, true] {
                for value in [-1, 0, 100, i32::MAX] {
                    let mut payload = midpoint_gradient_payload(
                        false,
                        if alpha { 25 } else { value },
                        if alpha { value } else { 75 },
                    );
                    if disabled {
                        let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                        payload[at] = 0;
                    }
                    let mut spec = Spec::paint("Synthetic invalid midpoint", SAVED);
                    spec.tags.push((*b"lfx2", payload));
                    assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
                }
                for (kind, value) in [
                    (b"doub", 25f64.to_be_bytes().to_vec()),
                    (b"bool", vec![1]),
                    (
                        b"UntF",
                        [b"#Prc".to_vec(), 25f64.to_be_bytes().to_vec()].concat(),
                    ),
                ] {
                    let mut payload = midpoint_gradient_payload(false, 25, 75);
                    let matches = payload
                        .windows(8)
                        .enumerate()
                        .filter_map(|(at, v)| (v == b"Mdpnlong").then_some(at))
                        .collect::<Vec<_>>();
                    let at = matches[if alpha { 2 } else { 0 }] + 4;
                    payload.splice(at..at + 8, [kind.to_vec(), value].concat());
                    if disabled {
                        let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                        payload[at] = 0;
                    }
                    let mut spec = Spec::paint("Synthetic invalid midpoint type", SAVED);
                    spec.tags.push((*b"lfx2", payload));
                    assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
                }
            }
        }
    }
}

#[test]
fn photoshop_gradient_destination_midpoints_ignore_only_valid_first_stop_ratios() {
    for depth in [8, 16] {
        let mut baseline = None;
        for (color_first, alpha_first) in [(1i32, 99i32), (99, 1), (17, 83)] {
            let mut payload = midpoint_gradient_payload(false, 75, 25);
            let offsets = payload
                .windows(8)
                .enumerate()
                .filter_map(|(at, v)| (v == b"Mdpnlong").then_some(at + 8))
                .collect::<Vec<_>>();
            payload[offsets[0]..offsets[0] + 4].copy_from_slice(&color_first.to_be_bytes());
            payload[offsets[2]..offsets[2] + 4].copy_from_slice(&alpha_first.to_be_bytes());
            let expected = std::array::from_fn::<_, 2, _>(|x| {
                let source = SAVED[x].map(|v| native(v, depth));
                let tint = (65535f64 * if x == 0 { 1. / 6. } else { 0.5 }).round();
                let alpha = (65535f64
                    * if x == 0 {
                        0.5
                    } else {
                        0.5 + (0.75 - 0.25) / (1. - 0.25) / 2.
                    })
                .round()
                    / 65535.;
                std::array::from_fn(|c| {
                    if c == 3 {
                        65535
                    } else {
                        native(
                            (f64::from(source[c]) * (1. - alpha) + tint * alpha).round() as u16,
                            depth,
                        )
                    }
                })
            });
            let mut spec = Spec::paint("Synthetic destination midpoints", SAVED);
            spec.tags.push((*b"lfx2", payload.clone()));
            let mut bytes = fixture(&[spec], depth);
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for c in 0..4 {
                for pixel in expected {
                    composite_sample(&mut bytes, pixel, c, depth);
                }
            }
            let doc = psd::decode(&bytes).unwrap();
            assert!(
                !doc.read_only,
                "{depth} {color_first} {alpha_first}: {:?}",
                doc.warnings
            );
            assert_eq!(rendered(&doc), expected.concat());
            assert_eq!(
                doc.layers[0].effects[0].settings["color_stops"][0]["midpoint"],
                0.75
            );
            assert_eq!(
                doc.layers[0].effects[0].settings["alpha_stops"][0]["midpoint"],
                0.25
            );
            assert_eq!(
                doc.layers[0].effects[0].settings["color_stops"][1]["midpoint"],
                0.5
            );
            let current = doc.export_png().unwrap();
            if let Some(before) = &baseline {
                assert_eq!(&current, before);
            } else {
                baseline = Some(current);
            }
            // An unused field is still validated, never a loophole for an
            // unsupported definition whose initial appearance happens to match.
            for index in [0, 2] {
                let mut malformed = payload.clone();
                malformed[offsets[index]..offsets[index] + 4].copy_from_slice(&0i32.to_be_bytes());
                let at = malformed.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                malformed[at] = 0;
                let mut invalid = Spec::paint("Synthetic invalid unused midpoint", SAVED);
                invalid.tags.push((*b"lfx2", malformed));
                assert!(psd::decode(&fixture(&[invalid], depth)).unwrap().read_only);
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
            if enabled[0] {
                pixel[..3].copy_from_slice(&[111 * 257, 67 * 257, 231 * 257]);
            }
        }
    }
    if mismatch {
        merged[0][0] = merged[0][0].saturating_add(if depth == 16 { 1 } else { 257 });
    }
    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
    for channel in 0..4 {
        for pixel in merged {
            composite_sample(&mut bytes, pixel, channel, depth);
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
                    ["pattern_overlay", "gradient_overlay", "color_overlay"]
                );
                assert_eq!(
                    effects.iter().map(|e| e.enabled).collect::<Vec<_>>(),
                    [enabled[1], enabled[2], enabled[0]].map(|v| v && master)
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
                engine.resident_state_mut().expect("resident fixture").document = doc;
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                let mut settings = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings.as_ref().clone();
                settings["phase_x"] = json!(1.);
                engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":settings})], None, None, "Mixed pattern phase").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                for bytes in [
                    psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
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

#[test]
fn photoshop_modern_normal_overlay_ids_retain_family_order_and_strict_protection() {
    // Photoshop's overlay-zorder calibration has an opaque red Color Overlay
    // above a Gradient Overlay and above a Pattern Overlay. Its saved samples
    // are red in both overlap panels. Use independent synthetic native-depth
    // sources here; the external file also has unsupported disabled styles.
    for depth in [8, 16] {
        for master in [false, true] {
            let mut payload = mixed_overlay_payload([true; 3], master);
            while payload.windows(12).any(|v| v == b"BlnM\0\0\0\0Nrml") {
                let at = payload
                    .windows(12)
                    .position(|v| v == b"BlnM\0\0\0\0Nrml")
                    .unwrap()
                    + 4;
                let mut modern = vec![];
                overlay_id(&mut modern, "normal");
                payload.splice(at..at + 8, modern);
            }
            let reference = mixed_overlay_fixture(depth, [true; 3], master, false);
            let mut spec = Spec::paint("Synthetic modern normal overlays", SAVED);
            spec.tags.push((*b"lfx2", payload.clone()));
            let mut bytes = inject_pattern(
                fixture(&[spec], depth),
                depth,
                &pattern_resource(depth, "synthetic-pattern"),
                false,
            );
            let composite_len = 8 * usize::from(depth / 8);
            bytes.truncate(bytes.len() - composite_len);
            bytes.extend_from_slice(&reference[reference.len() - composite_len..]);
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{depth} {master}: {:?}", doc.warnings);
            assert_eq!(
                doc.layers[0]
                    .effects
                    .iter()
                    .map(|e| e.kind.as_str())
                    .collect::<Vec<_>>(),
                ["pattern_overlay", "gradient_overlay", "color_overlay"]
            );
            assert!(doc.layers[0]
                .effects
                .iter()
                .all(|e| e.settings["blend"] == "normal" && e.enabled == master));
            let source = doc.layers[0].pixels.rgba16();
            assert_eq!(source, SAVED.map(|p| p.map(|v| native(v, depth))).concat());
            let current = doc.export_png().unwrap();
            for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                let reopened = psd::decode(&output).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                assert_eq!(reopened.export_png().unwrap(), current);
                assert_eq!(
                    saved_composite(&output, depth),
                    saved_composite(&bytes, depth)
                );
            }
            // Full IDs are exact, not case-insensitive guesses, even disabled.
            let at = payload.windows(6).position(|v| v == b"normal").unwrap();
            payload[at] = b'N';
            let mut unsupported = Spec::paint("Synthetic unsupported modern enum", SAVED);
            unsupported.tags.push((*b"lfx2", payload));
            let protected = psd::decode(&inject_pattern(
                fixture(&[unsupported], depth),
                depth,
                &pattern_resource(depth, "synthetic-pattern"),
                false,
            ))
            .unwrap();
            assert!(protected.read_only);
            assert!(psd::encode(&protected).is_err());
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
            composite_sample(&mut bytes, pixel, channel, depth);
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
fn photoshop_inversion_styles_preserve_sources_composites_and_undo() {
    assert_photoshop_style_blends(
        &[("Dfrn", "difference"), ("Xclu", "exclusion")],
        &["color", "pattern", "gradient", "inner"],
        SAVED,
        [111., 67., 231.],
    );
}

const ADDITIONAL_STYLE_BLENDS: [(&str, &str); 8] = [
    ("CBrn", "color_burn"),
    ("linearBurn", "linear_burn"),
    ("CDdg", "color_dodge"),
    ("linearDodge", "linear_dodge"),
    ("SftL", "soft_light"),
    ("HrdL", "hard_light"),
    ("Sbtr", "subtract"),
    ("divide", "divide"),
];

fn set_style_blend(payload: &mut Vec<u8>, mode: &str) {
    let at = payload.windows(4).position(|v| v == b"BlnM").unwrap() + 4;
    assert_eq!(&payload[at..at + 8], b"\0\0\0\0Nrml");
    let mut encoded = vec![];
    overlay_id(&mut encoded, mode);
    payload.splice(at..at + 8, encoded);
}

fn append_style_root_field(payload: &mut Vec<u8>, key: &str, kind: &[u8; 4], value: &[u8]) {
    let count_at = payload.windows(4).position(|v| v == b"Lefx").unwrap() + 4;
    let count = u32::from_be_bytes(payload[count_at..count_at + 4].try_into().unwrap());
    payload[count_at..count_at + 4].copy_from_slice(&(count + 1).to_be_bytes());
    overlay_id(payload, key);
    payload.extend_from_slice(kind);
    payload.extend_from_slice(value);
}

#[test]
fn photoshop_modifying_effect_counts_validate_hidden_stacks_without_losing_sources() {
    for depth in [8, 16] {
        for flags in 0u8..8 {
            let enabled = [flags & 1 != 0, flags & 2 != 0, flags & 4 != 0];
            for master in [false, true] {
                let mut payload = mixed_overlay_payload(enabled, master);
                append_style_root_field(
                    &mut payload,
                    "numModifyingFX",
                    b"long",
                    &flags.count_ones().to_be_bytes(),
                );
                let reference = mixed_overlay_fixture(depth, enabled, master, false);
                let mut spec = Spec::paint("Synthetic counted overlays", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let mut bytes = inject_pattern(
                    fixture(&[spec], depth),
                    depth,
                    &pattern_resource(depth, "synthetic-pattern"),
                    false,
                );
                let n = 8 * usize::from(depth / 8);
                bytes.truncate(bytes.len() - n);
                bytes.extend_from_slice(&reference[reference.len() - n..]);
                let doc = psd::decode(&bytes).unwrap();
                assert!(
                    !doc.read_only,
                    "{depth} {flags} {master}: {:?}",
                    doc.warnings
                );
                let source = SAVED.map(|p| p.map(|v| native(v, depth))).concat();
                assert_eq!(doc.layers[0].pixels.rgba16(), source);
                let definitions = doc.layers[0].effects.clone();
                let current = doc.export_png().unwrap();
                for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    let reopened = psd::decode(&output).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                    assert_eq!(reopened.export_png().unwrap(), current);
                    assert_eq!(
                        saved_composite(&output, depth),
                        saved_composite(&bytes, depth)
                    );
                    assert_eq!(
                        serde_json::to_value(&reopened.layers[0].effects).unwrap(),
                        serde_json::to_value(&definitions).unwrap()
                    );
                }
            }
        }
        // The stored count is independent of master-off presentation. It
        // counts every enabled definition, including multi entries.
        for flags in 0u8..16 {
            let enabled = std::array::from_fn(|i| flags & (1 << i) != 0);
            let mut payload = multi_overlay_payload(enabled, false, 16, 16, false);
            append_style_root_field(
                &mut payload,
                "numModifyingFX",
                b"long",
                &(flags.count_ones() * 8).to_be_bytes(),
            );
            let mut spec = Spec::paint("Synthetic counted hidden lists", SAVED);
            spec.tags.push((*b"lmfx", payload));
            let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
            assert!(!doc.read_only, "{depth} {flags}: {:?}", doc.warnings);
            assert_eq!(doc.layers[0].effects.len(), 32);
            assert!(doc.layers[0].effects.iter().all(|e| !e.enabled));
        }
        for flags in 0u8..32 {
            for list in [false, true] {
                let enabled = std::array::from_fn(|i| flags & (1 << i) != 0);
                let mut payload = mixed_shadow_overlay_payload(enabled, false, list, "");
                append_style_root_field(
                    &mut payload,
                    "numModifyingFX",
                    b"long",
                    &flags.count_ones().to_be_bytes(),
                );
                let mut spec = Spec::paint("Synthetic counted hidden shadows", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let doc = psd::decode(&inject_pattern(
                    fixture(&[spec], depth),
                    depth,
                    &pattern_resource(depth, "synthetic-pattern"),
                    false,
                ))
                .unwrap();
                assert!(!doc.read_only, "{depth} {flags} {list}: {:?}", doc.warnings);
                assert_eq!(doc.layers[0].effects.len(), 5);
            }
        }
        for master in [false, true] {
            let mut payload = overlay_payload(master, true, [111., 67., 231.]);
            let at = payload
                .windows(11)
                .position(|v| v == b"presentbool")
                .unwrap()
                + 11;
            payload[at] = 0;
            append_style_root_field(&mut payload, "numModifyingFX", b"long", &1i32.to_be_bytes());
            let mut spec = Spec::paint("Synthetic enabled unpresent counted style", SAVED);
            spec.tags.push((*b"lfx2", payload));
            let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
            assert!(!doc.read_only, "{depth} {master}: {:?}", doc.warnings);
            assert!(!doc.layers[0].effects[0].enabled);
        }
    }
}

#[test]
fn photoshop_modifying_effect_counts_protect_inconsistent_malformed_and_unsupported_definitions() {
    for depth in [8, 16] {
        for master in [false, true] {
            for (kind, value) in [
                (b"long", (-1i32).to_be_bytes().to_vec()),
                (b"long", 1i32.to_be_bytes().to_vec()),
                (b"long", 33i32.to_be_bytes().to_vec()),
                (b"doub", 0f64.to_be_bytes().to_vec()),
                (b"bool", vec![0]),
            ] {
                let mut payload = overlay_payload(master, false, [111., 67., 231.]);
                append_style_root_field(&mut payload, "numModifyingFX", kind, &value);
                let mut spec = Spec::paint("Synthetic inconsistent effect count", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
                assert!(doc.read_only);
                assert_eq!(doc.bit_depth, depth);
                assert_eq!(
                    doc.layers[0].pixels.rgba16(),
                    SAVED.map(|p| p.map(|v| native(v, depth))).concat()
                );
                assert!(doc.layers[0].effects.is_empty());
                assert!(psd::encode(&doc).is_err());
            }
            let mut payload = overlay_payload(master, false, [111., 67., 231.]);
            append_style_root_field(&mut payload, "numModifyingFX", b"long", &0i32.to_be_bytes());
            append_style_root_field(&mut payload, "unknownStyle", b"bool", &[0]);
            let mut spec = Spec::paint("Synthetic unsupported counted style", SAVED);
            spec.tags.push((*b"lfx2", payload));
            assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
        }
    }
}

// Independent normalized-channel expectations, including source/backdrop endpoint
// precedence and the Photoshop Soft Light low-backdrop polynomial.
fn expected_style_blend(base: f64, tint: f64, blend: &str) -> f64 {
    match blend {
        "normal" => tint,
        "multiply" => base * tint,
        "screen" => 1. - (1. - base) * (1. - tint),
        "overlay" if base < 0.5 => 2. * base * tint,
        "overlay" => 1. - 2. * (1. - base) * (1. - tint),
        "darken" => base.min(tint),
        "lighten" => base.max(tint),
        "difference" => (base - tint).abs(),
        "exclusion" => base + tint - 2. * base * tint,
        "linear_burn" => (base + tint - 1.).max(0.),
        "linear_dodge" => (base + tint).min(1.),
        "subtract" => (base - tint).max(0.),
        "divide" if base == 0. => 0.,
        "divide" if tint == 0. => 1.,
        "divide" => (base / tint).min(1.),
        "color_dodge" if base == 0. => 0.,
        "color_dodge" if tint == 1. => 1.,
        "color_dodge" => (base / (1. - tint)).min(1.),
        "color_burn" if base == 1. => 1.,
        "color_burn" if tint == 0. => 0.,
        "color_burn" => 1. - ((1. - base) / tint).min(1.),
        "hard_light" if tint <= 0.5 => 2. * base * tint,
        "hard_light" => 1. - 2. * (1. - base) * (1. - tint),
        "soft_light" if tint <= 0.5 => base - (1. - 2. * tint) * base * (1. - base),
        "soft_light" => {
            let lifted = if base <= 0.25 {
                base * (4. + base * (-12. + 16. * base))
            } else {
                base.sqrt()
            };
            base + (2. * tint - 1.) * (lifted - base)
        }
        _ => panic!("Missing independent style blend expectation: {blend}"),
    }
}

#[test]
fn photoshop_additional_style_blends_preserve_native_sources_composites_and_undo() {
    assert_photoshop_style_blends(
        &ADDITIONAL_STYLE_BLENDS,
        &["color", "pattern", "gradient", "inner"],
        SAVED,
        [111., 67., 231.],
    );
    for color in [[0., 255., 127.5], [255., 0., 127.5]] {
        assert_photoshop_style_blends(
            &ADDITIONAL_STYLE_BLENDS,
            &["color"],
            [[0, 65535, 16384, 65535], [65535, 0, 32768, 65535]],
            color,
        );
    }
}

#[test]
fn photoshop_canonical_string_blends_preserve_sources_composites_and_undo() {
    assert_photoshop_style_blends(
        &[
            ("normal", "normal"),
            ("multiply", "multiply"),
            ("screen", "screen"),
            ("overlay", "overlay"),
            ("darken", "darken"),
            ("lighten", "lighten"),
            ("difference", "difference"),
            ("exclusion", "exclusion"),
            ("colorBurn", "color_burn"),
            ("colorDodge", "color_dodge"),
            ("softLight", "soft_light"),
            ("hardLight", "hard_light"),
            ("subtract", "subtract"),
        ],
        &["color", "pattern", "gradient", "inner"],
        SAVED,
        [111., 67., 231.],
    );
}

#[test]
fn photoshop_blend_id_forms_do_not_unlock_malformed_disabled_or_exterior_styles() {
    for depth in [8, 16] {
        for family in ["color", "pattern", "gradient", "inner", "drop"] {
            for (value, class, string_id) in [
                ("Mltp", "BlnM", true),
                ("Scrn", "BlnM", true),
                ("Ovrl", "BlnM", true),
                ("Nrml", "BlnM", true),
                ("Dfrn", "BlnM", true),
                ("CBrn", "BlnM", true),
                ("multiply", "????", true),
                ("Multiply", "BlnM", true),
                ("soft_light", "BlnM", true),
                ("dissolve", "BlnM", true),
                ("Norm", "BlnM", false),
            ] {
                let mut payload = match family {
                    "color" => overlay_payload(false, false, [111., 67., 231.]),
                    "pattern" => pattern_payload(false, "synthetic-pattern", "", 100.),
                    "gradient" => gradient_payload(false, None, 0.),
                    "inner" => hard_shadow_payload("IrSh", false, false, 180., 37., ""),
                    _ => hard_shadow_payload("DrSh", false, false, 180., 37., ""),
                };
                let at = payload
                    .windows(16)
                    .position(|v| v == b"\0\0\0\0BlnM\0\0\0\0Nrml")
                    .unwrap();
                let mut encoded = vec![];
                overlay_id(&mut encoded, class);
                encoded.extend_from_slice(
                    &(if string_id { value.len() as u32 } else { 0 }).to_be_bytes(),
                );
                encoded.extend_from_slice(value.as_bytes());
                payload.splice(at..at + 16, encoded);
                if family == "gradient" {
                    let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                    payload[at] = 0;
                }
                let mut spec = Spec::paint("Synthetic unsupported blend ID form", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let mut bytes = fixture(&[spec], depth);
                if family == "pattern" {
                    bytes = inject_pattern(
                        bytes,
                        depth,
                        &pattern_resource(depth, "synthetic-pattern"),
                        false,
                    );
                }
                let doc = psd::decode(&bytes).unwrap();
                assert!(
                    doc.read_only,
                    "{depth} {family} {value} {class} {string_id}"
                );
                assert_eq!(doc.bit_depth, depth);
                assert_eq!(
                    doc.layers[0].pixels.rgba16(),
                    SAVED.map(|p| p.map(|v| native(v, depth))).concat()
                );
                assert!(doc.layers[0].effects.is_empty());
                assert!(psd::encode(&doc).is_err());
            }
            if family == "drop" {
                let mut payload = hard_shadow_payload("DrSh", false, false, 180., 37., "");
                set_style_blend(&mut payload, "multiply");
                let mut spec = Spec::paint("Synthetic unsupported exterior string blend", SAVED);
                spec.tags.push((*b"lfx2", payload));
                assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
            }
        }
    }
}

fn assert_photoshop_style_blends(
    modes: &[(&str, &str)],
    families: &[&str],
    pixels: [[u16; 4]; 2],
    color: [f64; 3],
) {
    for depth in [8, 16] {
        for &family in families {
            for enabled in [false, true] {
                for opacity in [37f64, 100.] {
                    for &(mode, blend) in modes {
                        let mut payload = match family {
                            "color" => overlay_payload(true, enabled, color),
                            "pattern" => pattern_payload(enabled, "synthetic-pattern", "", 100.),
                            "gradient" => gradient_payload(false, None, 0.),
                            _ => hard_shadow_payload("IrSh", enabled, true, 180., opacity, ""),
                        };
                        set_style_blend(&mut payload, mode);
                        let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                        payload[at] = u8::from(enabled);
                        let at = payload
                            .windows(12)
                            .position(|v| v == b"OpctUntF#Prc")
                            .unwrap()
                            + 12;
                        payload[at..at + 8].copy_from_slice(&opacity.to_be_bytes());
                        let mut spec = Spec::paint("Synthetic supported blend style", pixels);
                        spec.tags.push((*b"lfx2", payload));
                        let mut bytes = fixture(&[spec], depth);
                        if family == "pattern" {
                            bytes = inject_pattern(
                                bytes,
                                depth,
                                &pattern_resource(depth, "synthetic-pattern"),
                                false,
                            );
                        }
                        let mut merged = pixels.map(|p| p.map(|v| native(v, depth)));
                        if enabled {
                            let coverage = (65535. * (opacity / 100.)).round() / 65535.;
                            for x in 0..2 {
                                if family == "inner" && x == 1 {
                                    continue;
                                }
                                for c in 0..3 {
                                    let tint = match family {
                                        "pattern" => native(PATTERN[x][c], depth),
                                        "gradient" => {
                                            if x == 0 {
                                                16384
                                            } else {
                                                49151
                                            }
                                        }
                                        "color" => (color[c] / 255. * 65535.).round() as u16,
                                        _ => [111 * 257, 67 * 257, 231 * 257][c],
                                    };
                                    let base = f64::from(merged[x][c]) / 65535.;
                                    let tint = f64::from(tint) / 65535.;
                                    let color = expected_style_blend(base, tint, blend);
                                    merged[x][c] = native(
                                        ((base * (1. - coverage) + color * coverage) * 65535.)
                                            .round() as u16,
                                        depth,
                                    );
                                }
                            }
                        }
                        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                        for c in 0..4 {
                            for pixel in merged {
                                composite_sample(&mut bytes, pixel, c, depth);
                            }
                        }
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{depth} {family} {enabled} {opacity} {blend}: {:?}",
                            doc.warnings
                        );
                        assert_eq!(rendered(&doc), merged.concat());
                        assert_eq!(doc.layers[0].effects[0].settings["blend"], blend);
                        assert_eq!(doc.layers[0].effects[0].enabled, enabled);
                        let source = pixels.map(|p| p.map(|v| native(v, depth))).concat();
                        assert_eq!(doc.layers[0].pixels.rgba16(), source);
                        let current = doc.export_png().unwrap();
                        let settings = doc.layers[0].effects[0].settings.as_ref().clone();
                        let mut engine = Engine::new();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                        let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Inversion style bypass").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        for output in [
                            psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        ] {
                            assert_eq!(saved_composite(&output, depth), merged.concat());
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(reopened.export_png().unwrap(), current);
                            assert_eq!(
                                reopened.layers[0].effects[0].settings["blend"],
                                settings["blend"]
                            );
                            let standard = psd::decode(&ordinary(&output)).unwrap();
                            assert!(!standard.read_only);
                            assert_eq!(standard.export_png().unwrap(), current);
                        }
                        let at =
                            bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth == 16);
                        bytes[at] ^= 1;
                        let protected = psd::decode(&bytes).unwrap();
                        assert!(protected.read_only);
                        assert!(protected.layers[0].effects.is_empty());
                        assert_eq!(protected.bit_depth, depth);
                    }
                }
            }
        }
    }
}

#[test]
fn inversion_style_blends_do_not_unlock_unknown_or_independent_exterior_definitions() {
    for depth in [8, 16] {
        for family in ["color", "pattern", "gradient", "inner", "drop"] {
            for mode in [b"Dfrn", b"Xclu", b"????"] {
                if family != "drop" && mode != b"????" {
                    continue;
                }
                // Disabled definitions match the original composite, so this
                // proves descriptor protection rather than a pixel mismatch.
                let mut payload = match family {
                    "color" => overlay_payload(true, false, [111., 67., 231.]),
                    "pattern" => pattern_payload(false, "synthetic-pattern", "", 100.),
                    "gradient" => gradient_payload(false, None, 0.),
                    "inner" => hard_shadow_payload("IrSh", false, true, 180., 100., ""),
                    _ => hard_shadow_payload("DrSh", false, true, 180., 100., ""),
                };
                let at = payload.windows(4).position(|v| v == b"Nrml").unwrap();
                payload[at..at + 4].copy_from_slice(mode);
                let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                payload[at] = 0;
                let mut spec = Spec::paint("Synthetic protected inversion style", SAVED);
                spec.tags.push((*b"lfx2", payload));
                let mut bytes = fixture(&[spec], depth);
                if family == "pattern" {
                    bytes = inject_pattern(
                        bytes,
                        depth,
                        &pattern_resource(depth, "synthetic-pattern"),
                        false,
                    );
                }
                let doc = psd::decode(&bytes).unwrap();
                assert!(doc.read_only, "{depth} {family} {mode:?}");
                assert!(doc.layers[0].effects.is_empty());
                assert_eq!(
                    doc.layers[0].pixels.rgba16(),
                    SAVED.map(|p| p.map(|v| native(v, depth))).concat()
                );
                assert!(psd::encode(&doc).is_err());
                assert!(psd::encode_psb(&doc).is_err());
            }
        }
    }
}

#[test]
fn photoshop_additional_style_blends_keep_exterior_and_malformed_definitions_protected() {
    for depth in [8, 16] {
        for family in ["color", "pattern", "gradient", "inner", "drop"] {
            for (mode, _) in ADDITIONAL_STYLE_BLENDS {
                for variant in 0..4 {
                    if variant == 0 && family != "drop" {
                        continue;
                    }
                    // Disabled effects already match the stored source. Reject
                    // unsupported backdrop handling and malformed enum metadata
                    // before the whole-composite comparison can authorize editing.
                    let mut payload = match family {
                        "color" => overlay_payload(true, false, [111., 67., 231.]),
                        "pattern" => pattern_payload(false, "synthetic-pattern", "", 100.),
                        "gradient" => gradient_payload(false, None, 0.),
                        "inner" => hard_shadow_payload("IrSh", false, true, 180., 100., ""),
                        _ => hard_shadow_payload("DrSh", false, true, 180., 100., ""),
                    };
                    set_style_blend(&mut payload, mode);
                    let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                    payload[at] = 0;
                    match variant {
                        1 => {
                            let at = payload.windows(4).position(|v| v == b"BlnM").unwrap();
                            payload[at..at + 4].copy_from_slice(b"????");
                        }
                        2 => {
                            let at = payload.windows(4).position(|v| v == b"BlnM").unwrap() - 8;
                            assert_eq!(&payload[at..at + 4], b"enum");
                            payload[at..at + 4].copy_from_slice(b"doub");
                        }
                        3 => set_style_blend_to_unknown(&mut payload, mode),
                        _ => {}
                    }
                    let mut spec = Spec::paint("Synthetic protected style blend", SAVED);
                    spec.tags.push((*b"lfx2", payload));
                    let mut bytes = fixture(&[spec], depth);
                    if family == "pattern" {
                        bytes = inject_pattern(
                            bytes,
                            depth,
                            &pattern_resource(depth, "synthetic-pattern"),
                            false,
                        );
                    }
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(doc.read_only, "{depth} {family} {mode} {variant}");
                    assert_eq!(doc.bit_depth, depth);
                    assert!(doc.layers[0].effects.is_empty());
                    assert_eq!(
                        doc.layers[0].pixels.rgba16(),
                        SAVED.map(|p| p.map(|v| native(v, depth))).concat()
                    );
                    assert!(psd::encode(&doc).is_err());
                    assert!(psd::encode_psb(&doc).is_err());
                }
            }
        }
    }
}

fn set_style_blend_to_unknown(payload: &mut [u8], mode: &str) {
    let at = payload
        .windows(mode.len())
        .position(|v| v == mode.as_bytes())
        .unwrap();
    payload[at] = b'?';
}

#[test]
fn unsupported_or_mismatched_pattern_and_gradient_blends_keep_sources_protected() {
    for depth in [8, 16] {
        for pattern in [false, true] {
            for mode in [
                b"Nrml", b"Mltp", b"Scrn", b"Ovrl", b"Drkn", b"Lghn", b"????",
            ] {
                let mut bytes = blended_overlay_fixture(depth, pattern, mode, 50.);
                // One native-word difference must never unlock a styled source.
                let at = bytes.len() - 8 * usize::from(depth / 8);
                if mode != b"????" {
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
                            [if x == 0 { 16384 } else { 49151 }; 3],
                            [if x == 0 { 49151 } else { 16384 }; 3],
                            [111 * 257, 67 * 257, 231 * 257],
                            [31 * 257, 197 * 257, 83 * 257],
                        ];
                        for (active, tint) in [enabled[2], enabled[3], enabled[0], enabled[1]]
                            .into_iter()
                            .zip(colors)
                        {
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
                        composite_sample(&mut bytes, pixel, channel, depth);
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
                        "gradient_overlay",
                        "gradient_overlay",
                        "color_overlay",
                        "color_overlay"
                    ]
                );
                assert_eq!(
                    doc.layers[0]
                        .effects
                        .iter()
                        .map(|e| e.enabled)
                        .collect::<Vec<_>>(),
                    [enabled[2], enabled[3], enabled[0], enabled[1]].map(|v| v && master)
                );
                assert_eq!(
                    doc.layers[0].effects[2].settings["color"],
                    json!([111. / 255., 67. / 255., 231. / 255.])
                );
                assert_eq!(
                    doc.layers[0].effects[3].settings["color"],
                    json!([31. / 255., 197. / 255., 83. / 255.])
                );
                assert_eq!(doc.layers[0].effects[0].settings["reverse"], false);
                assert_eq!(doc.layers[0].effects[1].settings["reverse"], true);
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
                engine.resident_state_mut().expect("resident fixture").document = doc;
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"effect.reorder","layer":layer,"effect":effect,"index":3})],
                        None,
                        None,
                        "Reorder imported overlays",
                    )
                    .unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                for output in [
                    psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
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
                // Hard Light is now supported; Hue remains outside this subset.
                payload[at..at + 4].copy_from_slice(b"H   ");
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
                            composite_sample(&mut bytes, pixel, channel, depth);
                        }
                    }
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(
                        !doc.read_only,
                        "{depth} {masked} {opacity} {pattern}: {:?}",
                        doc.warnings
                    );
                    assert_eq!(doc.layers[0].opacity, f32::from(opacity) / 255.);
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
                    engine.resident_state_mut().expect("resident fixture").document = doc;
                    let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                    engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":false})], None, None, "Bypass masked overlay").unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                    assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.layers[0]
                            .mask
                            .as_ref()
                            .map(|m| m.steps[1].pixels.rgba16()),
                        mask
                    );
                    engine.undo("human").unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                    for output in [
                        psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
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
                        assert_eq!(reopened.layers[0].opacity, f32::from(opacity) / 255.);
                        assert_eq!(reopened.export_png().unwrap(), current);
                    }
                }
            }
        }
    }
}

fn solid_stroke_payload(position: &str, enabled: bool, master: bool, variant: &str) -> Vec<u8> {
    let unit = |unit: &[u8; 4], n: f64| [unit.to_vec(), n.to_be_bytes().to_vec()].concat();
    let enumeration = |class: &str, value: &str| {
        let mut out = vec![];
        overlay_id(&mut out, class);
        overlay_id(&mut out, value);
        out
    };
    let mut fields = vec![
        ("enab", b"bool", vec![u8::from(enabled)]),
        ("present", b"bool", vec![u8::from(variant != "absent")]),
        ("showInDialog", b"bool", vec![1]),
        ("overprint", b"bool", vec![u8::from(variant == "overprint")]),
        ("Styl", b"enum", enumeration("FStl", position)),
        (
            "PntT",
            b"enum",
            enumeration(
                "FrFl",
                if variant == "gradient" {
                    "GrFl"
                } else {
                    "SClr"
                },
            ),
        ),
        (
            "Md  ",
            b"enum",
            enumeration(
                "BlnM",
                if variant == "multiply" {
                    "Mltp"
                } else {
                    "Nrml"
                },
            ),
        ),
        ("Opct", b"UntF", unit(b"#Prc", 100.)),
        (
            "Sz  ",
            b"UntF",
            unit(
                if variant == "size_unit" {
                    b"#Prc"
                } else {
                    b"#Pxl"
                },
                if variant == "zero_size" {
                    0.
                } else if variant == "fractional" {
                    1.5
                } else if variant == "maximum" {
                    128.
                } else if variant == "oversize" {
                    129.
                } else if position == "CtrF" {
                    2.
                } else {
                    1.
                },
            ),
        ),
        (
            "Clr ",
            b"Objc",
            overlay_descriptor(
                "RGBC",
                vec![
                    ("Rd  ", b"doub", 255f64.to_be_bytes().to_vec()),
                    ("Grn ", b"doub", 0f64.to_be_bytes().to_vec()),
                    ("Bl  ", b"doub", 0f64.to_be_bytes().to_vec()),
                ],
            ),
        ),
    ];
    if variant == "unknown" {
        fields.push(("unsupportedStrokeProperty", b"bool", vec![0]));
    }
    if variant == "missing" {
        fields.retain(|(key, _, _)| *key != "PntT");
    }
    if variant == "invalid_flag" {
        fields[0].2 = vec![2];
    }
    let definition = overlay_descriptor("FrFX", fields);
    let list = matches!(variant, "list" | "multiple" | "aliases");
    let mut list_value = (if variant == "multiple" { 2u32 } else { 1 })
        .to_be_bytes()
        .to_vec();
    list_value.extend(b"Objc");
    list_value.extend(&definition);
    if variant == "multiple" {
        list_value.extend(b"Objc");
        list_value.extend(&definition);
    }
    let mut items = vec![
        ("masterFXSwitch", b"bool", vec![u8::from(master)]),
        (
            "Scl ",
            b"UntF",
            unit(b"#Prc", if variant == "scale" { 200. } else { 100. }),
        ),
        (
            "numModifyingFX",
            b"long",
            (if variant == "count" {
                2
            } else {
                i32::from(enabled) + i32::from(variant == "overlay")
            })
            .to_be_bytes()
            .to_vec(),
        ),
        (
            if list { "frameFXMulti" } else { "FrFX" },
            if list { b"VlLs" } else { b"Objc" },
            if list { list_value } else { definition.clone() },
        ),
    ];
    if variant == "aliases" {
        items.push(("FrFX", b"Objc", definition));
    }
    if variant == "mixed" {
        items.push(("SoFi", b"Objc", overlay_descriptor("SoFi", vec![])));
    }
    if variant == "overlay" {
        items.push(("SoFi", b"Objc", color_overlay_descriptor(true, [0., 255., 0.])));
    }
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        overlay_descriptor("Lefx", items),
    ]
    .concat()
}

#[test]
fn photoshop_stroke_above_color_overlay_retains_both_editable_effects() {
    for depth in [8, 16] {
        let source = [[12347, 23459, 34571, 0], [54321, 32123, 11223, 65535]]
            .map(|pixel| pixel.map(|word| native(word, depth)));
        let mut spec = Spec::paint("Stroke and Color Overlay", source);
        spec.tags.push((*b"lmfx", solid_stroke_payload("InsF", true, true, "overlay")));
        let mut bytes = fixture(&[spec], depth);
        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
        // The opaque pixel first turns green, then a one-pixel inner stroke
        // contributes half red. Reversing the order would leave solid green.
        let expected = [[0; 4], [native(32768, depth), native(32767, depth), 0, 65535]];
        for channel in 0..4 {
            for pixel in expected {
                composite_sample(&mut bytes, pixel, channel, depth);
            }
        }
        let doc = psd::decode(&bytes).unwrap();
        assert!(!doc.read_only, "{depth}: {:?}", doc.warnings);
        assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
        assert_eq!(rendered(&doc), expected.concat());
        let kinds = |doc: &Document| {
            doc.layers[0].effects.iter().map(|effect| effect.kind.clone()).collect::<Vec<_>>()
        };
        assert_eq!(kinds(&doc), ["color_overlay", "stroke"]);
        let standard = saved_composite(&bytes, depth);
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let reopened = psd::decode(&saved).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(kinds(&reopened), kinds(&doc));
            assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
            assert_eq!(saved_composite(&saved, depth), standard);
        }
        let at = bytes.len() - 5 * usize::from(depth / 8) + usize::from(depth == 16);
        bytes[at] ^= 1;
        assert!(psd::decode(&bytes).unwrap().read_only);
    }
}

#[test]
fn photoshop_stroke_and_hard_shadows_keep_the_original_casting_alpha() {
    let tint = [111 * 257, 67 * 257, 231 * 257, 65535];
    for depth in [8, 16] {
        for (position, stroke_enabled, stroke_variant, drop_shadow) in [
            ("InsF", true, "", Some((true, 100.))),
            ("OutF", true, "", None),
            ("OutF", true, "", Some((true, 100.))),
            ("CtrF", true, "", Some((true, 100.))),
            ("OutF", true, "", Some((false, 100.))),
            ("CtrF", false, "", Some((true, 100.))),
            ("CtrF", true, "", Some((true, 0.))),
            ("OutF", true, "zero_size", Some((true, 100.))),
        ] {
            let source = [[12347, 23459, 34571, 65535], [54321, 32123, 11223, 0]]
                .map(|pixel| pixel.map(|word| native(word, depth)));
            let mut payload = solid_stroke_payload(position, stroke_enabled, true, stroke_variant);
            let count = payload.windows(18).position(|bytes| bytes == b"numModifyingFXlong").unwrap() + 18;
            let enabled_count = 1 + u32::from(stroke_enabled)
                + u32::from(drop_shadow.is_some_and(|(enabled, _)| enabled));
            payload[count..count + 4].copy_from_slice(&enabled_count.to_be_bytes());
            for (kind, (enabled, opacity)) in
                [("IrSh", (true, 100.))].into_iter().chain(drop_shadow.map(|shadow| ("DrSh", shadow)))
            {
                let shadow = hard_shadow_payload(kind, enabled, true, 180., opacity, "");
                let marker = [kind.as_bytes(), b"Objc"].concat();
                let start = shadow.windows(marker.len()).position(|bytes| bytes == marker).unwrap() + marker.len();
                append_style_root_field(&mut payload, kind, b"Objc", &shadow[start..]);
            }
            let mut spec = Spec::paint("Stroke with hard shadows", source);
            spec.tags.push((*b"lmfx", payload));
            let mut bytes = fixture(&[spec], depth);
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            // The inner cast colors the left pixel. A Stroke touching the
            // inside edge mixes half red there. The right pixel receives the
            // exterior Stroke or shadow when that effect contributes pixels.
            let shadow_casts = drop_shadow.is_some_and(|(enabled, opacity)| enabled && opacity > 0.);
            let stroke_paints = stroke_enabled && stroke_variant != "zero_size";
            let mut painted = tint;
            if stroke_paints && position != "OutF" {
                for (channel, red) in painted[..3].iter_mut().zip([65535., 0., 0.]) {
                    let mixed = (f64::from(*channel) * 32767. + red * 32768.) / 65535.;
                    *channel = native(mixed.round() as u16, depth);
                }
            }
            let exterior = if shadow_casts {
                tint
            } else if stroke_paints && position != "InsF" {
                [65535, 0, 0, native(32768, depth)]
            } else {
                [0; 4]
            };
            let merged = [painted, exterior];
            for c in 0..4 {
                for pixel in merged {
                    composite_sample(&mut bytes, pixel, c, depth);
                }
            }
            let doc = psd::decode(&bytes).unwrap();
            if shadow_casts && stroke_paints && position != "InsF" {
                assert!(doc.read_only);
                assert!(doc.warnings.iter().any(|warning| warning.contains("combined with Drop Shadow")));
                continue;
            }
            assert!(!doc.read_only, "{depth} {position}: {:?}", doc.warnings);
            assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
            assert_eq!(rendered(&doc), merged.concat());
            let kinds = doc.layers[0].effects.iter().map(|effect| effect.kind.as_str()).collect::<Vec<_>>();
            assert_eq!(&kinds[..2], ["inner_shadow", "stroke"]);
            assert_eq!(kinds.get(2), drop_shadow.map(|_| &"drop_shadow"));
            assert_eq!(doc.layers[0].effects[1].enabled, stroke_enabled);
            if let Some((enabled, opacity)) = drop_shadow {
                assert_eq!(doc.layers[0].effects[2].enabled, enabled);
                assert_eq!(doc.layers[0].effects[2].settings["opacity"], opacity / 100.);
            }
            for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                let reopened = psd::decode(&saved).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                assert_eq!(rendered(&reopened), merged.concat());
                assert_eq!(reopened.layers[0].effects.len(), kinds.len());
                for (before, after) in doc.layers[0].effects.iter().zip(&reopened.layers[0].effects) {
                    assert_eq!((&before.kind, before.enabled, &before.settings),
                        (&after.kind, after.enabled, &after.settings));
                }
            }
        }
    }
}

#[test]
fn standalone_photoshop_solid_strokes_require_exact_composites_and_preserve_native_sources() {
    for depth in [8, 16] {
        for position in ["OutF", "InsF", "CtrF"] {
            for (enabled, master, variant) in [
                (true, true, ""),
                (true, true, "list"),
                (true, true, "maximum"),
                (false, true, ""),
                (true, false, ""),
                (true, true, "absent"),
            ] {
                let active = enabled && master && variant != "absent";
                let source = [[12347, 23459, 34571, 0], [54321, 32123, 11223, 65535]]
                    .map(|p| p.map(|n| native(n, depth)));
                // Each nearest edge is one pixel away. Size 1 (center size 2)
                // gives half coverage, quantized once to native effect alpha.
                let mut merged = [[0; 4], source[1]];
                let stroke_alpha = if variant == "maximum" { 65535 } else { 32768 };
                if active && position != "InsF" {
                    merged[0] = [65535, 0, 0, native(stroke_alpha, depth)];
                }
                if active && position != "OutF" {
                    for (c, tint) in [65535u16, 0, 0].into_iter().enumerate() {
                        merged[1][c] = native(
                            ((f64::from(source[1][c]) * f64::from(65535 - stroke_alpha)
                                + f64::from(tint) * f64::from(stroke_alpha)) / 65535.)
                                .round() as u16,
                            depth,
                        );
                    }
                }
                let mut spec = Spec::paint("Synthetic solid Stroke", source);
                spec.tags.push((
                    *b"lfx2",
                    solid_stroke_payload(position, enabled, master, variant),
                ));
                let mut bytes = fixture(&[spec], depth);
                bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                for c in 0..4 {
                    for pixel in merged {
                        composite_sample(&mut bytes, pixel, c, depth);
                    }
                }
                let doc = psd::decode(&bytes).unwrap();
                assert!(
                    !doc.read_only,
                    "{depth} {position} {enabled} {master} {variant}: {:?}",
                    doc.warnings
                );
                assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
                assert_eq!(doc.layers[0].effects.len(), 1);
                assert_eq!(doc.layers[0].effects[0].kind, "stroke");
                assert_eq!(doc.layers[0].effects[0].enabled, active);
                if variant == "maximum" {
                    assert_eq!(doc.layers[0].effects[0].settings["size"], 128.);
                }
                assert_eq!(rendered(&doc), merged.concat());
                let current = doc.export_png().unwrap();
                let standard = saved_composite(&bytes, depth);
                for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    assert_eq!(saved_composite(&output, depth), standard);
                    let reopened = psd::decode(&output).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                    assert_eq!(
                        reopened.layers[0].effects[0].settings,
                        doc.layers[0].effects[0].settings
                    );
                    assert_eq!(reopened.export_png().unwrap(), current);
                }
                let layer = doc.layers[0].id.clone();
                let effect = doc.layers[0].effects[0].id.clone();
                let mut engine = Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = doc;
                engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!active})], None, None, "Stroke bypass").unwrap();
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source.concat());
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                // Mutate opaque green, which cannot vanish in alpha-zero matting.
                let at = bytes.len() - 5 * usize::from(depth / 8) + usize::from(depth == 16);
                bytes[at] ^= 1;
                let protected = psd::decode(&bytes).unwrap();
                assert!(protected.read_only);
                assert_eq!(protected.bit_depth, depth);
                assert!(psd::encode(&protected).is_err());
            }
        }
        for variant in [
            "overprint",
            "gradient",
            "multiply",
            "fractional",
            "oversize",
            "size_unit",
            "unknown",
            "missing",
            "invalid_flag",
            "multiple",
            "aliases",
            "scale",
            "count",
            "mixed",
            "mask",
            "clip",
            "partial",
        ] {
            for enabled in [false, true] {
                let mut spec = Spec::paint("Synthetic unsupported Stroke", SAVED);
                spec.tags.push((
                    *b"lfx2",
                    solid_stroke_payload("OutF", enabled, true, variant),
                ));
                if variant == "mask" {
                    spec.mask = Some([65535; 2]);
                }
                if variant == "clip" {
                    spec.clipping = 1;
                }
                if variant == "partial" {
                    spec.pixels[0][3] = 12347;
                }
                let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
                assert!(doc.read_only, "{depth} {variant} {enabled}");
                assert_eq!(doc.bit_depth, depth);
                assert!(doc.layers[0].effects.is_empty());
                assert!(psd::encode(&doc).is_err());
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
fn photoshop_nonaxial_gradient_shapes_keep_native_grid_sources_and_exact_composites() {
    for depth in [8, 16] {
        for (shape, name) in [
            (b"Rdl ", "radial"),
            (b"Angl", "angle"),
            (b"Dmnd", "diamond"),
        ] {
            for degrees in [0., 90.] {
                for reverse in [false, true] {
                    for enabled in [false, true] {
                        let mut payload = axial_gradient_payload(degrees, false, reverse);
                        let at = payload.windows(4).position(|v| v == b"Lnr ").unwrap();
                        payload[at..at + 4].copy_from_slice(shape);
                        let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                        payload[at] = u8::from(enabled);
                        let mut spec =
                            Spec::paint("Synthetic integer-center gradient shape", SAVED);
                        spec.tags.push((*b"lfx2", payload));
                        let mut bytes = fixture(&[spec], depth);
                        // The 2x1 integer grid is centered at (1,0), radius 1.
                        // Radius/Manhattan samples are 1 and 0. Angle's left
                        // axis is 1/2 or 3/4; its singular center is 1/4.
                        let positions = if name == "angle" {
                            [if degrees == 0. { 0.5 } else { 0.75 }, 0.25]
                        } else {
                            [1., 0.]
                        };
                        let expected = std::array::from_fn::<_, 2, _>(|x| {
                            if !enabled {
                                return SAVED[x].map(|n| native(n, depth));
                            }
                            let t: f64 = if reverse {
                                1. - positions[x]
                            } else {
                                positions[x]
                            };
                            let channel = native((t * 65535.).round() as u16, depth);
                            [channel, channel, channel, 65535]
                        });
                        bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                        for channel in 0..4 {
                            for pixel in expected {
                                composite_sample(&mut bytes, pixel, channel, depth);
                            }
                        }
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{depth} {name} {degrees} {reverse} {enabled}: {:?}",
                            doc.warnings
                        );
                        assert_eq!(doc.layers[0].effects[0].settings["style"], name);
                        assert_eq!(doc.layers[0].effects[0].settings["pixel_grid"], true);
                        assert_eq!(doc.layers[0].effects[0].enabled, enabled);
                        let source = SAVED.map(|p| p.map(|n| native(n, depth))).concat();
                        assert_eq!(doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(rendered(&doc), expected.concat());
                        let current = doc.export_png().unwrap();
                        for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(
                                reopened.layers[0].effects[0].settings,
                                doc.layers[0].effects[0].settings
                            );
                            assert_eq!(reopened.export_png().unwrap(), current);
                            assert_eq!(saved_composite(&output, depth), expected.concat());
                        }
                        let mut engine = Engine::new();
                        let layer = doc.layers[0].id.clone();
                        let effect = doc.layers[0].effects[0].id.clone();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Gradient shape bypass").unwrap();
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        let at =
                            bytes.len() - 8 * usize::from(depth / 8) + usize::from(depth / 8) - 1;
                        bytes[at] ^= 1;
                        assert!(psd::decode(&bytes).unwrap().read_only);
                    }
                }
            }
        }
        let mut payload = axial_gradient_payload(0., false, false);
        let at = payload.windows(4).position(|v| v == b"Lnr ").unwrap();
        payload[at..at + 4].copy_from_slice(b"????");
        let at = payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
        payload[at] = 0;
        let mut spec = Spec::paint("Synthetic disabled unknown shape", SAVED);
        spec.tags.push((*b"lfx2", payload));
        assert!(psd::decode(&fixture(&[spec], depth)).unwrap().read_only);
    }
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
                                composite_sample(&mut bytes, pixel, channel, depth);
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
fn arbitrary_gradient_rotation_and_unmatched_shapes_remain_protected() {
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
                    composite_sample(&mut bytes, pixel, channel, depth);
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
            engine.resident_state_mut().expect("resident fixture").document = doc;
            let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
            let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
            engine
                .edit(
                    "human",
                    &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":false})],
                    None,
                    None,
                    "Bypass clipped overlay",
                )
                .unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
            for output in [
                psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
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
            let standard = psd::decode(&ordinary(&psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap())).unwrap();
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
    let container_variant = variant;
    let variant = variant.strip_prefix("list:").unwrap_or(variant);
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
            unit(b"#Ang", if variant == "angle" { 45.5 } else { angle }),
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
    if variant == "missing_knockout" {
        properties.retain(|(key, _, _)| *key != "layerConceals");
    }
    if variant == "invalid_knockout" {
        properties
            .iter_mut()
            .find(|(key, _, _)| *key == "layerConceals")
            .unwrap()
            .2 = vec![2];
    }
    if variant == "unknown" {
        properties.push(("unsupportedShadowField", b"bool", vec![0]));
    }
    if variant == "missing" {
        properties.retain(|(key, _, _)| *key != "TrnS");
    }
    let definition = overlay_descriptor(
        if variant == "wrong_list_class" {
            if kind == "DrSh" {
                "IrSh"
            } else {
                "DrSh"
            }
        } else {
            kind
        },
        properties,
    );
    let multi_key = if kind == "DrSh" {
        "dropShadowMulti"
    } else {
        "innerShadowMulti"
    };
    let list = container_variant == "list"
        || container_variant.starts_with("list:")
        || matches!(
            variant,
            "empty_list" | "multiple_list" | "oversized_list" | "boolean_list" | "wrong_list_class" | "both_aliases"
        );
    let list_value = || {
        let count = match variant {
            "empty_list" => 0u32,
            "multiple_list" => 2,
            "oversized_list" => 17,
            _ => 1,
        };
        let mut value = count.to_be_bytes().to_vec();
        for _ in 0..count {
            if variant == "boolean_list" {
                value.extend(b"bool\0");
            } else {
                value.extend(b"Objc");
                value.extend(&definition);
            }
        }
        value
    };
    let mut entries = vec![
        ("masterFXSwitch", b"bool", vec![u8::from(master)]),
        ("Scl ", b"UntF", unit(b"#Prc", 100.)),
        (
            if list || variant == "plain_multi" {
                multi_key
            } else {
                kind
            },
            if list { b"VlLs" } else { b"Objc" },
            if list {
                list_value()
            } else {
                definition.clone()
            },
        ),
    ];
    if variant == "both_aliases" {
        entries.push((kind, b"Objc", definition));
    }
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
            composite_sample(&mut bytes, p, c, depth);
        }
    }
    (bytes, merged.into_iter().flatten().collect())
}

fn legacy_shadow_payload(key: &[u8; 4], version: u32, color: [u16; 3]) -> Vec<u8> {
    let mut rgb = vec![0, 0];
    for word in color {
        rgb.extend(word.to_be_bytes());
    }
    rgb.extend([0, 0]);
    let mut shadow = version.to_be_bytes().to_vec();
    for word in [0i32, 0, 180 * 65536, 65536] {
        shadow.extend(word.to_be_bytes());
    }
    shadow.extend(&rgb);
    shadow.extend(b"8BIMnorm");
    shadow.extend([1, 0, 255]);
    if version == 2 {
        shadow.extend(rgb);
    }
    let mut bytes = vec![0, 0, 0, 2];
    bytes.extend(b"8BIMcmnS");
    block(&mut bytes, &[0, 0, 0, 0, 1, 0, 0]);
    bytes.extend(b"8BIM");
    bytes.extend(key);
    block(&mut bytes, &shadow);
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes
}

#[test]
fn legacy_hard_shadows_require_exact_composites_and_explicit_drop_knockout() {
    for depth in [8, 16] {
        for version in [0, 2] {
            for drop in [false, true] {
                let mut source = SAVED;
                let color = if drop {
                    source[1][3] = 0;
                    [111 * 257, 67 * 257, 231 * 257]
                } else {
                    [64515, 17221, 54321]
                };
                let key = if drop { b"dsdw" } else { b"isdw" };
                let mut spec = Spec::paint("Synthetic legacy hard shadow", source);
                spec.tags.push((*b"lrFX", legacy_shadow_payload(key, version, color)));
                let mut merged = source;
                merged[usize::from(drop)] = [color[0], color[1], color[2], 65535];
                let with_composite = |spec: &Spec| {
                    let mut bytes = fixture(std::slice::from_ref(spec), depth);
                    bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                    for channel in 0..4 {
                        for pixel in merged {
                            composite_sample(&mut bytes, pixel, channel, depth);
                        }
                    }
                    bytes
                };
                if drop {
                    assert!(psd::decode(&with_composite(&spec)).unwrap().read_only);
                    spec.tags.push((*b"lfx2", hard_shadow_payload("DrSh", true, true, 180., 100., "knockout")));
                }
                let mut bytes = with_composite(&spec);
                for doc in [
                    psd::decode(&bytes).unwrap(),
                    psd::decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
                ] {
                    assert!(!doc.read_only, "{depth} {version} {drop}: {:?}", doc.warnings);
                    let layer = &doc.layers[0];
                    assert_eq!(layer.effects.len(), 1);
                    let effect = &layer.effects[0];
                    assert_eq!(effect.kind, if drop { "drop_shadow" } else { "inner_shadow" });
                    if drop {
                        assert_eq!(effect.settings["source_knockout"], false);
                    }
                    assert_eq!(effect.settings["angle"], 0.);
                    assert_eq!(effect.settings["distance"], 1.);
                    assert_eq!(layer.pixels.rgba16(), source.into_iter().flatten().map(|word| native(word, depth)).collect::<Vec<_>>());
                    for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                        let restored = psd::decode(&ordinary(&output)).unwrap();
                        assert!(!restored.read_only, "{:?}", restored.warnings);
                        assert_eq!(restored.layers[0].effects[0].settings, effect.settings);
                        assert_eq!(restored.layers[0].pixels.rgba16(), layer.pixels.rgba16());
                        assert_eq!(restored.export_png().unwrap(), doc.export_png().unwrap());
                    }
                }
                if drop {
                    let legacy = &mut spec.tags[0].1;
                    let distance = 4 + 12 + 7 + 12 + 16;
                    legacy[distance..distance + 4].copy_from_slice(&(2i32 * 65536).to_be_bytes());
                    assert!(psd::decode(&with_composite(&spec)).unwrap().read_only);
                } else {
                    let start = bytes.len() - 8 * usize::from(depth / 8);
                    bytes[start] ^= 1;
                    assert!(psd::decode(&bytes).unwrap().read_only);
                    spec.mask = Some([65535; 2]);
                    assert!(psd::decode(&with_composite(&spec)).unwrap().read_only);
                }
            }
        }
    }
}

#[test]
fn photoshop_global_light_shadows_round_trip_and_local_angle_edits_detach() {
    let resource = |angle: u32| {
        let mut bytes = b"8BIM\x04\x0d\0\0".to_vec();
        block(&mut bytes, &angle.to_be_bytes());
        bytes
    };
    let with_resource = |mut bytes: Vec<u8>, resource: &[u8]| {
        assert_eq!(&bytes[30..34], &[0; 4]);
        let mut framed = (resource.len() as u32).to_be_bytes().to_vec();
        framed.extend(resource);
        bytes.splice(30..34, framed);
        bytes
    };
    let global = resource(180);
    let local_angle = -12.5;
    let tint = [111 * 257, 67 * 257, 231 * 257];
    for depth in [8, 16] {
        for (kind, key, drop) in [("IrSh", b"isdw", false), ("DrSh", b"dsdw", true)] {
            let mut source = SAVED;
            if drop {
                source[1][3] = 0;
            }
            let mut spec = Spec::paint("Synthetic global-light shadow", source);
            let modern = hard_shadow_payload(kind, true, true, local_angle, 100., "global");
            let mut legacy = legacy_shadow_payload(key, 2, tint);
            let payload = 4 + 12 + 7 + 12;
            let legacy_angle = (local_angle * 65536.) as i32;
            legacy[payload + 12..payload + 16].copy_from_slice(&legacy_angle.to_be_bytes());
            legacy[payload + 39] = 1;
            spec.tags.extend([(*b"lfx2", modern), (*b"lrFX", legacy)]);
            let mut bytes = fixture(&[spec], depth);
            let mut merged = source;
            merged[usize::from(drop)] = [tint[0], tint[1], tint[2], 65535];
            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
            for channel in 0..4 {
                for pixel in merged {
                    composite_sample(&mut bytes, pixel, channel, depth);
                }
            }
            assert!(psd::decode(&bytes).unwrap().read_only);
            let mismatched = with_resource(bytes.clone(), &resource(0));
            assert!(psd::decode(&mismatched).unwrap().read_only);
            let bytes = with_resource(bytes, &global);
            let doc = psd::decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            let effect = &doc.layers[0].effects[0];
            assert_eq!(effect.settings["global_light"], json!([180, local_angle]));
            assert_eq!(effect.settings["angle"], 0.);
            for output in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                let reopened = psd::decode(&output).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
                assert!(output.windows(global.len()).any(|entry| entry == global.as_slice()));
                let standard = psd::decode(&with_resource(ordinary(&output), &global)).unwrap();
                assert!(!standard.read_only, "{:?}", standard.warnings);
                assert_eq!(standard.layers[0].effects[0].settings, effect.settings);
                assert_eq!(standard.export_png().unwrap(), doc.export_png().unwrap());
            }

            let layer = doc.layers[0].id.clone();
            let effect_id = effect.id.clone();
            let mut settings = effect.settings.as_ref().clone();
            settings["angle"] = json!(90.);
            let mut engine = Engine::new();
            engine.resident_state_mut().unwrap().document = doc;
            engine.edit("human", &[json!({"op":"effect.update", "layer":layer, "effect":effect_id, "settings":settings})], None, None, "Set local shadow angle").unwrap();
            let changed = &engine.resident_state().unwrap().document;
            assert!(changed.layers[0].effects[0].settings.get("global_light").is_none());
            let local = psd::decode(&ordinary(&psd::encode(changed).unwrap())).unwrap();
            assert!(!local.read_only, "{:?}", local.warnings);
            assert_eq!(local.layers[0].effects[0].settings["angle"], 90.);
            engine.undo("human").unwrap();
            assert_eq!(engine.resident_state().unwrap().document.layers[0].effects[0].settings["global_light"], json!([180, local_angle]));

            let original = engine.resident_state().unwrap().document.export_png().unwrap();
            let command = json!({"op":"document.global_light","angle":0});
            engine.edit("agent", &[command], None, Some("change-light"), "Move Global Light").unwrap();
            let changed = &engine.resident_state().unwrap().document;
            assert_eq!(changed.psd_global_angle, Some(0u32.to_be_bytes()));
            assert_eq!(changed.layers[0].effects[0].settings["global_light"], json!([0, local_angle]));
            assert_ne!(changed.export_png().unwrap(), original);
            let output = psd::encode(&changed).unwrap();
            let resource = resource(0);
            assert!(output.windows(resource.len()).any(|entry| entry == resource.as_slice()));
            let reopened = psd::decode(&with_resource(ordinary(&output), &resource)).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(reopened.export_png().unwrap(), changed.export_png().unwrap());

            let settings = changed.layers[0].effects[0].settings.as_ref().clone();
            let kind = changed.layers[0].effects[0].kind.clone();
            engine.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":kind,"settings":settings})], None, None, "Add linked shadow").unwrap();
            let review = engine.inspect_task_undo("human", "agent", "change-light").unwrap();
            assert_eq!(review["can_undo"], false);
            assert!(engine.undo_task("human", "agent", "change-light").is_err());
            assert_eq!(engine.resident_state().unwrap().document.layers[0].effects.len(), 2);
            engine.undo("human").unwrap();
            engine.undo_task("human", "agent", "change-light").unwrap();
            assert_eq!(engine.resident_state().unwrap().document.export_png().unwrap(), original);
            let source = &engine.resident_state().unwrap().document;
            let snapshot = peerbrush::layer_clipboard::copy(source, &[layer.clone()]).unwrap();
            for (destination_angle, linked) in [(Some(180u32), true), (Some(0), false), (None, false)] {
                let mut destination = Document::new_depth(2, 1, depth).unwrap();
                destination.psd_global_angle = destination_angle.map(u32::to_be_bytes);
                let copied = peerbrush::layer_clipboard::paste(&mut destination, &snapshot, "").unwrap();
                let pasted = destination.layers.iter().find(|layer| layer.id == copied[0]).unwrap();
                assert_eq!(pasted.effects[0].settings.get("global_light").is_some(), linked);
                assert_eq!(pasted.effects[0].settings["angle"], 0.);
            }
        }
    }
}

#[test]
fn authored_shadow_links_initialize_global_angle_and_round_trip_as_standard_styles() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(3, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, SAVED[0]);
        doc.layers[0].pixels.set16(1, 0, SAVED[1]);
        let layer = doc.layers[0].id.clone();
        let linked = Engine::preview_edits(doc, &[json!({
            "op":"effect.add", "layer":layer, "kind":"inner_shadow", "use_global_light":true,
            "settings":{"angle":0., "size":0., "distance":1., "blend":"normal", "opacity":1.}
        })]).unwrap();
        assert_eq!(linked.psd_global_angle, Some(180u32.to_be_bytes()));
        let effect = &linked.layers[0].effects[0];
        assert_eq!(effect.settings["global_light"], json!([180, -180.]));
        let output = psd::encode_psb(&linked).unwrap();
        let reopened = psd::decode(&output).unwrap();
        assert!(!reopened.read_only, "{:?}", reopened.warnings);
        assert_eq!(reopened.psd_global_angle, linked.psd_global_angle);
        assert_eq!(reopened.layers[0].effects[0].settings, effect.settings);
        let global_resource = [b"8BIM\x04\x0d\0\0\0\0\0\x04".as_slice(), &180u32.to_be_bytes()].concat();
        assert!(output.windows(global_resource.len()).any(|entry| entry == global_resource.as_slice()));
        let mut standard = ordinary(&output);
        standard.splice(30..34, (global_resource.len() as u32).to_be_bytes().into_iter().chain(global_resource));
        let standard = psd::decode(&standard).unwrap();
        assert!(!standard.read_only, "{:?}", standard.warnings);
        assert_eq!(standard.layers[0].effects[0].settings, effect.settings);
        assert_eq!(standard.export_png().unwrap(), linked.export_png().unwrap());

        let effect_id = effect.id.clone();
        let relit = Engine::preview_edits(linked, &[json!({"op":"document.global_light","angle":0})]).unwrap();
        assert_eq!(relit.layers[0].effects[0].settings["angle"], -180.);
        let png = relit.export_png().unwrap();
        let local = Engine::preview_edits(relit, &[json!({
            "op":"effect.update","layer":layer,"effect":effect_id,"use_global_light":false
        })]).unwrap();
        assert_eq!(local.psd_global_angle, Some(0u32.to_be_bytes()));
        assert!(local.layers[0].effects[0].settings.get("global_light").is_none());
        assert_eq!(local.layers[0].effects[0].settings["angle"], -180.);
        assert_eq!(local.export_png().unwrap(), png);
        assert!(Engine::preview_edits(local.clone(), &[json!({
            "op":"effect.add","layer":layer,"kind":"blur","use_global_light":true
        })]).is_err());
        assert!(Engine::preview_edits(local, &[json!({
            "op":"effect.update","layer":layer,"effect":effect_id,"use_global_light":"true"
        })]).is_err());
    }
}

fn framed_shadow_fixture(
    depth: u16,
    source: &[[u16; 4]],
    payload: &[u8],
    merged: &[[u16; 4]],
) -> Vec<u8> {
    framed_shadow_fixture_opacity(depth, source, payload, merged, 255)
}

fn framed_shadow_fixture_opacity(
    depth: u16,
    source: &[[u16; 4]],
    payload: &[u8],
    merged: &[[u16; 4]],
    opacity: u8,
) -> Vec<u8> {
    assert_eq!(source.len(), 16);
    assert_eq!(merged.len(), 16);
    let mut record = vec![];
    for value in [0i32, 0, 4, 4] {
        record.extend(value.to_be_bytes());
    }
    record.extend(4u16.to_be_bytes());
    let mut channels = vec![];
    for channel in [-1i16, 0, 1, 2] {
        let mut data = 0u16.to_be_bytes().to_vec();
        for pixel in source {
            sample(
                &mut data,
                pixel[if channel == -1 { 3 } else { channel as usize }],
                depth,
            );
        }
        record.extend(channel.to_be_bytes());
        record.extend((data.len() as u32).to_be_bytes());
        channels.extend(data);
    }
    record.extend(b"8BIMnorm");
    record.extend([opacity, 0, 0, 0]);
    let mut extra = vec![0; 8];
    let name = b"Synthetic pixel shadow";
    extra.push(name.len() as u8);
    extra.extend(name);
    extra.resize((extra.len() + 3) & !3, 0);
    tag(&mut extra, b"lfx2", payload);
    block(&mut record, &extra);
    let mut info = (-1i16).to_be_bytes().to_vec();
    info.extend(record);
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
    out.extend(4u32.to_be_bytes());
    out.extend(4u32.to_be_bytes());
    out.extend(depth.to_be_bytes());
    out.extend(3u16.to_be_bytes());
    out.extend([0; 8]);
    block(&mut out, &section);
    out.extend(0u16.to_be_bytes());
    for channel in 0..4 {
        for &pixel in merged {
            composite_sample(&mut out, pixel, channel, depth);
        }
    }
    out
}

#[test]
fn photoshop_pixel_aligned_hard_shadows_retain_angles_sources_undo_and_standard_saves() {
    // Independent integer casts, including light-angle half-pixel arithmetic.
    for (angle, distance, dx, dy) in [
        (45f64, 2f64, -1, 1),
        (135., 2., 1, 1),
        (-45., 2., -1, -1),
        (-135., 2., 1, -1),
        (120., 2., 1, 2),
        (30., 2., -2, 1),
        (60., 1., -1, 1),
        (-120., 1., 0, -1),
    ] {
        for depth in [8, 16] {
            let source = std::array::from_fn::<_, 16, _>(|i| {
                let x = i % 4;
                let y = i / 4;
                [
                    12347 + x as u16 * 139,
                    23459 + y as u16 * 197,
                    34571,
                    if (1..3).contains(&x) && (1..3).contains(&y) {
                        65535
                    } else {
                        0
                    },
                ]
                .map(|v| native(v, depth))
            });
            for kind in ["DrSh", "IrSh"] {
                for enabled in [false, true] {
                    for list in [false, true] {
                        let mut payload = hard_shadow_payload(
                            kind,
                            enabled,
                            true,
                            angle,
                            37.,
                            if list { "list" } else { "" },
                        );
                        let at = payload
                            .windows(12)
                            .position(|v| v == b"DstnUntF#Pxl")
                            .unwrap()
                            + 12;
                        payload[at..at + 8].copy_from_slice(&distance.to_be_bytes());
                        let mut merged = source;
                        let coverage = (0.37f64 * 65535.).round() / 65535.;
                        for i in 0..16 {
                            let x = (i % 4) as i32;
                            let y = (i / 4) as i32;
                            let sx = x - dx;
                            let sy = y - dy;
                            let cast = if (0..4).contains(&sx) && (0..4).contains(&sy) {
                                source[(sy * 4 + sx) as usize][3]
                            } else {
                                0
                            };
                            if enabled && kind == "DrSh" && source[i][3] == 0 && cast != 0 {
                                merged[i] = [
                                    111 * 257,
                                    67 * 257,
                                    231 * 257,
                                    (coverage * 65535.).round() as u16,
                                ]
                                .map(|v| native(v, depth));
                            } else if enabled && kind == "IrSh" && source[i][3] != 0 && cast == 0 {
                                for channel in 0..3 {
                                    merged[i][channel] = native(
                                        ((f64::from(source[i][channel]) / 65535. * (1. - coverage)
                                            + f64::from([111 * 257, 67 * 257, 231 * 257][channel])
                                                / 65535.
                                                * coverage)
                                            * 65535.)
                                            .round() as u16,
                                        depth,
                                    );
                                }
                            }
                            if merged[i][3] == 0 {
                                merged[i] = [0; 4];
                            }
                        }
                        let mut bytes = framed_shadow_fixture(depth, &source, &payload, &merged);
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{angle} {distance} {depth} {kind} {enabled} {list}: {:?}",
                            doc.warnings
                        );
                        assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
                        assert_eq!(rendered(&doc), merged.concat());
                        let effect = &doc.layers[0].effects[0];
                        assert_eq!(effect.enabled, enabled);
                        assert_eq!(effect.settings["pixel_snap"], true);
                        assert_eq!(
                            effect.settings["angle"],
                            (angle + 360.).rem_euclid(360.) - 180.
                        );
                        assert_eq!(effect.settings["distance"], distance);
                        let settings = effect.settings.as_ref().clone();
                        let layer = doc.layers[0].id.clone();
                        let effect = effect.id.clone();
                        let current = doc.export_png().unwrap();
                        let mut engine = Engine::new();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})],None,None,"Pixel shadow bypass").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source.concat());
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        let max = if depth == 16 { 65535u64 } else { 255 };
                        let divisor = if depth == 16 { 1u64 } else { 257 };
                        let matte = merged.map(|pixel| {
                            let pixel = pixel.map(|v| u64::from(v) / divisor);
                            std::array::from_fn::<_, 4, _>(|channel| {
                                if channel == 3 {
                                    (pixel[3] * divisor) as u16
                                } else {
                                    (((pixel[channel] * pixel[3]
                                        + max * (max - pixel[3])
                                        + max / 2)
                                        / max)
                                        * divisor) as u16
                                }
                            })
                        });
                        for output in [
                            psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        ] {
                            assert_eq!(saved_composite(&output, depth), matte.concat());
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                            assert_eq!(*reopened.layers[0].effects[0].settings, settings);
                            assert_eq!(reopened.export_png().unwrap(), current);
                            let standard = psd::decode(&ordinary(&output)).unwrap();
                            assert!(!standard.read_only);
                            assert_eq!(standard.export_png().unwrap(), current);
                        }
                        // Change an opaque source pixel, so alpha-zero matte
                        // normalization cannot erase the independent mismatch.
                        let at = bytes.len() - 64 * usize::from(depth / 8)
                            + 5 * usize::from(depth / 8)
                            + usize::from(depth == 16);
                        bytes[at] ^= 1;
                        let protected = psd::decode(&bytes).unwrap();
                        assert!(protected.read_only);
                        assert_eq!(protected.bit_depth, depth);
                        assert!(protected.layers[0].effects.is_empty());
                        assert!(psd::encode(&protected).is_err());
                    }
                }
            }
        }
    }
}

#[test]
fn photoshop_partial_alpha_hard_drop_shadows_retain_explicit_knockout_sources_and_native_saves() {
    for depth in [8, 16] {
        let alpha = [
            0u16, 1, 12347, 65535, 32768, 65534, 257, 23459, 54321, 0, 10001, 45679, 34571, 51347,
            22371, 65535,
        ];
        let source = std::array::from_fn::<_, 16, _>(|i| {
            [
                native(12347 + i as u16 * 107, depth),
                native(23459 + i as u16 * 113, depth),
                native(34571 + i as u16 * 127, depth),
                native(alpha[i], depth),
            ]
        });
        for (angle, dx, dy) in [(180., 1i32, 0i32), (45., -1, 1)] {
            for flags in 0u8..4 {
                let (enabled, master) = (flags & 1 != 0, flags & 2 != 0);
                for knockout in [false, true] {
                    for list in [false, true] {
                        for inner in [false, true] {
                            for opacity in [64u8, 255] {
                                let payload = partial_drop_shadow_payload(
                                    enabled, master, list, knockout, inner, angle, "",
                                );
                                let mix = |under: [u16; 3], over: [u16; 3], coverage: f64| {
                                    let weight = (coverage * 65535.).round();
                                    std::array::from_fn(|c| {
                                        native(
                                            ((f64::from(under[c]) * (65535. - weight)
                                                + f64::from(over[c]) * weight)
                                                / 65535.)
                                                .round()
                                                as u16,
                                            depth,
                                        )
                                    })
                                };
                                let at_alpha = |x: i32, y: i32| {
                                    if (0..4).contains(&x) && (0..4).contains(&y) {
                                        source[(y * 4 + x) as usize][3]
                                    } else {
                                        0
                                    }
                                };
                                // Independent source-alpha/knockout reference.
                                // Inner/color styles never alter the casting alpha.
                                let merged = std::array::from_fn::<_, 16, _>(|i| {
                                    let (x, y) = (i as i32 % 4, i as i32 / 4);
                                    let mut rgb: [u16; 3] = source[i][..3].try_into().unwrap();
                                    if master && inner && source[i][3] != 0 {
                                        rgb = mix(rgb, [201 * 257, 43 * 257, 77 * 257], 0.43);
                                        rgb = mix(
                                            rgb,
                                            [111 * 257, 67 * 257, 231 * 257],
                                            (1. - f64::from(at_alpha(x - 1, y)) / 65535.) * 0.23,
                                        );
                                    }
                                    let a = f64::from(source[i][3]) / 65535.;
                                    let shade = if master && enabled {
                                        (f64::from(at_alpha(x - dx, y - dy))
                                            * if knockout { 1. - a } else { 1. }
                                            * 0.37)
                                            .round()
                                            / 65535.
                                    } else {
                                        0.
                                    };
                                    let total = a + shade * (1. - a);
                                    let mut pixel = if total == 0. {
                                        [0; 4]
                                    } else {
                                        let mut p = [0; 4];
                                        for c in 0..3 {
                                            p[c] = ((f64::from(rgb[c]) * a
                                                + f64::from(
                                                    [111u16 * 257, 67 * 257, 231 * 257][c],
                                                ) * shade
                                                    * (1. - a))
                                                / total)
                                                .round()
                                                as u16;
                                        }
                                        p[3] = (total * 65535.).round() as u16;
                                        p
                                    };
                                    if depth == 8 {
                                        pixel = pixel.map(|v| native(v, depth));
                                    }
                                    // A cast projected to zero native coverage
                                    // cannot contribute hidden tint to compositing.
                                    if pixel[3] == 0 {
                                        return [0; 4];
                                    }
                                    pixel[3] = if depth == 16 {
                                        (f64::from(pixel[3]) * f64::from(f32::from(opacity) / 255.))
                                            .round() as u16
                                    } else {
                                        ((f32::from(pixel[3] / 257) * (f32::from(opacity) / 255.))
                                            .round()
                                            as u16)
                                            * 257
                                    };
                                    pixel
                                });
                                let mut bytes = framed_shadow_fixture_opacity(
                                    depth, &source, &payload, &merged, opacity,
                                );
                                let doc = psd::decode(&bytes).unwrap();
                                assert!(!doc.read_only,"{depth} {angle} {flags} {knockout} {list} {inner} {opacity}: {:?}",doc.warnings);
                                assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
                                assert_eq!(
                                    rendered(&doc),
                                    merged.concat(),
                                    "{depth} {angle} {flags} {knockout} {list} {inner} {opacity}"
                                );
                                assert_eq!(doc.layers[0].effects.len(), if inner { 3 } else { 1 });
                                let drop = doc.layers[0].effects.last().unwrap();
                                assert_eq!(drop.kind, "drop_shadow");
                                assert_eq!(drop.settings["source_knockout"], knockout);
                                assert_eq!(drop.enabled, master && enabled);
                                let settings = doc.layers[0]
                                    .effects
                                    .iter()
                                    .map(|e| (e.kind.clone(), e.enabled, e.settings.clone()))
                                    .collect::<Vec<_>>();
                                let current = doc.export_png().unwrap();
                                let layer = doc.layers[0].id.clone();
                                let effect = drop.id.clone();
                                let mut engine = Engine::new();
                                engine.resident_state_mut().expect("resident fixture").document = doc;
                                let mut changed = engine.resident_state().expect("resident fixture").document.layers[0]
                                    .effects
                                    .last()
                                    .unwrap()
                                    .settings
                                    .as_ref()
                                    .clone();
                                changed["source_knockout"] = json!(!knockout);
                                engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":changed})],None,None,"Imported knockout toggle").unwrap();
                                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source.concat());
                                engine.undo("human").unwrap();
                                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                                for output in [
                                    psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                                    psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                                ] {
                                    assert_eq!(
                                        saved_composite(&output, depth),
                                        saved_composite(&bytes, depth)
                                    );
                                    let reopened = psd::decode(&output).unwrap();
                                    assert!(!reopened.read_only);
                                    assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                                    assert_eq!(
                                        reopened.layers[0]
                                            .effects
                                            .iter()
                                            .map(|e| (
                                                e.kind.clone(),
                                                e.enabled,
                                                e.settings.clone()
                                            ))
                                            .collect::<Vec<_>>(),
                                        settings
                                    );
                                    assert_eq!(reopened.export_png().unwrap(), current);
                                    assert_eq!(
                                        psd::decode(&ordinary(&output))
                                            .unwrap()
                                            .export_png()
                                            .unwrap(),
                                        current
                                    );
                                }
                                let at = bytes.len() - 64 * usize::from(depth / 8)
                                    + 15 * usize::from(depth / 8)
                                    + usize::from(depth == 16);
                                bytes[at] ^= 1;
                                let protected = psd::decode(&bytes).unwrap();
                                assert!(protected.read_only);
                                assert!(psd::encode(&protected).is_err());
                            }
                        }
                    }
                }
            }
        }
    }
}

fn partial_drop_shadow_payload(
    enabled: bool,
    master: bool,
    list: bool,
    knockout: bool,
    inner: bool,
    angle: f64,
    variant: &str,
) -> Vec<u8> {
    let variant = if !knockout && variant.is_empty() {
        "knockout"
    } else {
        variant
    };
    let list_variant = format!("list:{variant}");
    let source = hard_shadow_payload(
        "DrSh",
        enabled,
        master,
        angle,
        37.,
        if list { &list_variant } else { variant },
    );
    let key = if list { "dropShadowMulti" } else { "DrSh" };
    let kind = if list { b"VlLs" } else { b"Objc" };
    let needle = [key.as_bytes(), kind].concat();
    let at = source
        .windows(needle.len())
        .position(|v| v == needle)
        .unwrap();
    let mut drop = source[at + needle.len()..].to_vec();
    if angle == 45. {
        if let Some(at) = drop.windows(12).position(|v| v == b"DstnUntF#Pxl") {
            drop[at + 12..at + 20].copy_from_slice(&2f64.to_be_bytes());
        }
    }
    let mut entries = vec![
        ("masterFXSwitch", b"bool", vec![u8::from(master)]),
        (key, kind, drop),
    ];
    if inner {
        let source = hard_shadow_payload("IrSh", true, master, 180., 23., "");
        let at = source.windows(8).position(|v| v == b"IrShObjc").unwrap() + 8;
        entries.push(("IrSh", b"Objc", source[at..].to_vec()));
        let source = overlay_payload(true, true, [201., 43., 77.]);
        let at = source.windows(8).position(|v| v == b"SoFiObjc").unwrap() + 8;
        let mut color = source[at..].to_vec();
        let at = color
            .windows(12)
            .position(|v| v == b"OpctUntF#Prc")
            .unwrap()
            + 12;
        color[at..at + 8].copy_from_slice(&43f64.to_be_bytes());
        entries.push(("SoFi", b"Objc", color));
    }
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        overlay_descriptor("Lefx", entries),
    ]
    .concat()
}

#[test]
fn photoshop_partial_alpha_drop_shadows_protect_unsupported_disabled_knockout_definitions() {
    for depth in [8, 16] {
        let source = [[
            native(12347, depth),
            native(23459, depth),
            native(34571, depth),
            native(23459, depth),
        ]; 16];
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
            "missing_knockout",
            "invalid_knockout",
            "blend",
        ] {
            for list in [false, true] {
                let payload =
                    partial_drop_shadow_payload(false, false, list, true, false, 180., variant);
                let doc =
                    psd::decode(&framed_shadow_fixture(depth, &source, &payload, &source)).unwrap();
                assert!(doc.read_only, "{depth} {list} {variant}");
                assert!(doc.layers[0].effects.is_empty());
                assert_eq!(doc.bit_depth, depth);
                assert!(psd::encode(&doc).is_err());
            }
        }
    }
}

#[test]
fn photoshop_partial_alpha_hard_inner_shadows_retain_native_sources_coverage_and_standard_saves() {
    for depth in [8, 16] {
        let alpha = [
            0u16, 1, 12347, 65535, 32768, 65534, 257, 23459, 54321, 0, 10001, 45679, 34571, 51347,
            22371, 65535,
        ];
        let source = std::array::from_fn::<_, 16, _>(|i| {
            [
                native(12347 + i as u16 * 107, depth),
                native(23459 + i as u16 * 113, depth),
                native(34571 + i as u16 * 127, depth),
                native(alpha[i], depth),
            ]
        });
        for (angle, dx, dy) in [(180., 1i32, 0i32), (45., -1, 1)] {
            for flags in 0u8..4 {
                let enabled = flags & 1 != 0;
                let master = flags & 2 != 0;
                for list in [false, true] {
                    for overlay in [false, true] {
                        for opacity in [64u8, 255] {
                            let payload = partial_inner_shadow_payload(
                                enabled, master, list, overlay, angle, "",
                            );
                            // No renderer/encoder under test supplies this reference.
                            // The inverse translated source alpha is the inner
                            // coverage; the original alpha remains independent.
                            let mix = |under: [u16; 3], over: [u16; 3], coverage: f64| {
                                let weight = (65535. * coverage).round();
                                std::array::from_fn(|c| {
                                    native(
                                        ((f64::from(under[c]) * (65535. - weight)
                                            + f64::from(over[c]) * weight)
                                            / 65535.)
                                            .round() as u16,
                                        depth,
                                    )
                                })
                            };
                            let merged = std::array::from_fn::<_, 16, _>(|i| {
                                if source[i][3] == 0 {
                                    return [0; 4];
                                }
                                let (x, y) = (i as i32 % 4 - dx, i as i32 / 4 - dy);
                                let shifted_alpha = if (0..4).contains(&x) && (0..4).contains(&y) {
                                    source[(y * 4 + x) as usize][3]
                                } else {
                                    0
                                };
                                let mut rgb = source[i][..3].try_into().unwrap();
                                if master && overlay {
                                    rgb = mix(rgb, [201 * 257, 43 * 257, 77 * 257], 0.43);
                                }
                                if master && enabled {
                                    rgb = mix(
                                        rgb,
                                        [111 * 257, 67 * 257, 231 * 257],
                                        (1. - f64::from(shifted_alpha) / 65535.) * 0.37,
                                    );
                                }
                                let a = if depth == 16 {
                                    (f64::from(source[i][3]) * f64::from(f32::from(opacity) / 255.))
                                        .round() as u16
                                } else {
                                    ((f32::from(source[i][3] / 257) * (f32::from(opacity) / 255.))
                                        .round() as u16)
                                        * 257
                                };
                                [rgb[0], rgb[1], rgb[2], a]
                            });
                            let mut bytes = framed_shadow_fixture_opacity(
                                depth, &source, &payload, &merged, opacity,
                            );
                            let doc = psd::decode(&bytes).unwrap();
                            assert!(
                                !doc.read_only,
                                "{depth} {angle} {flags} {list} {overlay} {opacity}: {:?}",
                                doc.warnings
                            );
                            assert_eq!(doc.bit_depth, depth);
                            assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
                            assert_eq!(rendered(&doc), merged.concat());
                            assert_eq!(doc.layers[0].effects.len(), if overlay { 2 } else { 1 });
                            let inner = doc.layers[0].effects.last().unwrap();
                            assert_eq!(inner.kind, "inner_shadow");
                            assert_eq!(inner.enabled, enabled && master);
                            let settings = doc.layers[0]
                                .effects
                                .iter()
                                .map(|e| (e.kind.clone(), e.enabled, e.settings.clone()))
                                .collect::<Vec<_>>();
                            let layer = doc.layers[0].id.clone();
                            let effect = inner.id.clone();
                            let current = doc.export_png().unwrap();
                            let mut engine = Engine::new();
                            engine.resident_state_mut().expect("resident fixture").document = doc;
                            engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!(enabled && master)})], None, None, "Partial inner bypass").unwrap();
                            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                            assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source.concat());
                            engine.undo("human").unwrap();
                            assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                            for output in [
                                psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                                psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            ] {
                                assert_eq!(
                                    saved_composite(&output, depth),
                                    saved_composite(&bytes, depth)
                                );
                                let reopened = psd::decode(&output).unwrap();
                                assert!(!reopened.read_only);
                                assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                                assert_eq!(
                                    reopened.layers[0]
                                        .effects
                                        .iter()
                                        .map(|e| (e.kind.clone(), e.enabled, e.settings.clone()))
                                        .collect::<Vec<_>>(),
                                    settings
                                );
                                assert_eq!(reopened.export_png().unwrap(), current);
                                let standard = psd::decode(&ordinary(&output)).unwrap();
                                assert!(!standard.read_only);
                                assert_eq!(standard.export_png().unwrap(), current);
                            }
                            // Native one-word/one-byte matte differences protect
                            // the whole source, with no tolerance for soft alpha.
                            let at = bytes.len() - 64 * usize::from(depth / 8)
                                + 15 * usize::from(depth / 8)
                                + usize::from(depth == 16);
                            bytes[at] ^= 1;
                            let protected = psd::decode(&bytes).unwrap();
                            assert!(protected.read_only);
                            assert!(protected.layers[0].effects.is_empty());
                            assert!(psd::encode(&protected).is_err());
                        }
                    }
                }
            }
        }
    }
}

fn partial_inner_shadow_payload(
    enabled: bool,
    master: bool,
    list: bool,
    overlay: bool,
    angle: f64,
    variant: &str,
) -> Vec<u8> {
    let list_variant = format!("list:{variant}");
    let source = hard_shadow_payload(
        "IrSh",
        enabled,
        master,
        angle,
        37.,
        if list { &list_variant } else { variant },
    );
    let key = if list { "innerShadowMulti" } else { "IrSh" };
    let kind = if list { b"VlLs" } else { b"Objc" };
    let needle = [key.as_bytes(), kind].concat();
    let at = source
        .windows(needle.len())
        .position(|v| v == needle)
        .unwrap();
    let mut inner = source[at + needle.len()..].to_vec();
    if angle == 45. {
        if let Some(at) = inner.windows(12).position(|v| v == b"DstnUntF#Pxl") {
            inner[at + 12..at + 20].copy_from_slice(&2f64.to_be_bytes());
        }
    }
    // Deliberately store the shadow before its overlay. Descriptor property
    // order must not replace the defined native overlay/inner execution order.
    let mut entries = vec![
        ("masterFXSwitch", b"bool", vec![u8::from(master)]),
        (key, kind, inner),
    ];
    if overlay {
        let source = overlay_payload(true, true, [201., 43., 77.]);
        let at = source.windows(8).position(|v| v == b"SoFiObjc").unwrap() + 8;
        let mut color = source[at..].to_vec();
        let at = color
            .windows(12)
            .position(|v| v == b"OpctUntF#Prc")
            .unwrap()
            + 12;
        color[at..at + 8].copy_from_slice(&43f64.to_be_bytes());
        entries.push(("SoFi", b"Objc", color));
    }
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        overlay_descriptor("Lefx", entries),
    ]
    .concat()
}

#[test]
fn photoshop_partial_alpha_inner_shadows_keep_unsupported_definitions_and_unknown_drop_knockout_protected(
) {
    for depth in [8, 16] {
        let source = std::array::from_fn::<_, 16, _>(|i| {
            [
                native(12347 + i as u16 * 97, depth),
                native(23459, depth),
                native(34571, depth),
                native(
                    if i == 15 {
                        65535
                    } else {
                        12347 + i as u16 * 107
                    },
                    depth,
                ),
            ]
        });
        for variant in [
            "global",
            "antialias",
            "angle",
            "choke",
            "size",
            "noise",
            "contour",
            "unknown",
            "missing",
        ] {
            for list in [false, true] {
                let payload = partial_inner_shadow_payload(false, false, list, true, 180., variant);
                let doc =
                    psd::decode(&framed_shadow_fixture(depth, &source, &payload, &source)).unwrap();
                assert!(doc.read_only, "{depth} {list} {variant}");
                assert!(doc.layers[0].effects.is_empty());
                assert_eq!(doc.bit_depth, depth);
                assert!(psd::encode(&doc).is_err());
            }
        }
        // An invisible Drop Shadow cannot bypass required knockout validation.
        for variant in ["missing_knockout", "invalid_knockout", "blend"] {
            let payload = hard_shadow_payload("DrSh", false, false, 180., 37., variant);
            let protected =
                psd::decode(&framed_shadow_fixture(depth, &source, &payload, &source)).unwrap();
            assert!(protected.read_only);
            assert!(psd::encode(&protected).is_err());
        }
    }
}

#[test]
fn photoshop_mixed_overlays_and_hard_shadows_keep_original_casts_sources_and_native_saves() {
    for depth in [8, 16] {
        let source = std::array::from_fn::<_, 16, _>(|i| {
            let (x, y) = (i % 4, i / 4);
            [
                native(12347 + i as u16 * 107, depth),
                native(23459 + i as u16 * 113, depth),
                native(34571 + i as u16 * 127, depth),
                if (1..=2).contains(&x) && (1..=2).contains(&y) {
                    65535
                } else {
                    0
                },
            ]
        });
        for flags in 0u8..32 {
            let enabled = std::array::from_fn(|i| flags & (1 << i) != 0);
            for master in [false, true] {
                let opacity = if flags % 2 == 0 { 255 } else { 128 };
                let payload = mixed_shadow_overlay_payload(enabled, master, flags % 3 == 0, "");
                // Independent per-pixel reference: each shadow reads source
                // alpha, overlays tint only original coverage, and the outer
                // shadow is placed beneath the complete styled image.
                let mix = |under: [u16; 3], over: [u16; 3], percent: f64| {
                    let alpha = (65535. * percent / 100.).round();
                    std::array::from_fn(|c| {
                        native(
                            ((f64::from(under[c]) * (65535. - alpha) + f64::from(over[c]) * alpha)
                                / 65535.)
                                .round() as u16,
                            depth,
                        )
                    })
                };
                let tint = [111u16 * 257, 67 * 257, 231 * 257];
                let cast = |x: i32, y: i32| {
                    (0..4).contains(&x)
                        && (0..4).contains(&y)
                        && source[(y * 4 + x) as usize][3] == 65535
                };
                let merged = std::array::from_fn::<_, 16, _>(|i| {
                    let (x, y) = (i as i32 % 4, i as i32 / 4);
                    let mut result = source[i];
                    if result[3] != 0 {
                        let mut rgb: [u16; 3] = result[..3].try_into().unwrap();
                        if master {
                            if enabled[1] {
                                rgb = mix(
                                    rgb,
                                    std::array::from_fn(|c| {
                                        native(PATTERN[(x % 2) as usize][c], depth)
                                    }),
                                    50.,
                                );
                            }
                            if enabled[2] {
                                rgb = mix(
                                    rgb,
                                    [((f64::from(x) + 0.5) / 4. * 65535.).round() as u16; 3],
                                    50.,
                                );
                            }
                            if enabled[0] {
                                rgb = mix(rgb, tint, 43.);
                            }
                            if enabled[3] && !cast(x + 1, y - 1) {
                                rgb = mix(rgb, tint, 37.);
                            }
                        }
                        result[..3].copy_from_slice(&rgb);
                    } else {
                        result = if master && enabled[4] && cast(x - 1, y) {
                            [
                                tint[0],
                                tint[1],
                                tint[2],
                                native((65535f64 * 0.23).round() as u16, depth),
                            ]
                        } else {
                            [0; 4]
                        };
                    }
                    result[3] = if depth == 16 {
                        (f64::from(result[3]) * f64::from(f32::from(opacity) / 255.)).round() as u16
                    } else {
                        ((f32::from(result[3] / 257) * (f32::from(opacity) / 255.)).round() as u16)
                            * 257
                    };
                    result
                });
                let bytes = inject_pattern(
                    framed_shadow_fixture_opacity(depth, &source, &payload, &merged, opacity),
                    depth,
                    &pattern_resource(depth, "synthetic-pattern"),
                    false,
                );
                let doc = psd::decode(&bytes).unwrap();
                assert!(
                    !doc.read_only,
                    "{depth} {flags} {master}: {:?}",
                    doc.warnings
                );
                assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
                assert_eq!(rendered(&doc), merged.concat());
                assert_eq!(
                    doc.layers[0]
                        .effects
                        .iter()
                        .map(|e| e.kind.as_str())
                        .collect::<Vec<_>>(),
                    [
                        "pattern_overlay",
                        "gradient_overlay",
                        "color_overlay",
                        "inner_shadow",
                        "drop_shadow"
                    ]
                );
                assert_eq!(
                    doc.layers[0]
                        .effects
                        .iter()
                        .map(|e| e.enabled)
                        .collect::<Vec<_>>(),
                    [enabled[1], enabled[2], enabled[0], enabled[3], enabled[4]]
                        .map(|v| master && v)
                );
                let tile = peerbrush::effects::patterns::get(
                    doc.layers[0].effects[0].settings["bitmap_png"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(tile.depth, depth);
                for x in 0..2 {
                    assert_eq!(
                        tile.sample(x as f64, 0., &doc.layers[0].effects[0].settings),
                        PATTERN[x].map(|v| native(v, depth))
                    );
                }
                let definitions = doc.layers[0]
                    .effects
                    .iter()
                    .map(|e| (e.kind.clone(), e.settings.clone(), e.enabled))
                    .collect::<Vec<_>>();
                let current = doc.export_png().unwrap();
                let layer = doc.layers[0].id.clone();
                let inner = doc.layers[0].effects[3].id.clone();
                let mut engine = Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = doc;
                engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":inner,"enabled":!(master && enabled[3])})], None, None, "Mixed inner bypass").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source.concat());
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                for output in [
                    psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                ] {
                    assert_eq!(
                        saved_composite(&output, depth),
                        saved_composite(&bytes, depth)
                    );
                    let reopened = psd::decode(&output).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                    assert_eq!(
                        reopened.layers[0]
                            .effects
                            .iter()
                            .map(|e| (e.kind.clone(), e.settings.clone(), e.enabled))
                            .collect::<Vec<_>>(),
                        definitions
                    );
                    assert_eq!(reopened.export_png().unwrap(), current);
                    let standard = psd::decode(&ordinary(&output)).unwrap();
                    assert!(!standard.read_only);
                    assert_eq!(standard.export_png().unwrap(), current);
                }
                // An opaque original source sample cannot be hidden by matte
                // normalization; a one-word discrepancy retains protection.
                let mut mismatch = bytes;
                let at = mismatch.len() - 64 * usize::from(depth / 8)
                    + 5 * usize::from(depth / 8)
                    + usize::from(depth == 16);
                mismatch[at] ^= 1;
                let protected = psd::decode(&mismatch).unwrap();
                assert!(protected.read_only);
                assert!(psd::encode(&protected).is_err());
            }
        }
    }
}

fn mixed_shadow_overlay_payload(
    enabled: [bool; 5],
    master: bool,
    list: bool,
    variant: &str,
) -> Vec<u8> {
    let list_variant = format!("list:{variant}");
    let sources = [
        (
            if list { "dropShadowMulti" } else { "DrSh" },
            if list { b"VlLs" } else { b"Objc" },
            hard_shadow_payload(
                "DrSh",
                enabled[4],
                master,
                180.,
                23.,
                if list { "list" } else { "" },
            ),
        ),
        ("GrFl", b"Objc", gradient_payload(false, None, 0.)),
        (
            if list { "innerShadowMulti" } else { "IrSh" },
            if list { b"VlLs" } else { b"Objc" },
            hard_shadow_payload(
                "IrSh",
                enabled[3],
                master,
                45.,
                37.,
                if list { &list_variant } else { variant },
            ),
        ),
        (
            "SoFi",
            b"Objc",
            overlay_payload(true, enabled[0], [111., 67., 231.]),
        ),
        (
            "patternFill",
            b"Objc",
            pattern_payload(enabled[1], "synthetic-pattern", "", 100.),
        ),
    ];
    let mut entries = vec![("masterFXSwitch", b"bool", vec![u8::from(master)])];
    for (key, kind, source) in sources {
        let needle = [key.as_bytes(), kind].concat();
        let at = source
            .windows(needle.len())
            .position(|v| v == needle)
            .unwrap();
        let mut definition = source[at + needle.len()..].to_vec();
        if key == "GrFl" {
            let at = definition
                .windows(8)
                .position(|v| v == b"enabbool")
                .unwrap()
                + 8;
            definition[at] = u8::from(enabled[2]);
        }
        if ["SoFi", "GrFl", "patternFill"].contains(&key) {
            let percent: f64 = if key == "SoFi" { 43. } else { 50. };
            let at = definition
                .windows(12)
                .position(|v| v == b"OpctUntF#Prc")
                .unwrap()
                + 12;
            definition[at..at + 8].copy_from_slice(&percent.to_be_bytes());
        }
        if ["IrSh", "innerShadowMulti"].contains(&key) {
            if let Some(at) = definition.windows(12).position(|v| v == b"DstnUntF#Pxl") {
                definition[at + 12..at + 20].copy_from_slice(&2f64.to_be_bytes());
            }
        }
        entries.push((key, kind, definition));
    }
    [
        0u32.to_be_bytes().to_vec(),
        16u32.to_be_bytes().to_vec(),
        overlay_descriptor("Lefx", entries),
    ]
    .concat()
}

#[test]
fn photoshop_mixed_overlays_and_shadows_protect_unsupported_disabled_definitions() {
    for depth in [8, 16] {
        let source = [[
            native(12347, depth),
            native(23459, depth),
            native(34571, depth),
            65535,
        ]; 16];
        for list in [false, true] {
            for variant in [
                "global",
                "antialias",
                "angle",
                "choke",
                "size",
                "noise",
                "contour",
                "unknown",
                "missing",
                "oversized_list",
                "empty_list",
                "boolean_list",
                "wrong_list_class",
                "both_aliases",
            ] {
                if !list
                    && [
                        "oversized_list",
                        "empty_list",
                        "boolean_list",
                        "wrong_list_class",
                        "both_aliases",
                    ]
                    .contains(&variant)
                {
                    continue;
                }
                let payload = mixed_shadow_overlay_payload([false; 5], false, list, variant);
                let bytes = inject_pattern(
                    framed_shadow_fixture(depth, &source, &payload, &source),
                    depth,
                    &pattern_resource(depth, "synthetic-pattern"),
                    false,
                );
                let doc = psd::decode(&bytes).unwrap();
                assert!(doc.read_only, "{depth} {list} {variant}");
                assert_eq!(rendered(&doc), source.concat());
                assert!(doc.layers[0].effects.is_empty());
                assert!(psd::encode(&doc).is_err());
            }
        }
    }
}

#[test]
fn photoshop_mixed_shadow_stacks_enforce_shared_effect_count_and_original_alpha_limits() {
    let definition = |key: &str, source: Vec<u8>| {
        let needle = [key.as_bytes(), b"Objc"].concat();
        let at = source
            .windows(needle.len())
            .position(|v| v == needle)
            .unwrap();
        source[at + needle.len()..].to_vec()
    };
    let color = definition("SoFi", overlay_payload(true, false, [111., 67., 231.]));
    let gradient = definition("GrFl", gradient_payload(false, None, 0.));
    let inner = definition(
        "IrSh",
        hard_shadow_payload("IrSh", false, false, 180., 37., ""),
    );
    let drop = definition(
        "DrSh",
        hard_shadow_payload("DrSh", false, false, 180., 23., ""),
    );
    let list = |definition: &[u8], count: u32| {
        let mut bytes = count.to_be_bytes().to_vec();
        for _ in 0..count {
            bytes.extend(b"Objc");
            bytes.extend(definition);
        }
        bytes
    };
    for depth in [8, 16] {
        let source = [[
            native(12347, depth),
            native(23459, depth),
            native(34571, depth),
            65535,
        ]; 16];
        for gradient_count in [14, 15, 16] {
            let payload = [
                0u32.to_be_bytes().to_vec(),
                16u32.to_be_bytes().to_vec(),
                overlay_descriptor(
                    "Lefx",
                    vec![
                        ("masterFXSwitch", b"bool", vec![0]),
                        ("solidFillMulti", b"VlLs", list(&color, 16)),
                        (
                            "gradientFillMulti",
                            b"VlLs",
                            list(&gradient, gradient_count),
                        ),
                        ("IrSh", b"Objc", inner.clone()),
                        ("DrSh", b"Objc", drop.clone()),
                    ],
                ),
            ]
            .concat();
            let doc =
                psd::decode(&framed_shadow_fixture(depth, &source, &payload, &source)).unwrap();
            assert_eq!(
                doc.read_only,
                gradient_count != 14,
                "{depth} {gradient_count}: {:?}",
                doc.warnings
            );
            assert_eq!(rendered(&doc), source.concat());
            if gradient_count == 14 {
                assert_eq!(doc.layers[0].effects.len(), 32);
                assert!(doc.layers[0].effects.iter().all(|e| !e.enabled));
                let reopened = psd::decode(&psd::encode(&doc).unwrap()).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].effects.len(), 32);
            } else {
                assert!(psd::encode(&doc).is_err());
            }
        }
        let payload = mixed_shadow_overlay_payload([false; 5], false, false, "");
        let mut partial = source;
        partial[5][3] = native(12347, depth);
        let bytes = inject_pattern(
            framed_shadow_fixture(depth, &partial, &payload, &partial),
            depth,
            &pattern_resource(depth, "synthetic-pattern"),
            false,
        );
        let supported = psd::decode(&bytes).unwrap();
        assert!(!supported.read_only, "{:?}", supported.warnings);
        assert_eq!(supported.layers[0].pixels.rgba16(), partial.concat());
        assert_eq!(
            supported.layers[0].effects.last().unwrap().settings["source_knockout"],
            true
        );
        assert_eq!(supported.bit_depth, depth);
        assert!(
            !psd::decode(&psd::encode(&supported).unwrap())
                .unwrap()
                .read_only
        );
    }
}

#[test]
fn photoshop_hard_shadows_fade_with_layer_opacity_after_casting_and_retain_native_sources() {
    // Independent binary-alpha casts, then one fade of the complete layer.
    // Applying layer opacity before casting would fade the shadow twice.
    for depth in [8, 16] {
        let source = std::array::from_fn::<_, 16, _>(|i| {
            let (x, y) = (i % 4, i / 4);
            [
                native(12347 + i as u16 * 107, depth),
                native(23459 + i as u16 * 113, depth),
                native(34571 + i as u16 * 127, depth),
                if (1..=2).contains(&x) && (1..=2).contains(&y) {
                    65535
                } else {
                    0
                },
            ]
        });
        for (angle, distance, dx, dy) in [(180., 1f64, 1i32, 0i32), (45., 2., -1, 1)] {
            for kind in ["DrSh", "IrSh"] {
                for opacity in [0u8, 1, 64, 128, 254] {
                    for enabled in [false, true] {
                        let mut payload = hard_shadow_payload(kind, enabled, true, angle, 37., "");
                        let at = payload.windows(4).position(|v| v == b"Dstn").unwrap() + 12;
                        payload[at..at + 8].copy_from_slice(&distance.to_be_bytes());
                        let style_alpha = (65535f64 * 0.37).round() as u16;
                        let tint = [111u16 * 257, 67 * 257, 231 * 257];
                        let mut merged = source;
                        for i in 0..16 {
                            let (x, y) = (i as i32 % 4 - dx, i as i32 / 4 - dy);
                            let cast = (0..4).contains(&x)
                                && (0..4).contains(&y)
                                && source[(y * 4 + x) as usize][3] == 65535;
                            if merged[i][3] == 0 {
                                merged[i] = if enabled && kind == "DrSh" && cast {
                                    [tint[0], tint[1], tint[2], native(style_alpha, depth)]
                                } else {
                                    [0; 4]
                                };
                            } else if enabled && kind == "IrSh" && !cast {
                                for c in 0..3 {
                                    merged[i][c] = native(
                                        ((f64::from(source[i][c]) * f64::from(65535 - style_alpha)
                                            + f64::from(tint[c]) * f64::from(style_alpha))
                                            / 65535.)
                                            .round() as u16,
                                        depth,
                                    );
                                }
                            }
                            if opacity == 0 {
                                merged[i] = [0; 4];
                            } else if depth == 16 {
                                merged[i][3] = (f64::from(merged[i][3])
                                    * f64::from(f32::from(opacity) / 255.))
                                .round() as u16;
                            } else {
                                merged[i][3] =
                                    ((f32::from(merged[i][3] / 257) * (f32::from(opacity) / 255.))
                                        .round() as u16)
                                        * 257;
                            }
                        }
                        let bytes = framed_shadow_fixture_opacity(
                            depth, &source, &payload, &merged, opacity,
                        );
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(
                            !doc.read_only,
                            "{depth} {kind} {angle} {opacity} {enabled}: {:?}",
                            doc.warnings
                        );
                        assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
                        assert_eq!(doc.layers[0].opacity, f32::from(opacity) / 255.);
                        assert_eq!(doc.layers[0].effects[0].enabled, enabled);
                        assert_eq!(rendered(&doc), merged.concat());
                        let current = doc.export_png().unwrap();
                        let settings = doc.layers[0].effects[0].settings.as_ref().clone();
                        let layer = doc.layers[0].id.clone();
                        let mut engine = Engine::new();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        engine
                            .edit(
                                "human",
                                &[json!({"op":"layer.update","layer":layer,"opacity":1.})],
                                None,
                                None,
                                "Restore layer opacity",
                            )
                            .unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source.concat());
                        assert_eq!(*engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings, settings);
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        for output in [
                            psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        ] {
                            // Compare independent standard white-matte words/alpha,
                            // including zero opacity, without an RGB unmatting tolerance.
                            assert_eq!(
                                saved_composite(&output, depth),
                                saved_composite(&bytes, depth)
                            );
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                            assert_eq!(reopened.layers[0].opacity, f32::from(opacity) / 255.);
                            assert_eq!(*reopened.layers[0].effects[0].settings, settings);
                            assert_eq!(reopened.export_png().unwrap(), current);
                        }
                        // Opacity does not weaken descriptor protection, even
                        // when neither the layer nor the effect is visible.
                        let invalid = hard_shadow_payload(kind, false, true, angle, 37., "global");
                        let protected = psd::decode(&framed_shadow_fixture_opacity(
                            depth, &source, &invalid, &merged, opacity,
                        ))
                        .unwrap();
                        assert!(protected.read_only);
                        assert!(psd::encode(&protected).is_err());
                    }
                }
            }
        }
    }
}

#[test]
fn photoshop_pixel_aligned_hard_shadows_protect_unsupported_geometry_and_definitions() {
    for depth in [8, 16] {
        for kind in ["DrSh", "IrSh"] {
            for variant in [
                "angle",
                "fractional",
                "distance",
                "global",
                "antialias",
                "choke",
                "size",
                "noise",
                "contour",
                "unknown",
                "missing",
                "mixed",
            ] {
                let source = SAVED.map(|p| p.map(|v| native(v, depth))).repeat(8);
                let payload = hard_shadow_payload(kind, false, true, 45., 100., variant);
                let doc =
                    psd::decode(&framed_shadow_fixture(depth, &source, &payload, &source)).unwrap();
                assert!(doc.read_only, "{depth} {kind} {variant}");
                assert_eq!(doc.bit_depth, depth);
                assert!(doc.layers[0].effects.is_empty());
                assert!(psd::encode(&doc).is_err());
                assert!(psd::encode_psb(&doc).is_err());
            }
        }
    }
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
                            engine.resident_state_mut().expect("resident fixture").document = doc;
                            let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                            let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                            engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})],None,None,"Hard shadow bypass").unwrap();
                            assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                            engine.undo("human").unwrap();
                            assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                            for bytes in [
                                psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                                psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            ] {
                                let reopened = psd::decode(&bytes).unwrap();
                                assert!(!reopened.read_only);
                                assert_eq!(reopened.export_png().unwrap(), current);
                                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                                let settings = reopened.layers[0].effects[0].settings.as_ref();
                                for key in
                                    ["angle", "distance", "opacity", "size", "spread", "contour"]
                                {
                                    assert_eq!(
                                        settings[key],
                                        engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings[key]
                                    );
                                }
                                for channel in 0..3 {
                                    assert!(
                                        (settings["color"][channel].as_f64().unwrap()
                                            - engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["color"]
                                                [channel]
                                                .as_f64()
                                                .unwrap())
                                        .abs()
                                            < 1e-12
                                    );
                                }
                            }
                            let standard =
                                psd::decode(&ordinary(&psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap())).unwrap();
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
fn single_entry_photoshop_shadow_lists_preserve_sources_history_and_native_saves() {
    for depth in [8, 16] {
        for kind in ["DrSh", "IrSh"] {
            for vertical in [false, true] {
                for positive in [false, true] {
                    for flags in 0u8..4 {
                        let (bytes, expected) = shadow_fixture(
                            depth,
                            kind,
                            flags & 1 != 0,
                            flags & 2 != 0,
                            vertical,
                            positive,
                            50.,
                            "list",
                        );
                        let doc = psd::decode(&bytes).unwrap();
                        assert!(!doc.read_only, "{depth} {kind} {flags}: {:?}", doc.warnings);
                        assert_eq!(rendered(&doc), expected);
                        assert_eq!(doc.layers[0].effects.len(), 1);
                        assert_eq!(doc.layers[0].effects[0].enabled, flags == 3);
                        let (single, _) = shadow_fixture(
                            depth,
                            kind,
                            flags & 1 != 0,
                            flags & 2 != 0,
                            vertical,
                            positive,
                            50.,
                            "",
                        );
                        let single = psd::decode(&single).unwrap();
                        assert_eq!(
                            doc.layers[0].effects[0].kind,
                            single.layers[0].effects[0].kind
                        );
                        assert_eq!(
                            doc.layers[0].effects[0].settings,
                            single.layers[0].effects[0].settings
                        );
                        let source = doc.layers[0].pixels.rgba16();
                        let current = doc.export_png().unwrap();
                        let mut engine = Engine::new();
                        engine.resident_state_mut().expect("resident fixture").document = doc;
                        let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                        let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})], None, None, "Listed shadow bypass").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                        for bytes in [
                            psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
                            psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
                        ] {
                            let reopened = psd::decode(&bytes).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.bit_depth, depth);
                            assert_eq!(reopened.export_png().unwrap(), current);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(
                                reopened.layers[0].effects[0].kind,
                                engine.resident_state().expect("resident fixture").document.layers[0].effects[0].kind
                            );
                            assert_eq!(reopened.layers[0].effects[0].enabled, flags == 3);
                            assert_eq!(
                                psd::decode(&ordinary(&bytes))
                                    .unwrap()
                                    .export_png()
                                    .unwrap(),
                                current
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn ordered_hard_inner_shadows_match_saved_pixels_and_reopen_as_standard_styles() {
    let definition = |enabled, color: [u8; 3], variant| {
        let source = hard_shadow_payload("IrSh", enabled, true, 180., 100., variant);
        let start = source.windows(8).position(|value| value == b"IrShObjc").unwrap() + 8;
        let mut entry = source[start..].to_vec();
        for (key, component) in [b"Rd  doub", b"Grn doub", b"Bl  doub"].into_iter().zip(color) {
            let at = entry.windows(key.len()).position(|value| value == key).unwrap() + key.len();
            entry[at..at + 8].copy_from_slice(&f64::from(component).to_be_bytes());
        }
        entry
    };
    let payload = |first, last, middle_variant| {
        let mut list = 3u32.to_be_bytes().to_vec();
        for (enabled, color, variant) in [(true, first, ""), (false, [0, 0, 255], middle_variant), (true, last, "")] {
            list.extend(b"Objc");
            list.extend(definition(enabled, color, variant));
        }
        [0u32.to_be_bytes().to_vec(), 16u32.to_be_bytes().to_vec(), overlay_descriptor("Lefx", vec![
            ("masterFXSwitch", b"bool", vec![1]),
            ("innerShadowMulti", b"VlLs", list),
            ("numModifyingFX", b"long", 2u32.to_be_bytes().to_vec()),
        ])].concat()
    };
    let first = [231, 9, 40];
    let last = [24, 186, 107];
    for depth in [8, 16] {
        let source = [[native(12451, depth), native(26771, depth), native(51233, depth), 65535]; 16];
        // Both casts shade the same first column. The later opaque color wins.
        let merged = std::array::from_fn::<_, 16, _>(|index| if index % 4 == 0 {
            [u16::from(last[0]) * 257, u16::from(last[1]) * 257, u16::from(last[2]) * 257, 65535]
        } else { source[index] });
        let file = |payload: &[u8]| {
            let mut bytes = framed_shadow_fixture(depth, &source, payload, &merged);
            let tag = bytes.windows(8).position(|value| value == b"8BIMlfx2").unwrap() + 4;
            bytes[tag..tag + 4].copy_from_slice(b"lmfx");
            bytes
        };
        let bytes = file(&payload(first, last, ""));
        let doc = psd::decode(&bytes).unwrap();
        assert!(!doc.read_only, "{:?}", doc.warnings);
        assert_eq!(rendered(&doc), merged.concat());
        assert_eq!(doc.layers[0].pixels.rgba16(), source.concat());
        let effects = &doc.layers[0].effects;
        assert_eq!(effects.iter().map(|effect| effect.enabled).collect::<Vec<_>>(), [true, false, true]);
        for saved in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            assert_eq!(saved_composite(&saved, depth), saved_composite(&bytes, depth));
            assert!(saved.windows(8).any(|value| value == b"8BIMlmfx"));
            for candidate in [&saved, &ordinary(&saved)] {
                let reopened = psd::decode(candidate).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                assert_eq!(reopened.layers[0].effects.len(), effects.len());
                for (actual, expected) in reopened.layers[0].effects.iter().zip(effects) {
                    assert_eq!(actual.settings, expected.settings);
                    assert_eq!(actual.enabled, expected.enabled);
                }
            }
        }
        for descriptor in [payload(last, first, ""), payload(first, last, "size")] {
            let protected = psd::decode(&file(&descriptor)).unwrap();
            assert!(protected.read_only);
            assert_eq!(rendered(&protected), merged.concat());
        }
    }
}

#[test]
fn ambiguous_malformed_or_unsupported_shadow_lists_remain_protected() {
    for depth in [8, 16] {
        for kind in ["DrSh", "IrSh"] {
            for enabled in [false, true] {
                for variant in [
                    "empty_list",
                    "multiple_list",
                    "oversized_list",
                    "boolean_list",
                    "wrong_list_class",
                    "both_aliases",
                    "plain_multi",
                    "list:global",
                    "list:antialias",
                    "list:angle",
                    "list:fractional",
                    "list:distance",
                    "list:choke",
                    "list:size",
                    "list:noise",
                    "list:contour",
                    "list:unknown",
                    "list:missing",
                    "list:mixed",
                ] {
                    if kind == "IrSh" && variant == "multiple_list" {
                        continue;
                    }
                    let (bytes, expected) =
                        shadow_fixture(depth, kind, enabled, true, false, true, 100., variant);
                    let doc = psd::decode(&bytes).unwrap();
                    assert!(doc.read_only, "{depth} {kind} {enabled} {variant}");
                    assert_eq!(rendered(&doc), expected);
                    assert_eq!(doc.bit_depth, depth);
                }
                let (mut bytes, mut expected) =
                    shadow_fixture(depth, kind, enabled, true, false, true, 100., "list");
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
                    for variant in ["blend", "missing_knockout", "invalid_knockout"] {
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

#[test]
fn photoshop_gradient_placement_retains_native_sources_and_exact_standard_output() {
    for depth in [8, 16] {
        for (width, height) in [(2, 1), (1, 2)] {
            for (angle, scale, offset) in [
                (0f64, 0.1, (0.25, -0.375)),
                (180., 0.5, (-0.25, 0.375)),
                (90., 2., (0.125, -0.375)),
                (-90., 10., (-1., 1.)),
                (0., 1., (1., -1.)),
            ] {
                for reflected in [false, true] {
                    for reverse in [false, true] {
                        for enabled in [false, true] {
                            let mut payload = positioned_gradient_payload(
                                reverse,
                                None,
                                0.,
                                scale * 100.,
                                Some((offset.0 * 100., offset.1 * 100.)),
                            );
                            let at = payload
                                .windows(12)
                                .position(|v| v == b"AnglUntF#Ang")
                                .unwrap()
                                + 12;
                            payload[at..at + 8].copy_from_slice(&angle.to_be_bytes());
                            if reflected {
                                let at = payload.windows(4).position(|v| v == b"Lnr ").unwrap();
                                payload[at..at + 4].copy_from_slice(b"Rflc");
                            }
                            if !enabled {
                                let at =
                                    payload.windows(8).position(|v| v == b"enabbool").unwrap() + 8;
                                payload[at] = 0;
                            }
                            let mut spec = Spec::paint("Synthetic positioned gradient", SAVED);
                            spec.tags.push((*b"lfx2", payload));
                            let mut bytes = fixture_size(&[spec], depth, width, height);
                            let expected = std::array::from_fn::<_, 2, _>(|i| {
                                if !enabled {
                                    return SAVED[i].map(|n| native(n, depth));
                                }
                                let x = if width == 2 { i as f64 } else { 0. };
                                let y = if height == 2 { i as f64 } else { 0. };
                                let px = (x + 0.5) / f64::from(width) - 0.5 - offset.0;
                                let py = (y + 0.5) / f64::from(height) - 0.5 - offset.1;
                                let p = if angle == 0. {
                                    px
                                } else if angle == 180. {
                                    -px
                                } else if angle == 90. {
                                    -py
                                } else {
                                    py
                                };
                                let mut t = if reflected {
                                    2. * p.abs() / scale
                                } else {
                                    0.5 + p / scale
                                };
                                if reverse {
                                    t = 1. - t;
                                }
                                let n = native((t.clamp(0., 1.) * 65535.).round() as u16, depth);
                                [n, n, n, 65535]
                            });
                            bytes.truncate(bytes.len() - 8 * usize::from(depth / 8));
                            for c in 0..4 {
                                for p in expected {
                                    composite_sample(&mut bytes, p, c, depth);
                                }
                            }
                            let doc = psd::decode(&bytes).unwrap();
                            assert!(!doc.read_only,"{depth} {width}x{height} {angle} {scale} {offset:?} {reflected} {reverse} {enabled}: {:?}",doc.warnings);
                            assert_eq!(doc.layers[0].effects[0].enabled, enabled);
                            assert_eq!(doc.layers[0].effects[0].settings["scale"], scale);
                            assert_eq!(doc.layers[0].effects[0].settings["offset_x"], offset.0);
                            assert_eq!(doc.layers[0].effects[0].settings["offset_y"], offset.1);
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
                            for output in
                                [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()]
                            {
                                let reopened = psd::decode(&output).unwrap();
                                assert!(!reopened.read_only);
                                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                                assert_eq!(reopened.export_png().unwrap(), current);
                                assert_eq!(
                                    reopened.layers[0].effects[0].settings,
                                    doc.layers[0].effects[0].settings
                                );
                            }
                            let layer = doc.layers[0].id.clone();
                            let effect = doc.layers[0].effects[0].id.clone();
                            let mut e = Engine::new();
                            e.resident_state_mut().expect("resident fixture").document = doc;
                            e.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})],None,None,"Gradient bypass").unwrap();
                            e.undo("human").unwrap();
                            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), current);
                            assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), source);
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn photoshop_gradient_placement_rejects_invalid_units_ranges_and_composite_mismatch() {
    for depth in [8, 16] {
        for (scale, offset) in [
            (0., (0., 0.)),
            (9.99, (0., 0.)),
            (1000.01, (0., 0.)),
            (100., (-100.01, 0.)),
            (100., (0., 100.01)),
        ] {
            let mut spec = Spec::paint("Synthetic protected placement", SAVED);
            spec.tags.push((
                *b"lfx2",
                positioned_gradient_payload(false, None, 0., scale, Some(offset)),
            ));
            let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
            assert!(doc.read_only);
            assert!(doc.layers[0].effects.is_empty());
            assert_eq!(
                doc.layers[0].pixels.get16(0, 0),
                SAVED[0].map(|n| native(n, depth))
            );
        }
        for needle in [b"HrznUntF#Prc", b"Scl UntF#Prc"] {
            let mut payload = positioned_gradient_payload(false, None, 0., 50., Some((25., -37.5)));
            let at = payload.windows(12).rposition(|v| v == needle).unwrap() + 8;
            payload[at..at + 4].copy_from_slice(b"#Pxl");
            let mut spec = Spec::paint("Synthetic malformed placement", SAVED);
            spec.tags.push((*b"lfx2", payload));
            let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
            assert!(doc.read_only);
            assert!(doc.layers[0].effects.is_empty());
        }
        let mut spec = Spec::paint("Synthetic mismatched placement", SAVED);
        spec.tags.push((
            *b"lfx2",
            positioned_gradient_payload(false, None, 0., 50., Some((25., -37.5))),
        ));
        let doc = psd::decode(&fixture(&[spec], depth)).unwrap();
        assert!(doc.read_only);
        assert!(doc.layers[0].effects.is_empty());
    }
}
