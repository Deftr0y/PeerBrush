use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    native::worker::{
        self,
        tests::{capture, Fixture},
    },
};
use serde_json::json;

fn contents() -> Vec<serde_json::Value> {
    vec![
        json!({"kind":"shape","shape":"ellipse","bounds":[1.25,2.5,277.75,62.25],
            "points":[],"closed":true,"fill":[12347,23459,34571,45679],
            "stroke":[54321,32109,10987,49153],"stroke_width":1.375}),
        json!({"kind":"bezier","nodes":[{"point":[1.25,2.5],"out_handle":[80.,51.]},
            {"point":[277.75,2.5],"in_handle":[200.,47.]},{"point":[180.,62.25]}],
            "closed":true,"fill":[12347,23459,34571,45679],"stroke":[54321,32109,10987,49153],"stroke_width":1.375}),
        // Deliberately omit optional layout, letter-spacing and source matrix.
        json!({"kind":"text","text":"AV office\nÀé Wj","font":"regular","size":23.125,
            "line_height":0.875,"align":"center","color":[12501,23003,34507,43871]}),
        json!({"kind":"text","layout":"shaped","text":"AV office\nÀé Wj","font":"regular","size":23.125,
            "line_height":0.875,"letter_spacing":-1.75,"align":"center","color":[12501,23003,34507,43871]}),
        json!({"kind":"svg","svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"280\" height=\"67\"><path d=\"M 1.25 2.5 Q 180 60 277.75 2.5 L 180 62.25 Z\" fill=\"#317fc3\" fill-opacity=\".67\"/></svg>"}),
    ]
}
fn compare(native: &NativeDocument, source: &crate::source::Source, depth: u16) {
    let layer = native.metadata().layer(0).unwrap();
    assert_eq!(layer.kind(), "paint");
    assert_eq!(layer.origin(), [-7, 3]);
    assert_eq!(layer.pixels().depth(), depth);
    let saved: crate::source::Source =
        serde_json::from_value(json!(layer.source().unwrap())).unwrap();
    assert_eq!(&saved, source);
    let oracle = source.render(source.width, source.height, depth).unwrap();
    let region = native
        .source(layer.pixels().slot())
        .unwrap()
        .prepare_region(
            crate::raster::backed::Region::new([0, 0, 280, 67]).unwrap(),
            &mut || Ok(()),
        )
        .unwrap();
    for y in 0..67 {
        for x in 0..280 {
            assert_eq!(
                region.pixel16(x, y).unwrap(),
                oracle.get16(x, y),
                "{depth} at {x},{y}"
            );
        }
    }
}
fn compare_scene(native: &NativeDocument, expected: &crate::engine::scene::SceneSnapshot) {
    assert_eq!(json!(native.metadata()), json!(expected.metadata()));
    assert_eq!(native.roster().len(), expected.rasters().len());
    for ((_, actual), (_, expected)) in native.roster().iter().zip(expected.rasters().iter()) {
        assert_eq!(
            [actual.width(), actual.height()],
            [expected.width(), expected.height()]
        );
        assert_eq!(actual.depth(), expected.depth());
        let bounds = crate::raster::backed::Region::new([
            0,
            0,
            actual.width() as i32,
            actual.height() as i32,
        ])
        .unwrap();
        let a = actual.prepare_region(bounds, &mut || Ok(())).unwrap();
        let e = expected.prepare_region(bounds, &mut || Ok(())).unwrap();
        for y in 0..actual.height() as i32 {
            for x in 0..actual.width() as i32 {
                assert_eq!(
                    a.pixel16(x, y).unwrap(),
                    e.pixel16(x, y).unwrap(),
                    "native source plane at {x},{y}"
                );
            }
        }
    }
}
fn compare_projection(native: &NativeDocument, expected: &crate::engine::Layer) {
    let layer = native.metadata().layer(0).unwrap();
    assert_eq!(layer.origin(), [expected.x, expected.y]);
    let source: crate::source::Source =
        serde_json::from_value(json!(layer.source().unwrap())).unwrap();
    assert_eq!(&source, expected.source.as_deref().unwrap());
    assert_eq!(
        [
            layer.pixels().width(),
            layer.pixels().height(),
            u32::from(layer.pixels().depth())
        ],
        [
            expected.pixels.width,
            expected.pixels.height,
            u32::from(expected.pixels.depth)
        ]
    );
    let actual = native
        .source(layer.pixels().slot())
        .unwrap()
        .prepare_region(
            crate::raster::backed::Region::new([
                0,
                0,
                expected.pixels.width as i32,
                expected.pixels.height as i32,
            ])
            .unwrap(),
            &mut || Ok(()),
        )
        .unwrap();
    for y in 0..expected.pixels.height as i32 {
        for x in 0..expected.pixels.width as i32 {
            assert_eq!(
                actual.pixel16(x, y).unwrap(),
                expected.pixels.get16(x, y),
                "native source transform at {x},{y}"
            );
        }
    }
}
#[test]
fn source_creation_previews_and_history_keep_exact_native_render_and_command_defaults() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 64 * 1024 * 1024);
    for depth in [8, 16] {
        for content in contents() {
            let session = runtime.begin().unwrap();
            let mut state = DocumentAuthority::from_native(
                NativeDocument::new_blank(
                    32,
                    16,
                    depth,
                    &runtime.resident(),
                    &runtime.disk(),
                    &|| Ok(()),
                )
                .unwrap(),
                8,
            )
            .unwrap();
            let key = state.native().unwrap().key();
            let command = json!({"op":"source.add","x":-7,"y":3,"source":{"width":280,"height":67,"content":content}});
            let source = crate::source::parse(&command).unwrap();
            let before = runtime.disk().used().unwrap();
            let prepared =
                worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            compare(prepared.preview(), &source, depth);
            assert_eq!(prepared.created_layers().count(), 1);
            assert_eq!(state.native().unwrap().key(), key);
            assert_eq!(state.history_lengths(), [0, 0]);
            prepared
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), before);
            let prepared =
                worker::prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            let (base, history, delta) = prepared.into_parts();
            let committed = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Editable source",
                        task: None,
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            let gate = || Publication {
                actor: "human",
                leases: &[],
                now: 0,
                closed: false,
                write_available: true,
            };
            state
                .commit_native(gate(), committed)
                .unwrap()
                .retire(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(state.history_lengths(), [1, 0]);
            compare(state.native().unwrap(), &source, depth);
            let source_key = state
                .native()
                .unwrap()
                .source(
                    state
                        .native()
                        .unwrap()
                        .metadata()
                        .layer(0)
                        .unwrap()
                        .pixels()
                        .slot(),
                )
                .unwrap()
                .key();
            for (direction, count) in [(Direction::Undo, 1), (Direction::Redo, 2)] {
                let moved = state
                    .native_history()
                    .unwrap()
                    .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                    .unwrap();
                drop(state.move_native(gate(), moved).unwrap());
                assert_eq!(state.view().layers().len(), count);
            }
            compare(state.native().unwrap(), &source, depth);
            assert_eq!(
                state
                    .native()
                    .unwrap()
                    .source(
                        state
                            .native()
                            .unwrap()
                            .metadata()
                            .layer(0)
                            .unwrap()
                            .pixels()
                            .slot()
                    )
                    .unwrap()
                    .key(),
                source_key
            );
            let mut changed = source.clone();
            changed.matrix = [0.9375, 0.125, -0.0625, 1.125, 3.25, -0.625];
            let update = json!({"op":"source.update","layer":state.view().layer(0).unwrap().id(),"source":changed});
            let before_update = runtime.disk().used().unwrap();
            let preview =
                worker::prepare(capture(&state, &[update]), &session, &|| Ok(())).unwrap();
            compare(preview.preview(), &changed, depth);
            assert_eq!(state.history_lengths(), [1, 0]);
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), before_update);
            let mut oracle = crate::engine::Document::new_depth(32, 16, depth).unwrap();
            let mut layer =
                crate::engine::Layer::new(source.label(), "paint", source.width, source.height);
            layer.id = state.view().layer(0).unwrap().id().to_owned();
            layer.x = -7;
            layer.y = 3;
            layer.pixels = source.render(source.width, source.height, depth).unwrap();
            layer.source = Some(Arc::new(source.clone()));
            oracle.layers.insert(0, layer);
            for mut command in [
                json!({"op":"transform","angle":17.5,"scale_x":1.125,"scale_y":0.875,"skew_x":9.,"dx":3.25,"dy":-2.125}),
                json!({"op":"transform","angle":-90.,"scale_x":-1.,"scale_y":1.,"pivot":[7.25,11.5],"dx":-3.5,"dy":2.75}),
                json!({"op":"move","dx":9,"dy":-4}),
            ] {
                command["layer"] = json!(oracle.layers[0].id);
                command["selection_only"] = json!(false);
                let expected =
                    crate::engine::Engine::preview_edits(oracle.clone(), &[command.clone()])
                        .unwrap();
                let before_transform = runtime.disk().used().unwrap();
                let preview =
                    worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(()))
                        .unwrap();
                compare_projection(preview.preview(), &expected.layers[0]);
                if command["op"] == "move" {
                    let pixels = preview.preview().metadata().layer(0).unwrap().pixels();
                    assert_eq!(
                        preview.preview().source(pixels.slot()).unwrap().key(),
                        source_key
                    );
                }
                assert_eq!(state.history_lengths(), [1, 0]);
                preview
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), before_transform);
            }
            for mut command in [
                json!({"op":"transform","projective":{"rect":[-7,3,273,70],"quad":[[-6.,2.],[274.,4.],[272.,71.],[-8.,69.]]}}),
                json!({"op":"transform","warp":{"rect":[-7,3,273,70],"preset":"arc","amount":0.07}}),
            ] {
                command["layer"] = json!(oracle.layers[0].id);
                command["selection_only"] = json!(false);
                let before_transform = runtime.disk().used().unwrap();
                let error = worker::prepare(capture(&state, &[command]), &session, &|| Ok(()))
                    .err()
                    .unwrap();
                assert!(error.reason.contains("Rasterize"), "{}", error.reason);
                assert_eq!(runtime.disk().used().unwrap(), before_transform);
            }
            drop((base, history, state, session));
        }
    }
    drop((runtime, connection));
}
#[test]
fn fitted_text_draft_keeps_its_transformed_frame_in_both_engines() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 64 * 1024 * 1024);
    for (depth, text) in [(8, 2), (16, 3)] {
        let session = runtime.begin().unwrap();
        let state = DocumentAuthority::from_native(
            NativeDocument::new_blank(32, 16, depth, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap(),
            8,
        ).unwrap();
        let command = json!({
            "op":"source.add","x":-7,"y":3,"fit_text":true,
            "source":{"width":280,"height":67,"matrix":[0.,1.,-1.,0.,7.,-2.],"content":contents().remove(text)}
        });
        let expected = crate::engine::Engine::preview_edits(
            crate::engine::Document::new_depth(32, 16, depth).unwrap(),
            std::slice::from_ref(&command),
        ).unwrap();
        let layer = &expected.layers[0];
        assert_eq!([layer.x, layer.y], [-67, 1]);
        assert_eq!([layer.pixels.width, layer.pixels.height], [67, 280]);
        assert_eq!(layer.source.as_ref().unwrap().matrix, [0., 1., -1., 0., 67., 0.]);
        assert!((0..280).any(|y| (0..67).any(|x| layer.pixels.get16(x, y)[3] != 0)));
        let mut carets = None;
        let preview = worker::prepare_text_preview(
            capture(&state, &[command]), &session, &|| Ok(()), &mut carets,
        ).unwrap();
        compare_projection(preview.preview(), layer);
        assert_eq!(state.history_lengths(), [0, 0]);
        preview.abort(&session, &|| session.cleanup_check()).unwrap();
        let map = carets.unwrap();
        let at = |index| map.map.at(crate::source::text_position::Caret { index, trailing: false }).unwrap().0;
        assert!(at(1) > at(0));
    }
}

