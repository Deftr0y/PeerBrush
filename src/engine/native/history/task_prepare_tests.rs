use super::*;
use crate::{
    engine::{scene::RasterCollector, Document, Engine, History, Lease},
    large_document::{BlockRef, DiskBudget, FreshFragment, Limits, Snapshot, Store},
    raster::{backed::Region, Raster},
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Session {
    path: std::path::PathBuf,
    pool: ResidentBudget,
    disk: DiskBudget,
    original: Option<Store>,
    counter: AtomicUsize,
}
fn limits() -> Limits {
    Limits {
        disk_bytes: 32 * 1024 * 1024,
        max_blocks: 128,
        max_resident_blocks: 8,
        manifest_bytes: 65536,
    }
}
impl Session {
    fn new() -> Self {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-task-prepare-{}", crate::engine::id()));
        std::fs::create_dir(&path).unwrap();
        let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(32 * 1024 * 1024, 32).unwrap();
        let original = Store::create_with_disk_budget(
            &path.join("original"),
            pool.clone(),
            disk.clone(),
            limits(),
        )
        .unwrap();
        Self {
            path,
            pool,
            disk,
            original: Some(original),
            counter: AtomicUsize::new(0),
        }
    }
    fn capture(&self, document: &Document) -> NativeDocument {
        struct Collector<'a> {
            session: &'a Session,
            sources: Vec<Surface>,
        }
        impl RasterCollector for Collector<'_> {
            fn resident(&mut self, raster: &Raster) -> Result<RasterRef36> {
                let surface = if raster.tiles.is_empty() && raster.samples16.is_empty() {
                    Surface::zero(raster.width, raster.height, raster.depth).unwrap()
                } else {
                    Surface::stage_current_pixels(
                        raster,
                        self.session.original.as_ref().unwrap().clone(),
                        &mut || Ok(()),
                    )
                    .unwrap()
                };
                self.sources.push(surface);
                Ok(RasterRef36 {
                    id: self.sources.len() as u32,
                    width: raster.width,
                    height: raster.height,
                    depth: raster.depth,
                    retained: None,
                })
            }
            fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
                Err("No opaque fixture conversion".into())
            }
        }
        let mut collector = Collector {
            session: self,
            sources: vec![],
        };
        let metadata = Document36::capture(document, &mut collector).unwrap();
        let metadata = copy_metadata(&metadata, &self.pool, &|| Ok(())).unwrap();
        NativeDocument::from_pending(
            PendingScene::new(
                metadata,
                RasterRoster::new(collector.sources).unwrap(),
                self.pool.clone(),
                self.disk.clone(),
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn output(&self, base: &NativeDocument, raster: &Raster) -> StagedOutput {
        let fragment = self.fresh_fragment().unwrap();
        let surface =
            Surface::stage_current_pixels(raster, fragment.store().clone(), &mut || Ok(()))
                .unwrap();
        OutputDraft::new(surface, vec![fragment])
            .seal(base, self, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason()))
    }
}
impl StageSession for Session {
    fn check(&self) -> Result<()> {
        Ok(())
    }
    fn fresh_fragment(&self) -> Result<FreshFragment> {
        FreshFragment::create(
            &self.path.join(format!(
                "fragment{}",
                self.counter.fetch_add(1, Ordering::Relaxed)
            )),
            self.pool.clone(),
            self.disk.clone(),
            limits(),
        )
        .map_err(|e| e.to_string())
    }
    fn store_for_lineage(&self, _: [u8; 16]) -> Result<Store> {
        Err("Fixture has no lineage lookup".into())
    }
    fn seal_store(&self, store: &Store, blocks: &[BlockRef], metadata: &[u8]) -> Result<Snapshot> {
        let parent = store.checkpoint_reference().map_err(|e| e.to_string())?;
        store
            .commit(
                parent.map_or(store.identity(), |p| p.source()),
                parent.map_or(1, |p| p.revision() + 1),
                parent,
                metadata,
                blocks,
                &mut || Ok(()),
            )
            .map_err(|e| e.to_string())
    }
    fn abort_fragment(
        &self,
        fragment: FreshFragment,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        fragment
            .abort(&mut || cleanup().map_err(|_| crate::large_document::Error::Cancelled))
            .map_err(|e| e.to_string())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        drop(self.original.take());
        assert_eq!(
            self.pool.used(),
            0,
            "all task/history/preview owners release before fixture cleanup"
        );
        let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = std::fs::canonicalize(&self.path).unwrap();
        assert_eq!(path.parent(), Some(parent.as_path()));
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("peerbrush-task-prepare-"));
        // Keep every published namespace through the last same-ledger creation.
        std::fs::remove_dir_all(path).unwrap();
    }
}
fn gate(actor: &str) -> Publication<'_> {
    Publication {
        actor,
        leases: &[],
        now: 1,
        closed: false,
        write_available: true,
    }
}
fn replace(
    session: &Session,
    current: &mut NativeDocument,
    history: &mut NativeHistory,
    raster: &Raster,
    actor: &str,
    task: Option<&str>,
) {
    let layer = current.metadata().layer(0).unwrap().id().to_owned();
    let descriptor=CommandDescriptor::from_normalized(current,current.metadata().revision(),&[serde_json::json!({"op":"pixels.replace","layer":layer,"rect":[0,0,raster.width,raster.height]})],&||Ok(())).unwrap();
    let captured = current
        .capture_descriptor(descriptor, actor, &[], 1, false, true)
        .unwrap();
    let output = session.output(current, raster);
    let slot = current.layer_slot(&layer).unwrap();
    let delta = captured
        .raster_patch(
            RasterPatch {
                target: slot,
                output,
                retained: RetainedUpdate::Clear,
                layer_origin: None,
            },
            &|| Ok(()),
        )
        .unwrap_or_else(|e| panic!("{}", e.reason()));
    let prepared = history
        .snapshot()
        .prepare_commit(
            current,
            delta,
            HistoryRequest {
                actor,
                label: "Pixels",
                task,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    history
        .commit(current, gate(actor), prepared)
        .unwrap()
        .retire(session, &|| Ok(()))
        .unwrap();
}
fn capture(history: &NativeHistory, current: &NativeDocument) -> TaskCapture {
    history
        .capture_task(
            current,
            TaskRequest::new(current, "human", "agent", "task", &|| Ok(())).unwrap(),
        )
        .unwrap()
}

#[test]
fn task_frame_undo_preserves_a_saved_filename_and_still_refuses_later_human_pixels() {
    for depth in [8, 16] {
        for later_pixels in [false, true] {
            let session = Session::new();
            let mut before = Document::new_depth(4, 2, depth).unwrap();
            before.layers[0]
                .pixels
                .set16(0, 0, [12347, 23459, 34571, 65535]);
            let mut current = session.capture(&before);
            let mut history = NativeHistory::new(&session.pool, 8).unwrap();
            let descriptor = CommandDescriptor::from_normalized(&current, 0,
                &[serde_json::json!({"op":"canvas.resize","width":5,"height":2,"anchor":"top_left"})], &|| Ok(())).unwrap();
            let edit = current
                .capture_descriptor(descriptor, "agent", &[], 1, false, true)
                .unwrap();
            let delta = crate::engine::native::canvas::prepare(edit, &|| Ok(())).unwrap();
            let prepared = history
                .snapshot()
                .prepare_commit(
                    &current,
                    delta,
                    HistoryRequest {
                        actor: "agent",
                        label: "Canvas",
                        task: Some("task"),
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            history
                .commit(&mut current, gate("agent"), prepared)
                .unwrap()
                .retire(&session, &|| Ok(()))
                .unwrap();
            let name = history
                .snapshot()
                .prepare_save_name(&current, "Human save.psb", &|| Ok(()))
                .unwrap();
            drop(history.commit_save_name(&mut current, name).unwrap());
            let mut human = before.layers[0].pixels.clone();
            if later_pixels {
                human.set16(2, 1, [45001, 35003, 25007, 65535]);
                replace(&session, &mut current, &mut history, &human, "human", None);
            }
            let key = current.key();
            let prepared = capture(&history, &current).prepare(&session, &|| Ok(()));
            if later_pixels {
                assert!(prepared.is_err());
                assert_eq!(current.key(), key);
                assert_eq!(current.metadata().width(), 5);
                let region = current
                    .source(current.metadata().layer(0).unwrap().pixels().slot())
                    .unwrap()
                    .prepare_region(Region::new([0, 0, 4, 2]).unwrap(), &mut || Ok(()))
                    .unwrap();
                assert_eq!(region.pixel16(2, 1).unwrap(), human.get16(2, 1));
            } else {
                let candidate = prepared.unwrap_or_else(|error| panic!("{}", error.reason));
                assert_eq!(candidate.preview().metadata().name(), "Human save.psb");
                let prepared = history
                    .snapshot()
                    .prepare_task(candidate, &|| Ok(()))
                    .unwrap();
                history
                    .commit(&mut current, gate("human"), prepared)
                    .unwrap()
                    .retire(&session, &|| Ok(()))
                    .unwrap();
                assert_eq!(current.metadata().width(), 4);
                assert_eq!(pixel(&current, 0), before.layers[0].pixels.get16(0, 0));
            }
            assert_eq!(current.metadata().name(), "Human save.psb");
            assert_eq!(current.metadata().depth(), depth);
        }
    }
}
fn pixel(current: &NativeDocument, x: i32) -> [u16; 4] {
    current
        .source(current.metadata().layer(0).unwrap().pixels().slot())
        .unwrap()
        .prepare_region(Region::new([0, 0, 4, 2]).unwrap(), &mut || Ok(()))
        .unwrap()
        .pixel16(x, 0)
        .unwrap()
}

#[test]
fn task_pixels_preserve_later_human_bytes_and_compensate_once_with_undo_redo() {
    for depth in [8, 16] {
        let session = Session::new();
        let mut before = Document::new_depth(4, 2, depth).unwrap();
        before.layers[0]
            .pixels
            .set16(0, 0, [10001, 20003, 30007, 40009]);
        let mut after = before.clone();
        after.layers[0]
            .pixels
            .set16(0, 0, [12001, 22003, 32007, 42009]);
        let mut human = after.clone();
        human.layers[0]
            .pixels
            .set16(2, 0, [17001, 27003, 37007, 47009]);
        let mut current = session.capture(&before);
        let mut history = NativeHistory::new(&session.pool, 20).unwrap();
        replace(
            &session,
            &mut current,
            &mut history,
            &after.layers[0].pixels,
            "agent",
            Some("task"),
        );
        replace(
            &session,
            &mut current,
            &mut history,
            &human.layers[0].pixels,
            "human",
            None,
        );
        let mut final_agent = human.clone();
        final_agent.layers[0]
            .pixels
            .set16(1, 0, [19001, 29003, 39007, 49009]);
        replace(
            &session,
            &mut current,
            &mut history,
            &final_agent.layers[0].pixels,
            "agent",
            Some("task"),
        );
        let mut oracle = Engine::new();
        oracle
            .resident_state_mut()
            .expect("resident fixture")
            .document = final_agent.clone();
        oracle.resident_state_mut().expect("resident fixture").undo = vec![
            History {
                before: before.clone(),
                after: after.clone(),
                actor: "agent".into(),
                label: "Pixels".into(),
                task: Some("task".into()),
                gesture: None,
                scopes: vec![Scope::layer(&before.layers[0].id)],
                reverted: vec![],
            },
            History {
                before: human.clone(),
                after: final_agent.clone(),
                actor: "agent".into(),
                label: "Pixels".into(),
                task: Some("task".into()),
                gesture: None,
                scopes: vec![Scope::layer(&before.layers[0].id)],
                reverted: vec![],
            },
        ];
        oracle.undo_task("human", "agent", "task").unwrap();
        let candidate = capture(&history, &current)
            .prepare(&session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{} {:?}", e.reason, e.cleanup_error));
        let prepared = history
            .snapshot()
            .prepare_task(candidate, &|| Ok(()))
            .unwrap();
        history
            .commit(&mut current, gate("human"), prepared)
            .unwrap()
            .retire(&session, &|| Ok(()))
            .unwrap();
        assert_eq!((current.metadata().revision(), history.undo_len()), (4, 4));
        let last = history.entries().next_back().unwrap();
        assert_eq!(last.actor(), "human");
        assert_eq!(last.label(), "Undo task");
        assert_eq!(last.revision(), 4);
        let mut reverted = last.reverted_batches().to_vec();
        reverted.sort_unstable();
        assert_eq!(reverted, vec![1, 3]);
        for y in 0..2 {
            for x in 0..4 {
                let region = current
                    .source(current.metadata().layer(0).unwrap().pixels().slot())
                    .unwrap()
                    .prepare_region(Region::new([0, 0, 4, 2]).unwrap(), &mut || Ok(()))
                    .unwrap();
                assert_eq!(
                    region.pixel16(x, y).unwrap(),
                    oracle
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .layers[0]
                        .pixels
                        .get16(x, y)
                );
            }
        }
        let request = TaskRequest::new(&current, "human", "agent", "task", &|| Ok(())).unwrap();
        assert!(history.capture_task(&current, request).is_err());
        let undo = history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(&mut current, gate("human"), undo)
                .unwrap(),
        );
        assert_eq!(pixel(&current, 0), after.layers[0].pixels.get16(0, 0));
        assert_eq!(pixel(&current, 1), final_agent.layers[0].pixels.get16(1, 0));
        assert_eq!(pixel(&current, 2), human.layers[0].pixels.get16(2, 0));
        let redo = history
            .snapshot()
            .prepare_move(&current, Direction::Redo, "human", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(&mut current, gate("human"), redo)
                .unwrap(),
        );
        assert_eq!(pixel(&current, 0), before.layers[0].pixels.get16(0, 0));
        assert_eq!(pixel(&current, 1), before.layers[0].pixels.get16(1, 0));
        assert_eq!(pixel(&current, 2), human.layers[0].pixels.get16(2, 0));
    }
}
#[test]
fn overlapping_low_bit_conflict_refuses_entire_task_without_publication_or_disk_debt() {
    let session = Session::new();
    let before = Document::new_depth(4, 2, 16).unwrap();
    let mut after = before.clone();
    after.layers[0]
        .pixels
        .set16(0, 0, [12345, 23457, 34569, 45671]);
    let mut human = after.clone();
    human.layers[0]
        .pixels
        .set16(0, 0, [12346, 23457, 34569, 45671]);
    let mut current = session.capture(&before);
    let mut history = NativeHistory::new(&session.pool, 20).unwrap();
    replace(
        &session,
        &mut current,
        &mut history,
        &after.layers[0].pixels,
        "agent",
        Some("task"),
    );
    replace(
        &session,
        &mut current,
        &mut history,
        &human.layers[0].pixels,
        "human",
        None,
    );
    let key = current.key();
    let bytes = session.disk.used().unwrap();
    let refused = capture(&history, &current)
        .prepare(&session, &|| Ok(()))
        .err()
        .unwrap();
    assert!(!refused.report.as_ref().unwrap().conflicts.is_empty());
    assert!(refused.cleanup_error.is_none());
    assert_eq!(
        (
            current.key(),
            history.undo_len(),
            session.disk.used().unwrap()
        ),
        (key, 2, bytes)
    );
    assert_eq!(pixel(&current, 0), human.layers[0].pixels.get16(0, 0));
}
#[test]
fn late_lease_cancel_and_stale_history_refuse_task_adoption_and_retire_only_provisional_sources() {
    for mode in [0, 1, 2] {
        let session = Session::new();
        let before = Document::new_depth(4, 2, 16).unwrap();
        let mut after = before.clone();
        after.layers[0]
            .pixels
            .set16(0, 0, [12345, 23457, 34569, 45671]);
        let mut current = session.capture(&before);
        let mut history = NativeHistory::new(&session.pool, 20).unwrap();
        replace(
            &session,
            &mut current,
            &mut history,
            &after.layers[0].pixels,
            "agent",
            Some("task"),
        );
        let bytes = session.disk.used().unwrap();
        let key = current.key();
        let task = capture(&history, &current);
        let cancellation = task.cancellation();
        let candidate = task
            .prepare(&session, &|| Ok(()))
            .unwrap_or_else(|e| panic!("{}", e.reason));
        assert!(session.disk.used().unwrap() > bytes);
        let prepared = history
            .snapshot()
            .prepare_task(candidate, &|| Ok(()))
            .unwrap();
        let layer = current.metadata().layer(0).unwrap().id().to_owned();
        let leases = [Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "test".into(),
            scopes: vec![Scope {
                target: Some(layer),
                rect: Some([0, 0, 1, 1]),
            }],
            expires: 100,
        }];
        if mode == 1 {
            cancellation.cancel();
        }
        if mode == 2 {
            history.epoch += 1;
        }
        let refused = history
            .commit(
                &mut current,
                Publication {
                    actor: "human",
                    leases: if mode == 0 { &leases } else { &[] },
                    now: 1,
                    closed: false,
                    write_available: true,
                },
                prepared,
            )
            .err()
            .unwrap();
        assert_eq!(current.key(), key);
        assert_eq!(history.undo_len(), 1);
        refused.abort(&session, &|| Ok(())).unwrap();
        assert_eq!(session.disk.used().unwrap(), bytes);
        assert_eq!(pixel(&current, 0), after.layers[0].pixels.get16(0, 0));
    }
}
#[test]
fn metadata_compensation_rechecks_the_separate_visibility_reservation_domain() {
    let session = Session::new();
    let before = Document::new_depth(4, 2, 16).unwrap();
    let mut current = session.capture(&before);
    let mut history = NativeHistory::new(&session.pool, 20).unwrap();
    let layer = current.metadata().layer(0).unwrap().id().to_owned();
    let descriptor = CommandDescriptor::from_normalized(
        &current,
        0,
        &[serde_json::json!({"op":"layer.update","layer":layer,"visible":false})],
        &|| Ok(()),
    )
    .unwrap();
    let delta = current
        .capture_descriptor(descriptor, "agent", &[], 1, false, true)
        .unwrap()
        .visibility(&layer, false, &|| Ok(()))
        .unwrap();
    let prepared = history
        .snapshot()
        .prepare_commit(
            &current,
            delta,
            HistoryRequest {
                actor: "agent",
                label: "Visibility",
                task: Some("task"),
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    drop(
        history
            .commit(&mut current, gate("agent"), prepared)
            .unwrap(),
    );
    let candidate = capture(&history, &current)
        .prepare(&session, &|| Ok(()))
        .unwrap_or_else(|e| panic!("{}", e.reason));
    let prepared = history
        .snapshot()
        .prepare_task(candidate, &|| Ok(()))
        .unwrap();
    let leases = [Lease {
        id: "visibility".into(),
        owner: "other".into(),
        description: "test".into(),
        scopes: vec![Scope::layer(&format!("@visibility:{layer}"))],
        expires: 100,
    }];
    let key = current.key();
    let refused = history
        .commit(
            &mut current,
            Publication {
                actor: "human",
                leases: &leases,
                now: 1,
                closed: false,
                write_available: true,
            },
            prepared,
        )
        .err()
        .unwrap();
    refused.abort(&session, &|| Ok(())).unwrap();
    assert_eq!(current.key(), key);
    assert!(!current.metadata().layer(0).unwrap().visible());
    assert_eq!(session.counter.load(Ordering::Relaxed), 0);
    assert_eq!(history.undo_len(), 1);
}
