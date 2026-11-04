use super::*;
use crate::clipboard::native::{
    layers::{LayerCopyPlan, PastePlacement},
    promotion,
};
use crate::engine::{Layer, Mask, MaskStep};

fn promotion_doc() -> Document {
    let mut value = doc(8, 3);
    // A real physical tile can contain nonlogical bytes or be entirely zero.
    Arc::make_mut(value.layers[0].pixels.tiles.get_mut(&(0, 0)).unwrap())
        [((201 * TILE + 200) * 4) as usize] = 233;
    value.layers[1]
        .pixels
        .tiles
        .insert((0, 0), Arc::new(vec![0; (TILE * TILE * 4) as usize]));
    let mut pixels = Raster::new(17, 19);
    pixels.set(3, 5, [71, 19, 127, 0]);
    let mut retained = crate::retained::Original::from(&pixels);
    retained.pixels.set(9, 13, [129, 67, 31, 255]);
    pixels.retained = Some(Box::new(retained));
    value.layers[0].mask = Some(Mask {
        enabled: false,
        linked: false,
        cache_key: crate::engine::id(),
        steps: vec![MaskStep {
            id: "promote-mask".into(),
            kind: "paint".into(),
            enabled: true,
            weight: 1.,
            value: 1.,
            pixels,
            settings: serde_json::json!({}),
        }],
    });
    value
}

