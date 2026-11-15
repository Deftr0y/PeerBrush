use super::*;
use crate::{
    engine::{native::CommandDescriptor, Document, Engine},
    large_document::{BlockRef, Limits, Snapshot},
    retained::planning,
    transform::native::Checks,
};
use serde_json::json;
use std::{cell::Cell, path::PathBuf};

struct TestSession {
    pool: ResidentBudget,
    disk: DiskBudget,
    parent: PathBuf,
    created: Cell<usize>,
}
impl StageSession for TestSession {
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn cleanup_check(&self) -> Result<(), String> {
        Ok(())
    }
    fn store_for_lineage(&self, _: [u8; 16]) -> Result<Store, String> {
        Err("No reopened fixture lineage".into())
    }
    fn seal_store(
        &self,
        store: &Store,
        refs: &[BlockRef],
        metadata: &[u8],
    ) -> Result<Snapshot, String> {
        let parent = store.checkpoint_reference().map_err(|e| e.to_string())?;
        store
            .commit(
                parent.map_or(store.identity(), |p| p.source()),
                parent.map_or(1, |p| p.revision() + 1),
                parent,
                metadata,
                refs,
                &mut || Ok(()),
            )
            .map_err(|e| e.to_string())
    }
    fn abort_fragment(
        &self,
        fragment: FreshFragment,
        cleanup: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        fragment
            .abort(&mut || cleanup().map_err(|_| Error::Cancelled))
            .map_err(|e| e.to_string())
    }
}
impl Session for TestSession {
    fn resident(&self) -> ResidentBudget {
        self.pool.clone()
    }
    fn disk(&self) -> DiskBudget {
        self.disk.clone()
    }
    fn fresh(&self) -> Result<FreshFragment, String> {
        let next = self.created.get() + 1;
        self.created.set(next);
        FreshFragment::create(
            &self
                .parent
                .join(format!("fragment-{next}-{}", crate::engine::id())),
            self.pool.clone(),
            self.disk.clone(),
            limits(),
        )
        .map_err(|e| e.to_string())
    }
}
fn limits() -> Limits {
    Limits {
        disk_bytes: 32 * 1024 * 1024,
        max_blocks: 256,
        max_resident_blocks: 16,
        manifest_bytes: 1024 * 1024,
    }
}
struct Fixture {
    pool: ResidentBudget,
    disk: DiskBudget,
    parent: PathBuf,
    input: FreshFragment,
    document: NativeDocument,
    legacy: Document,
}
impl Fixture {
    fn new(depth: u16, kind: &str, linked: Option<bool>) -> Self {
        Self::from_legacy(planning::tests::fixture(depth, kind, linked))
    }
    fn from_legacy(legacy: Document) -> Self {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(64 * 1024 * 1024, 16).unwrap();
        let parent = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "peerbrush-transform-transaction-{}",
                crate::engine::id()
            ));
        std::fs::create_dir(&parent).unwrap();
        let input =
            FreshFragment::create(&parent.join("input"), pool.clone(), disk.clone(), limits())
                .unwrap();
        let document = planning::tests::native_document(&legacy, &pool, &disk, input.store());
        Self {
            pool,
            disk,
            parent,
            input,
            document,
            legacy,
        }
    }
    fn session(&self) -> TestSession {
        TestSession {
            pool: self.pool.clone(),
            disk: self.disk.clone(),
            parent: self.parent.clone(),
            created: Cell::new(0),
        }
    }
    fn finish(self) {
        let Self {
            pool,
            disk,
            parent,
            input,
            document,
            legacy,
        } = self;
        drop(legacy);
        drop(document);
        input.abort(&mut || Ok(())).unwrap();
        assert_eq!(pool.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
        std::fs::remove_dir_all(parent).unwrap();
    }
}
#[test]
fn complete_linked_outputs_and_generated_fill_original_match_legacy_in_one_candidate() {
    for depth in [8, 16] {
        for (kind, linked) in [
            ("paint", Some(true)),
            ("paint", Some(false)),
            ("fill", Some(true)),
        ] {
            let fixture = Fixture::new(depth, kind, linked);
            let baseline = fixture.disk.used().unwrap();
            let session = fixture.session();
            let command = json!({"op":"transform","layer":fixture.legacy.layers[0].id,"angle":17.5,"dx":3,"dy":-2,"selection_only":false});
            let expected =
                Engine::preview_edits(fixture.legacy.clone(), &[command.clone()]).unwrap();
            let mut work = Work::new(10_000_000).unwrap();
            let plan = planning::layer(
                &fixture.document,
                &fixture.legacy.layers[0].id,
                &command,
                &mut work,
                &|| Ok(()),
            )
            .unwrap()
            .unwrap();
            let prepared = prepare(
                fixture.document.capture_edit().unwrap(),
                &[plan],
                &session,
                &mut work,
                &|| Ok(()),
            )
            .unwrap();
            let candidate = prepared.preview();
            assert_eq!(
                candidate.metadata().revision(),
                fixture.document.metadata().revision() + 1
            );
            assert_eq!(fixture.document.metadata().revision(), 0);
            let layer = candidate.metadata().layers().next().unwrap();
            let old = &expected.layers[0];
            assert_eq!(layer.origin(), [old.x, old.y]);
            assert_eq!(layer.kind(), old.kind);
            planning::tests::assert_pixels(
                candidate.source(layer.pixels().slot()).unwrap(),
                &old.pixels,
                &fixture.pool,
            );
            if let Some(retained) = layer.pixels().retained() {
                let expected = old.pixels.retained.as_ref().unwrap();
                assert_eq!(retained.matrix(), expected.matrix);
                planning::tests::assert_pixels(
                    candidate.source(retained.pixels().slot()).unwrap(),
                    &expected.pixels,
                    &fixture.pool,
                );
            }
            for (step, old) in layer
                .mask()
                .unwrap()
                .steps()
                .zip(&old.mask.as_ref().unwrap().steps)
            {
                planning::tests::assert_pixels(
                    candidate.source(step.pixels().slot()).unwrap(),
                    &old.pixels,
                    &fixture.pool,
                );
            }
            // Source edits still require guarded Engine adoption. The refusal
            // owns every current, generated-original and mask retirement token.
            fixture
                .document
                .adopt(prepared)
                .err()
                .unwrap()
                .abort(&session, &|| Ok(()))
                .unwrap();
            assert_eq!(fixture.disk.used().unwrap(), baseline);
            assert!(session.created.get() >= 2);
            drop(session);
            fixture.finish();
        }
    }
}
#[test]
fn cancel_after_first_sealed_plane_retires_all_outputs_and_preserves_base() {
    for editable in [false, true] {
        let mut legacy = planning::tests::fixture(16, "paint", Some(true));
        if editable {
            legacy.layers[0].source = Some(std::sync::Arc::new(serde_json::from_value::<crate::source::Source>(json!({
            "width":17,"height":19,"content":{"kind":"shape","shape":"ellipse","bounds":[0.,0.,17.,19.],
                "points":[],"closed":true,"fill":[12347,23459,34571,45679],"stroke":[0,0,0,0],"stroke_width":0.}
        })).unwrap()));
        }
        let fixture = Fixture::from_legacy(legacy);
        let baseline = fixture.disk.used().unwrap();
        let session = fixture.session();
        let command = json!({"op":"transform","angle":17,"selection_only":false});
        let mut work = Work::new(10_000_000).unwrap();
        let plan = planning::layer(
            &fixture.document,
            &fixture.legacy.layers[0].id,
            &command,
            &mut work,
            &|| Ok(()),
        )
        .unwrap()
        .unwrap();
        let reason = prepare(
            fixture.document.capture_edit().unwrap(),
            &[plan],
            &session,
            &mut work,
            &|| {
                if session.created.get() >= 2 {
                    Err("cancel after first sealed plane".into())
                } else {
                    Ok(())
                }
            },
        )
        .err()
        .unwrap();
        assert_eq!(reason, "cancel after first sealed plane");
        assert_eq!(session.created.get(), 2);
        assert_eq!(fixture.disk.used().unwrap(), baseline);
        assert_eq!(fixture.document.metadata().revision(), 0);
        let layer = fixture.document.metadata().layers().next().unwrap();
        planning::tests::assert_pixels(
            fixture.document.source(layer.pixels().slot()).unwrap(),
            &fixture.legacy.layers[0].pixels,
            &fixture.pool,
        );
        drop(session);
        fixture.finish();
    }
}

