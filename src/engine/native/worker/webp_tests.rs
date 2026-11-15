use super::tests::{exercise_edits, Fixture};
use crate::image_import::webp::tests::{fixture, lossless, lossy};
use serde_json::json;

#[test]
fn native_webp_static_and_selected_animation_commands_match_resident_and_undo() {
    let root = Fixture::new();
    let path = root.path().join("source.WEBP");
    for (bytes, frames) in [
        (lossless(270, 19, 17, false), 1),
        (lossless(270, 19, 53, true), 1),
        (lossy(false), 1),
        (lossy(true), 1),
        (fixture(1, false), 1),
        (fixture(4, false), 4),
        (fixture(4, true), 4),
    ] {
        std::fs::write(&path, bytes).unwrap();
        for depth in [8, 16] {
            let original = super::canvas_tests::document(depth);
            for frame in 0..frames {
                for command in [
                    json!({"op":"image.import","path":path,"frame":frame,"x":-3,"y":2,"parent":"B"}),
                    json!({"op":"image.place","path":path,"frame":frame,"layer":"outside","new_layer":false,"rect":[-2,1,9,8]}),
                    json!({"op":"image.paste","path":path,"frame":frame,"layer":"outside","origin":[-4,2]}),
                ] {
                    exercise_edits(original.clone(), &[command], true);
                }
            }
            exercise_edits(
                original.clone(),
                &[json!({"op":"image.import","path":path})],
                frames == 1,
            );
            exercise_edits(
                original.clone(),
                &[
                    json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":2,"rect":[0,0,8,7]}),
                ],
                true,
            );
            for choice in [
                json!({"frame":frames}),
                json!({"frame":-1}),
                json!({"page":0}),
                json!({"variant":0}),
            ] {
                let first = json!({"op":"image.import","path":path,"frame":0});
                let mut wrong = json!({"op":"image.import","path":path});
                wrong
                    .as_object_mut()
                    .unwrap()
                    .extend(choice.as_object().unwrap().clone());
                exercise_edits(original.clone(), &[first, wrong], false);
            }
        }
    }
}

#[test]
fn webp_declared_profile_refusal_is_atomic_while_raw_patches_ignore_color_metadata() {
    let root = Fixture::new();
    let path = root.path().join("profile.webp");
    let mut missing = fixture(1, false);
    missing[20] |= 32;
    let mut invalid = fixture(1, true);
    let start = invalid.windows(4).position(|b| b == b"ICCP").unwrap() + 8;
    invalid[start..start + 128].fill(0);
    for bytes in [missing, invalid] {
        std::fs::write(&path, bytes).unwrap();
        for depth in [8, 16] {
            let original = super::canvas_tests::document(depth);
            for op in ["image.import", "image.place", "image.paste"] {
                exercise_edits(
                    original.clone(),
                    &[json!({"op":op,"path":path,"layer":"outside","frame":0})],
                    false,
                );
            }
            exercise_edits(
                original,
                &[json!({"op":"image.patch","path":path,"layer":"outside"})],
                true,
            );
        }
    }
}

#[test]
fn webp_failed_streams_cancellation_and_late_reservations_adopt_nothing() {
    super::qoi_tests::failed_streams("webp", lossless(270, 19, 17, true));
}