#[test]
fn text_resize_and_transforms_preserve_native_masks_and_editable_sources() {
    use crate::{
        engine::{
            scene::{CapturedScene, LegacyTileCache},
            Document, Engine, Mask, MaskStep,
        },
        raster::Raster,
    };
    for (depth, linked, fit_text) in [
        (8, false, false), (8, true, false), (16, false, false), (16, true, false),
        (8, false, true), (16, true, true),
    ] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(256 * 1024 * 1024, 64 * 1024 * 1024);
        let mut oracle = Engine::new();
        oracle.resident_state_mut().expect("resident fixture").document = Document::new_depth(84, 48, depth).unwrap();
        let source: crate::source::Source = serde_json::from_value(json!({
            "width":36,"height":24,"content":contents().remove(2),
        }))
        .unwrap();
        let layer = &mut oracle.resident_state_mut().expect("resident fixture").document.layers[0];
        layer.x = -3;
        layer.y = 5;
        layer.pixels = source.render(36, 24, depth).unwrap();
        layer.source = Some(Arc::new(source.clone()));
        let mut mask_original = Raster::new_depth(58, 39, depth);
        for y in 0..39 {
            for x in 0..58 {
                mask_original.set16(
                    x,
                    y,
                    [
                        ((x * 941 + y * 617 + 12347) % 65536) as u16,
                        23459,
                        34571,
                        65535,
                    ],
                );
            }
        }
        layer.mask = Some(Mask {
            enabled: true,
            linked,
            cache_key: crate::engine::id(),
            steps: vec![
                MaskStep {
                    id: crate::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 0.73,
                    value: 0.,
                    settings: json!({}),
                    pixels: crate::retained::Original::from(&mask_original)
                        .render(20, 12)
                        .unwrap(),
                },
                MaskStep {
                    id: crate::engine::id(),
                    kind: "fill".into(),
                    enabled: false,
                    weight: 0.61,
                    value: 0.4,
                    settings: json!({}),
                    pixels: Raster::new_depth(13, 9, depth),
                },
            ],
        });
        let mut clip = Raster::new(7, 5);
        clip.set(2, 3, [137, 0, 0, 0]);
        let coverage = crate::selection::Coverage {
            mask: clip,
            bounds: [2, 1, 5, 4],
            origin: [-1, 2],
            contours: vec![vec![[2., 1.], [5., 1.], [5., 4.]]],
        };
        layer.effects.push(crate::effects::Effect {id:crate::engine::id(),kind:"liquify".into(),enabled:true,weight:0.67,
            settings:json!({"radius":3.75,"strength":0.7,"strokes":[{"mode":"push","points":[[1.25,2.5],[5.75,3.125]],
                "selection":[0,0,12,10],"polygon":[[0.,0.],[12.,0.],[12.,10.],[0.,10.]],"coverage":coverage}]}).into()});
        layer.parent = Some("source-folder".into());
        let mut folder = crate::engine::Layer::new("Source folder", "group", 84, 48);
        folder.id = "source-folder".into();
        folder.pixels = Raster::new_depth(84, 48, depth);
        oracle.resident_state_mut().expect("resident fixture").document.layers.push(folder);
        let original = oracle.resident_state().expect("resident fixture").document.clone();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(
            oracle.resident_state().expect("resident fixture").document.clone(),
            "source-update-base".into(),
            vec![],
            None,
            None,
        )
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
        let state = DocumentAuthority::from_native(
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap(),
            8,
        )
        .unwrap();
        let session = staged.session.take().unwrap();
        let key = state.native().unwrap().key();
        let mut changed = source;
        if !fit_text {
            changed.width = 76;
            changed.height = 43;
        }
        changed.matrix = [0.9375, 0.125, -0.0625, 1.125, -2.25, -1.625];
        let command =
            json!({"op":"source.update","layer":oracle.resident_state().expect("resident fixture").document.layers[0].id,"source":changed,"fit_text":fit_text});
        oracle
            .edit("human", &[command.clone()], Some(0), None, "Resize text")
            .unwrap();
        let expected = CapturedScene::from_parts(
            oracle.resident_state().expect("resident fixture").document.clone(),
            "source-update-oracle".into(),
            vec![],
            None,
            None,
        )
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
        let before = runtime.disk().used().unwrap();
        let preview =
            worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
        compare_scene(preview.preview(), &expected.scene);
        assert_eq!(state.native().unwrap().key(), key);
        assert_eq!(state.history_lengths(), [0, 0]);
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), before);
        drop(expected);
        for transform in [
            json!({"op":"transform","layer":original.layers[0].id,"angle":17.5,"scale_x":1.125,"scale_y":0.875,"skew_x":9.,"dx":3.25,"dy":-2.125,"selection_only":false}),
            json!({"op":"move","layer":original.layers[0].id,"dx":3,"dy":-2,"selection_only":false}),
            json!({"op":"transform","layer":"source-folder","angle":-17.5,"scale_x":0.875,"scale_y":1.125,"skew_y":-7.,"dx":-1.25,"dy":2.5,"selection_only":false}),
        ] {
            let mut expected_doc =
                Engine::preview_edits(original.clone(), &[transform.clone()]).unwrap();
            expected_doc.revision = state.view().revision() + 1;
            let expected = CapturedScene::from_parts(
                expected_doc,
                "source-transform-oracle".into(),
                vec![],
                None,
                None,
            )
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
            let before = runtime.disk().used().unwrap();
            let preview =
                worker::prepare(capture(&state, &[transform]), &session, &|| Ok(())).unwrap();
            compare_scene(preview.preview(), &expected.scene);
            assert_eq!(state.native().unwrap().key(), key);
            assert_eq!(state.history_lengths(), [0, 0]);
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), before);
        }
        let completed = crate::source::stage::tests::CompletedTiles::observe();
        let baseline = runtime.disk().used().unwrap();
        let error = worker::prepare(capture(&state, &[command]), &session, &|| {
            if completed.count() != 0 {
                Err("source update canceled".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(error.reason.contains("source update canceled"));
        assert_eq!(completed.count(), 1);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.native().unwrap().key(), key);
        drop((state, staged.seals, session, cache, runtime, connection));
    }
}
#[test]
fn source_creation_refusal_and_cancellation_leave_document_and_disk_unchanged() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let state = DocumentAuthority::from_native(
        NativeDocument::new_blank(32, 16, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap(),
        8,
    )
    .unwrap();
    let key = state.native().unwrap().key();
    let command = json!({"op":"source.add","source":{"width":280,"height":67,"content":contents().remove(0)}});
    let before = runtime.disk().used().unwrap();
    for (field, value) in [
        ("x", json!(0.5)),
        ("parent", json!(state.view().layer(0).unwrap().id())),
        ("parent", json!("missing-folder")),
    ] {
        let mut invalid = command.clone();
        invalid[field] = value;
        let failed = worker::prepare(capture(&state, &[invalid]), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(failed.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), before);
        assert_eq!(state.native().unwrap().key(), key);
        assert_eq!(state.history_lengths(), [0, 0]);
    }
    let completed = crate::source::stage::tests::CompletedTiles::observe();
    let error = worker::prepare(capture(&state, &[command]), &session, &|| {
        if completed.count() != 0 {
            Err("source creation canceled".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(error.reason.contains("source creation canceled"));
    assert_eq!(completed.count(), 1);
    assert!(error.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), before);
    assert_eq!(state.native().unwrap().key(), key);
    drop((state, session, runtime, connection));
}

#[test]
fn full_size_pen_uses_native_guides_and_stages_only_path_tiles() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(32 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let native = NativeDocument::new_blank(
        16000, 16000, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()),
    ).unwrap();
    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let commands = [json!({"op":"path.create","nodes":[{"point":[15970.,15980.],"out_handle":[15975.,15978.]},
        {"point":[15995.,15980.],"in_handle":[15990.,15978.]}],
        "stroke":[54321,32109,10987,49153],"stroke_width":4.})];
    let (origin, guides) = crate::path::preview_controls(state.view(), None, &commands)
        .unwrap().unwrap();
    assert_eq!(origin, [0, 0]);
    assert_eq!([guides.width, guides.height], [16000, 16000]);
    let disk = runtime.disk().used().unwrap();
    let pending = worker::prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
    let layer = pending.preview().metadata().layer(0).unwrap();
    let source = pending.preview().source(layer.pixels().slot()).unwrap();
    assert_eq!(source.physical_tiles().map(|tile| tile.coordinate()).collect::<Vec<_>>(), [(62, 62)]);
    let crop = crate::raster::backed::Region::new([15980, 15979, 15981, 15980]).unwrap();
    let view = source.prepare_region(crop, &mut || Ok(())).unwrap();
    assert_eq!(view.pixel16(15980, 15979).unwrap(), [54321, 32109, 10987, 49153]);
    drop(view);
    pending.abort(&session, &|| session.cleanup_check()).unwrap();
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(runtime.disk().used().unwrap(), disk);
}

#[test]
fn path_commands_share_geometry_native_precision_and_atomic_history() {
    for (depth, explicit_frame) in [(8, false), (8, true), (16, false), (16, true)] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(32 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let mut state = DocumentAuthority::from_native(
            NativeDocument::new_blank(32, 16, depth, &runtime.resident(), &runtime.disk(), &|| {
                Ok(())
            })
            .unwrap(),
            32,
        )
        .unwrap();
        let mut oracle = crate::engine::Document::new_depth(32, 16, depth).unwrap();
        let mut create = json!({"op":"path.create","nodes":[
            {"point":[1.25,4.5],"out_handle":[7.,12.]},
            {"point":[23.75,5.5],"in_handle":[17.,-2.],"smooth":true},
            {"point":[19.125,14.25]}],"stroke":[54321,32109,10987,49153],"stroke_width":1.375});
        if explicit_frame {
            create["frame"] = json!([-7, 3, 273, 70]);
        }
        let before = runtime.disk().used().unwrap();
        let key = state.native().unwrap().key();
        for (field, value) in [
            ("frame", json!([-100001, 0, 32, 16])),
            ("frame", json!([3, 0, 3, 16])),
            ("parent", json!("missing-folder")),
            ("closed", json!("true")),
            ("nodes", json!(vec![json!({"point":[1.,2.]}); 129])),
        ] {
            let mut invalid = create.clone();
            invalid[field] = value;
            let failed = worker::prepare(capture(&state, &[invalid]), &session, &|| Ok(()))
                .err()
                .unwrap();
            assert!(failed.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), before);
            assert_eq!(state.native().unwrap().key(), key);
            assert_eq!(state.history_lengths(), [0, 0]);
        }
        crate::path::create(&mut oracle, &create).unwrap();
        let before = runtime.disk().used().unwrap();
        let preview =
            worker::prepare(capture(&state, &[create.clone()]), &session, &|| Ok(())).unwrap();
        compare_projection(preview.preview(), &oracle.layers[0]);
        assert_eq!(
            preview.preview().metadata().layer(0).unwrap().name(),
            "Pen path"
        );
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), before);
        let publish = |state: &mut DocumentAuthority, prepared: worker::PreparedNative| {
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Pen path",
                        task: None,
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            state
                .commit_native(
                    Publication {
                        actor: "human",
                        leases: &[],
                        now: 0,
                        closed: false,
                        write_available: true,
                    },
                    commit,
                )
                .unwrap()
                .retire(&session, &|| session.cleanup_check())
                .unwrap();
        };
        let prepared = worker::prepare(capture(&state, &[create]), &session, &|| Ok(())).unwrap();
        publish(&mut state, prepared);
        oracle.layers[0].id = state.view().layer(0).unwrap().id().into();
        // A reflected/sheared source and nonzero origin exercise document-to-local edits.
        let mut source = oracle.layers[0].source.as_deref().unwrap().clone();
        source.matrix = [-0.875, 0.125, 0.0625, 1.125, 25.25, -1.625];
        let mut command = json!({"op":"source.update","layer":oracle.layers[0].id,"source":source});
        crate::source::update(&mut oracle.layers[0], &command).unwrap();
        let prepared =
            worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
        publish(&mut state, prepared);
        for edit in [
            json!({"op":"path.node.add","point":[4.5,12.75],"in_handle":[2.,10.],"out_handle":[7.,15.],"smooth":true}),
            json!({"op":"path.node.move","index":1,"point":[12.125,7.375]}),
            json!({"op":"path.node.convert","index":1,"smooth":true}),
            json!({"op":"path.handle.move","index":1,"handle":"in","point":[8.5,4.25]}),
            json!({"op":"path.handle.move","index":1,"handle":"out","point":[16.25,10.75]}),
            json!({"op":"path.node.split","index":0,"t":0.375}),
            json!({"op":"path.update","closed":true,"fill":[12347,23459,34571,45679]}),
            json!({"op":"path.node.split","index":4,"t":0.625}),
            json!({"op":"path.node.add","index":2,"point":[18.5,10.25]}),
            json!({"op":"path.node.convert","index":1,"smooth":false,"reset_handles":true}),
            json!({"op":"path.node.delete","index":2}),
            json!({"op":"path.update","closed":false,"stroke_width":0.875}),
        ] {
            command = edit;
            command["layer"] = json!(oracle.layers[0].id);
            let previous = oracle.layers[0].clone();
            crate::path::update(&mut oracle.layers[0], &command).unwrap();
            let key = state.native().unwrap().key();
            let history = state.history_lengths();
            let baseline = runtime.disk().used().unwrap();
            let preview =
                worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            compare_projection(preview.preview(), &oracle.layers[0]);
            assert_eq!(state.native().unwrap().key(), key);
            assert_eq!(state.history_lengths(), history);
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            let prepared =
                worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            publish(&mut state, prepared);
            compare_projection(state.native().unwrap(), &oracle.layers[0]);
            assert_eq!(state.history_lengths(), [history[0] + 1, 0]);
            for (direction, expected) in [
                (Direction::Undo, &previous),
                (Direction::Redo, &oracle.layers[0]),
            ] {
                let moved = state
                    .native_history()
                    .unwrap()
                    .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                    .unwrap();
                drop(
                    state
                        .move_native(
                            Publication {
                                actor: "human",
                                leases: &[],
                                now: 0,
                                closed: false,
                                write_available: true,
                            },
                            moved,
                        )
                        .unwrap(),
                );
                compare_projection(state.native().unwrap(), expected);
            }
        }
        let key = state.native().unwrap().key();
        let baseline = runtime.disk().used().unwrap();
        let history = state.history_lengths();
        let locked = json!({"op":"layer.update","layer":oracle.layers[0].id,"locked":true});
        let failed = worker::prepare(
            capture(&state, &[locked, command.clone()]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(
            failed.reason.to_lowercase().contains("locked"),
            "{}",
            failed.reason
        );
        assert!(failed.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.native().unwrap().key(), key);
        for invalid in [
            json!({"op":"path.node.split","index":999}),
            json!({"op":"path.handle.move","index":0,"handle":"sideways","point":[2.,4.]}),
            json!({"op":"path.node.move","index":0,"point":[100001.,0.]}),
            json!({"op":"path.update","stroke_width":-1.}),
        ] {
            let mut invalid = invalid;
            invalid["layer"] = json!(oracle.layers[0].id);
            // Failure after a valid rendered step must retire both its metadata and pixels.
            let failed = worker::prepare(
                capture(&state, &[command.clone(), invalid]),
                &session,
                &|| Ok(()),
            )
            .err()
            .unwrap();
            assert!(failed.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            assert_eq!(state.native().unwrap().key(), key);
            assert_eq!(state.history_lengths(), history);
        }
        let completed = crate::source::stage::tests::CompletedTiles::observe();
        let failed = worker::prepare(capture(&state, &[command]), &session, &|| {
            if completed.count() > 0 {
                Err("path edit canceled".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(failed.reason.contains("path edit canceled"));
        assert!(failed.cleanup_error.is_none());
        assert_eq!(completed.count(), 1);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.native().unwrap().key(), key);
        drop((state, session, runtime, connection));
    }
}
