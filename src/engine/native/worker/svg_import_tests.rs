use super::tests::{exercise_edits, Fixture};
use serde_json::json;
use std::io::Write;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="270" height="9"><path d="M .25 .5 Q 130 8 269.5 .5 L 180 8.75 Z" fill="#317fc3" fill-opacity=".67"/></svg>"##;

#[test]
fn svg_files_retain_import_definitions_rasterize_placement_and_paste_and_undo() {
    let fixture = Fixture::new();
    for extension in ["svg", "SVGZ"] {
        let path = fixture.path().join(format!("vector.{extension}"));
        let bytes = if extension == "svg" {
            SVG.as_bytes().to_vec()
        } else {
            let mut gzip =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gzip.write_all(SVG.as_bytes()).unwrap();
            gzip.finish().unwrap()
        };
        std::fs::write(&path, bytes).unwrap();
        for depth in [8, 16] {
            let original = super::canvas_tests::document(depth);
            let commands = [
                json!({"op":"image.import","path":path,"parent":"B","x":-2,"y":3}),
                json!({"op":"image.place","path":path,"layer":"outside","new_layer":false,"rect":[-5,-2,9,7]}),
                json!({"op":"image.place","path":path,"parent":"B","rect":[2,1,8,6]}),
                json!({"op":"image.paste","path":path,"layer":"outside","origin":[-4,7]}),
            ];
            for command in &commands {
                exercise_edits(original.clone(), &[command.clone()], true);
                for field in ["frame", "page", "variant"] {
                    let mut invalid = command.clone();
                    invalid[field] = json!(0);
                    exercise_edits(original.clone(), &[commands[0].clone(), invalid], false);
                }
            }
            exercise_edits(original, &commands, true);
        }
    }
}

#[test]
fn malformed_or_unsupported_svg_never_adopts_partial_imports() {
    let fixture = Fixture::new();
    let good = fixture.path().join("good.svg");
    std::fs::write(&good, SVG).unwrap();
    let bad = fixture.path().join("bad.svg");
    for bytes in [
        b"<svg".to_vec(),
        vec![0xff, 0xfe, 0xff],
        SVG.replace("<path", "<image href=\"https://example.invalid/image.png\"")
            .into_bytes(),
        vec![b' '; crate::svg::MARKUP_LIMIT + 1],
    ] {
        std::fs::write(&bad, bytes).unwrap();
        for operation in ["image.import", "image.place", "image.paste"] {
            exercise_edits(
                super::canvas_tests::document(16),
                &[
                    json!({"op":"image.import","path":good}),
                    json!({"op":operation,"path":bad,"layer":"outside"}),
                ],
                false,
            );
        }
    }
}
