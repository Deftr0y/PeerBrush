//! Chronological history uses the same cancellation/publication fence as edits.
use super::*;
use crate::engine::native::Direction;

pub(crate) fn history_shared(
    shared: &Shared,
    actor: &str,
    direction: Direction,
    expected: Option<u64>,
    document: Option<&str>,
) -> Result<EditReply, String> {
    history_shared_controlled(
        shared,
        actor,
        direction,
        expected,
        document,
        &EditControl::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Shared, String) {
        let mut engine = Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = crate::engine::Document::new_depth(4, 4, 16).unwrap();
        engine.resident_state_mut().expect("resident fixture").document.layers[0]
            .pixels
            .set16(1, 2, [12347, 33559, 51237, 65535]);
        let document = engine.resident_state().expect("resident fixture").document.id.clone();
        let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
        engine
            .edit(
                "worker",
                &[json!({"op":"layer.update", "layer":layer, "name":"Edited"})],
                Some(0),
                None,
                "Rename",
            )
            .unwrap();
        (Arc::new(Mutex::new(engine)), document)
    }
    #[test]
    fn shared_history_restores_native_words_with_monotonic_revisions_and_one_cursor_move() {
        let (shared, document) = fixture();
        let words = shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].pixels.rgba16();
        for (direction, revision, lengths) in
            [(Direction::Undo, 2, [0, 1]), (Direction::Redo, 3, [1, 0])]
        {
            let reply = history_shared(
                &shared,
                "worker",
                direction,
                Some(revision - 1),
                Some(&document),
            )
            .unwrap();
            assert_eq!(reply.data()["revision"], revision);
            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), lengths);
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), words);
            assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, 16);
            assert_eq!(engine.ai_change.as_ref().unwrap().tool, "history");
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
        }
    }
    #[test]
    fn refused_history_keeps_document_and_both_stacks_unchanged() {
        for reason in [
            "revision",
            "document",
            "actor",
            "missing_revision",
            "reservation",
            "cancel",
            "overflow",
        ] {
            for direction in [Direction::Undo, Direction::Redo] {
                let (shared, document) = fixture();
                let control = EditControl::default();
                {
                    let mut engine = shared.lock().unwrap();
                    if direction == Direction::Redo {
                        engine.undo("human").unwrap();
                    }
                    if reason == "reservation" {
                        engine
                            .reserve(
                                "other",
                                "Hold document",
                                vec![Scope {
                                    target: None,
                                    rect: None,
                                }],
                            )
                            .unwrap();
                    }
                    if reason == "overflow" {
                        engine.resident_state_mut().expect("resident fixture").document.revision = u64::MAX;
                    }
                }
                if reason == "cancel" {
                    assert!(control.cancel());
                }
                let (revision, lengths, words, name) = {
                    let engine = shared.lock().unwrap();
                    (
                        engine.resident_state().expect("resident fixture").document.revision,
                        engine.history_lengths(),
                        engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        engine.resident_state().expect("resident fixture").document.layers[0].name.clone(),
                    )
                };
                assert!(
                    history_shared_controlled(
                        &shared,
                        if reason == "actor" { "other" } else { "worker" },
                        direction,
                        if reason == "missing_revision" {
                            None
                        } else {
                            Some(if reason == "revision" { 0 } else { revision })
                        },
                        Some(if reason == "document" {
                            "replaced-document"
                        } else {
                            &document
                        }),
                        &control,
                    )
                    .is_err(),
                    "{reason} / {direction:?}"
                );
                let engine = shared.lock().unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.revision, revision);
                assert_eq!(engine.history_lengths(), lengths);
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(), words);
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].name, name);
                assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
                assert!(control.is_finished());
            }
        }
    }
}

pub(crate) fn history_shared_controlled(
    shared: &Shared,
    actor: &str,
    direction: Direction,
    expected: Option<u64>,
    document: Option<&str>,
    control: &EditControl,
) -> Result<EditReply, String> {
    let _run = control.start()?;
    let _operation = FileOperation::begin(shared)?;
    if actor != "human" && expected.is_none() {
        return Err(
            "AI history changes require expected_revision from the latest observation".into(),
        );
    }
    let (seed, history, captured) = {
        let mut engine = shared.lock().unwrap();
        let view = crate::engine::view::current(&engine);
        if document.is_some_and(|id| id != view.id())
            || expected.is_some_and(|revision| revision != view.revision())
        {
            return Err("Document changed before undo/redo. Observe again.".into());
        }
        let Some(seed) = engine.native_seed()? else {
            control.begin_commit()?;
            match direction {
                Direction::Undo => engine.undo(actor)?,
                Direction::Redo => engine.redo(actor)?,
            }
            return Ok(EditReply::resident(
                json!({"revision":crate::engine::view::current(&engine).revision()}),
            ));
        };
        let history = engine.capture_native_history(&seed)?;
        (seed, history, engine.capture_scene(None)?)
    };
    let scene = captured.into_native()?;
    // Retain authenticated source ownership through the final publication fence,
    // even though history only restores existing owners and writes no new tiles.
    let session = super::command_source_session(shared, &scene, || control.check())?;
    drop(scene);
    let active = || {
        control.check()?;
        session.check()
    };
    let prepared = history.prepare_move(seed.document(), direction, actor, &active)?;
    drop(history);
    let reply = EditReply::history(prepared.runtime_view().candidate)?;
    let effects = crate::engine::runtime_value::PreparedEngineEffects::prepare_move(
        &seed, &prepared, &active,
    )?;
    active()?;
    let mut prepared = Some(prepared);
    let mut effects = Some(effects);
    let result = {
        let mut engine = shared.lock().unwrap();
        session.with_live_fence(|| {
            control.begin_commit()?;
            Ok::<_, String>(engine.move_native_with_effects(
                &seed,
                actor,
                prepared.take().unwrap(),
                effects.take().unwrap(),
            ))
        })
    };
    // All candidate, source, response and activity owners retire off-lock on
    // both success and refusal. A cursor move has no provisional file cleanup.
    let result = result??;
    match result {
        Ok(retired) => {
            drop(retired);
            Ok(reply)
        }
        Err(refused) => Err(refused.reason().to_owned()),
    }
}
