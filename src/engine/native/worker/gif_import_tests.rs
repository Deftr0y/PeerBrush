use super::tests::{exercise_edits, Fixture};
use serde_json::json;
use std::borrow::Cow;

pub(crate) fn fixture() -> Vec<u8> {
    use ::gif::{DisposalMethod::*, Encoder, Frame};
    let mut bytes = Vec::new();
    let mut encoder = Encoder::new(
        &mut bytes,
        270,
        9,
        &[17, 29, 43, 255, 13, 27, 31, 239, 83, 11, 47, 241],
    )
    .unwrap();
    for (index, (left, top, width, height, dispose, interlaced)) in [
        (0, 0, 270, 9, Keep, true),
        (2, 1, 263, 7, Background, true),
        (0, 0, 270, 9, Previous, false),
        (268, 6, 6, 4, Any, true),
        (400, 20, 3, 2, Keep, false),
    ]
    .into_iter()
    .enumerate()
    {
        let rows: Vec<u16> = if interlaced {
            [(0, 8), (4, 8), (2, 4), (1, 2)]
                .into_iter()
                .flat_map(|(start, step)| (start..height).step_by(step))
                .collect()
        } else {
            (0..height).collect()
        };
        let pixels = rows
            .into_iter()
            .flat_map(|y| (0..width).map(move |x| ((x + y * 3 + index as u16) % 4) as u8))
            .collect();
        encoder
            .write_frame(&Frame {
                left,
                top,
                width,
                height,
                dispose,
                interlaced,
                transparent: Some(0),
                buffer: Cow::Owned(pixels),
                ..Frame::default()
            })
            .unwrap();
    }
    drop(encoder);
    bytes
}

pub(crate) fn static_geometry(width: u16, height: u16, interlaced: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = ::gif::Encoder::new(&mut bytes, width, height, &[17, 29, 43, 53, 67, 89]).unwrap();
    let rows: Vec<_> = if interlaced {
        [(0, 8), (4, 8), (2, 4), (1, 2)].into_iter()
            .flat_map(|(start, step)| (start..height).step_by(step)).collect()
    } else {
        (0..height).collect()
    };
    let buffer = rows.into_iter().flat_map(|y| (0..width).map(move |x| ((x ^ y) & 1) as u8)).collect();
    encoder.write_frame(&::gif::Frame {
        width, height, interlaced, transparent: Some(0), buffer: Cow::Owned(buffer),
        ..Default::default()
    }).unwrap();
    drop(encoder);
    bytes
}

#[test]
fn gif_frame_composition_interlace_disposal_and_clipping_match_resident_at_both_depths() {
    let root = Fixture::new();
    let path = root.path().join("animation.GIF");
    std::fs::write(&path, fixture()).unwrap();
    for depth in [8, 16] {
        let original = super::canvas_tests::document(depth);
        for frame in 0..5 {
            for command in [
                json!({"op":"image.import","path":path,"frame":frame,"parent":"B","x":-3,"y":2}),
                json!({"op":"image.place","path":path,"frame":frame,"layer":"outside","new_layer":false,"rect":[-5,-2,9,7]}),
                json!({"op":"image.paste","path":path,"frame":frame,"layer":"outside","origin":[-4,7]}),
            ] {
                exercise_edits(original.clone(), &[command], true);
            }
        }
        for fields in [
            json!({}),
            json!({"frame":5}),
            json!({"frame":-1}),
            json!({"frame":null}),
            json!({"frame":0,"page":0}),
            json!({"frame":0,"variant":0}),
        ] {
            for operation in ["image.import", "image.place", "image.paste"] {
                let mut invalid = json!({"op":operation,"path":path,"layer":"outside"});
                invalid
                    .as_object_mut()
                    .unwrap()
                    .extend(fields.as_object().unwrap().clone());
                exercise_edits(
                    original.clone(),
                    &[json!({"op":"image.import","path":path,"frame":0}), invalid],
                    false,
                );
            }
        }
    }
}

#[test]
fn static_gif_preserves_hidden_palette_rgb_and_zeroes_invalid_indices_per_row() {
    let root = Fixture::new();
    let path = root.path().join("static.gif");
    let mut bytes = Vec::new();
    {
        let mut encoder = ::gif::Encoder::new(&mut bytes, 2, 2, &[13, 27, 41, 53, 67, 89]).unwrap();
        encoder
            .write_frame(&::gif::Frame {
                width: 2,
                height: 2,
                transparent: Some(1),
                buffer: Cow::Borrowed(&[0, 1, 3, 0]),
                ..::gif::Frame::default()
            })
            .unwrap();
    }
    std::fs::write(&path, bytes).unwrap();
    for depth in [8, 16] {
        for op in ["image.import", "image.place", "image.paste"] {
            exercise_edits(
                super::canvas_tests::document(depth),
                &[json!({"op":op,"path":path,"layer":"outside"})],
                true,
            );
        }
    }
}

pub(crate) fn profiled_fixture() -> Vec<u8> {
    let mut bytes = fixture();
    // Patch intentionally consumes raw channels; these bytes are not an importable ICC profile.
    bytes.splice(
        25..25,
        b"\x21\xff\x0bICCRGBG1012\x03\x01\x01\x7f\0".iter().copied(),
    );
    bytes
}

#[test]
fn gif_patches_use_raw_default_frame_with_selection_clipping_and_native_history() {
    let root = Fixture::new();
    let path = root.path().join("patch.GIF");
    for bytes in [fixture(), profiled_fixture()] {
        std::fs::write(&path, bytes).unwrap();
        for depth in [8, 16] {
            for (x, y, rect) in [
                (-3, 0, [-1, 0, 6, 5]),
                (3, 2, [2, 1, 9, 8]),
                (500, 500, [0, 0, 9, 8]),
            ] {
                exercise_edits(
                    super::canvas_tests::document(depth),
                    &[
                        serde_json::json!({"op":"image.patch","path":path,"layer":"outside","x":x,"y":y,"rect":rect}),
                    ],
                    true,
                );
            }
        }
    }
}
