use super::*;
use crate::{
    engine::{
        authority::DocumentAuthority,
        native::worker,
        scene::{CapturedScene, LegacyTileCache},
        Document,
    },
    workspace::source_runtime::SourceSession,
};

fn render(
    document: &NativeDocument,
    session: Arc<SourceSession>,
    project: &str,
    edge: u32,
) -> Frame {
    request(document, session, project, edge).render(&mut || Ok(())).unwrap()
}

fn request(
    document: &NativeDocument,
    session: Arc<SourceSession>,
    project: &str,
    edge: u32,
) -> Request {
    let scene = Arc::new(
        SceneSnapshot::from_native(
            SceneIdentity {
                project_id: project.into(),
                document_id: document.metadata().id().into(),
                revision: document.metadata().revision(),
            },
            document.clone(),
            vec![],
            None,
            None,
        )
        .unwrap(),
    );
    Request::capture(scene, session, None, edge, None, false).unwrap()
}

#[test]
fn streamed_fades_match_cached_alpha_and_retire_both_renderers_on_refusal() {
    use std::{sync::mpsc, time::Duration};
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 32 * 1024 * 1024);
        let pool = runtime.resident();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut before = Document::new_depth(513, 1025, depth).unwrap();
        for y in [0, 254, 255, 509, 510, 1024] {
            before.layers[0].pixels.set16(4, y, [61237, 12347, 23459, 49157]);
        }
        let mut after = before.clone();
        after.revision += 1;
        for y in [0, 254, 255, 509, 510, 1024] {
            after.layers[0].pixels.set16(4, y, [12347, 23459, 61237, 32771]);
        }
        let stage = |document| {
            let staged = CapturedScene::from_parts(document, "fade".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(())).unwrap();
            (NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap(), staged.seals)
        };
        let (old, _old_seals) = stage(before);
        let (current, _current_seals) = stage(after);
        let authority = DocumentAuthority::from_native(current.clone(), 4).unwrap();
        let metadata = serde_json::to_value(current.metadata()).unwrap();
        let session = Arc::new(runtime.begin().unwrap());
        let weak = Arc::downgrade(&session);
        let baseline = Baseline::capture(&render(&old, session.clone(), "fade", 1025), &old, "fade", &|| Ok(())).unwrap();
        for t in [0., 0.25, 0.5, 1.] {
            let blend = Blend::capture(&old, &current, t, &|| Ok(())).unwrap();
            let (expected, _) = blend.render(&baseline, render(&current, session.clone(), "fade", 1025), &|| Ok(())).unwrap();
            let streamed = blend.streamed(
                request(&old, session.clone(), "fade", 1025).for_canvas(1025).unwrap(),
                request(&current, session.clone(), "fade", 1025).for_canvas(1025).unwrap(), &|| Ok(()),
            ).unwrap();
            let mut actual = Vec::new();
            let mut next = 0;
            let mut stripes = 0;
            let complete = streamed.render_into(&mut || Ok(()), &mut |rows, bytes| {
                assert_eq!(rows[0], next);
                next = rows[1];
                stripes += 1;
                actual.extend_from_slice(&bytes);
                Ok(())
            }).unwrap();
            assert!(stripes > 2);
            assert_eq!(next, 1025);
            assert_eq!(actual, &*expected.copy_rgba8(&mut || Ok(())).unwrap());
            assert!(complete.tag.same_allocation(&streamed.tag()));
            assert!(streamed.origin().candidate_tag().same_allocation(&complete.tag));
            assert_ne!(complete.tag.key().source, current.key());
        }
        let blend = Blend::capture(&old, &current, 0.5, &|| Ok(())).unwrap();
        for (project, edge) in [("other", 1025), ("fade", 512)] {
            assert!(blend.streamed(request(&old, session.clone(), "fade", 1025),
                request(&current, session.clone(), project, edge), &|| Ok(())).is_err());
        }
        drop(baseline);
        let resident = pool.used();
        let disk = runtime.disk().used().unwrap();
        let streamed = blend.streamed(request(&old, session.clone(), "fade", 1025),
            request(&current, session.clone(), "fade", 1025), &|| Ok(())).unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let mut consumed = 0;
            let result = streamed.render_into(&mut || Ok(()), &mut |_, _| {
                consumed += 1;
                Err("consumer cancelled".into())
            });
            drop(streamed);
            send.send((result.err(), consumed)).unwrap();
        });
        let (error, consumed) = receive.recv_timeout(Duration::from_secs(30)).expect("Both renderers must retire after consumer refusal");
        worker.join().unwrap();
        assert!(error.unwrap().contains("consumer cancelled"));
        assert_eq!(consumed, 1);
        assert_eq!(pool.used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(serde_json::to_value(current.metadata()).unwrap(), metadata);
        assert_eq!(authority.history_lengths(), [0, 0]);
        drop(session);
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn full_size_fade_starts_without_materializing_either_endpoint() {
    use crate::engine::native::CommandDescriptor;
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(32 * 1024 * 1024, 32 * 1024 * 1024);
        let pool = runtime.resident();
        let old = NativeDocument::new_blank(16000, 16000, depth, &pool, &runtime.disk(), &|| Ok(())).unwrap();
        let command = serde_json::json!({"op":"layer.update", "layer":old.metadata().layers().next().unwrap().id(), "opacity":0.5});
        let descriptor = CommandDescriptor::from_normalized(&old, old.metadata().revision(), &[command], &|| Ok(())).unwrap();
        let token = descriptor.commands().next().unwrap().token();
        let prepared = old.capture_descriptor(descriptor, "human", &[], 1, false, true).unwrap()
            .metadata_command(token, &|| Ok(())).unwrap();
        let current = prepared.preview();
        let blend = Blend::capture(&old, current, 0.5, &|| Ok(())).unwrap();
        let session = Arc::new(runtime.begin().unwrap());
        let streamed = blend.streamed(
            request(&old, session.clone(), "fade", 16000).for_canvas(16000).unwrap(),
            request(current, session, "fade", 16000).for_canvas(16000).unwrap(), &|| Ok(()),
        ).unwrap();
        assert_eq!((streamed.key().width, streamed.key().height), (16000, 16000));
        assert!(streamed.tag().matches_edge(16000));
        let baseline = pool.used();
        let mut stripes = 0;
        let error = streamed.render_into(&mut || Ok(()), &mut |rows, bytes| {
            assert!(rows[1] - rows[0] <= 8);
            assert!(bytes.len() <= MAX_REGION_PIXELS * 4);
            assert!(pool.used() - baseline < 16 * 1024 * 1024);
            stripes += 1;
            if stripes == 3 { Err("bounded sample complete".into()) } else { Ok(()) }
        }).err().unwrap();
        assert!(error.contains("bounded sample complete"));
        assert_eq!(stripes, 3);
        assert_eq!(pool.used(), baseline);
    }
}

