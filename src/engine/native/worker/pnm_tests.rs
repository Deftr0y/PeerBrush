use super::tests::{exercise_edits, Fixture};
use serde_json::json;

#[test]
fn netpbm_import_placement_patch_and_promotion_preserve_precision_and_undo() {
    let root = Fixture::new();
    for (kind, extension, channels, max) in [
        (1, "pbm", 1, 1),
        (4, "PBM", 1, 1),
        (2, "pgm", 1, 1023),
        (5, "PGM", 1, 65535),
        (3, "ppm", 3, 255),
        (6, "PPM", 3, 65535),
        (7, "pam", 2, 65535),
        (7, "PNM", 4, 65535),
    ] {
        let path = root.path().join(format!("source.{extension}"));
        std::fs::write(
            &path,
            crate::image_import::pnm::tests::fixture(kind, 259, 3, channels, max),
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
                let succeeds = command["op"] != "image.patch" || max <= 255 || depth == 16;
                exercise_edits(original.clone(), &[command.clone()], succeeds);
            }
            // Earlier imports promote the document before a native16 patch.
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
fn netpbm_failures_cancel_batches_without_adopting_pixels_or_history() {
    super::qoi_tests::failed_streams(
        "pam",
        crate::image_import::pnm::tests::fixture(7, 259, 3, 4, 65535),
    );
}
