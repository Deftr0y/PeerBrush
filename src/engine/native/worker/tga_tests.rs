use super::tests::{exercise_edits, Fixture};
use serde_json::json;

#[test]
fn tga_import_placement_and_patch_preserve_orientation_native_channels_and_undo() {
    let root = Fixture::new();
    let path = root.path().join("source.TGA");
    for (kind, bits, palette, orientation) in [
        (2, 15, 0, 0),
        (10, 32, 0, 0x10),
        (9, 16, 32, 0x30),
        (3, 16, 0, 0x20),
    ] {
        std::fs::write(
            &path,
            crate::image_import::tga::tests::fixture(259, 3, kind, bits, palette, orientation),
        )
        .unwrap();
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
fn tga_failures_cancel_batches_and_late_reservations_adopt_nothing() {
    super::qoi_tests::failed_streams(
        "tga",
        crate::image_import::tga::tests::fixture(259, 3, 9, 16, 32, 0x10),
    );
}
