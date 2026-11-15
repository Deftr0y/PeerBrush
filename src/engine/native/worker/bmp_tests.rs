use super::tests::{exercise_edits, Fixture};
use serde_json::json;

#[test]
fn bmp_import_placement_patch_and_history_keep_native_documents() {
    let root = Fixture::new();
    let path = root.path().join("source.BMP");
    for bytes in [
        crate::image_import::bmp::tests::fixture(259, 3, 12, 4, 0, false, false),
        crate::image_import::bmp::tests::fixture(259, 3, 40, 24, 0, true, false),
        crate::image_import::bmp::tests::fixture(259, 3, 56, 16, 3, false, true),
        crate::image_import::bmp::tests::rle_fixture(4),
        crate::image_import::bmp::tests::rle_fixture(8),
    ] {
        std::fs::write(&path, bytes).unwrap();
        for depth in [8, 16] {
            let original = super::canvas_tests::document(depth);
            let commands = [
                json!({"op":"image.import","path":path,"x":-2,"y":3}),
                json!({"op":"image.import","path":path,"parent":"B","frame":0}),
                json!({"op":"image.paste","path":path,"layer":"outside","origin":[-4,7]}),
                json!({"op":"image.place","path":path,"layer":"outside","new_layer":false,"rect":[-5,-2,9,7]}),
                json!({"op":"image.place","path":path,"new_layer":true,"parent":"B","rect":[2,1,8,6]}),
                json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":0,"rect":[0,0,6,3]}),
            ];
            for command in &commands {
                exercise_edits(original.clone(), &[command.clone()], true);
            }
            exercise_edits(original.clone(), &commands, true);
            for choice in [json!({"frame":1}), json!({"page":0}), json!({"variant":0})] {
                let mut command = commands[0].clone();
                command
                    .as_object_mut()
                    .unwrap()
                    .extend(choice.as_object().unwrap().clone());
                exercise_edits(original.clone(), &[commands[0].clone(), command], false);
            }
        }
    }
}

#[test]
fn bmp_failures_cancel_batches_and_late_reservations_adopt_nothing() {
    for bits in [4, 8] {
        super::qoi_tests::failed_streams("bmp", crate::image_import::bmp::tests::rle_fixture(bits));
    }
}

#[test]
fn bmp_wide_channels_refuse_import_placement_and_patch_at_either_destination_depth() {
    let root = Fixture::new();
    let path = root.path().join("wide.bmp");
    std::fs::write(&path, crate::image_import::bmp::tests::wide_channels()).unwrap();
    for depth in [8, 16] {
        let original = super::canvas_tests::document(depth);
        for command in [
            json!({"op":"image.import","path":path}),
            json!({"op":"image.paste","path":path,"layer":"outside"}),
            json!({"op":"image.place","path":path,"layer":"outside","new_layer":false}),
            json!({"op":"image.patch","path":path,"layer":"outside"}),
        ] {
            exercise_edits(original.clone(), &[command], false);
        }
    }
}
