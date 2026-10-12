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
                        e.doc = doc;
                        let id = e.doc.layers[0].id.clone();
                        let effect = e.doc.layers[0].effects[0].id.clone();
                        let mut settings = e.doc.layers[0].effects[0].settings.clone();
                        settings["phase_x"] = json!(1.);
                        e.edit("human",&[json!({"op":"effect.update","layer":id,"effect":effect,"settings":settings})],None,None,"Transparent phase").unwrap();
                        assert_eq!(e.undo.len(), 1);
                        assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
                        e.undo("human").unwrap();
                        assert_eq!(e.doc.export_png().unwrap(), rendered);
                        for large in [false, true] {
                            let saved = if large {
                                psd::encode_psb(&e.doc).unwrap()
                            } else {
                                psd::encode(&e.doc).unwrap()
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
                engine.doc = doc;
                let layer = engine.doc.layers[0].id.clone();
                let effect = engine.doc.layers[0].effects[0].id.clone();
                engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Mixed precision pattern bypass").unwrap();
                assert_eq!(engine.doc.bit_depth, 16);
                assert_eq!(engine.doc.layers[0].pixels.rgba16(), SAVED.concat());
                assert_eq!(engine.undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.export_png().unwrap(), current);
                for output in [
                    psd::encode(&engine.doc).unwrap(),
                    psd::encode_psb(&engine.doc).unwrap(),
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
                engine.doc = doc;
                let layer = engine.doc.layers[0].id.clone();
                let effect = engine.doc.layers[0].effects[0].id.clone();
                engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})],None,None,"Optional-angle bypass").unwrap();
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
                    assert_eq!(reopened.bit_depth, depth);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                    assert_eq!(
                        reopened.layers[0].effects[0].settings["bitmap_png"],
                        engine.doc.layers[0].effects[0].settings["bitmap_png"]
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
                    engine.doc = doc;
                    engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Pattern storage-frame bypass").unwrap();
                    assert_eq!(engine.undo.len(), 1);
                    engine.undo("human").unwrap();
                    assert_eq!(engine.doc.layers[0].pixels.rgba16(), source.concat());
                    assert_eq!(engine.doc.export_png().unwrap(), current);
                    for output in [
                        psd::encode(&engine.doc).unwrap(),
                        psd::encode_psb(&engine.doc).unwrap(),
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
                        engine.doc = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})], None, None, "HSB style bypass").unwrap();
                        assert_eq!(engine.undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.doc.export_png().unwrap(), current);
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
                        engine.doc = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})], None, None, "Grayscale style bypass").unwrap();
                        assert_eq!(engine.undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.doc.export_png().unwrap(), current);
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
                    let settings = &doc.layers[0].effects[0].settings;
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
                    engine.doc = doc;
                    let layer = engine.doc.layers[0].id.clone();
                    let effect = engine.doc.layers[0].effects[0].id.clone();
                    engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})], None, None, "Normalized RGB bypass").unwrap();
                    assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                    assert_eq!(engine.undo.len(), 1);
                    engine.undo("human").unwrap();
                    assert_eq!(engine.doc.export_png().unwrap(), current);
                    for output in [
                        psd::encode(&engine.doc).unwrap(),
                        psd::encode_psb(&engine.doc).unwrap(),
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
                let midpoint = f64::from(pair[0].1) / 100.;
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
                        let settings = doc.layers[0].effects[0].settings.clone();
                        let color_stops = settings["color_stops"].as_array().unwrap();
                        assert_eq!(
                            color_stops.len(),
                            colors.len()
                                + usize::from(colors[0].0 != 0)
                                + usize::from(colors.last().unwrap().0 != 4096)
                        );
                        for &(position, midpoint, color) in colors {
                            let stop = color_stops
                                .iter()
                                .find(|s| s["position"] == f64::from(position) / 4096.)
                                .unwrap();
                            assert_eq!(stop["midpoint"], f64::from(midpoint) / 100.);
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
                            for &(position, midpoint, opacity) in alpha {
                                let stop = alpha_stops
                                    .iter()
                                    .find(|s| s["position"] == f64::from(position) / 4096.)
                                    .unwrap();
                                assert_eq!(stop["midpoint"], f64::from(midpoint) / 100.);
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
                            assert_eq!(reopened.layers[0].effects[0].settings, settings);
                            assert_eq!(reopened.export_png().unwrap(), current);
                            let standard = psd::decode(&ordinary(&output)).unwrap();
                            assert!(!standard.read_only);
                            assert_eq!(standard.layers[0].pixels.rgba16(), merged.concat());
                            assert_eq!(standard.export_png().unwrap(), current);
                        }
                        let mut engine = Engine::new();
                        let layer = doc.layers[0].id.clone();
                        let effect = doc.layers[0].effects[0].id.clone();
                        engine.doc = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Clamped gradient bypass").unwrap();
                        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.doc.export_png().unwrap(), current);
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
    for (at, value) in midpoints.into_iter().zip([color, 50, alpha, 50]) {
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
                        let settings = &doc.layers[0].effects[0].settings;
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
                            assert_eq!(reopened.layers[0].effects[0].settings, *settings);
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
                        engine.doc = doc;
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Gradient bypass").unwrap();
                        assert_eq!(engine.undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.doc.export_png().unwrap(), current);
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

// Independent normalized-channel expectations, including source/backdrop endpoint
// precedence and the Photoshop Soft Light low-backdrop polynomial.
fn expected_style_blend(base: f64, tint: f64, blend: &str) -> f64 {
    match blend {
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
                        let settings = doc.layers[0].effects[0].settings.clone();
                        let mut engine = Engine::new();
                        engine.doc = doc;
                        let layer = engine.doc.layers[0].id.clone();
                        let effect = engine.doc.layers[0].effects[0].id.clone();
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})], None, None, "Inversion style bypass").unwrap();
                        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
                        assert_eq!(engine.undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.doc.export_png().unwrap(), current);
                        for output in [
                            psd::encode(&engine.doc).unwrap(),
                            psd::encode_psb(&engine.doc).unwrap(),
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
                        assert_eq!(reopened.layers[0].opacity, f32::from(opacity) / 255.);
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
            "empty_list" | "multiple_list" | "boolean_list" | "wrong_list_class" | "both_aliases"
        );
    let list_value = || {
        let count = match variant {
            "empty_list" => 0u32,
            "multiple_list" => 2,
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
                        let settings = effect.settings.clone();
                        let layer = doc.layers[0].id.clone();
                        let effect = effect.id.clone();
                        let current = doc.export_png().unwrap();
                        let mut engine = Engine::new();
                        engine.doc = doc;
                        engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})],None,None,"Pixel shadow bypass").unwrap();
                        assert_eq!(engine.undo.len(), 1);
                        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source.concat());
                        engine.undo("human").unwrap();
                        assert_eq!(engine.doc.export_png().unwrap(), current);
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
                            psd::encode(&engine.doc).unwrap(),
                            psd::encode_psb(&engine.doc).unwrap(),
                        ] {
                            assert_eq!(saved_composite(&output, depth), matte.concat());
                            let reopened = psd::decode(&output).unwrap();
                            assert!(!reopened.read_only);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source.concat());
                            assert_eq!(reopened.layers[0].effects[0].settings, settings);
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
                            engine.doc = doc;
                            engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!(enabled && master)})], None, None, "Partial inner bypass").unwrap();
                            assert_eq!(engine.undo.len(), 1);
                            assert_eq!(engine.doc.layers[0].pixels.rgba16(), source.concat());
                            engine.undo("human").unwrap();
                            assert_eq!(engine.doc.export_png().unwrap(), current);
                            for output in [
                                psd::encode(&engine.doc).unwrap(),
                                psd::encode_psb(&engine.doc).unwrap(),
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
fn photoshop_partial_alpha_inner_shadows_keep_unsupported_definitions_and_drop_casts_protected() {
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
        // A disabled, otherwise valid Drop Shadow with an exact source
        // composite still needs the separate partial-alpha knockout model.
        for enabled in [false, true] {
            let payload = hard_shadow_payload("DrSh", enabled, false, 180., 37., "");
            let protected =
                psd::decode(&framed_shadow_fixture(depth, &source, &payload, &source)).unwrap();
            assert!(protected.read_only);
            assert!(protected
                .warnings
                .iter()
                .any(|v| v.contains("Drop Shadows on partial-alpha")));
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
                            if enabled[0] {
                                rgb = mix(rgb, tint, 43.);
                            }
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
                        "color_overlay",
                        "pattern_overlay",
                        "gradient_overlay",
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
                    enabled.map(|v| master && v)
                );
                let tile = peerbrush::effects::patterns::get(
                    doc.layers[0].effects[1].settings["bitmap_png"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(tile.depth, depth);
                for x in 0..2 {
                    assert_eq!(
                        tile.sample(x as f64, 0., &doc.layers[0].effects[1].settings),
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
                engine.doc = doc;
                engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":inner,"enabled":!(master && enabled[3])})], None, None, "Mixed inner bypass").unwrap();
                assert_eq!(engine.doc.layers[0].pixels.rgba16(), source.concat());
                assert_eq!(engine.undo.len(), 1);
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.export_png().unwrap(), current);
                for output in [
                    psd::encode(&engine.doc).unwrap(),
                    psd::encode_psb(&engine.doc).unwrap(),
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
                "multiple_list",
                "empty_list",
                "boolean_list",
                "wrong_list_class",
                "both_aliases",
            ] {
                if !list
                    && [
                        "multiple_list",
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
        let protected = psd::decode(&bytes).unwrap();
        assert!(protected.read_only);
        assert!(protected
            .warnings
            .iter()
            .any(|v| v.contains("partial-alpha")));
        assert_eq!(protected.bit_depth, depth);
        assert!(psd::encode(&protected).is_err());
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
                        let settings = doc.layers[0].effects[0].settings.clone();
                        let layer = doc.layers[0].id.clone();
                        let mut engine = Engine::new();
                        engine.doc = doc;
                        engine
                            .edit(
                                "human",
                                &[json!({"op":"layer.update","layer":layer,"opacity":1.})],
                                None,
                                None,
                                "Restore layer opacity",
                            )
                            .unwrap();
                        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source.concat());
                        assert_eq!(engine.doc.layers[0].effects[0].settings, settings);
                        assert_eq!(engine.undo.len(), 1);
                        engine.undo("human").unwrap();
                        assert_eq!(engine.doc.export_png().unwrap(), current);
                        for output in [
                            psd::encode(&engine.doc).unwrap(),
                            psd::encode_psb(&engine.doc).unwrap(),
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
                            assert_eq!(reopened.layers[0].effects[0].settings, settings);
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
                                let settings = &reopened.layers[0].effects[0].settings;
                                for key in
                                    ["angle", "distance", "opacity", "size", "spread", "contour"]
                                {
                                    assert_eq!(
                                        settings[key],
                                        engine.doc.layers[0].effects[0].settings[key]
                                    );
                                }
                                for channel in 0..3 {
                                    assert!(
                                        (settings["color"][channel].as_f64().unwrap()
                                            - engine.doc.layers[0].effects[0].settings["color"]
                                                [channel]
                                                .as_f64()
                                                .unwrap())
                                        .abs()
                                            < 1e-12
                                    );
                                }
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
                        engine.doc = doc;
                        let layer = engine.doc.layers[0].id.clone();
                        let effect = engine.doc.layers[0].effects[0].id.clone();
                        engine.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":flags!=3})], None, None, "Listed shadow bypass").unwrap();
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
                            assert_eq!(reopened.bit_depth, depth);
                            assert_eq!(reopened.export_png().unwrap(), current);
                            assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                            assert_eq!(
                                reopened.layers[0].effects[0].kind,
                                engine.doc.layers[0].effects[0].kind
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
fn ambiguous_malformed_or_unsupported_shadow_lists_remain_protected() {
    for depth in [8, 16] {
        for kind in ["DrSh", "IrSh"] {
            for enabled in [false, true] {
                for variant in [
                    "empty_list",
                    "multiple_list",
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
                            e.doc = doc;
                            e.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!enabled})],None,None,"Gradient bypass").unwrap();
                            e.undo("human").unwrap();
                            assert_eq!(e.doc.export_png().unwrap(), current);
                            assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
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
