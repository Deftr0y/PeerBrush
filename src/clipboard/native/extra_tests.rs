use super::*;
use crate::{
    engine::{Mask, MaskStep},
    large_document::ResidentBudget,
    raster::Raster,
};
use std::cell::Cell;

#[test]
fn shared_paste_never_materializes_native_clipboard_into_resident_authority() {
    let fixture = Fixture::new(doc(16, 3));
    let original = fixture.source_hash();
    let baseline = fixture.disk.used().unwrap();
    let snapshot = crate::clipboard::LayerSnapshot::Native(
        layers::LayerCopyPlan::capture(
            &fixture.document,
            &[fixture.legacy.layers[0].id.clone()],
            &mut Work::new(1_000_000).unwrap(),
            &|| Ok(()),
        )
        .unwrap(),
    );
    let mut engine = crate::engine::Engine::new();
    engine.resident_state_mut().expect("resident fixture").document = Document::new(8, 8).unwrap();
    let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
    let document = engine.resident_state().expect("resident fixture").document.id.clone();
    let target = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
    let shared = Arc::new(std::sync::Mutex::new(engine));
    let error = crate::server::paste_layers_shared(
        &shared,
        "human",
        &snapshot,
        &target,
        Some(0),
        None,
        Some(&document),
        None,
    )
    .err()
    .unwrap();
    assert!(error.contains("cannot be pasted into a resident document"));
    let engine = shared.lock().unwrap();
    assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
    assert_eq!(engine.history_lengths(), [0, 0]);
    assert_eq!(
        engine
            .file_operations
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    assert_eq!(fixture.source_hash(), original);
    drop(snapshot);
    fixture.finish();
}

#[test]
fn copy_cancel_work_and_foreign_source_or_pool_never_return_partial_pixels() {
    let fixture = Fixture::new(doc(16, 3));
    let original = fixture.source_hash();
    let baseline = fixture.disk.used().unwrap();
    let request = Request {
        target: &fixture.legacy.layers[0].id,
        mask: false,
        merged: false,
    };
    let session = fixture.session();
    assert_eq!(
        stage_copy(
            &fixture.document,
            &fixture.scene,
            request,
            &session,
            &mut Work::new(1_000_000).unwrap(),
            &|| Err("cancel copy before staging".into())
        )
        .err()
        .unwrap(),
        "cancel copy before staging"
    );
    assert_eq!(session.created.get(), 0);
    let session = TestSession {
        pool: ResidentBudget::new(1024 * 1024).unwrap(),
        disk: fixture.disk.clone(),
        parent: fixture.parent.clone(),
        created: Cell::new(0),
    };
    assert_eq!(
        stage_copy(
            &fixture.document,
            &fixture.scene,
            request,
            &session,
            &mut Work::new(1_000_000).unwrap(),
            &|| Ok(())
        )
        .err()
        .unwrap(),
        "Clipboard source session uses another admission"
    );
    assert_eq!(session.created.get(), 0);
    drop(session);
    let other = Fixture::new(doc(16, 3));
    let session = fixture.session();
    assert_eq!(
        stage_copy(
            &fixture.document,
            &other.scene,
            request,
            &session,
            &mut Work::new(1_000_000).unwrap(),
            &|| Ok(())
        )
        .err()
        .unwrap(),
        "Clipboard scene differs from the captured source generation"
    );
    assert_eq!(session.created.get(), 0);
    drop(session);
    other.finish();
    let session = fixture.session();
    assert!(stage_copy(
        &fixture.document,
        &fixture.scene,
        request,
        &session,
        &mut Work::new(1).unwrap(),
        &|| Ok(())
    )
    .is_err());
    assert_eq!(session.created.get(), 1);
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    drop(session);
    let session = fixture.session();
    let calls = Cell::new(0usize);
    let error = stage_copy(
        &fixture.document,
        &fixture.scene,
        request,
        &session,
        &mut Work::new(1_000_000).unwrap(),
        &|| {
            if session.created.get() > 0 {
                calls.set(calls.get() + 1);
                if calls.get() == 50 {
                    return Err("cancel admitted clipboard I/O".into());
                }
            }
            Ok(())
        },
    )
    .err()
    .unwrap();
    assert_eq!(error, "cancel admitted clipboard I/O");
    assert_eq!(calls.get(), 50);
    assert_eq!(session.created.get(), 1);
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    assert_eq!(fixture.source_hash(), original);
    drop(session);
    fixture.finish();
}

#[test]
fn raw_mask_copy_keeps_disabled_samples_and_applies_enabled_masks_to_color() {
    for depth in [8, 16] {
        for enabled in [false, true] {
            let mut legacy = doc(depth, 0);
            legacy.layers[0].mask = Some(Mask {
                enabled,
                linked: true,
                cache_key: crate::engine::id(),
                steps: vec![MaskStep {
                    id: "paint-mask".into(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 1.,
                    pixels: Raster::new_depth(17, 19, depth),
                    settings: serde_json::json!({}),
                }],
            });
            let fixture = Fixture::new(legacy);
            let original = fixture.source_hash();
            let baseline = fixture.disk.used().unwrap();
            for mask in [false, true] {
                let session = fixture.session();
                let request = Request {
                    target: &fixture.legacy.layers[0].id,
                    mask,
                    merged: false,
                };
                let result = stage_copy(
                    &fixture.document,
                    &fixture.scene,
                    request,
                    &session,
                    &mut Work::new(1_000_000).unwrap(),
                    &|| Ok(()),
                );
                let copy = result.unwrap();
                let expected =
                    crate::clipboard::Image::copy(&fixture.legacy, request.target, mask, false)
                        .unwrap();
                super::assert_copy(&copy, &expected, &fixture.pool);
                copy.abort(&session).unwrap();
                assert_eq!(fixture.disk.used().unwrap(), baseline);
                drop(session);
            }
            assert_eq!(fixture.source_hash(), original);
            fixture.finish();
        }
    }
}

#[test]
fn missing_named_input_is_a_real_read_error_and_never_transparent_copy() {
    let fixture = Fixture::new(doc(16, 0));
    let original = fixture.source_hash();
    let baseline = fixture.disk.used().unwrap();
    let named = fixture.parent.join("input/blocks.pbt");
    let held = fixture.parent.join("input/held-blocks.pbt");
    assert!(!held.exists());
    std::fs::rename(&named, &held).unwrap();
    let session = fixture.session();
    let request = Request {
        target: &fixture.legacy.layers[0].id,
        mask: false,
        merged: false,
    };
    let result = stage_copy(
        &fixture.document,
        &fixture.scene,
        request,
        &session,
        &mut Work::new(1_000_000).unwrap(),
        &|| Ok(()),
    );
    // Restore only the exact owned entry before examining/retiring the fixture.
    assert!(!named.exists());
    std::fs::rename(&held, &named).unwrap();
    assert!(result.is_err());
    assert_eq!(session.created.get(), 0);
    assert_eq!(fixture.disk.used().unwrap(), baseline);
    assert_eq!(fixture.source_hash(), original);
    drop(session);
    fixture.finish();
}
