use super::*;
use crate::{
    engine::{Document, Engine},
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
        let legacy = planning::tests::fixture(depth, kind, linked);
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