#[test]
fn editable_plane_refuses_another_definition_wrong_depth_and_cancel_before_io() {
    let mut legacy = planning::tests::fixture(16, "paint", None);
    legacy.layers[0].source = Some(std::sync::Arc::new(serde_json::from_value::<crate::source::Source>(json!({
        "width":17,"height":19,"content":{"kind":"shape","shape":"rectangle","bounds":[0.,0.,17.,19.],
            "points":[],"closed":true,"fill":[12347,23459,34571,45679],"stroke":[0,0,0,0],"stroke_width":0.}
    })).unwrap()));
    let fixture = Fixture::from_legacy(legacy);
    let session = fixture.session();
    let layer = fixture.document.metadata().layer(0).unwrap();
    let source = layer.source().unwrap();
    let foreign: crate::engine::scene::Source36 = serde_json::from_value(json!(source)).unwrap();
    let edit = fixture.document.capture_edit().unwrap();
    let cancellation = edit.cancellation();
    let mut work = Work::new(10_000_000).unwrap();
    let baseline = fixture.disk.used().unwrap();
    for (definition, depth) in [(&foreign, 16), (source, 8)] {
        let plane = Plane {
            target: layer.pixels().slot(),
            pixels: Pixels::Editable {
                source: definition,
                matrix: source.matrix,
                depth,
            },
        };
        let error = stage_plane(
            &fixture.document,
            &plane,
            [17, 19],
            &session,
            &cancellation,
            &mut work,
            &|| Ok(()),
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(error.contains("differs from its captured source"));
        assert_eq!(session.created.get(), 0);
        assert_eq!(fixture.disk.used().unwrap(), baseline);
    }
    cancellation.cancel();
    let plane = Plane {
        target: layer.pixels().slot(),
        pixels: Pixels::Editable {
            source,
            matrix: source.matrix,
            depth: 16,
        },
    };
    assert!(stage_plane(
        &fixture.document,
        &plane,
        [17, 19],
        &session,
        &cancellation,
        &mut work,
        &|| Ok(()),
        &|| Ok(())
    )
    .is_err());
    assert_eq!(session.created.get(), 0);
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    drop((plane, foreign, edit, session));
    fixture.finish();
}

#[test]
fn foreign_session_refuses_before_creation_and_duplicate_batch_retires_every_plane() {
    let fixture = Fixture::new(16, "paint", Some(true));
    let baseline = fixture.disk.used().unwrap();
    let mut session = fixture.session();
    let command = json!({"op":"transform","angle":17,"selection_only":false});
    let mut work = Work::new(10_000_000).unwrap();
    let plan = planning::layer(
        &fixture.document,
        &fixture.legacy.layers[0].id,
        &command,
        &mut work,
        &|| Ok(()),
    )
    .unwrap()
    .unwrap();
    session.pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    let reason = prepare(
        fixture.document.capture_edit().unwrap(),
        &[plan],
        &session,
        &mut work,
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert_eq!(
        reason,
        "Native transform session uses another document admission"
    );
    assert_eq!(session.created.get(), 0);
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    drop(session);
    let session = fixture.session();
    let first = planning::layer(
        &fixture.document,
        &fixture.legacy.layers[0].id,
        &command,
        &mut work,
        &|| Ok(()),
    )
    .unwrap()
    .unwrap();
    let duplicate = planning::layer(
        &fixture.document,
        &fixture.legacy.layers[0].id,
        &command,
        &mut work,
        &|| Ok(()),
    )
    .unwrap()
    .unwrap();
    let reason = prepare(
        fixture.document.capture_edit().unwrap(),
        &[first, duplicate],
        &session,
        &mut work,
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert!(reason.contains("Duplicate"), "{reason}");
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    assert_eq!(fixture.document.metadata().revision(), 0);
    drop(session);
    fixture.finish();
}

#[test]
fn production_entry_uses_immutable_descriptor_parameters_and_rejects_foreign_tokens() {
    let fixture = Fixture::new(16, "paint", Some(true));
    let baseline = fixture.disk.used().unwrap();
    let mut commands = vec![json!({"op":"transform","layer":fixture.legacy.layers[0].id,
        "angle":17.5,"dx":3,"dy":-2,"selection_only":false})];
    let expected = Engine::preview_edits(fixture.legacy.clone(), &commands).unwrap();
    let descriptor =
        CommandDescriptor::from_normalized(&fixture.document, 0, &commands, &|| Ok(())).unwrap();
    let token = descriptor.commands().next().unwrap().token();
    let captured = fixture
        .document
        .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
        .unwrap();
    commands[0]["dx"] = json!(900);
    let session = fixture.session();
    let prepared = prepare_command(
        captured,
        token,
        &session,
        &mut Work::new(10_000_000).unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let layer = prepared.preview().metadata().layers().next().unwrap();
    assert_eq!(layer.origin(), [expected.layers[0].x, expected.layers[0].y]);
    planning::tests::assert_pixels(
        prepared.preview().source(layer.pixels().slot()).unwrap(),
        &expected.layers[0].pixels,
        &fixture.pool,
    );
    fixture
        .document
        .adopt(prepared)
        .err()
        .unwrap()
        .abort(&session, &|| Ok(()))
        .unwrap();
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    drop(session);
    let session = fixture.session();
    let another =
        CommandDescriptor::from_normalized(&fixture.document, 0, &commands, &|| Ok(())).unwrap();
    let captured = fixture
        .document
        .capture_descriptor(descriptor, "human", &[], 1, false, true)
        .unwrap();
    let reason = prepare_command(
        captured,
        another.commands().next().unwrap().token(),
        &session,
        &mut Work::new(10_000_000).unwrap(),
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert_eq!(
        reason,
        "Native transform command belongs to another descriptor"
    );
    assert_eq!(session.created.get(), 0);
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    drop(another);
    drop(session);
    fixture.finish();
}

fn folder_document(depth: u16) -> Document {
    let mut doc = planning::tests::fixture(depth, "paint", Some(true));
    let mut hidden = doc.layers.remove(0);
    let mut root = crate::engine::Layer::new("Root", "group", 17, 19);
    root.pixels = crate::raster::Raster::new_depth(17, 19, depth);
    let mut nested = crate::engine::Layer::new("Nested", "group", 17, 19);
    nested.pixels = crate::raster::Raster::new_depth(17, 19, depth);
    nested.parent = Some(root.id.clone());
    hidden.parent = Some(root.id.clone());
    hidden.visible = false;
    hidden.pixels.set(16, 0, [255, 3, 7, 0]);
    let mut second = planning::tests::fixture(depth, "paint", Some(false))
        .layers
        .remove(0);
    second.parent = Some(nested.id.clone());
    second.x = 21;
    second.y = -3;
    // Alpha1 is visible support at native16 and must not be display-projected.
    // Hidden RGB elsewhere must not influence the folder's default pivot.
    second.pixels.set16(16, 18, [10001, 20003, 30007, 1]);
    doc.layers = vec![root, hidden, nested, second];
    doc
}

#[test]
fn folder_transforms_hidden_nested_children_and_masks_once_at_native_precision() {
    for depth in [8, 16] {
        for mut command in [
            json!({"op":"move","dx":3,"dy":-2,"selection_only":false}),
            json!({"op":"transform","angle":17.5,"scale_x":0.83,"scale_y":1.07,"skew_x":11.25,"dx":3.25,"dy":-2.75,"fractional_translation":true,"selection_only":false}),
            json!({"op":"transform","projective":{"rect":[-12,-8,48,40],"quad":[[-11,-9],[49,-6],[46,42],[-13,38]]},"selection_only":false}),
            json!({"op":"transform","warp":{"rect":[-15,-10,50,45],"preset":"arc","amount":0.03},"selection_only":false}),
        ] {
            let fixture = Fixture::from_legacy(folder_document(depth));
            let baseline = fixture.disk.used().unwrap();
            command["layer"] = json!(fixture.legacy.layers[0].id);
            let expected =
                Engine::preview_edits(fixture.legacy.clone(), &[command.clone()]).unwrap();
            let descriptor =
                CommandDescriptor::from_normalized(&fixture.document, 0, &[command], &|| Ok(()))
                    .unwrap();
            let token = descriptor.commands().next().unwrap().token();
            let captured = fixture
                .document
                .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
                .unwrap();
            let session = fixture.session();
            let prepared = prepare_command(
                captured,
                token,
                &session,
                &mut Work::new(20_000_000).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            let candidate = prepared.preview();
            assert_eq!(candidate.metadata().revision(), 1);
            assert_eq!(fixture.document.metadata().revision(), 0);
            for (layer, old) in candidate.metadata().layers().zip(&expected.layers) {
                assert_eq!(layer.id(), old.id);
                assert_eq!(layer.parent(), old.parent.as_deref());
                assert_eq!(layer.visible(), old.visible);
                assert_eq!(layer.origin(), [old.x, old.y]);
                assert_eq!(layer.kind(), old.kind);
                planning::tests::assert_pixels(
                    candidate.source(layer.pixels().slot()).unwrap(),
                    &old.pixels,
                    &fixture.pool,
                );
                match (layer.pixels().retained(), old.pixels.retained.as_ref()) {
                    (Some(retained), Some(expected)) => {
                        assert_eq!(retained.matrix(), expected.matrix);
                        assert_eq!(retained.projective(), expected.projective);
                        assert_eq!(
                            serde_json::to_value(retained.warps()).unwrap(),
                            serde_json::to_value(&expected.warps).unwrap()
                        );
                        planning::tests::assert_pixels(
                            candidate.source(retained.pixels().slot()).unwrap(),
                            &expected.pixels,
                            &fixture.pool,
                        );
                    }
                    (None, None) => (),
                    _ => panic!("Folder retained source mismatch"),
                }
                if let Some(mask) = layer.mask() {
                    for (step, old) in mask.steps().zip(&old.mask.as_ref().unwrap().steps) {
                        planning::tests::assert_pixels(
                            candidate.source(step.pixels().slot()).unwrap(),
                            &old.pixels,
                            &fixture.pool,
                        );
                    }
                }
            }
            fixture
                .document
                .adopt(prepared)
                .err()
                .unwrap()
                .abort(&session, &|| Ok(()))
                .unwrap();
            assert_eq!(fixture.disk.used().unwrap(), baseline);
            drop(descriptor);
            drop(session);
            fixture.finish();
        }
    }
}

#[test]
fn sparse_affine_transform_keeps_native_samples_across_a_tile_boundary() {
    use crate::raster::{backed::Region, project16, Raster, TILE};

    for depth in [8, 16] {
        let mut doc = Document::new_depth(1, 1, depth).unwrap();
        doc.width = 16000;
        doc.height = 16000;
        let mut raster = Raster::new_depth(16000, 16000, depth);
        let color = [12347, 23459, 34571, 65535];
        raster.set16(1, 0, color);
        raster.set16(TILE as i32, TILE as i32 + 1, color);
        raster.set16(15999, 15999, color);
        doc.layers[0].pixels = raster;
        let fixture = Fixture::from_legacy(doc);
        let session = fixture.session();
        let baseline = fixture.disk.used().unwrap();
        let input = fixture.document.metadata().layers().next().unwrap().pixels();
        let input_key = fixture.document.source(input.slot()).unwrap().key();
        let command = json!({
            "op": "transform",
            "layer": fixture.legacy.layers[0].id,
            "selection_only": false,
            "fractional_translation": true,
            "dx": -0.5
        });
        let descriptor =
            CommandDescriptor::from_normalized(&fixture.document, 0, &[command], &|| Ok(())).unwrap();
        let token = descriptor.commands().next().unwrap().token();
        let captured = fixture
            .document
            .capture_descriptor(descriptor, "human", &[], 1, false, true)
            .unwrap();
        let prepared = prepare_command(
            captured,
            token,
            &session,
            &mut Work::new(2_000_000).unwrap(),
            &|| Ok(()),
        )
        .unwrap();
        {
            let preview = prepared.preview();
            let layer = preview.metadata().layers().next().unwrap();
            assert_eq!(layer.origin(), [0, 0]);
            let pixels = layer.pixels();
            let surface = preview.source(pixels.slot()).unwrap();
            assert_eq!([surface.width(), surface.height()], [16000, 16000]);
            assert_eq!(surface.depth(), depth);
            assert_eq!(surface.physical_tiles().count(), 4);
            let retained = pixels.retained().unwrap().pixels().slot();
            assert_eq!(preview.source(retained).unwrap().key(), input_key);
            let x = TILE as i32;
            let y = x + 1;
            let row = surface
                .prepare_region(Region::new([x - 2, y, x + 2, y + 1]).unwrap(), &mut || Ok(()))
                .unwrap();
            let expected = if depth == 8 {
                let [r, g, b, _] = color.map(|v| u16::from(project16(v)) * 257);
                [r, g, b, 128 * 257]
            } else {
                [color[0], color[1], color[2], 32768]
            };
            assert_eq!(row.pixel16(x - 2, y).unwrap(), [0; 4]);
            assert_eq!(row.pixel16(x - 1, y).unwrap(), expected);
            assert_eq!(row.pixel16(x, y).unwrap(), expected);
            assert_eq!(row.pixel16(x + 1, y).unwrap(), [0; 4]);
        }
        fixture
            .document
            .adopt(prepared)
            .err()
            .unwrap()
            .abort(&session, &|| Ok(()))
            .unwrap();
        assert_eq!(fixture.document.metadata().revision(), 0);
        assert_eq!(fixture.disk.used().unwrap(), baseline);
        drop(session);
        fixture.finish();
    }
}

#[test]
fn folder_locked_descendant_and_work_limit_refuse_before_output_creation() {
    let mut doc = folder_document(16);
    doc.layers[3].locked = true;
    let fixture = Fixture::from_legacy(doc);
    let command = json!({"op":"transform","layer":fixture.legacy.layers[0].id,"angle":17,"selection_only":false});
    let mut work = Work::new(20_000_000).unwrap();
    assert_eq!(
        planning::folder(
            &fixture.document,
            &fixture.legacy.layers[0].id,
            &command,
            &mut work,
            &|| Ok(())
        )
        .err()
        .unwrap(),
        "A layer inside the folder is locked"
    );
    drop(work);
    fixture.finish();
    let fixture = Fixture::from_legacy(folder_document(16));
    let session = fixture.session();
    let descriptor = CommandDescriptor::from_normalized(&fixture.document, 0, &[json!({"op":"transform","layer":fixture.legacy.layers[0].id,"angle":17,"selection_only":false})], &|| Ok(())).unwrap();
    let token = descriptor.commands().next().unwrap().token();
    let captured = fixture
        .document
        .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
        .unwrap();
    let reason = prepare_command(
        captured,
        token,
        &session,
        &mut Work::new(1).unwrap(),
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert_eq!(reason, "Native transform mapping work budget exceeded");
    assert_eq!(session.created.get(), 0);
    assert_eq!(fixture.document.metadata().revision(), 0);
    drop(descriptor);
    drop(session);
    fixture.finish();
}

#[test]
fn folder_cancel_during_admitted_native_read_is_an_error_and_preserves_all_sources() {
    let fixture = Fixture::from_legacy(folder_document(16));
    let disk_baseline = fixture.disk.used().unwrap();
    let session = fixture.session();
    let descriptor = CommandDescriptor::from_normalized(&fixture.document, 0,
        &[json!({"op":"transform","layer":fixture.legacy.layers[0].id,"angle":17,"selection_only":false})], &|| Ok(())).unwrap();
    let token = descriptor.commands().next().unwrap().token();
    let captured = fixture
        .document
        .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
        .unwrap();
    let baseline = fixture.pool.payload_used();
    let cancelled = Cell::new(false);
    let reason = prepare_command(
        captured,
        token,
        &session,
        &mut Work::new(20_000_000).unwrap(),
        &|| {
            if fixture.pool.payload_used() > baseline + 128 * 1024 {
                cancelled.set(true);
                Err("cancel folder backing read".into())
            } else {
                Ok(())
            }
        },
    )
    .err()
    .unwrap();
    assert!(cancelled.get());
    assert_eq!(reason, "cancel folder backing read");
    assert_eq!(session.created.get(), 0);
    assert_eq!(fixture.disk.used().unwrap(), disk_baseline);
    assert_eq!(fixture.document.metadata().revision(), 0);
    for (layer, expected) in fixture
        .document
        .metadata()
        .layers()
        .zip(&fixture.legacy.layers)
    {
        planning::tests::assert_pixels(
            fixture.document.source(layer.pixels().slot()).unwrap(),
            &expected.pixels,
            &fixture.pool,
        );
    }
    drop(descriptor);
    drop(session);
    fixture.finish();
}

#[test]
fn folder_move_does_not_scan_native_pixels_for_an_unused_pivot() {
    let fixture = Fixture::from_legacy(folder_document(16));
    let baseline = fixture.pool.used();
    // Each present source tile contains 323 logical pixels. This allowance is
    // enough for hierarchy visits, but cannot admit even one alpha scan.
    let mut work = Work::new(64).unwrap();
    let plans = planning::folder(
        &fixture.document,
        &fixture.legacy.layers[0].id,
        &json!({"op":"move","dx":3,"dy":-2,"selection_only":false}),
        &mut work,
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(plans.layers.len(), 4);
    drop(plans);
    assert_eq!(fixture.pool.used(), baseline);
    fixture.finish();
}

#[test]
fn sparse_selected_scale_samples_across_tile_seams_and_reuses_unselected_pixels() {
    use crate::raster::TILE;
    use crate::transform::native::selected::{self, Placement, Selection};

    for depth in [8, 16] {
        let mut doc = Document::new_depth(4096, 2048, depth).unwrap();
        let paint = [12347, 23459, 34571, 65535];
        let hidden = [54321, 43213, 32123, 0];
        doc.layers[0].pixels.set16(512, 512, paint);
        doc.layers[0].pixels.set16(4000, 1800, hidden);
        doc.selection = Some([0, 0, 3800, 1600]);
        let fixture = Fixture::from_legacy(doc);
        let session = fixture.session();
        let fragment = session.fresh().unwrap();
        let selection = Selection::new(&fixture.document).unwrap();
        let placement = Placement::for_selection(&selection, &json!({
            "op":"transform", "pivot":[0.,0.], "scale_x":0.5, "scale_y":0.5,
            "dx":0.5, "dy":0.25, "fractional_translation":true,
        }), &fixture.pool).unwrap();
        let layer = fixture.document.metadata().layer(0).unwrap();
        let source = fixture.document.source(layer.pixels().slot()).unwrap();
        let written = selected::write(
            source, &selection, &placement, layer.origin(), [4096, 2048], None, true,
            fragment.store(), &mut Work::new(4_000_000).unwrap(),
            Checks { active: &|| Ok(()), owner: &|| Ok(()) },
        ).unwrap();
        assert!(written.cleared_retained);
        let untouched = (4000 / TILE, 1800 / TILE);
        assert_eq!(written.surface.physical_tile(untouched).unwrap().reference(),
            source.physical_tile(untouched).unwrap().reference());
        let at = |surface: &Surface, x, y| {
            let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
            surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap()
        };
        let native = |pixel: [u16; 4]| {
            if depth == 8 { pixel.map(|v| u16::from(crate::raster::project16(v)) * 257) } else { pixel }
        };
        assert_eq!(at(&written.surface, 256, 256), native([paint[0], paint[1], paint[2], 32768]));
        assert_eq!(at(&written.surface, 512, 512), [0; 4]);
        assert_eq!(at(&written.surface, 4000, 1800), native(hidden));
        assert_eq!(at(source, 512, 512), native(paint));
        drop((written, placement, selection));
        session.abort_fragment(fragment, &|| Ok(())).unwrap();
        drop(session);
        fixture.finish();
    }
}

#[test]
fn virtual_selected_cut_remainder_and_blend_match_actual_legacy_native_pixels() {
    use crate::transform::native::selected::{self, Placement, Selection};
    let mut cases = 0;
    for depth in [8, 16] {
        for kind in ["paint", "fill"] {
            for coverage in 0..3 {
                for mut command in [
                    json!({"op":"move","dx":-3,"dy":2}),
                    json!({"op":"transform","angle":90}),
                    json!({"op":"transform","angle":17.5,"scale_x":0.83,"scale_y":1.07,"skew_x":11.25,"dx":3.25,"dy":-2.75,"fractional_translation":true}),
                    json!({"op":"transform","projective":{"rect":[-4,-3,13,15],"quad":[[-3,-4],[14,-1],[11,17],[-5,13]]}}),
                    json!({"op":"transform","warp":{"rect":[-7,-6,20,22],"preset":"arc","amount":0.03}}),
                    json!({"op":"transform","angle":0}),
                ] {
                    let mut doc = planning::tests::fixture(depth, kind, None);
                    doc.layers[0].x = 2;
                    doc.layers[0].y = 3;
                    doc.layers[0].pixels.set(8, 1, [17, 83, 211, 0]);
                    let area = [-4, -3, 13, 15];
                    doc.selection = Some(area);
                    if coverage == 1 {
                        doc.selection_polygon = Some(vec![[-4., -3.], [13., -3.], [2., 15.]]);
                    }
                    if coverage == 2 {
                        let mut mask = crate::raster::Raster::new(17, 19);
                        for y in 0..19 {
                            for x in 0..17 {
                                let value =
                                    [0, 1, 64, 127, 128, 254, 255][(x + y * 3) as usize % 7];
                                mask.set(x, y, [value, 7, 11, 255]);
                            }
                        }
                        doc.selection_coverage = Some(crate::selection::Coverage {
                            mask,
                            bounds: area,
                            origin: [1, -2],
                            contours: vec![],
                        });
                    }
                    command["layer"] = json!(doc.layers[0].id);
                    let expected = Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
                    let fixture = Fixture::from_legacy(doc);
                    let session = fixture.session();
                    let fragment = session.fresh().unwrap();
                    let selection = Selection::new(&fixture.document).unwrap();
                    let placement =
                        Placement::for_selection(&selection, &command, &fixture.pool).unwrap();
                    let layer = fixture.document.metadata().layers().next().unwrap();
                    let source = fixture.document.source(layer.pixels().slot()).unwrap();
                    let written = selected::write(
                        source,
                        &selection,
                        &placement,
                        layer.origin(),
                        [layer.pixels().width(), layer.pixels().height()],
                        (kind == "fill").then_some(layer.color()),
                        true,
                        fragment.store(),
                        &mut Work::new(20_000_000).unwrap(),
                        Checks { active: &|| Ok(()), owner: &|| Ok(()) },
                    )
                    .unwrap();
                    assert_eq!(
                        [written.surface.width(), written.surface.height()],
                        [
                            expected.layers[0].pixels.width,
                            expected.layers[0].pixels.height
                        ],
                        "case{cases} depth{depth} kind{kind} coverage{coverage} command{command}"
                    );
                    planning::tests::assert_pixels(
                        &written.surface,
                        &expected.layers[0].pixels,
                        &fixture.pool,
                    );
                    if coverage == 2 {
                        let transformed = selected::write_coverage(
                            &selection,
                            &placement,
                            fragment.store(),
                            &mut Work::new(20_000_000).unwrap(),
                            Checks { active: &|| Ok(()), owner: &|| Ok(()) },
                        )
                        .unwrap();
                        let old = expected.selection_coverage.as_ref().unwrap();
                        assert_eq!(transformed.bounds(), old.bounds);
                        assert_eq!(transformed.origin(), old.origin);
                        planning::tests::assert_pixels(
                            transformed.mask(),
                            &old.mask,
                            &fixture.pool,
                        );
                        assert_eq!(
                            contour_edges(transformed.contours()),
                            contour_edges(&old.contours)
                        );
                    }
                    let b = placement.bounds;
                    let expected_origin = if placement.noop {
                        layer.origin()
                    } else {
                        [layer.origin()[0].min(b[0]), layer.origin()[1].min(b[1])]
                    };
                    assert_eq!(
                        expected_origin,
                        [expected.layers[0].x, expected.layers[0].y]
                    );
                    planning::tests::assert_pixels(
                        source,
                        &fixture.legacy.layers[0].pixels,
                        &fixture.pool,
                    );
                    drop(written);
                    drop(selection);
                    drop(placement);
                    session.abort_fragment(fragment, &|| Ok(())).unwrap();
                    drop(session);
                    fixture.finish();
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 72);
}

fn contour_edges(contours: &[Vec<[f32; 2]>]) -> Vec<([i32; 2], [i32; 2])> {
    let mut edges = Vec::new();
    for contour in contours {
        for (i, point) in contour.iter().enumerate() {
            let a = point.map(|v| v as i32);
            let b = contour[(i + 1) % contour.len()].map(|v| v as i32);
            let step = [(b[0] - a[0]).signum(), (b[1] - a[1]).signum()];
            assert!(step[0] == 0 || step[1] == 0);
            let mut p = a;
            while p != b {
                let q = [p[0] + step[0], p[1] + step[1]];
                edges.push((p, q));
                p = q;
            }
        }
    }
    edges.sort_unstable();
    edges
}

#[test]
fn sparse_selected_scale_keeps_soft_mask_samples_and_contours() {
    use crate::transform::native::selected::{self, Placement, Selection};

    let mut doc = Document::new_depth(4096, 2048, 16).unwrap();
    let mut mask = crate::raster::Raster::new(4096, 2048);
    mask.set(512, 512, [191; 4]);
    mask.set(3000, 1000, [255; 4]);
    let area = [0, 0, 3800, 1600];
    doc.selection = Some(area);
    doc.selection_coverage = Some(crate::selection::Coverage {
        mask, bounds: area, origin: [0, 0], contours: vec![],
    });
    let fixture = Fixture::from_legacy(doc);
    let session = fixture.session();
    let fragment = session.fresh().unwrap();
    let selection = Selection::new(&fixture.document).unwrap();
    let placement = Placement::for_selection(&selection, &json!({
        "op":"transform", "pivot":[0.,0.], "scale_x":0.5, "scale_y":0.5,
        "dx":0.5, "dy":0.25, "fractional_translation":true,
    }), &fixture.pool).unwrap();
    let output = selected::write_coverage(
        &selection, &placement, fragment.store(), &mut Work::new(1_200_000).unwrap(),
        Checks { active: &|| Ok(()), owner: &|| Ok(()) },
    ).unwrap();
    assert_eq!(output.bounds(), [256, 256, 1501, 501]);
    assert_eq!(output.origin(), [0, 0]);
    assert_eq!(output.mask().physical_tiles().len(), 2);
    assert_eq!(contour_edges(output.contours()).len(), 4);
    for (x, y, value) in [(256, 256, 96), (1500, 500, 128), (1000, 380, 0)] {
        let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
        let view = output.mask().prepare_region(region, &mut || Ok(())).unwrap();
        assert_eq!(view.pixel(x, y).unwrap(), [value; 4]);
    }
    drop((output, placement, selection));
    session.abort_fragment(fragment, &|| Ok(())).unwrap();
    drop(session);
    fixture.finish();
}

#[test]
fn selected_folder_geometry_scalar_preserves_canvas_clipping_and_contour_edges() {
    use crate::transform::native::selected::{self, Placement, Selection};
    for polygon in [false, true] {
        for mut command in [
            json!({"op":"move","dx":-3,"dy":2}),
            json!({"op":"transform","angle":90}),
            json!({"op":"transform","angle":17.5,"skew_x":11.25,"dx":3.25,"dy":-2.75,"fractional_translation":true}),
        ] {
            let mut doc = folder_document(16);
            doc.selection = Some([-4, -3, 13, 15]);
            if polygon {
                doc.selection_polygon = Some(vec![[-4., -3.], [13., -3.], [2., 15.]]);
            }
            command["layer"] = json!(doc.layers[0].id);
            let expected = Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
            let fixture = Fixture::from_legacy(doc);
            let session = fixture.session();
            let fragment = session.fresh().unwrap();
            let selection = Selection::for_folder(&fixture.document).unwrap();
            let placement = Placement::for_selection(&selection, &command, &fixture.pool).unwrap();
            let output = selected::write_coverage(
                &selection,
                &placement,
                fragment.store(),
                &mut Work::new(20_000_000).unwrap(),
                Checks { active: &|| Ok(()), owner: &|| Ok(()) },
            )
            .unwrap();
            let old = expected.selection_coverage.as_ref().unwrap();
            assert_eq!(output.bounds(), old.bounds);
            assert_eq!(output.origin(), old.origin);
            planning::tests::assert_pixels(output.mask(), &old.mask, &fixture.pool);
            assert_eq!(
                contour_edges(output.contours()),
                contour_edges(&old.contours)
            );
            drop(output);
            drop(placement);
            drop(selection);
            session.abort_fragment(fragment, &|| Ok(())).unwrap();
            drop(session);
            fixture.finish();
        }
    }
}

fn assert_raster_metadata(
    native: &NativeDocument,
    metadata: crate::engine::native::RasterMetadata<'_>,
    expected: &crate::raster::Raster,
    pool: &ResidentBudget,
) {
    planning::tests::assert_pixels(native.source(metadata.slot()).unwrap(), expected, pool);
    match (metadata.retained(), expected.retained.as_ref()) {
        (None, None) => (),
        (Some(actual), Some(expected)) => {
            assert_eq!(actual.matrix(), expected.matrix);
            assert_eq!(actual.projective(), expected.projective);
            assert_eq!(
                serde_json::to_value(actual.warps()).unwrap(),
                serde_json::to_value(&expected.warps).unwrap()
            );
            planning::tests::assert_pixels(
                native.source(actual.pixels().slot()).unwrap(),
                &expected.pixels,
                pool,
            );
        }
        _ => panic!(
            "Native retained metadata differs: actual{} expected{} frame{}x{}",
            metadata.retained().is_some(),
            expected.retained.is_some(),
            expected.width,
            expected.height
        ),
    }
}
fn assert_coverage_metadata(
    native: &NativeDocument,
    metadata: crate::engine::native::CoverageMetadata<'_>,
    expected: &crate::selection::Coverage,
    pool: &ResidentBudget,
) {
    assert_eq!(metadata.bounds(), expected.bounds);
    assert_eq!(metadata.origin(), expected.origin);
    assert_raster_metadata(native, metadata.mask(), &expected.mask, pool);
    let diagonal = expected.contours.iter().any(|line| {
        line.iter().enumerate().any(|(i, p)| {
            let q = line[(i + 1) % line.len()];
            p[0] != q[0] && p[1] != q[1]
        })
    });
    if diagonal {
        assert_eq!(metadata.contours(), expected.contours);
    } else {
        assert_eq!(
            contour_edges(metadata.contours()),
            contour_edges(&expected.contours)
        );
    }
}
fn assert_selected_candidate(native: &NativeDocument, expected: &Document, pool: &ResidentBudget) {
    for (layer, old) in native.metadata().layers().zip(&expected.layers) {
        assert_eq!(layer.id(), old.id);
        assert_eq!(layer.parent(), old.parent.as_deref());
        assert_eq!(layer.origin(), [old.x, old.y]);
        assert_eq!(layer.kind(), old.kind);
        assert_raster_metadata(native, layer.pixels(), &old.pixels, pool);
        match (layer.mask(), old.mask.as_ref()) {
            (None, None) => (),
            (Some(mask), Some(old)) => {
                assert_eq!(mask.enabled(), old.enabled);
                assert_eq!(mask.linked(), old.linked);
                assert_eq!(mask.steps().len(), old.steps.len());
                for (step, old) in mask.steps().zip(&old.steps) {
                    assert_eq!(step.id(), old.id);
                    assert_eq!(step.kind(), old.kind);
                    assert_eq!(step.enabled(), old.enabled);
                    assert_eq!(step.weight(), old.weight);
                    assert_eq!(step.value(), old.value);
                    assert_raster_metadata(native, step.pixels(), &old.pixels, pool);
                }
            }
            _ => panic!("Native mask presence differs"),
        }
        assert_eq!(layer.effects().len(), old.effects.len());
        for (effect, old) in layer.effects().zip(&old.effects) {
            assert_eq!(effect.id(), old.id);
            assert_eq!(effect.kind(), old.kind);
            assert_eq!(effect.enabled(), old.enabled);
            assert_eq!(effect.weight(), old.weight);
            let mut expected = (*old.settings).clone();
            let mut clips = 0;
            if let Some(strokes) = expected
                .get_mut("strokes")
                .and_then(serde_json::Value::as_array_mut)
            {
                for (index, stroke) in strokes.iter_mut().enumerate() {
                    if let Some(raw) = stroke.get("coverage").filter(|v| !v.is_null()) {
                        let coverage: crate::selection::Coverage =
                            serde_json::from_value(raw.clone()).unwrap();
                        let actual = effect.clips().find(|(s, _)| *s == index as u32).unwrap().1;
                        assert_coverage_metadata(native, actual, &coverage, pool);
                        stroke.as_object_mut().unwrap().remove("coverage");
                        clips += 1;
                    }
                }
            }
            assert_eq!(effect.clips().len(), clips);
            assert_eq!(effect.settings(), &expected);
        }
    }
    assert_eq!(native.metadata().selection(), expected.selection);
    assert_eq!(
        native.metadata().selection_polygon(),
        expected.selection_polygon.as_deref()
    );
    for (actual, old) in [
        (
            native.metadata().selection_coverage(),
            expected.selection_coverage.as_ref(),
        ),
        (
            native.metadata().selection_previous(),
            expected.selection_previous.as_ref(),
        ),
    ] {
        match (actual, old) {
            (None, None) => (),
            (Some(actual), Some(old)) => assert_coverage_metadata(native, actual, old, pool),
            _ => panic!("Native selection source presence differs"),
        }
    }
}
fn selected_document(depth: u16, kind: &str, linked: Option<bool>, raw: bool) -> Document {
    let mut doc = planning::tests::fixture(depth, kind, linked);
    doc.layers[0].x = 2;
    doc.layers[0].y = 3;
    doc.layers[0].pixels.set(8, 1, [17, 83, 211, 0]);
    if let Some(mask) = &mut doc.layers[0].mask {
        let mut pixels = crate::raster::Raster::new_depth(17, 19, depth);
        pixels.set16(1, 4, [20123, 31007, 41009, 65003]);
        mask.steps.push(crate::engine::MaskStep {
            id: "second".into(),
            kind: "paint".into(),
            enabled: false,
            weight: 0.45,
            value: 1.,
            pixels,
            settings: json!({}),
        });
    }
    doc.selection = Some([-4, -3, 13, 15]);
    if raw {
        let mut mask = crate::raster::Raster::new(17, 19);
        for y in 0..19 {
            for x in 0..17 {
                let v = [0, 1, 64, 127, 128, 254, 255][(x + y * 3) as usize % 7];
                mask.set(x, y, [v, 7, 11, 255]);
            }
        }
        doc.selection_coverage = Some(crate::selection::Coverage {
            mask,
            bounds: doc.selection.unwrap(),
            origin: [1, -2],
            contours: vec![],
        });
    } else {
        doc.selection_polygon = Some(vec![[-4., -3.], [13., -3.], [2., 15.]]);
    }
    let mut mask = crate::raster::Raster::new(3, 4);
    mask.set(1, 2, [83; 4]);
    let original = crate::retained::Original::from(&mask);
    mask.retained = Some(Box::new(original));
    doc.selection_previous = Some(crate::selection::Coverage {
        mask,
        bounds: [1, 1, 3, 4],
        origin: [1, 1],
        contours: vec![vec![[1., 1.], [3., 1.], [3., 4.], [1., 4.]]],
    });
    doc
}

#[test]
fn selected_command_atomically_matches_legacy_color_masks_selection_and_originals() {
    let mut cases = 0;
    for depth in [8, 16] {
        for (kind, linked) in [
            ("paint", None),
            ("paint", Some(true)),
            ("paint", Some(false)),
            ("fill", Some(true)),
            ("fill", Some(false)),
        ] {
            for raw in [false, true] {
                for mut command in [
                    json!({"op":"transform","angle":17.5,"scale_x":0.83,"scale_y":1.07,"skew_x":11.25,"dx":3.25,"dy":-2.75,"fractional_translation":true}),
                    json!({"op":"transform","mask":true,"step":"paint","angle":-13.25,"dx":-2}),
                    json!({"op":"move","mask":true,"dx":-3,"dy":2}),
                ] {
                    if linked.is_none() && command["mask"] == true {
                        continue;
                    }
                    let doc = selected_document(depth, kind, linked, raw);
                    command["layer"] = json!(doc.layers[0].id);
                    let expected = Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
                    let fixture = Fixture::from_legacy(doc);
                    let baseline = fixture.disk.used().unwrap();
                    let session = fixture.session();
                    let descriptor = CommandDescriptor::from_normalized(
                        &fixture.document,
                        0,
                        &[command.clone()],
                        &|| Ok(()),
                    )
                    .unwrap();
                    let token = descriptor.commands().next().unwrap().token();
                    let captured = fixture
                        .document
                        .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
                        .unwrap();
                    let prepared=prepare_command(captured,token,&session,&mut Work::new(50_000_000).unwrap(),&||Ok(())).unwrap_or_else(|reason|panic!("case{cases} depth{depth} {kind} {linked:?} raw{raw} {command}: {reason}"));
                    assert_eq!(prepared.preview().metadata().revision(), 1);
                    assert_eq!(fixture.document.metadata().revision(), 0);
                    assert_selected_candidate(prepared.preview(), &expected, &fixture.pool);
                    fixture
                        .document
                        .adopt(prepared)
                        .err()
                        .unwrap()
                        .abort(&session, &|| Ok(()))
                        .unwrap();
                    assert_eq!(fixture.disk.used().unwrap(), baseline);
                    drop(descriptor);
                    drop(session);
                    fixture.finish();
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 52);
}

#[test]
fn selected_different_mask_frames_preserve_current_vs_original_and_final_linked_reframe() {
    for depth in [8, 16] {
        for linked in [false, true] {
            for mut command in [
                json!({"op":"transform","angle":0}),
                json!({"op":"transform","angle":17.5}),
                json!({"op":"transform","mask":true,"step":"paint","angle":17.5}),
            ] {
                let mut doc = selected_document(depth, "paint", Some(linked), true);
                doc.selection = Some([4, 6, 9, 12]);
                doc.selection_coverage = None;
                let mask = doc.layers[0].mask.as_mut().unwrap();
                let mut large = crate::raster::Raster::new_depth(31, 29, depth);
                large.set16(16, 18, [10001, 20003, 30007, 65009]);
                large.set16(30, 25, [41001, 31003, 21007, 64009]);
                let original = crate::retained::Original::from(&large);
                mask.steps[0].pixels = original.render(7, 9).unwrap();
                mask.steps[1].pixels = large;
                command["layer"] = json!(doc.layers[0].id);
                let expected = Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
                let fixture = Fixture::from_legacy(doc);
                let baseline = fixture.disk.used().unwrap();
                let session = fixture.session();
                let descriptor = CommandDescriptor::from_normalized(
                    &fixture.document,
                    0,
                    &[command.clone()],
                    &|| Ok(()),
                )
                .unwrap();
                let token = descriptor.commands().next().unwrap().token();
                let captured = fixture
                    .document
                    .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
                    .unwrap();
                let cancellation = captured.cancellation();
                let prepared = prepare_command(
                    captured,
                    token,
                    &session,
                    &mut Work::new(50_000_000).unwrap(),
                    &|| Ok(()),
                )
                .unwrap_or_else(|e| panic!("depth{depth} linked{linked} {command}: {e}"));
                assert_selected_candidate(prepared.preview(), &expected, &fixture.pool);
                cancellation.cancel();
                fixture
                    .document
                    .publish("human", &[], 1, false, prepared)
                    .err()
                    .unwrap()
                    .abort(&session, &|| Ok(()))
                    .unwrap();
                assert_eq!(fixture.disk.used().unwrap(), baseline);
                drop(descriptor);
                drop(session);
                fixture.finish();
            }
        }
    }
}

#[test]
fn selected_expansion_translates_typed_liquify_controls_clips_and_preserves_unknown_fields() {
    for depth in [8, 16] {
        for mask_only in [false, true] {
            let mut doc = selected_document(depth, "paint", Some(false), true);
            let mut clip = crate::raster::Raster::new(3, 4);
            clip.set(0, 1, [127, 7, 11, 255]);
            clip.set(1, 2, [255; 4]);
            clip.retained = Some(Box::new(crate::retained::Original::from(&clip)));
            let coverage = crate::selection::Coverage {
                mask: clip,
                bounds: [0, 0, 8, 10],
                origin: [-1, 2],
                contours: vec![vec![[0., 0.], [8., 0.], [0., 10.]]],
            };
            let settings = json!({"radius":4.,"strength":0.5,"future":{"untouched":[true,7,"exact"]},"strokes":[
                {"mode":"push","points":[[2.125,3.75],[5.5,7.25]],"selection":[0,0,8,10],"polygon":[[0.,0.],[8.,0.],[0.,10.]],"coverage":coverage,"future_stroke":17},
                {"mode":"expand","points":[[4.,5.]],"selection":[1,2,7,9]}
            ]});
            crate::effects::validate("liquify", &settings).unwrap();
            doc.layers[0].effects.push(crate::effects::Effect {
                id: "local-warp".into(),
                kind: "liquify".into(),
                enabled: false,
                weight: 0.35,
                settings: settings.into(),
            });
            let command = json!({"op":"move","layer":doc.layers[0].id,"mask":mask_only,"step":"paint","dx":-20,"dy":-10});
            let expected = Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
            let fixture = Fixture::from_legacy(doc);
            let baseline = fixture.disk.used().unwrap();
            let session = fixture.session();
            let descriptor =
                CommandDescriptor::from_normalized(&fixture.document, 0, &[command], &|| Ok(()))
                    .unwrap();
            let token = descriptor.commands().next().unwrap().token();
            let captured = fixture
                .document
                .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
                .unwrap();
            let prepared = prepare_command(
                captured,
                token,
                &session,
                &mut Work::new(50_000_000).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            assert_selected_candidate(prepared.preview(), &expected, &fixture.pool);
            fixture
                .document
                .adopt(prepared)
                .err()
                .unwrap()
                .abort(&session, &|| Ok(()))
                .unwrap();
            assert_eq!(fixture.disk.used().unwrap(), baseline);
            drop(descriptor);
            drop(session);
            fixture.finish();
        }
    }
}

#[test]
fn selected_cancel_stale_generation_and_late_lease_refusal_retire_complete_packets() {
    let fixture = Fixture::from_legacy(selected_document(16, "paint", Some(true), true));
    let baseline = fixture.disk.used().unwrap();
    let command = json!({"op":"move","layer":fixture.legacy.layers[0].id,"dx":-20,"dy":-10});
    for mode in [0, 1, 2, 3] {
        let session = fixture.session();
        let descriptor = CommandDescriptor::from_normalized(
            &fixture.document,
            0,
            &[command.clone()],
            &|| Ok(()),
        )
        .unwrap();
        let token = descriptor.commands().next().unwrap().token();
        let captured = fixture
            .document
            .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
            .unwrap();
        let cancellation = captured.cancellation();
        let result = prepare_command(
            captured,
            token,
            &session,
            &mut Work::new(50_000_000).unwrap(),
            &|| {
                if mode == 0 && session.created.get() >= 2 {
                    Err("cancel selected intermediate".into())
                } else {
                    Ok(())
                }
            },
        );
        if mode == 0 {
            assert_eq!(result.err().unwrap(), "cancel selected intermediate");
            assert_eq!(session.created.get(), 2);
        } else {
            let prepared = result.unwrap();
            let refused = if mode == 1 {
                cancellation.cancel();
                fixture
                    .document
                    .publish("human", &[], 1, false, prepared)
                    .err()
                    .unwrap()
            } else if mode == 2 {
                let target = fixture.document.metadata().layers().next().unwrap().id();
                let changed = fixture
                    .document
                    .capture_edit()
                    .unwrap()
                    .visibility(target, false, &|| Ok(()))
                    .unwrap();
                let current = fixture.document.adopt(changed).ok().unwrap();
                current
                    .publish("human", &[], 1, false, prepared)
                    .err()
                    .unwrap()
            } else {
                let lease = crate::engine::Lease {
                    id: "late-selected".into(),
                    owner: "other".into(),
                    description: "selected pixels".into(),
                    scopes: vec![crate::engine::Scope::layer(&fixture.legacy.layers[0].id)],
                    expires: 100,
                };
                fixture
                    .document
                    .publish("human", &[lease], 1, false, prepared)
                    .err()
                    .unwrap()
            };
            refused.abort(&session, &|| Ok(())).unwrap();
        }
        assert_eq!(fixture.disk.used().unwrap(), baseline);
        assert_eq!(fixture.document.metadata().revision(), 0);
        drop(descriptor);
        drop(session);
    }
    fixture.finish();
}

fn selected_case(doc: Document, command: serde_json::Value) {
    let expected = Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
    let fixture = Fixture::from_legacy(doc);
    let baseline = fixture.disk.used().unwrap();
    let session = fixture.session();
    let descriptor =
        CommandDescriptor::from_normalized(&fixture.document, 0, &[command.clone()], &|| Ok(()))
            .unwrap();
    let token = descriptor.commands().next().unwrap().token();
    let captured = fixture
        .document
        .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
        .unwrap();
    let cancellation = captured.cancellation();
    let prepared = prepare_command(
        captured,
        token,
        &session,
        &mut Work::new(50_000_000).unwrap(),
        &|| Ok(()),
    )
    .unwrap_or_else(|reason| panic!("{command}: {reason}"));
    assert_selected_candidate(prepared.preview(), &expected, &fixture.pool);
    assert_eq!(fixture.document.metadata().revision(), 0);
    cancellation.cancel();
    fixture
        .document
        .publish("human", &[], 1, false, prepared)
        .err()
        .unwrap()
        .abort(&session, &|| Ok(()))
        .unwrap();
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    drop(descriptor);
    drop(session);
    fixture.finish();
}

#[test]
fn selected_folder_complete_batch_keeps_hidden_children_container_masks_and_clipped_geometry() {
    for depth in [8, 16] {
        for raw in [false, true] {
            for command in [
                json!({"op":"transform","angle":17.5,"skew_x":11.25}),
                json!({"op":"move","dx":-3,"dy":2}),
            ] {
                let mut doc = folder_document(depth);
                doc.selection = Some([-4, -3, 13, 15]);
                if raw {
                    let mut mask = crate::raster::Raster::new(7, 9);
                    for y in 0..9 {
                        for x in 0..7 {
                            let v = [0, 1, 83, 127, 254, 255][(x + 3 * y) as usize % 6];
                            mask.set(x, y, [v, 7, 11, 255]);
                        }
                    }
                    doc.selection_coverage = Some(crate::selection::Coverage {
                        mask,
                        bounds: doc.selection.unwrap(),
                        origin: [1, -2],
                        contours: vec![],
                    });
                } else {
                    doc.selection_polygon = Some(vec![[-4., -3.], [13., -3.], [2., 15.]]);
                }
                let mut command = command;
                command["layer"] = json!(doc.layers[0].id);
                selected_case(doc, command);
            }
        }
    }
    // The folder mask route is one temporary paint target; children are untouched.
    for depth in [8, 16] {
        for linked in [false, true] {
            let mut doc = selected_document(depth, "paint", Some(linked), false);
            doc.layers[0].kind = "group".into();
            let mut child = planning::tests::fixture(depth, "paint", None)
                .layers
                .remove(0);
            child.parent = Some(doc.layers[0].id.clone());
            doc.layers.push(child);
            let command = json!({"op":"transform","layer":doc.layers[0].id,"mask":true,"step":"paint","angle":-13.25});
            selected_case(doc, command);
        }
    }
}

#[test]
fn selected_retained_current_and_stale_coverage_preserve_exact_legacy_source_policy() {
    for depth in [8, 16] {
        for mask in [false, true] {
            for noop in [false, true] {
                let mut doc = selected_document(depth, "paint", Some(false), false);
                let original = crate::retained::Original::from(&doc.layers[0].pixels);
                doc.layers[0].pixels = original.render(17, 19).unwrap();
                let original = crate::retained::Original::from(
                    &doc.layers[0].mask.as_ref().unwrap().steps[0].pixels,
                );
                doc.layers[0].mask.as_mut().unwrap().steps[0].pixels =
                    original.render(17, 19).unwrap();
                let mut stale = crate::raster::Raster::new(3, 4);
                stale.set(1, 2, [123; 4]);
                stale.retained = Some(Box::new(crate::retained::Original::from(&stale)));
                doc.selection_coverage = Some(crate::selection::Coverage {
                    mask: stale,
                    bounds: [1, 1, 3, 4],
                    origin: [-1, 2],
                    contours: vec![vec![[1., 1.], [3., 1.], [1., 4.]]],
                });
                let command = json!({"op":"transform","layer":doc.layers[0].id,"mask":mask,"step":"paint","angle":if noop {0.}else{17.5}});
                selected_case(doc, command);
            }
        }
    }
}

#[test]
fn selected_empty_raw_and_folder_geometry_and_named_noop_keep_legacy_early_return_order() {
    for depth in [8, 16] {
        for folder in [false, true] {
            let mut doc = if folder {
                folder_document(depth)
            } else {
                selected_document(depth, "paint", None, false)
            };
            doc.selection = Some([0, 0, 0, 0]);
            doc.selection_polygon = None;
            doc.selection_coverage = Some(crate::selection::Coverage {
                mask: crate::raster::Raster::new(3, 4),
                bounds: [0, 0, 0, 0],
                origin: [-1, 2],
                contours: vec![],
            });
            let command = json!({"op":"transform","layer":doc.layers[0].id,"mask":!folder,"step":"missing","angle":17.5});
            selected_case(doc, command);
            if folder {
                for empty in [true, false] {
                    let mut doc = folder_document(depth);
                    doc.selection = Some(if empty {
                        [0, 0, 0, 0]
                    } else {
                        [-20, -20, -10, -10]
                    });
                    let command = json!({"op":"move","layer":doc.layers[0].id,"dx":2,"dy":3});
                    selected_case(doc, command);
                }
            }
        }
        let doc = selected_document(depth, "paint", None, false);
        let command = json!({"op":"transform","layer":doc.layers[0].id,"mask":true,"step":"missing","angle":0.});
        selected_case(doc, command);
    }
}

#[test]
fn selected_native_tile_edges_and_soft_coverage_match_complete_legacy_planes() {
    for depth in [8, 16] {
        for moving in [false, true] {
            let mut doc = Document::new_depth(259, 257, depth).unwrap();
            for (x, y, p) in [
                (0, 0, [12345, 23003, 34007, 65535]),
                (255, 255, [20123, 31007, 41009, 50003]),
                (256, 255, [12001, 22003, 32009, 1]),
                (258, 256, [17003, 23007, 43009, 0]),
            ] {
                doc.layers[0].pixels.set16(x, y, p);
            }
            doc.selection = Some([252, 250, 259, 257]);
            let mut mask = crate::raster::Raster::new(259, 257);
            for y in 249..257 {
                for x in 251..259 {
                    let v = [1, 64, 127, 128, 254, 255][(x + 3 * y) as usize % 6];
                    mask.set(x, y, [v, 7, 11, 255]);
                }
            }
            doc.selection_coverage = Some(crate::selection::Coverage {
                mask,
                bounds: doc.selection.unwrap(),
                origin: [0, 0],
                contours: vec![],
            });
            let command = if moving {
                json!({"op":"move","layer":doc.layers[0].id,"dx":-3,"dy":-2})
            } else {
                json!({"op":"transform","layer":doc.layers[0].id,"angle":17.5,"dx":-2,"dy":-3})
            };
            selected_case(doc, command);
        }
    }
}

#[test]
fn direct_adjustment_geometry_refuses_before_staging_like_shared_legacy_engine() {
    for depth in [8, 16] {
        for selected in [false, true] {
            for mask in [false, true] {
                for op in ["move", "transform"] {
                    let mut doc = planning::tests::fixture(depth, "adjustment", Some(false));
                    doc.selection = selected.then_some([0, 0, 7, 9]);
                    let command = json!({"op":op,"layer":doc.layers[0].id,"mask":mask,"step":"paint","dx":2,"angle":17.5});
                    let expected = Engine::preview_edits(doc.clone(), &[command.clone()])
                        .err()
                        .unwrap();
                    let fixture = Fixture::from_legacy(doc);
                    let session = fixture.session();
                    let descriptor = CommandDescriptor::from_normalized(
                        &fixture.document,
                        0,
                        &[command],
                        &|| Ok(()),
                    )
                    .unwrap();
                    let token = descriptor.commands().next().unwrap().token();
                    let captured = fixture
                        .document
                        .capture_descriptor(descriptor.clone(), "human", &[], 1, false, true)
                        .unwrap();
                    assert_eq!(
                        prepare_command(
                            captured,
                            token,
                            &session,
                            &mut Work::new(1_000_000).unwrap(),
                            &|| Ok(())
                        )
                        .err()
                        .unwrap(),
                        expected
                    );
                    assert_eq!(session.created.get(), 0);
                    drop(descriptor);
                    drop(session);
                    fixture.finish();
                }
            }
        }
    }
}
