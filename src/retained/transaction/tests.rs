use super::*;
use crate::{
    engine::{native::CommandDescriptor, Document, Engine},
    large_document::{BlockRef, Limits, Snapshot},
    retained::planning,
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
            &self.parent.join(format!("fragment-{next}")),
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
    let fixture = Fixture::new(16, "paint", Some(true));
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
                        (kind == "fill").then_some(layer.color()),
                        fragment.store(),
                        &mut Work::new(20_000_000).unwrap(),
                        &|| Ok(()),
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
                            &|| Ok(()),
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
                &|| Ok(()),
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
