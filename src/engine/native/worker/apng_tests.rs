use super::tests::{exercise_edits, Fixture};
use crate::image_import::prepared_png::apng::tests::fixture;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;

#[test]
fn wide_inline_animation_refuses_before_staging_or_history_changes() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 8193, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_animated(1, 0).unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[13, 57, 123, 255].repeat(8193)).unwrap();
        writer.finish().unwrap();
    }
    let encoded = STANDARD.encode(bytes);
    for depth in [8, 16] {
        let native = crate::engine::native::NativeDocument::new_blank(
            16000, 2, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
        ).unwrap();
        let state = crate::engine::authority::DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let layer = native.metadata().layers().next().unwrap().id();
        for op in ["image.place", "image.paste"] {
            let disk = runtime.disk().used().unwrap();
            let command = json!({"op":op,"png":encoded,"layer":layer,"frame":0});
            let refused = super::prepare(
                super::tests::capture(&state, &[command]), &session, &|| Ok(()),
            ).err().unwrap();
            assert!(refused.reason.contains("PNG animation exceeds its native size limits"));
            assert!(refused.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
        }
    }
}

#[test]
fn apng_color_orientation_base64_and_default_image_patch_keep_their_policies() {
    let root = Fixture::new();
    let path = root.path().join("animation.png");
    let bytes = crate::image_import::prepared_png::apng::tests::profiled();
    std::fs::write(&path, &bytes).unwrap();
    for depth in [8, 16] {
        let original = super::canvas_tests::document(depth);
        for op in ["image.place", "image.paste"] {
            exercise_edits(
                original.clone(),
                &[json!({"op":op,"png":STANDARD.encode(&bytes),"frame":3,"layer":"outside"})],
                true,
            );
        }
        exercise_edits(
            original.clone(),
            &[json!({"op":"image.import","path":path,"frame":4})],
            true,
        );
        exercise_edits(
            original,
            &[
                json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":0,"rect":[-1,0,6,5]}),
            ],
            true,
        );
    }
    std::fs::write(
        &path,
        crate::image_import::prepared_png::apng::tests::single(),
    )
    .unwrap();
    for command in [
        json!({"op":"image.import","path":path}),
        json!({"op":"image.import","path":path,"frame":0}),
    ] {
        let succeeds = command.get("frame").is_some();
        exercise_edits(super::canvas_tests::document(16), &[command], succeeds);
    }
}

#[test]
fn selected_apng_frames_match_resident_disposal_blending_interlace_and_undo() {
    let root = Fixture::new();
    let path = root.path().join("animation.PNG");
    for (interlaced, hidden, color) in [
        (false, false, 6),
        (true, true, 6),
        (true, false, 3),
        (false, true, 0),
        (true, false, 2),
        (false, false, 4),
    ] {
        std::fs::write(&path, fixture(interlaced, hidden, 8, color)).unwrap();
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
        }
    }
}

#[test]
fn missing_or_invalid_apng_choices_leave_batches_unchanged() {
    let root = Fixture::new();
    let path = root.path().join("animation.png");
    std::fs::write(&path, fixture(false, false, 8, 6)).unwrap();
    for depth in [8, 16] {
        let original = super::canvas_tests::document(depth);
        for fields in [
            json!({}),
            json!({"frame":5}),
            json!({"frame":-1}),
            json!({"frame":null}),
            json!({"frame":0,"page":0}),
            json!({"frame":0,"variant":0}),
        ] {
            for op in ["image.import", "image.place", "image.paste"] {
                let mut command = json!({"op":op,"path":path,"layer":"outside"});
                command
                    .as_object_mut()
                    .unwrap()
                    .extend(fields.as_object().unwrap().clone());
                exercise_edits(
                    original.clone(),
                    &[json!({"op":"image.import","path":path,"frame":0}), command],
                    false,
                );
            }
        }
    }
}

#[test]
fn native_16bit_apng_selects_interlaced_frames_and_preserves_precision_through_edits() {
    let root = Fixture::new();
    let path = root.path().join("Animation16.png");
    std::fs::write(&path, fixture(true, true, 16, 6)).unwrap();
    for op in ["image.import", "image.place", "image.paste"] {
        exercise_edits(
            super::canvas_tests::document(8),
            &[json!({"op":op,"path":path,"frame":3,"layer":"outside"})],
            true,
        );
    }
}