#[test]
fn native_promotion_preserves_all_physical_words_originals_and_masks_but_not_coverage() {
    for selected_only in [false, true] {
        let fixture = Fixture::new(promotion_doc());
        let hash = fixture.source_hash();
        let disk = fixture.disk.used().unwrap();
        let session = fixture.session();
        let root = fixture
            .document
            .metadata()
            .layer(0)
            .unwrap()
            .pixels()
            .slot();
        let members = [root];
        let result = promotion::stage(
            &fixture.document,
            selected_only.then_some(members.as_slice()),
            &session,
            &mut Work::new(2_000_000).unwrap(),
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(result.source(), fixture.document.key());
        assert_eq!(result.entries().len(), if selected_only { 4 } else { 5 });
        let coverage = fixture
            .document
            .metadata()
            .selection_coverage()
            .unwrap()
            .mask()
            .slot();
        assert!(!result
            .entries()
            .iter()
            .any(|item| item.target() == coverage));
        for entry in result.entries() {
            let input = fixture.document.source(entry.target()).unwrap();
            let output = entry.output().surface();
            assert_eq!(entry.input(), input.key());
            assert_eq!(
                (output.width(), output.height(), output.depth()),
                (input.width(), input.height(), 16)
            );
            assert_eq!(
                output.physical_tiles().count(),
                input.physical_tiles().count()
            );
            for tile in input.physical_tiles() {
                let before = input
                    .prepare_tile(tile.coordinate(), &mut || Ok(()))
                    .unwrap()
                    .copy_to_scratch(&fixture.pool)
                    .unwrap();
                let after = output
                    .prepare_tile(tile.coordinate(), &mut || Ok(()))
                    .unwrap()
                    .copy_to_scratch(&fixture.pool)
                    .unwrap();
                assert_eq!(after.len(), before.len() * 2);
                for (byte, word) in before.iter().zip(after.chunks_exact(2)) {
                    assert_eq!(word, &[*byte, *byte]);
                }
            }
        }
        result.abort(&session).unwrap();
        assert_eq!(fixture.disk.used().unwrap(), disk);
        assert_eq!(fixture.source_hash(), hash);
        drop(session);
        fixture.finish();
    }
}

#[test]
fn promotion_cancellation_retires_every_completed_candidate_and_keeps_sources() {
    let fixture = Fixture::new(promotion_doc());
    let hash = fixture.source_hash();
    let disk = fixture.disk.used().unwrap();
    let session = fixture.session();
    let reason = promotion::stage(
        &fixture.document,
        None,
        &session,
        &mut Work::new(2_000_000).unwrap(),
        &|| {
            if session.created.get() >= 2 {
                Err("cancel second promotion".into())
            } else {
                Ok(())
            }
        },
    )
    .err()
    .unwrap();
    assert_eq!(reason, "cancel second promotion");
    assert_eq!(session.created.get(), 2);
    assert_eq!(fixture.disk.used().unwrap(), disk);
    assert_eq!(fixture.source_hash(), hash);
    drop(session);
    fixture.finish();
}

fn tree_doc(depth: u16) -> Document {
    let mut value = doc(depth, 0);
    let mut folder = Layer::new("Folder", "group", 17, 19);
    folder.pixels = Raster::new_depth(17, 19, depth);
    let parent = folder.id.clone();
    value.layers[0].parent = Some(parent.clone());
    value.layers[1].parent = Some(parent);
    value.layers[0].clip_to = Some(value.layers[1].id.clone());
    value.layers[1].visible = false;
    // Children intentionally precede their parent in storage.
    value.layers.push(folder);
    let mut other = Layer::new("Other", "paint", 17, 19);
    other.pixels = Raster::new_depth(17, 19, depth);
    value.layers.push(other);
    value
}

#[test]
fn layer_plan_matches_legacy_roots_hidden_members_order_and_cut_lock_refusal() {
    for depth in [8, 16] {
        for locked in [false, true] {
            let mut legacy = tree_doc(depth);
            legacy.layers[2].locked = locked;
            let fixture = Fixture::new(legacy);
            let ids = vec![
                fixture.legacy.layers[3].id.clone(),
                fixture.legacy.layers[0].id.clone(),
                fixture.legacy.layers[2].id.clone(),
                fixture.legacy.layers[2].id.clone(),
            ];
            let expected = crate::layer_clipboard::copy(&fixture.legacy, &ids).unwrap();
            let plan = LayerCopyPlan::capture(
                &fixture.document,
                &ids,
                &mut Work::new(100_000).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(plan.key(), fixture.document.key());
            assert_eq!(plan.source().key(), fixture.document.key());
            let roots: Vec<_> = plan
                .roots()
                .iter()
                .map(|i| plan.layer(*i).unwrap().id())
                .collect();
            assert_eq!(
                roots,
                expected
                    .roots
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            );
            let members: Vec<_> = plan
                .members()
                .iter()
                .map(|i| plan.layer(*i).unwrap().id())
                .collect();
            assert_eq!(
                members,
                expected
                    .layers
                    .iter()
                    .map(|l| l.id.as_str())
                    .collect::<Vec<_>>()
            );
            let order: Vec<_> = plan
                .preorder()
                .iter()
                .map(|i| plan.layer(*i).unwrap().id())
                .collect();
            assert_eq!(
                order,
                vec![
                    fixture.legacy.layers[2].id.as_str(),
                    fixture.legacy.layers[0].id.as_str(),
                    fixture.legacy.layers[1].id.as_str(),
                    fixture.legacy.layers[3].id.as_str()
                ]
            );
            let actual = plan.check_cut(&mut Work::new(100_000).unwrap(), &|| Ok(()));
            let expected = crate::layer_clipboard::cut_commands(&fixture.legacy, &ids);
            assert_eq!(actual.is_ok(), expected.is_ok());
            if let (Err(a), Err(b)) = (actual, expected) {
                assert_eq!(a, b);
            }
            let split = LayerCopyPlan::capture(
                &fixture.document,
                &[fixture.legacy.layers[0].id.clone()],
                &mut Work::new(100_000).unwrap(),
                &|| Ok(()),
            );
            assert!(split.is_err());
            assert!(crate::layer_clipboard::copy(
                &fixture.legacy,
                &[fixture.legacy.layers[0].id.clone()]
            )
            .is_err());
            drop(plan);
            fixture.finish();
        }
    }
}

#[test]
fn layer_paste_placement_matches_actual_legacy_empty_folder_child_and_clip_unit() {
    for depth in [8, 16] {
        let source = Fixture::new(tree_doc(depth));
        let ids = vec![source.legacy.layers[2].id.clone()];
        let snapshot = crate::layer_clipboard::copy(&source.legacy, &ids).unwrap();
        let plan = LayerCopyPlan::capture(
            &source.document,
            &ids,
            &mut Work::new(100_000).unwrap(),
            &|| Ok(()),
        )
        .unwrap();
        for mode in 0..5 {
            let mut destination = tree_doc(depth);
            if mode == 0 {
                destination.layers.clear();
            }
            if mode == 4 {
                destination.layers[0].parent = None;
                destination.layers[1].parent = None;
            }
            let target = match mode {
                0 => "".into(),
                1 => destination.layers[2].id.clone(),
                2 => destination.layers[0].id.clone(),
                3 => "".into(),
                _ => destination.layers[1].id.clone(),
            };
            let fixture = Fixture::new(destination);
            let policy = PastePlacement::new(
                &fixture.document,
                &plan,
                &target,
                &mut Work::new(100_000).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            let mut expected = fixture.legacy.clone();
            let created = crate::layer_clipboard::paste(&mut expected, &snapshot, &target).unwrap();
            let first = expected
                .layers
                .iter()
                .position(|l| l.id == created[0])
                .unwrap();
            assert_eq!(policy.destination(), fixture.document.key());
            assert_eq!(policy.insertion(), first);
            let parent = expected.layers[first].parent.as_deref();
            if let Some(base) = policy.wrap_base() {
                let group = parent.unwrap();
                let base = fixture.document.source(base).unwrap().key();
                assert!(policy.wrap_members().iter().any(|slot| fixture
                    .document
                    .source(*slot)
                    .unwrap()
                    .key()
                    == base));
                for slot in policy.wrap_members() {
                    let old = fixture
                        .document
                        .metadata()
                        .layers()
                        .find(|l| l.pixels().slot() == *slot)
                        .unwrap();
                    assert_eq!(
                        expected
                            .layers
                            .iter()
                            .find(|l| l.id == old.id())
                            .unwrap()
                            .parent
                            .as_deref(),
                        Some(group)
                    );
                }
            } else {
                let actual = policy.parent().map(|slot| {
                    fixture
                        .document
                        .metadata()
                        .layers()
                        .find(|l| l.pixels().slot() == slot)
                        .unwrap()
                        .id()
                });
                assert_eq!(actual, parent);
            }
            drop(policy);
            fixture.finish();
        }
        drop(plan);
        source.finish();
    }
}
