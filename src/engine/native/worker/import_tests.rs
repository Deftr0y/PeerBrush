use super::*;
use super::{
    effect_create_tests::compare_raster,
    grouping_tests::{compare, gate},
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::json;

pub(super) fn native_png16(width: u32, height: u32, words: &[u16]) -> Vec<u8> {
    use image::ImageEncoder;
    crate::raster::CanvasLimits::Native.check(width, height).unwrap();
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(bytemuck::cast_slice(words), width, height, image::ExtendedColorType::Rgba16)
        .unwrap();
    bytes
}

pub(super) fn file(fixture: &Fixture, depth: u16) -> std::path::PathBuf {
    let path = fixture.path().join(format!("Imported-{depth}.png"));
    let words = [12347, 23459, 34571, 49153, 54321, 32109, 10987, 0];
    let png = if depth == 16 {
        crate::raster::png16(2, 1, &words)
    } else {
        crate::raster::png(2, 1, &words.map(crate::raster::project16))
    }
    .unwrap();
    std::fs::write(&path, png).unwrap();
    path
}
pub(super) fn pixels(native: &NativeDocument, expected: &Document, original: &Document) {
    let mut expected = expected.clone();
    let mut renamed = Vec::new();
    for (actual, layer) in native.metadata().layers().zip(&mut expected.layers) {
        if !original.layers.iter().any(|old| old.id == layer.id) {
            assert!(!original.layers.iter().any(|old| old.id == actual.id()));
            renamed.push((layer.id.clone(), actual.id().to_owned()));
            layer.id = actual.id().into();
        }
    }
    for layer in &mut expected.layers {
        for reference in [&mut layer.parent, &mut layer.clip_to] {
            if let Some((_, new)) = renamed
                .iter()
                .find(|(old, _)| reference.as_ref() == Some(old))
            {
                *reference = Some(new.clone());
            }
        }
    }
    compare(native, &expected);
    for (actual, layer) in native.metadata().layers().zip(&expected.layers) {
        compare_raster(native, actual.pixels(), &layer.pixels);
        if let Some(mask) = actual.mask() {
            for (actual, step) in mask.steps().zip(&layer.mask.as_ref().unwrap().steps) {
                compare_raster(native, actual.pixels(), &step.pixels);
            }
        }
    }
}

#[test]
fn native_import_inserts_and_promotes_without_losing_retained_sources_or_selections() {
    for (depth, incoming) in [(8, 8), (8, 16), (16, 8), (16, 16)] {
        for parent in [None, Some("B")] {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
            let original = super::settings_tests::document(depth);
            let path = file(&fixture, incoming);
            let command =
                json!({"op":"image.import","path":path,"parent":parent,"x":-4,"y":7,"frame":0});
            let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "native-import".into(),
                vec![],
                None,
                None,
            )
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
            let native =
                NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                    .unwrap();
            let session = staged.session.take().unwrap();
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let disk = runtime.disk().used().unwrap();
            let preview =
                prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            pixels(preview.preview(), &expected, &original);
            assert_eq!(preview.preview().metadata().depth(), depth.max(incoming));
            let layer = preview
                .preview()
                .metadata()
                .layers()
                .find(|l| l.name() == format!("Imported-{incoming}"))
                .unwrap();
            assert_eq!(layer.origin(), [-4, 7]);
            assert_eq!(layer.parent(), parent);
            let source = preview.preview().source(layer.pixels().slot()).unwrap();
            let region = source
                .prepare_region(
                    crate::raster::backed::Region::new([0, 0, 2, 1]).unwrap(),
                    &mut || Ok(()),
                )
                .unwrap();
            assert_eq!(
                region.pixel16(1, 0).unwrap(),
                if incoming == 16 {
                    [54321, 32109, 10987, 0]
                } else {
                    [54321, 32109, 10987, 0].map(|v| u16::from(crate::raster::project16(v)) * 257)
                }
            );
            drop(region);
            let before = native.metadata().selection_coverage().unwrap();
            let after = preview.preview().metadata().selection_coverage().unwrap();
            assert_eq!(
                native.source(before.mask().slot()).unwrap().key(),
                preview.preview().source(after.mask().slot()).unwrap().key()
            );
            let old = native
                .metadata()
                .layers()
                .find(|l| l.id() == "a_base")
                .unwrap()
                .effects()
                .find(|e| e.kind() == "liquify")
                .unwrap()
                .clips()
                .next()
                .unwrap()
                .1;
            let kept = preview
                .preview()
                .metadata()
                .layers()
                .find(|l| l.id() == "a_base")
                .unwrap()
                .effects()
                .find(|e| e.kind() == "liquify")
                .unwrap()
                .clips()
                .next()
                .unwrap()
                .1;
            assert_eq!(
                native.source(old.mask().slot()).unwrap().key(),
                preview.preview().source(kept.mask().slot()).unwrap().key()
            );
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Import image",
                        task: None,
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            state
                .commit_native(gate(&[]), commit)
                .unwrap()
                .retire(&session, &|| session.cleanup_check())
                .unwrap();
            for (direction, mut expected) in [
                (Direction::Undo, original.clone()),
                (Direction::Redo, expected),
            ] {
                let moved = state
                    .native_history()
                    .unwrap()
                    .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                    .unwrap();
                drop(state.move_native(gate(&[]), moved).unwrap());
                expected.revision = state.view().revision();
                pixels(state.native().unwrap(), &expected, &original);
            }
            drop((
                state,
                native,
                session,
                staged.seals,
                cache,
                runtime,
                connection,
            ));
        }
    }
}

#[test]
fn failed_native_imports_retire_the_batch_and_late_promotion_reservations_win() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
    let original = super::settings_tests::document(8);
    let path = file(&fixture, 16);
    let valid = json!({"op":"image.import","path":path,"parent":"B"});
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "import-refusals".into(),
        vec![],
        None,
        None,
    )
    .stage(&runtime, &cache, &|| Ok(()))
    .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    for fields in [
        json!({"parent":"A"}),
        json!({"parent":"outside"}),
        json!({"parent":true}),
        json!({"x":100001}),
        json!({"frame":1}),
        json!({"page":0}),
        json!({"source_sha256": (vec![0u8; 32])}),
        json!({"path":fixture.path().join("missing.png")}),
    ] {
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let refused = prepare(
            capture(&state, &[valid.clone(), invalid]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let refused = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("import interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("import interrupted"));
    assert!(refused.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    let (_, _, edit) = capture(&state, &[valid.clone()]).into_parts();
    let mut budget = crate::image_import::BUDGET as u64 + 1;
    let reason = super::super::import::prepare(edit, &session, &mut budget, None, &|| Ok(()))
        .err()
        .unwrap();
    assert!(reason.contains("256 MiB"));
    assert_eq!(budget, crate::image_import::BUDGET as u64 + 1);
    assert_eq!(runtime.disk().used().unwrap(), disk);
    let prepared = prepare(capture(&state, &[valid]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Import",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Unrelated layer".into(),
        scopes: vec![crate::engine::Scope::layer("outside")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    assert_eq!(runtime.disk().used().unwrap(), disk);
    drop((
        state,
        native,
        session,
        staged.seals,
        cache,
        runtime,
        connection,
    ));
}
