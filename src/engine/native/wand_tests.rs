use super::*;
use crate::{
    engine::native::worker::tests::Fixture, raster::Raster,
    workspace::source_runtime::SourceSession,
};
use serde_json::json;

fn capture(base: &NativeDocument, commands: &[Value]) -> CapturedEdit {
    base.capture_descriptor(
        CommandDescriptor::from_normalized(base, base.metadata().revision(), commands, &|| Ok(()))
            .unwrap(),
        "human",
        &[],
        0,
        false,
        true,
    )
    .unwrap()
}
fn gate<'a>(leases: &'a [crate::engine::Lease]) -> Publication<'a> {
    Publication {
        actor: "human",
        leases,
        now: 0,
        closed: false,
        write_available: true,
    }
}
fn commit(
    base: &mut NativeDocument,
    history: &mut NativeHistory,
    delta: PreparedDelta,
    session: &SourceSession,
) {
    let prepared = history
        .snapshot()
        .prepare_commit(
            base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Wand owner",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    history
        .commit(base, gate(&[]), prepared)
        .unwrap()
        .retire(session, &|| session.cleanup_check())
        .unwrap();
}
fn selection(edit: CapturedEdit, session: &SourceSession, x: Option<i32>) -> PreparedDelta {
    let base = edit.document();
    let previous = base.metadata().selection_coverage().map(|c| CoveragePatch {
        pixels: CoveragePixels::Existing(c.mask().slot()),
        bounds: c.bounds(),
        origin: c.origin(),
        contours: copy_admitted(&c.contours(), base.budget(), true, &|| Ok(())).unwrap(),
    });
    let mut raster = Raster::new(4, 2);
    if let Some(x) = x {
        raster.set(x, 0, [137; 4]);
    }
    let output = crate::retained::transaction::stage(base, session, &|| Ok(()), |store, check| {
        Surface::stage_raster(&raster, store.clone(), &mut || check()).map_err(|e| e.to_string())
    })
    .unwrap();
    let bounds = x.map_or([0; 4], |x| [x, 0, x + 1, 1]);
    let contours = copy_admitted(
        &Vec::<Vec<[f32; 2]>>::new(),
        base.budget(),
        true,
        &|| Ok(()),
    )
    .unwrap();
    edit.batch(
        RasterBatch {
            rasters: vec![],
            layers: vec![],
            effects: vec![],
            selection: Some(SelectionPatch {
                bounds: Some(bounds),
                polygon: None,
                current: Some(CoveragePatch {
                    pixels: CoveragePixels::Output(output),
                    bounds,
                    origin: [0, 0],
                    contours,
                }),
                previous,
            }),
        },
        &|| Ok(()),
    )
    .unwrap_or_else(|refused| panic!("{}", refused.reason()))
}

#[test]
fn replay_owns_baseline_only_fragments_and_survives_batch_restamping_and_history() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let mut base =
        NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let commands = [
        json!({"op":"selection","rect":[0,0,1,1]}),
        json!({"op":"selection","rect":[2,0,3,1]}),
        json!({"op":"selection","kind":"wand","point":[1,0],"mode":"add","tolerance":24,"ignored":"not retained"}),
    ];
    let mut transaction = capture(&base, &commands).transaction().unwrap();
    for x in [Some(0), Some(2)] {
        let step = transaction.next(&|| Ok(())).unwrap();
        transaction
            .complete(selection(step, &session, x), &|| Ok(()))
            .unwrap();
    }
    let step = transaction.next(&|| Ok(())).unwrap();
    let replay = Replay::prepare(&step, &|| Ok(())).unwrap();
    let baseline_key = replay.baseline().key();
    let id = replay.id().to_owned();
    assert!(replay.command().get("ignored").is_none());
    let baseline_coverage = replay.baseline().metadata().selection_previous().unwrap();
    let baseline_source = replay
        .baseline()
        .source(baseline_coverage.mask().slot())
        .unwrap()
        .key();
    let delta = selection(step, &session, None)
        .with_wand(replay, &|| Ok(()))
        .unwrap();
    assert!(!delta
        .preview()
        .roster()
        .iter()
        .any(|(_, s)| s.key() == baseline_source));
    assert!(delta
        .preview()
        .owned_sources()
        .any(|s| s.key() == baseline_source));
    let detached = restamp(
        delta.preview(),
        delta.preview().metadata().revision(),
        None,
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(
        json!(detached.metadata()),
        json!(delta.preview().metadata())
    );
    assert_eq!(
        detached.render_key().unwrap(),
        delta.preview().render_key().unwrap()
    );
    drop(detached);
    transaction.complete(delta, &|| Ok(())).unwrap();
    let delta = transaction.finish(&|| Ok(())).unwrap();
    let mut history = NativeHistory::new(&runtime.resident(), 1).unwrap();
    commit(&mut base, &mut history, delta, &session);
    let wand = base.wand().unwrap();
    assert_eq!(wand.id(), id);
    assert_eq!(wand.baseline().key(), baseline_key);
    let old = wand.baseline().metadata().selection_previous().unwrap();
    let source = wand.baseline().source(old.mask().slot()).unwrap();
    assert_eq!(source.key(), baseline_source);
    assert!(source
        .physical_tiles()
        .all(|tile| tile.owner().publication_marked()));
    assert_eq!(
        source
            .prepare_region(
                crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap(),
                &mut || Ok(())
            )
            .unwrap()
            .pixel(0, 0)
            .unwrap(),
        [137; 4]
    );
    let fresh = Replay::prepare(
        &capture(
            &base,
            &[json!({"op":"selection","kind":"wand","point":[2,0]})],
        ),
        &|| Ok(()),
    )
    .unwrap();
    assert_ne!(fresh.id(), id);
    assert!(fresh.baseline().wand().is_none());
    assert_eq!(json!(fresh.baseline().metadata()), json!(base.metadata()));
    drop(fresh);
    let update =
        json!({"op":"selection.wand_update","tolerance":70,"feather":2,"sample_merged":true});
    let edit = capture(&base, &[update]);
    let replay = Replay::prepare(&edit, &|| Ok(())).unwrap();
    assert_eq!(replay.id(), id);
    assert_eq!(replay.baseline().key(), baseline_key);
    assert_eq!(replay.command()["tolerance"], 70.);
    assert_eq!(replay.command()["point"], json!([1, 0]));
    let token = edit
        .descriptor()
        .unwrap()
        .commands()
        .next()
        .unwrap()
        .token();
    let delta = edit
        .unchanged(token, &|| Ok(()))
        .unwrap()
        .with_wand(replay, &|| Ok(()))
        .unwrap();
    commit(&mut base, &mut history, delta, &session);
    for (direction, tolerance) in [(Direction::Undo, 24.), (Direction::Redo, 70.)] {
        let moved = history
            .snapshot()
            .prepare_move(&base, direction, "human", &|| Ok(()))
            .unwrap();
        drop(history.move_cursor(&mut base, gate(&[]), moved).unwrap());
        assert_eq!(base.wand().unwrap().command()["tolerance"], tolerance);
        assert_eq!(base.wand().unwrap().id(), id);
        assert_eq!(base.wand().unwrap().baseline().key(), baseline_key);
    }
    // Even an ordinary command with unchanged pixels invalidates replay, as in Engine.apply.
    let edit = capture(&base, &[json!({"op":"selection.refine"})]);
    let token = edit
        .descriptor()
        .unwrap()
        .commands()
        .next()
        .unwrap()
        .token();
    let delta = edit.unchanged(token, &|| Ok(())).unwrap();
    assert!(delta.preview().wand().is_none());
    commit(&mut base, &mut history, delta, &session);
    assert!(Replay::prepare(
        &capture(&base, &[json!({"op":"selection.wand_update"})]),
        &|| Ok(())
    )
    .is_err());
    let moved = history
        .snapshot()
        .prepare_move(&base, Direction::Undo, "human", &|| Ok(()))
        .unwrap();
    drop(history.move_cursor(&mut base, gate(&[]), moved).unwrap());
    assert_eq!(base.wand().unwrap().id(), id);
    drop((history, base, session, runtime, connection));
}

#[test]
fn replay_rejects_foreign_tokens_invalid_updates_and_cancellation_without_source_loss() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let base =
        NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let command = json!({"op":"selection","kind":"wand"});
    let first = capture(&base, &[command.clone()]);
    let replay = Replay::prepare(&first, &|| Ok(())).unwrap();
    let second = capture(&base, &[command.clone()]);
    let token = second
        .descriptor()
        .unwrap()
        .commands()
        .next()
        .unwrap()
        .token();
    let delta = second.unchanged(token, &|| Ok(())).unwrap();
    let refused = delta.with_wand(replay, &|| Ok(())).err().unwrap();
    assert!(refused.reason().contains("another captured command"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    let replay = Replay::prepare(&first, &|| Ok(())).unwrap();
    let token = first
        .descriptor()
        .unwrap()
        .commands()
        .next()
        .unwrap()
        .token();
    let delta = first
        .unchanged(token, &|| Ok(()))
        .unwrap()
        .with_wand(replay, &|| Ok(()))
        .unwrap();
    for invalid in [
        json!({"tolerance":256}),
        json!({"feather":-1}),
        json!({"contiguous":"true"}),
        json!({"color_similarity":1}),
    ] {
        let mut command = invalid;
        command["op"] = json!("selection.wand_update");
        assert!(Replay::prepare(&capture(delta.preview(), &[command]), &|| Ok(())).is_err());
        assert_eq!(delta.preview().wand().unwrap().command()["tolerance"], 24.);
    }
    delta.abort(&session, &|| session.cleanup_check()).unwrap();
    let edit = capture(&base, &[command]);
    let replay = Replay::prepare(&edit, &|| Ok(())).unwrap();
    let cancellation = edit.cancellation();
    let token = edit
        .descriptor()
        .unwrap()
        .commands()
        .next()
        .unwrap()
        .token();
    let delta = edit.unchanged(token, &|| Ok(())).unwrap();
    cancellation.cancel();
    let refused = delta.with_wand(replay, &|| Ok(())).err().unwrap();
    assert!(refused.reason().contains("cancelled"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert!(base.wand().is_none());
    assert_eq!(base.metadata().revision(), 0);
    assert_eq!(runtime.disk().used().unwrap(), 0);
    drop((base, session, runtime, connection));
}
