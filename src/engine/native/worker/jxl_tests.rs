use super::tests::{exercise_edits_with_budget, Fixture};
use crate::jpeg_xl::{self, Options, PixelsRef};
use serde_json::json;

#[test]
fn isolated_jxl_rows_preserve_precision_profiles_placement_and_undo() {
    let _codec = jpeg_xl::test_process_guard();
    let root = Fixture::new();
    let path = root.path().join("source.JXL");
    for incoming in [8, 16] {
        let bytes: Vec<_> = (0..270 * 3 * 4).map(|n| (n * 31) as u8).collect();
        let words: Vec<_> = (0..270 * 3 * 4)
            .map(|n| (n * 1009 + 12347) as u16)
            .collect();
        for profiled in [false, true] {
            let profile = if profiled {
                moxcms::ColorProfile::new_display_p3().encode().unwrap()
            } else {
                crate::color_profile::srgb_profile().to_vec()
            };
            let encoded = jpeg_xl::encode(
                270,
                3,
                if incoming == 8 {
                    PixelsRef::Eight(&bytes)
                } else {
                    PixelsRef::Sixteen(&words)
                },
                Some(&profile),
                Options::default(),
                &|| Ok(()),
            )
            .unwrap();
            std::fs::write(&path, &encoded).unwrap();
            for depth in [8, 16] {
                let original = super::canvas_tests::document(depth);
                for command in [
                    json!({"op":"image.import","path":path,"x":-3,"y":2,"parent":"B"}),
                    json!({"op":"image.place","path":path,"layer":"outside","new_layer":false,"rect":[-2,1,9,8]}),
                    json!({"op":"image.paste","path":path,"layer":"outside","origin":[-4,2]}),
                ] {
                    exercise_edits_with_budget(
                        original.clone(),
                        &[command],
                        true,
                        768 * 1024 * 1024,
                    );
                }
                exercise_edits_with_budget(
                    original.clone(),
                    &[json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":2,"rect":[0,0,267,7]})],
                    incoming <= depth,
                    768 * 1024 * 1024,
                );
                for key in ["frame", "page", "variant"] {
                    let first = json!({"op":"image.import","path":path});
                    let mut wrong = first.clone();
                    wrong[key] = json!(0);
                    exercise_edits_with_budget(
                        original.clone(),
                        &[first, wrong],
                        false,
                        768 * 1024 * 1024,
                    );
                }
            }
        }
    }
}