#[test]
fn straight_alpha_blends_have_exact_endpoints_and_do_not_darkly_overlay_two_images() {
    for (a, b, t, expected) in [
        ([255, 0, 0, 255], [0, 0, 255, 0], 0.5, [255, 0, 0, 128]),
        ([255, 0, 0, 255], [0, 0, 255, 255], 0.5, [128, 0, 128, 255]),
        ([37, 81, 19, 0], [71, 93, 11, 0], 0.5, [0, 0, 0, 0]),
        ([37, 81, 19, 0], [71, 93, 11, 0], 0., [37, 81, 19, 0]),
        ([37, 81, 19, 0], [71, 93, 11, 0], 1., [71, 93, 11, 0]),
    ] {
        let mut out = [0; 4];
        pixel(&a, &b, t, &mut out);
        assert_eq!(out, expected);
    }
}

#[test]
fn real_native_frames_fade_without_retaining_source_sessions_or_changing_history() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 32 * 1024 * 1024);
        let pool = runtime.resident();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut before = Document::new_depth(21, 17, depth).unwrap();
        before.layers[0]
            .pixels
            .set16(4, 5, [61237, 12347, 23459, 49157]);
        let mut after = before.clone();
        after.revision += 1;
        after.layers[0]
            .pixels
            .set16(4, 5, [12347, 23459, 61237, 32771]);
        after.layers[0]
            .pixels
            .set16(9, 8, [34127, 12539, 58793, 65535]);
        let original = serde_json::to_value(&after).unwrap();
        let mut a = CapturedScene::from_parts(before.clone(), "fade".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
        let old =
            NativeDocument::from_pending(a.scene.into_provisional_pending().unwrap()).unwrap();
        let mut b = CapturedScene::from_parts(after.clone(), "fade".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
        let current =
            NativeDocument::from_pending(b.scene.into_provisional_pending().unwrap()).unwrap();
        let authority = DocumentAuthority::from_native(current.clone(), 4).unwrap();
        let key = current.key();
        let metadata = serde_json::to_value(current.metadata()).unwrap();
        let old_session = Arc::new(a.session.take().unwrap());
        let old_weak = Arc::downgrade(&old_session);
        let old_frame = render(&old, old_session, "fade", 21);
        assert!(old_weak.upgrade().is_none());
        let old_pixels = old_frame.copy_rgba8(&mut || Ok(())).unwrap();
        let baseline = Baseline::capture(&old_frame, &old, "fade", &|| Ok(())).unwrap();
        assert!(Baseline::capture(&old_frame, &current, "fade", &|| Ok(())).is_err());
        drop(old_frame);
        let session = Arc::new(b.session.take().unwrap());
        let weak = Arc::downgrade(&session);
        let mut retained = None;
        for t in [0., 0.25, 0.5, 0.75, 1.] {
            let frame = render(&current, session.clone(), "fade", 21);
            let tag = frame.tag();
            let pixels = frame.copy_rgba8(&mut || Ok(())).unwrap();
            let blend = Blend::capture(&old, &current, t, &|| Ok(())).unwrap();
            assert!(blend.matches(&current));
            assert!(!blend.matches(&old));
            let (shown, origin) = blend.render(&baseline, frame, &|| Ok(())).unwrap();
            assert_ne!(shown.key().source, key);
            assert!(origin.matches_source("fade", key, after.revision));
            assert!(origin.matches_frame(&shown));
            assert_eq!(origin.presentation_signature(), Some(blend.signature()));
            assert!(
                !shown.matches(&tag),
                "A display fade cannot qualify as committed output"
            );
            let actual = shown.copy_rgba8(&mut || Ok(())).unwrap();
            for ((a, b), out) in old_pixels
                .chunks_exact(4)
                .zip(pixels.chunks_exact(4))
                .zip(actual.chunks_exact(4))
            {
                if t == 0. {
                    assert_eq!(out, a);
                    continue;
                }
                if t == 1. {
                    assert_eq!(out, b);
                    continue;
                }
                let left = 1. - f64::from(t);
                let right = f64::from(t);
                let alpha = f64::from(a[3]) * left + f64::from(b[3]) * right;
                assert_eq!(out[3], alpha.round() as u8);
                for channel in 0..3 {
                    let premultiplied = f64::from(a[channel]) * f64::from(a[3]) * left
                        + f64::from(b[channel]) * f64::from(b[3]) * right;
                    assert_eq!(
                        out[channel],
                        if alpha == 0. {
                            0
                        } else {
                            (premultiplied / alpha).round() as u8
                        }
                    );
                }
            }
            retained = Some((shown, origin));
        }
        assert_eq!(current.key(), key);
        assert_eq!(serde_json::to_value(current.metadata()).unwrap(), metadata);
        assert_eq!(serde_json::to_value(&after).unwrap(), original);
        assert_eq!(authority.history_lengths(), [0, 0]);
        let blend = Blend::capture(&old, &current, 0.5, &|| Ok(())).unwrap();
        for frame in [
            render(&current, session.clone(), "other-project", 21),
            render(&current, session.clone(), "fade", 10),
        ] {
            assert!(blend.render(&baseline, frame, &|| Ok(())).is_err());
        }
        for t in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            assert!(Blend::capture(&old, &current, t, &|| Ok(())).is_err());
        }
        assert!(Blend::capture(&current, &old, 0.5, &|| Ok(())).is_err());
        let frame = render(&current, session.clone(), "fade", 21);
        let used = pool.used();
        let full = pool
            .reserve_bytes(pool.limit() - used, pool.limit())
            .unwrap();
        assert!(Baseline::capture(&frame, &current, "fade", &|| Ok(())).is_err());
        assert!(Baseline::try_capture(&frame, &current, "fade", &|| Ok(()))
            .unwrap()
            .is_none());
        assert!(blend.render(&baseline, frame, &|| Ok(())).is_err());
        drop(full);
        let frame = render(&current, session.clone(), "fade", 21);
        let copy_checks = std::cell::Cell::new(0);
        assert!(
            Baseline::try_capture(&frame, &current, "fade", &|| {
                copy_checks.set(copy_checks.get() + 1);
                if copy_checks.get() == 2 {
                    Err("one-shot cancellation".into())
                } else {
                    Ok(())
                }
            })
            .is_err(),
            "Optional caching must not swallow cancellation"
        );
        let used = pool.used();
        let checks = std::cell::Cell::new(0);
        assert!(blend
            .render(&baseline, frame, &|| {
                checks.set(checks.get() + 1);
                if checks.get() == 8 {
                    Err("interrupted during display rows".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert!(
            pool.used() < used,
            "Consumed failed frame and partial display rows retire"
        );
        assert_eq!(
            baseline.row(5).unwrap(),
            &old_pixels[5 * 21 * 4..6 * 21 * 4]
        );
        drop((session, old, current, authority, cache, a.seals, b.seals));
        assert!(weak.upgrade().is_none());
        assert!(retained
            .as_ref()
            .unwrap()
            .1
            .matches_frame(&retained.as_ref().unwrap().0));
        drop((retained, blend, baseline, old_pixels, runtime, connection));
    }
}
