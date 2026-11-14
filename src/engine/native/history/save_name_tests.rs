use super::*;
use crate::large_document::{DiskBudget, ResidentBudget};

fn document(pool: &ResidentBudget, depth: u16) -> NativeDocument {
    NativeDocument::new_blank(
        4,
        2,
        depth,
        pool,
        &DiskBudget::new(4 * 1024 * 1024, 8).unwrap(),
        &|| Ok(()),
    )
    .unwrap()
}
fn gate() -> Publication<'static> {
    Publication {
        actor: "human",
        leases: &[],
        now: 1,
        closed: false,
        write_available: true,
    }
}
fn visibility(current: &mut NativeDocument, history: &mut NativeHistory, visible: bool) {
    let layer = current.metadata().layer(0).unwrap().id().to_owned();
    let descriptor = CommandDescriptor::from_normalized(
        current,
        current.metadata().revision(),
        &[serde_json::json!({"op":"layer.update","layer":layer,"visible":visible})],
        &|| Ok(()),
    )
    .unwrap();
    let delta = current
        .capture_descriptor(descriptor, "human", &[], 1, false, true)
        .unwrap()
        .visibility(&layer, visible, &|| Ok(()))
        .unwrap();
    let prepared = history
        .snapshot()
        .prepare_commit(
            current,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Visibility",
                task: None,
                gesture: Some("same gesture"),
            },
            &|| Ok(()),
        )
        .unwrap();
    drop(history.commit(current, gate(), prepared).unwrap());
}

#[test]
fn saved_names_preserve_frozen_sources_and_both_history_stacks_without_rewriting_records() {
    for depth in [8, 16] {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut current = document(&pool, depth);
        let mut history = NativeHistory::new(&pool, 8).unwrap();
        visibility(&mut current, &mut history, false);
        let frozen = current.clone();
        let record = history.undo[0].clone();
        let source = current
            .source(current.metadata().layer(0).unwrap().pixels().slot())
            .unwrap()
            .key();
        let stale_move = history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        let prepared = history
            .snapshot()
            .prepare_save_name(&current, "Saved artwork.psd", &|| Ok(()))
            .unwrap();
        drop(history.commit_save_name(&mut current, prepared).unwrap());
        assert_eq!(current.metadata().name(), "Saved artwork.psd");
        assert_eq!(current.metadata().revision(), 1);
        assert_eq!((history.undo_len(), history.redo_len()), (1, 0));
        assert!(Arc::ptr_eq(&record, &history.undo[0]));
        assert!(Arc::ptr_eq(&current.core.roster, &frozen.core.roster));
        assert_eq!(frozen.metadata().name(), "Untitled.psd");
        assert!(history
            .move_cursor(&mut current, gate(), stale_move)
            .is_err());
        for (direction, revision, visible) in
            [(Direction::Undo, 2, true), (Direction::Redo, 3, false)]
        {
            let prepared = history
                .snapshot()
                .prepare_move(&current, direction, "human", &|| Ok(()))
                .unwrap();
            drop(history.move_cursor(&mut current, gate(), prepared).unwrap());
            assert_eq!(current.metadata().name(), "Saved artwork.psd");
            assert_eq!(current.metadata().revision(), revision);
            assert_eq!(current.metadata().layer(0).unwrap().visible(), visible);
            assert_eq!(current.metadata().depth(), depth);
            assert_eq!(
                current
                    .source(current.metadata().layer(0).unwrap().pixels().slot())
                    .unwrap()
                    .key(),
                source
            );
        }
        visibility(&mut current, &mut history, true);
        let before = history.undo_len();
        let prepared = history
            .snapshot()
            .prepare_save_name(&current, "Saved artwork.psd", &|| Ok(()))
            .unwrap();
        drop(history.commit_save_name(&mut current, prepared).unwrap());
        visibility(&mut current, &mut history, false);
        assert_eq!(
            history.undo_len(),
            before + 1,
            "A save must separate later gesture history"
        );
        drop((record, frozen, current, history));
        assert_eq!(pool.used(), 0);
    }
}

#[test]
fn saved_name_refusal_preserves_the_exact_owner_and_preparation_refuses_without_leaking_admission()
{
    use std::cell::Cell;
    for depth in [8, 16] {
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let mut current = document(&pool, depth);
        let mut history = NativeHistory::new(&pool, 8).unwrap();
        let snapshot = history.snapshot();
        let prepared = snapshot
            .prepare_save_name(&current, "Retained.psb", &|| Ok(()))
            .unwrap();
        let candidate = prepared.candidate.key();
        let mut other = NativeHistory::new(&pool, 8).unwrap();
        let refused = other.commit_save_name(&mut current, prepared).unwrap_err();
        let prepared = refused.into_prepared();
        assert_eq!(prepared.candidate.key(), candidate);
        drop(other);
        let key = current.key();
        let baseline = pool.used();
        let count = Cell::new(0);
        drop(
            snapshot
                .prepare_save_name(&current, "Another.psd", &|| {
                    count.set(count.get() + 1);
                    Ok(())
                })
                .unwrap(),
        );
        assert_eq!(pool.used(), baseline);
        for fail in [1, 2, count.get() / 2, count.get()] {
            let calls = Cell::new(0);
            assert!(snapshot
                .prepare_save_name(&current, "Another.psd", &|| {
                    calls.set(calls.get() + 1);
                    if calls.get() == fail {
                        Err("cancel saved name".into())
                    } else {
                        Ok(())
                    }
                })
                .is_err());
            assert_eq!(pool.used(), baseline);
            assert_eq!(current.key(), key);
        }
        let pressure = pool
            .reserve_bytes(pool.limit() - baseline, pool.limit())
            .unwrap();
        assert!(snapshot
            .prepare_save_name(&current, "Refused.psd", &|| Ok(()))
            .is_err());
        drop(pressure);
        for name in ["", "invalid\0name"] {
            assert!(snapshot
                .prepare_save_name(&current, name, &|| Ok(()))
                .is_err());
        }
        assert_eq!(pool.used(), baseline);
        drop(history.commit_save_name(&mut current, prepared).unwrap());
        assert_eq!(current.metadata().name(), "Retained.psb");
        assert_eq!(current.metadata().revision(), 0);
        let stale = snapshot
            .prepare_save_name(&current, "Stale.psd", &|| Ok(()))
            .unwrap();
        assert!(history.commit_save_name(&mut current, stale).is_err());
        drop((snapshot, current, history));
        assert_eq!(pool.used(), 0);
    }
}
