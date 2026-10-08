use flate2::{write::ZlibEncoder, Compression};
use peerbrush::{engine::Engine, psd};
use serde_json::json;
use std::io::Write;

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend(v.to_be_bytes());
}
fn be32(out: &mut Vec<u8>, v: u32) {
    out.extend(v.to_be_bytes());
}
fn block(out: &mut Vec<u8>, v: &[u8]) {
    be32(out, v.len() as u32);
    out.extend(v);
}
fn tag(out: &mut Vec<u8>, key: &[u8; 4], v: &[u8]) {
    out.extend(b"8BIM");
    out.extend(key);
    block(out, v);
    out.resize(out.len() + (4 - v.len() % 4) % 4, 0);
}
fn matte(color: u16, alpha: u16) -> u16 {
    ((color as u64 * alpha as u64 + 65535 * (65535 - alpha as u64) + 32767) / 65535) as u16
}
fn sample_planes() -> Vec<Vec<u16>> {
    let colors = [
        [0, 0, 0],
        [128, 0, 255],
        [255, 64, 16],
        [12, 180, 77],
        [150, 60, 10],
        [1, 2, 3],
    ];
    let alpha = [0u16, 65535, 32768, 257, 1, 65000];
    let mut planes = vec![vec![]; 4];
    for (rgb, a) in colors.into_iter().zip(alpha) {
        for c in 0..3 {
            planes[c].push(matte(rgb[c] as u16 * 257, a));
        }
        planes[3].push(a);
    }
    planes
}
// Realistic Photoshop shape: empty primary info; one raw 16-bit layer under Lr16.
fn layer_info(w: u32, h: u32, planes: &[Vec<u16>], negative: bool) -> Vec<u8> {
    let mut info = vec![];
    be16(&mut info, if negative { (-1i16) as u16 } else { 1 });
    for v in [0, 0, h, w] {
        be32(&mut info, v);
    }
    be16(&mut info, planes.len() as u16);
    for (c, plane) in planes.iter().enumerate() {
        be16(&mut info, if c == 3 { (-1i16) as u16 } else { c as u16 });
        be32(&mut info, 2 + plane.len() as u32 * 2);
    }
    info.extend(b"8BIMnorm");
    info.extend([255, 0, 0, 0]);
    let mut extra = vec![0; 8];
    extra.extend([6, b'S', b'o', b'u', b'r', b'c', b'e', 0]);
    tag(&mut extra, b"SoLd", &[]); // Unsupported smart-object source forces a protected merged preview.
    block(&mut info, &extra);
    for plane in planes {
        be16(&mut info, 0);
        for &v in plane {
            be16(&mut info, v);
        }
    }
    info
}
fn composite(w: u32, h: u32, planes: &[Vec<u16>], kind: u16) -> Vec<u8> {
    let raw = planes
        .iter()
        .flat_map(|p| p.iter().flat_map(|v| v.to_be_bytes()))
        .collect::<Vec<_>>();
    let mut out = vec![];
    be16(&mut out, kind);
    match kind {
        0 => out.extend(raw),
        1 => {
            let rows = raw
                .chunks_exact(w as usize * 2)
                .map(|row| {
                    let mut packed = vec![row.len() as u8 - 1];
                    packed.extend(row);
                    packed
                })
                .collect::<Vec<_>>();
            assert_eq!(rows.len(), h as usize * planes.len());
            for row in &rows {
                be16(&mut out, row.len() as u16);
            }
            for row in rows {
                out.extend(row);
            }
        }
        2 | 3 => {
            let mut data = raw;
            if kind == 3 {
                for row in data.chunks_exact_mut(w as usize * 2) {
                    let mut previous = 0u16;
                    for bytes in row.chunks_exact_mut(2) {
                        let v = u16::from_be_bytes([bytes[0], bytes[1]]);
                        bytes.copy_from_slice(&v.wrapping_sub(previous).to_be_bytes());
                        previous = v;
                    }
                }
            }
            let mut zip = ZlibEncoder::new(vec![], Compression::fast());
            zip.write_all(&data).unwrap();
            out.extend(zip.finish().unwrap());
        }
        _ => {}
    }
    out
}
fn fixture(
    w: u32,
    h: u32,
    planes: &[Vec<u16>],
    kind: u16,
    mt16: bool,
    negative: bool,
    has_composite: bool,
) -> Vec<u8> {
    let mut out = b"8BPS".to_vec();
    be16(&mut out, 1);
    out.extend([0; 6]);
    be16(&mut out, planes.len() as u16);
    be32(&mut out, h);
    be32(&mut out, w);
    be16(&mut out, 16);
    be16(&mut out, 3);
    block(&mut out, &[]);
    let mut resources = vec![];
    if !has_composite {
        resources.extend(b"8BIM");
        be16(&mut resources, 1057);
        resources.extend([0, 0]);
        block(&mut resources, &[0, 0, 0, 1, 0]);
        resources.push(0);
    }
    block(&mut out, &resources);
    let mut section = vec![0; 8];
    if mt16 {
        tag(&mut section, b"Mt16", &[]);
    }
    tag(&mut section, b"Lr16", &layer_info(w, h, planes, negative));
    tag(&mut section, b"cinf", &[1, 2, 3]); // Odd global lengths require four-byte alignment.
    block(&mut out, &section);
    out.extend(composite(w, h, planes, kind));
    out
}
fn pixels(bytes: &[u8]) -> Vec<u8> {
    let doc = psd::decode(bytes).unwrap();
    assert!(doc.read_only);
    doc.preview(None, doc.width.max(doc.height), None, false)
        .unwrap()
        .2
}

