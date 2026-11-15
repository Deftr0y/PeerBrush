use super::*;
use super::{
    effect_create_tests::compare_raster,
    layer_paste_tests::shared_large_document,
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;

#[test]
fn native_edits_preserve_large_unrelated_shared_sources_and_retire_only_their_outputs() {
    for depth in [8, 16] {
        for case in 0..5 {
            let fixture = Fixture::new();
            let (runtime, _connection) =
                fixture.runtime_with_budgets(128 * 1024 * 1024, 16 * 1024 * 1024);
            let mut small = Document::new_depth(8, 8, depth).unwrap();
            small.layers[0].id = "target".into();
            small.layers[0].pixels.set16(2, 2, [12347, 23459, 34571, 65535]);
            if case == 0 {
                small = Engine::preview_edits(small, &[
                    json!({"op":"effect.add","layer":"target","kind":"invert","weight":0.3}),
                ]).unwrap();
            }
            if case == 1 {
                let mut other = small.layers[0].clone();
                other.id = "other".into();
                other.x = 3;
                small.layers.push(other);
            }
            if case == 2 { small.selection = Some([0, 0, 8, 8]); }
            let command = match case {
                0 => json!({"op":"effect.bake","layer":"target","effect":small.layers[0].effects[0].id}),
                1 => json!({"op":"layer.merge","layers":["target","other"]}),
                2 => json!({"op":"layer.duplicate_selection","layer":"target"}),
                3 => json!({"op":"image.resize","width":16000,"height":512}),
                _ => {
                    let path = super::import_tests::file(&fixture, depth);
                    json!({"op":"image.import","png":STANDARD.encode(std::fs::read(path).unwrap())})
                }
            };
            let expected = if case == 3 { small.clone() }
                else { Engine::preview_edits(small.clone(), &[command.clone()]).unwrap() };
            let mut original = shared_large_document(depth);
            original.selection = small.selection;
            original.layers.splice(0..0, small.layers);
            let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let mut staged = CapturedScene::from_parts(original, "native-source-budget".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(())).unwrap();
            let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
            let session = staged.session.take().unwrap();
            let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let disk = runtime.disk().used().unwrap();
            let referenced_bytes: usize = native.metadata().layers().map(|l| {
                native.source(l.pixels().slot()).unwrap().physical_tiles()
                    .map(|t| t.reference().spec().bytes()).sum::<usize>()
            }).sum();
            assert!(referenced_bytes > 512 * 1024 * 1024);
            let preview = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            let metadata = preview.preview().metadata();
            assert_eq!((metadata.width(), metadata.height(), metadata.depth()), (16000, 512, depth));
            assert_eq!(metadata.layers().len(), expected.layers.len() + 17);
            for (actual, expected) in metadata.layers().take(expected.layers.len()).zip(&expected.layers) {
                assert_eq!(actual.origin(), [expected.x, expected.y]);
                compare_raster(preview.preview(), actual.pixels(), &expected.pixels);
            }
            for old in native.metadata().layers().filter(|l| l.id().starts_with("shared-")) {
                let new = metadata.layers().find(|l| l.id() == old.id()).unwrap();
                assert_eq!(native.source(old.pixels().slot()).unwrap().key(),
                    preview.preview().source(new.pixels().slot()).unwrap().key());
            }
            preview.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            if case != 3 {
                let refused = prepare(capture(&state, &[command]), &session, &|| {
                    if runtime.disk().used().unwrap() > disk { Err("Cancel admitted native edit".into()) }
                    else { Ok(()) }
                }).err().unwrap();
                assert!(refused.reason.contains("Cancel admitted native edit"), "{}", refused.reason);
                assert!(refused.cleanup_error.is_none());
                assert_eq!(runtime.disk().used().unwrap(), disk);
            }
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
        }
    }
}
