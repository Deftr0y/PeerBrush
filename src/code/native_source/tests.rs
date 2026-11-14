use super::*;
use crate::code::evaluation::{Pixels, ResidentPixels};
use crate::{
    engine::{
        authority::DocumentAuthority,
        native::worker::tests::Fixture,
        scene::{CapturedScene, LegacyTileCache},
    },
    workspace::{self, source_runtime::SourceRuntime},
};

pub(crate) fn stage(runtime: &Arc<SourceRuntime>, doc: &Document) -> NativeDocument {
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let staged = CapturedScene::from_parts(doc.clone(), "code-test".into(), vec![], None, None)
        .stage(runtime, &cache, &|| Ok(()))
        .unwrap();
    NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap()
}
pub(crate) fn project(
    runtime: &Arc<SourceRuntime>,
    depth: u16,
) -> (Shared, workspace::Registry, Document, Job) {
    let mut doc = Document::new_depth(270, 260, depth).unwrap();
    doc.layers[0].x = 7;
    doc.layers[0].y = -3;
    for (x, y, pixel) in [
        (1, 5, [31337, 14259, 52877, 0]),
        (255, 255, [12347, 23459, 34571, 61379]),
        (256, 256, [64001, 53003, 42007, 31013]),
    ] {
        doc.layers[0].pixels.set16(x, y, pixel);
    }
    let mut second = doc.layers[0].clone();
    second.id = engine::id();
    second.x = 13;
    second.y = 4;
    second.pixels.set16(3, 4, [51001, 41003, 31007, 21013]);
    doc.layers.push(second);
    let document = stage(runtime, &doc);
    let shared = Arc::new(Mutex::new(Engine::from_native(document, 8).unwrap()));
    let workspace = workspace::attach(&shared);
    workspace.lock().unwrap().source_runtime = Some(runtime.clone());
    let scopes: Vec<_> = doc
        .layers
        .iter()
        .map(|layer| Scope::layer(&layer.id))
        .collect();
    let lease = shared
        .lock()
        .unwrap()
        .reserve("agent", "Native code read", scopes.clone())
        .unwrap();
    let job = Job::new(Info {
        id: engine::id(),
        actor: "agent".into(),
        document: doc.id.clone(),
        task: lease.id,
        scopes,
        label: "Native code read".into(),
        native: None,
    });
    (shared, workspace, doc, job)
}
pub(crate) fn observed(shared: &Shared) -> NativeDocument {
    shared.lock().unwrap().native_document().unwrap().clone()
}
fn oracle(doc: Document) -> ResidentPixels {
    ResidentPixels::new(doc, vec![])
}

#[test]
fn native_code_reads_original_words_hidden_rgb_offsets_layers_and_tile_boundaries() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
        let (shared, workspace, original, job) = project(&runtime, depth);
        let document = observed(&shared);
        let key = document.key();
        let metadata = json!(document.metadata());
        let oracle = oracle(original.clone());
        let check = || {
            assert!(shared.try_lock().is_ok());
            assert!(workspace.try_lock().is_ok());
            Ok(())
        };
        let mut source = NativeSource::capture(&shared, &job, &document, &check).unwrap();
        for layer in &original.layers {
            for (x, y) in [
                (0, 0),
                (8, 2),
                (16, 8),
                (20, 20),
                (262, 252),
                (263, 253),
                (269, 259),
            ] {
                assert_eq!(
                    source
                        .pixel(&shared, &job, &layer.id, x, y, &check)
                        .unwrap(),
                    oracle.pixel(&layer.id, x, y).unwrap(),
                    "depth{depth} point{x},{y}"
                );
            }
        }
        let layer = &original.layers[0].id;
        if depth == 16 {
            assert_eq!(
                source.pixel(&shared, &job, layer, 8, 2, &check).unwrap(),
                [31337, 14259, 52877, 0]
            );
        }
        source
            .pixel(&shared, &job, layer, 262, 252, &check)
            .unwrap();
        let bytes = source.tile.as_ref().unwrap().bytes().as_ptr();
        let used = runtime.resident().used();
        assert_eq!(
            source
                .pixel(&shared, &job, layer, 262, 252, &check)
                .unwrap(),
            oracle.pixel(layer, 262, 252).unwrap()
        );
        assert_eq!(source.tile.as_ref().unwrap().bytes().as_ptr(), bytes);
        assert_eq!(runtime.resident().used(), used);
        for (x, y) in [(-1, 0), (270, 1), (0, 260), (INT::MAX, 0), (0, INT::MIN)] {
            assert!(source.pixel(&shared, &job, layer, x, y, &check).is_err());
        }
        assert!(source
            .pixel(&shared, &job, "missing", 1, 1, &check)
            .is_err());
        assert_eq!(observed(&shared).key(), key);
        assert_eq!(json!(observed(&shared).metadata()), metadata);
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    }
}

