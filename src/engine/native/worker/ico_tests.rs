use super::tests::{exercise_edits, Fixture};
use serde_json::json;

pub(crate) fn fixture(items: &[(u32, u32, u16, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = vec![0; 6 + items.len() * 16];
    bytes[..4].copy_from_slice(&[0, 0, 1, 0]);
    bytes[4..6].copy_from_slice(&(items.len() as u16).to_le_bytes());
    for (i, (w, h, bits, payload)) in items.iter().enumerate() {
        let offset = bytes.len() as u32;
        let e = &mut bytes[6 + i * 16..22 + i * 16];
        e[0] = (*w).min(256) as u8;
        e[1] = (*h).min(256) as u8;
        e[4..6].copy_from_slice(&1u16.to_le_bytes());
        e[6..8].copy_from_slice(&bits.to_le_bytes());
        e[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        e[12..16].copy_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(payload);
    }
    bytes
}

pub(crate) fn dib(mut bmp: Vec<u8>, mask: bool) -> (u32, u32, u16, Vec<u8>) {
    let offset = u32::from_le_bytes(bmp[10..14].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(bmp[14..18].try_into().unwrap()) as usize;
    let (width, height, bits) = if size == 12 {
        let width = u16::from_le_bytes(bmp[18..20].try_into().unwrap());
        let height = u16::from_le_bytes(bmp[20..22].try_into().unwrap());
        bmp[20..22].copy_from_slice(&(height * 2).to_le_bytes());
        (
            u32::from(width),
            u32::from(height),
            u16::from_le_bytes(bmp[24..26].try_into().unwrap()),
        )
    } else {
        let width = u32::from_le_bytes(bmp[18..22].try_into().unwrap());
        let height = i32::from_le_bytes(bmp[22..26].try_into().unwrap());
        bmp[22..26].copy_from_slice(&(height * 2).to_le_bytes());
        (
            width,
            height.unsigned_abs(),
            u16::from_le_bytes(bmp[28..30].try_into().unwrap()),
        )
    };
    // The BMP fixture has a thirteen-byte file-offset gap. ICO stores its
    // pixels directly after metadata (image's V4/V5 bitfields skip twelve).
    let mut payload = bmp[14..offset - 13].to_vec();
    if matches!(size, 108 | 124) && u32::from_le_bytes(bmp[30..34].try_into().unwrap()) == 3 {
        payload.extend_from_slice(&[0; 12]);
    }
    payload.extend_from_slice(&bmp[offset..]);
    if mask {
        let stride = width.div_ceil(32) as usize * 4;
        for y in 0..height {
            let mut row = vec![0; stride];
            for x in 0..width {
                if (x + y * 3) % 5 == 0 {
                    row[x as usize / 8] |= 128 >> (x % 8);
                }
            }
            payload.extend_from_slice(&row);
        }
    }
    (width, height, bits, payload)
}

#[test]
fn png_icon_variants_preserve_native16_selection_placement_paste_and_undo() {
    let root = Fixture::new();
    let path = root.path().join("variants.ICO");
    let p8 = crate::raster::png(2, 1, &[17, 29, 41, 0, 53, 67, 89, 193]).unwrap();
    let p16 =
        crate::raster::png16(2, 1, &[12347, 23459, 34571, 49153, 54321, 32109, 10987, 0]).unwrap();
    std::fs::write(&path, fixture(&[(2, 1, 32, p8), (2, 1, 64, p16)])).unwrap();
    for depth in [8, 16] {
        let original = super::canvas_tests::document(depth);
        for variant in [0, 1] {
            for command in [
                json!({"op":"image.import","path":path,"variant":variant,"parent":"B","x":-3,"y":2}),
                json!({"op":"image.place","path":path,"variant":variant,"layer":"outside","new_layer":false,"rect":[-5,-2,9,7]}),
                json!({"op":"image.paste","path":path,"variant":variant,"layer":"outside","origin":[-4,7]}),
            ] {
                exercise_edits(original.clone(), &[command], true);
            }
        }
        for fields in [
            json!({}),
            json!({"variant":2}),
            json!({"variant":null}),
            json!({"variant":-1}),
            json!({"variant":0,"frame":0}),
            json!({"variant":0,"page":0}),
        ] {
            for op in ["image.import", "image.place", "image.paste"] {
                let mut invalid = json!({"op":op,"path":path,"layer":"outside"});
                invalid
                    .as_object_mut()
                    .unwrap()
                    .extend(fields.as_object().unwrap().clone());
                exercise_edits(
                    original.clone(),
                    &[
                        json!({"op":"image.import","path":path,"variant":0}),
                        invalid,
                    ],
                    false,
                );
            }
        }
        // image's patch decoder deliberately refuses PNG16 ICO payloads.
        exercise_edits(
            original,
            &[json!({"op":"image.patch","path":path,"layer":"outside"})],
            false,
        );
    }
}

#[test]
fn bmp_icon_masks_palette_bitfields_rle_and_top_down_rows_match_resident() {
    let root = Fixture::new();
    let path = root.path().join("bitmap.ico");
    let mut items = Vec::new();
    for (size, bits, compression, alpha) in [
        (12, 1, 0, false),
        (40, 2, 0, false),
        (40, 4, 0, false),
        (40, 8, 0, false),
        (40, 16, 0, false),
        (40, 24, 0, false),
        (40, 32, 0, true),
        (40, 16, 3, false),
        (52, 32, 3, false),
        (56, 16, 3, true),
        (108, 32, 3, true),
        (124, 32, 3, true),
    ] {
        for top_down in [false, true].into_iter().filter(|t| size != 12 || !t) {
            for mask in [false, true] {
                items.push(dib(
                    crate::image_import::bmp::tests::fixture(
                        35,
                        3,
                        size,
                        bits,
                        compression,
                        top_down,
                        alpha,
                    ),
                    mask,
                ));
            }
        }
    }
    for bits in [4, 8] {
        items.push(dib(
            crate::image_import::bmp::tests::rle_fixture(bits),
            true,
        ));
    }
    items.push(dib(
        crate::image_import::bmp::tests::fixture(270, 9, 40, 32, 0, false, true),
        true,
    ));
    for item in items {
        std::fs::write(&path, fixture(&[item])).unwrap();
        for depth in [8, 16] {
            for command in [
                json!({"op":"image.import","path":path,"parent":"B"}),
                json!({"op":"image.paste","path":path,"layer":"outside","origin":[-4,7]}),
                json!({"op":"image.place","path":path,"layer":"outside","new_layer":false,"rect":[-5,-2,9,7]}),
                json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":0,"rect":[0,0,6,5]}),
            ] {
                exercise_edits(super::canvas_tests::document(depth), &[command], true);
            }
        }
    }
}

#[test]
fn truncated_icon_payloads_batches_cancellation_and_late_leases_adopt_nothing() {
    super::qoi_tests::failed_streams(
        "ico",
        fixture(&[(
            2,
            1,
            32,
            crate::raster::png(2, 1, &[17, 29, 41, 0, 53, 67, 89, 193]).unwrap(),
        )]),
    );
    super::qoi_tests::failed_streams(
        "ico",
        fixture(&[dib(
            crate::image_import::bmp::tests::fixture(270, 9, 40, 32, 0, false, true),
            true,
        )]),
    );
}

#[test]
fn png_icon_patch_uses_best_variant_ties_and_raw_hidden_rgb() {
    let root = Fixture::new();
    let path = root.path().join("best.ico");
    let first = crate::raster::png(2, 1, &[17, 29, 41, 0, 53, 67, 89, 193]).unwrap();
    let last = crate::raster::png(2, 1, &[97, 103, 107, 0, 109, 113, 127, 211]).unwrap();
    for depths in [[32, 32], [32, 8]] {
        std::fs::write(
            &path,
            fixture(&[
                (2, 1, depths[0], first.clone()),
                (2, 1, depths[1], last.clone()),
            ]),
        )
        .unwrap();
        for depth in [8, 16] {
            exercise_edits(
                super::canvas_tests::document(depth),
                &[json!({"op":"image.patch","path":path,"layer":"outside","x":1,"y":2})],
                true,
            );
        }
    }
}