#[test]
fn raw_rle_zip_and_sixteen_bit_prediction_show_the_same_saved_composite() {
    let planes = sample_planes();
    let expected = vec![
        0, 0, 0, 0, 128, 0, 255, 255, 255, 64, 16, 128, 12, 180, 77, 1, 0, 0, 0, 0, 1, 2, 3, 253,
    ];
    for compression in 0..=3 {
        assert_eq!(
            pixels(&fixture(3, 2, &planes, compression, true, false, true)),
            expected,
            "compression {compression}"
        );
    }
}

#[test]
fn lr16_negative_layer_count_is_transparency_and_unmarked_extra_channel_stays_opaque() {
    let planes = sample_planes();
    assert_eq!(
        pixels(&fixture(3, 2, &planes, 0, false, true, true)),
        pixels(&fixture(3, 2, &planes, 0, true, false, true))
    );
    let opaque = pixels(&fixture(3, 2, &planes, 0, false, false, true));
    assert!(opaque.chunks_exact(4).all(|p| p[3] == 255));
    assert_eq!(&opaque[..4], &[255, 255, 255, 255]);
}

#[test]
fn opaque_three_channel_sixteen_bit_images_are_readonly_and_cannot_overwrite_their_sources() {
    let planes = vec![
        vec![0, 65535, 12345],
        vec![32896, 257, 54321],
        vec![65535, 0, 32768],
    ];
    let doc = psd::decode(&fixture(3, 1, &planes, 0, false, false, true)).unwrap();
    assert_eq!(doc.layers.len(), 1);
    assert!(doc.read_only);
    assert!(doc
        .warnings
        .iter()
        .any(|w| w.contains("16-bit") && w.contains("read-only")));
    assert_eq!(doc.layers[0].pixels.get(0, 0), [0, 128, 255, 255]);
    assert!(psd::encode(&doc).is_err());
    let mut e = Engine::new();
    e.doc = doc;
    let id = e.doc.layers[0].id.clone();
    let before = e.doc.export_png().unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"paint.fill","layer":id,"color":[0,0,0,255]})],
            None,
            None,
            "must stay protected"
        )
        .is_err());
    assert_eq!(e.doc.export_png().unwrap(), before);
    assert!(e.undo.is_empty());
}

#[test]
fn missing_composite_corrupt_zip_truncation_and_rle_overflow_are_rejected_without_fake_images() {
    let planes = sample_planes();
    let missing = fixture(3, 2, &planes, 0, true, false, false);
    assert!(psd::decode(&missing)
        .unwrap_err()
        .contains("Maximize PSD Compatibility"));
    let mut zip = fixture(3, 2, &planes, 2, true, false, true);
    zip.truncate(zip.len() - 4);
    assert!(psd::decode(&zip).is_err());
    let mut raw = fixture(3, 2, &planes, 0, true, false, true);
    raw.truncate(raw.len() - 2);
    assert!(psd::decode(&raw).is_err());
    let mut rle = fixture(3, 2, &planes, 1, true, false, true);
    let compressed_len = composite(3, 2, &planes, 1).len();
    let start = rle.len() - compressed_len;
    rle[start + 2 + 8 * 2] = 7; // Eight literal bytes cannot fit a six-byte row.
    assert!(psd::decode(&rle).is_err());
    let mut huge = fixture(3, 2, &planes, 0, true, false, true);
    huge[14..18].copy_from_slice(&8192u32.to_be_bytes());
    huge[18..22].copy_from_slice(&8192u32.to_be_bytes());
    assert!(psd::decode(&huge).is_err());
}