#[test]
fn cached_native_code_pixels_refuse_cancellation_takeover_replacement_and_changed_service() {
    for failure in ["cancel", "takeover", "source", "service", "callback"] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
        let (shared, workspace, original, job) = project(&runtime, 16);
        let document = observed(&shared);
        let mut source = NativeSource::capture(&shared, &job, &document, &|| Ok(())).unwrap();
        let layer = &original.layers[0].id;
        let expected = source
            .pixel(&shared, &job, layer, 262, 252, &|| Ok(()))
            .unwrap();
        assert_eq!(expected, [12347, 23459, 34571, 61379]);
        match failure {
            "cancel" => job.cancel.store(true, Ordering::Relaxed),
            "takeover" => shared.lock().unwrap().take_over_tasks(),
            "source" => {
                let other = stage(&runtime, &original);
                assert_eq!(other.metadata().id(), document.metadata().id());
                assert_eq!(other.metadata().revision(), document.metadata().revision());
                assert_ne!(other.key(), document.key());
                shared.lock().unwrap().authority =
                    DocumentAuthority::from_native(other, 8).unwrap();
            }
            "service" => workspace.lock().unwrap().source_runtime = None,
            "callback" => (),
            _ => unreachable!(),
        }
        let check = || {
            if failure == "callback" {
                Err("Code evaluation ended".into())
            } else {
                Ok(())
            }
        };
        assert!(
            source
                .pixel(&shared, &job, layer, 262, 252, &check)
                .is_err(),
            "{failure}"
        );
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        workspace.lock().unwrap().source_runtime = Some(runtime.clone());
    }
}

#[test]
fn native_code_admission_failure_never_becomes_transparent_pixels() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
    let (shared, _workspace, original, job) = project(&runtime, 16);
    let document = observed(&shared);
    let mut source = NativeSource::capture(&shared, &job, &document, &|| Ok(())).unwrap();
    let pool = runtime.resident();
    let available = pool.limit() - pool.used();
    let held = pool.reserve_bytes(available, available).unwrap();
    assert!(source
        .pixel(&shared, &job, &original.layers[0].id, 262, 252, &|| Ok(()))
        .is_err());
    assert!(source.tile.is_none());
    drop(held);
    assert_eq!(
        source
            .pixel(&shared, &job, &original.layers[0].id, 262, 252, &|| Ok(()))
            .unwrap(),
        [12347, 23459, 34571, 61379]
    );
    assert_eq!(observed(&shared).key(), document.key());
    assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
}

#[test]
fn native_code_patch_words_are_admitted_scoped_and_outlive_only_the_read_owner() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
        let (shared, _workspace, original, mut job) = project(&runtime, depth);
        let document = observed(&shared);
        let mut source = NativeSource::capture(&shared, &job, &document, &|| Ok(())).unwrap();
        let layer = &original.layers[0].id;
        let rect = [0, 0, 270, 260];
        let oracle = oracle(original.clone());
        let expected: Vec<_> = (rect[1]..rect[3])
            .flat_map(|y| {
                (rect[0]..rect[2]).flat_map({
                    let oracle = &oracle;
                    move |x| oracle.pixel(layer, INT::from(x), INT::from(y)).unwrap()
                })
            })
            .collect();
        let words = source
            .patch(&shared, &job, layer, rect, &|| Ok(()))
            .unwrap();
        assert_eq!(&**words, &expected);
        assert!(words.owner().uses_budget(&runtime.resident()));
        job.info_mut().scopes = vec![Scope {
            target: Some(layer.clone()),
            rect: Some([8, 2, 9, 3]),
        }];
        assert!(source
            .patch(&shared, &job, layer, rect, &|| Ok(()))
            .is_err());
        let one = source
            .patch(&shared, &job, layer, [8, 2, 9, 3], &|| Ok(()))
            .unwrap();
        assert_eq!(&**one, &oracle.pixel(layer, 8, 2).unwrap());
        for bad in [[0, 0, 0, 1], [-1, 0, 1, 1], [0, 0, 271, 260]] {
            assert!(source.patch(&shared, &job, layer, bad, &|| Ok(())).is_err());
        }
        job.info_mut().scopes = original
            .layers
            .iter()
            .map(|layer| Scope::layer(&layer.id))
            .collect();
        let callbacks = std::cell::Cell::new(0);
        let cancel = || {
            callbacks.set(callbacks.get() + 1);
            if callbacks.get() > 20 {
                job.cancel.store(true, Ordering::Relaxed);
            }
            Ok(())
        };
        assert!(source.patch(&shared, &job, layer, rect, &cancel).is_err());
        assert!(job.cancel.load(Ordering::Relaxed));
        drop(source);
        assert_eq!(&**words, &expected);
        assert_eq!(observed(&shared).key(), document.key());
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
    }
}
