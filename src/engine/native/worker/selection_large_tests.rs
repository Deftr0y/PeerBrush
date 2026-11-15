use super::*;

pub(super) fn sample(native: &NativeDocument, x: i32, y: i32) -> u8 {
    let coverage = native.metadata().selection_coverage().unwrap();
    let source = native.source(coverage.mask().slot()).unwrap();
    let [ox, oy] = coverage.origin();
    let view = source.prepare_region(
        crate::raster::backed::Region::new([x-ox,y-oy,x-ox+1,y-oy+1]).unwrap(),
        &mut || Ok(()),
    ).unwrap();
    let pixel = view.pixel(x-ox,y-oy).unwrap();
    assert_eq!(pixel,[pixel[0];4]);
    pixel[0]
}

#[test]
fn full_size_native_geometry_keeps_soft_coverage_artwork_and_atomic_history() {
    for depth in [8,16] {
        let fixture = Fixture::new();
        let (runtime,connection) = fixture.runtime_with_budgets(96*1024*1024,128*1024*1024);
        let session = runtime.begin().unwrap();
        let native = NativeDocument::new_blank(16000,16000,depth,
            &runtime.resident(),&runtime.disk(),&|| Ok(())).unwrap();
        let artwork = native.source(native.metadata().layer(0).unwrap().pixels().slot()).unwrap().key();
        let mut state = DocumentAuthority::from_native(native,4).unwrap();
        let before = state.native().unwrap().key();
        let command = json!({"op":"selection","kind":"rectangle",
            "rect":[15990,15990,15994,15994],"feather":1});
        let prepared = worker::prepare(capture(&state,&[command]),&session,&|| Ok(())).unwrap();
        assert_eq!(prepared.preview().metadata().selection(),Some([15989,15989,15995,15995]));
        let coverage = prepared.preview().metadata().selection_coverage().unwrap();
        assert_eq!(coverage.origin(), [15989, 15989]);
        assert_eq!([coverage.mask().width(), coverage.mask().height()], [6, 6]);
        for (x,y,value) in [(15988,15988,0),(15989,15989,28),(15989,15991,85),
            (15990,15990,113),(15991,15991,255),(15994,15994,28),(15995,15995,0),(0,0,0)] {
            assert_eq!(sample(prepared.preview(),x,y),value);
        }
        assert_eq!(state.native().unwrap().key(),before);
        assert_eq!(state.history_lengths(),[0,0]);
        commit(&mut state,prepared,&session);
        let selected = state.native().unwrap().source(
            state.native().unwrap().metadata().selection_coverage().unwrap().mask().slot()).unwrap().key();
        for direction in [Direction::Undo,Direction::Redo] {
            let moved = state.native_history().unwrap().prepare_move(
                state.native().unwrap(),direction,"human",&|| Ok(())).unwrap();
            drop(state.move_native(publication(&[]),moved).unwrap());
            if matches!(direction,Direction::Undo) {
                assert!(state.native().unwrap().metadata().selection().is_none());
            } else {
                let doc = state.native().unwrap();
                assert_eq!(doc.source(doc.metadata().selection_coverage().unwrap().mask().slot()).unwrap().key(),selected);
                assert_eq!(sample(doc,15990,15990),113);
            }
        }
        let doc = state.native().unwrap();
        assert_eq!(doc.metadata().depth(),depth);
        assert_eq!(doc.source(doc.metadata().layer(0).unwrap().pixels().slot()).unwrap().key(),artwork);
        let baseline = runtime.disk().used().unwrap();
        let history = state.history_lengths();
        let key = state.native().unwrap().key();
        let completed = crate::selection::CompletedTiles::observe();
        let failure = worker::prepare(capture(&state,&[json!({"op":"selection",
            "kind":"ellipse","rect":[1,1,12,12],"mode":"add","feather":2})]),&session,&|| {
                if completed.count() > 0 { Err("large selection canceled after tile".into()) }
                else { Ok(()) }
            }).err().unwrap();
        assert!(failure.reason.contains("large selection canceled after tile"));
        assert!(failure.cleanup_error.is_none());
        assert_eq!(completed.count(),1);
        assert_eq!(runtime.disk().used().unwrap(),baseline);
        assert_eq!(state.history_lengths(),history);
        assert_eq!(state.native().unwrap().key(),key);
        drop((state,session,runtime,connection));
    }
}

#[test]
fn warp_small_feathered_selection_on_full_size_native_layer() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(96 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let native = NativeDocument::new_blank(
            16000,
            16000,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        let id = native.metadata().layer(0).unwrap().id().to_owned();
        let mut state = DocumentAuthority::from_native(native, 4).unwrap();
        for command in [
            json!({"op":"shape","layer":id,"rect":[250,250,262,262],"color":[211,73,99,255]}),
            json!({"op":"shape","layer":id,"rect":[15990,15990,15994,15994],"color":[25,75,125,255]}),
            json!({"op":"selection","rect":[251,251,261,261],"feather":1}),
        ] {
            let prepared = worker::prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            commit(&mut state, prepared, &session);
        }
        let pixel = |doc: &NativeDocument, x, y| {
            let slot = doc.metadata().layer(0).unwrap().pixels().slot();
            let view = doc
                .source(slot)
                .unwrap()
                .prepare_region(
                    crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap(),
                    &mut || Ok(()),
                )
                .unwrap();
            view.pixel16(x, y).unwrap()
        };
        let before = state.native().unwrap();
        let original = pixel(before, 255, 255);
        let distant = pixel(before, 15991, 15991);
        assert_eq!(original[3], 65535);
        assert_eq!(distant[3], 65535);
        let source_key = before.key();
        let history = state.history_lengths();
        let command = json!({"op":"transform","layer":id,"selection_only":true,"dx":32,
            "warp":{"rect":[247,247,265,265],"preset":"arc","amount":0.125}});
        let prepared = worker::prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
        let after = prepared.preview();
        assert_eq!(after.metadata().depth(), depth);
        assert_eq!(pixel(after, 255, 255), [0; 4]);
        assert_eq!(pixel(after, 287, 257), original);
        assert_eq!(pixel(after, 15991, 15991), distant);
        assert_eq!(sample(after, 287, 257), 255);
        assert_eq!(state.native().unwrap().key(), source_key);
        assert_eq!(state.history_lengths(), history);
        commit(&mut state, prepared, &session);
        let undo = state
            .native_history()
            .unwrap()
            .prepare_move(state.native().unwrap(), Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        drop(state.move_native(publication(&[]), undo).unwrap());
        let restored = state.native().unwrap();
        assert_eq!(pixel(restored, 255, 255), original);
        assert_eq!(pixel(restored, 287, 257), [0; 4]);
        assert_eq!(pixel(restored, 15991, 15991), distant);
        assert_eq!(sample(restored, 255, 255), 255);
    }
}

#[test]
fn large_native_selection_combinations_and_morphology_keep_exact_membership() {
    let fixture = Fixture::new();
    let (runtime,connection) = fixture.runtime_with_budgets(96*1024*1024,128*1024*1024);
    let session = runtime.begin().unwrap();
    let native = NativeDocument::new_blank(16000,1001,16,
        &runtime.resident(),&runtime.disk(),&|| Ok(())).unwrap();
    let state = DocumentAuthority::from_native(native,4).unwrap();
    let baseline = runtime.disk().used().unwrap();
    let original = [15990,990,15994,994];
    for (command,area,frame,inverted) in [
        (json!({"op":"selection.modify","mode":"expand","radius":2}),[15988,988,15996,996],[15988,988,15996,996],false),
        (json!({"op":"selection.modify","mode":"contract","radius":1}),[15991,991,15993,993],original,false),
        (json!({"op":"selection.invert"}),original,[0,0,16000,1001],true),
        (json!({"op":"selection","kind":"rectangle","rect":[15992,988,15998,996],"mode":"intersect"}),[15992,990,15994,994],[15992,990,15994,994],false),
        (json!({"op":"selection","kind":"rectangle","rect":[15992,989,15997,995],"mode":"subtract"}),[15990,990,15992,994],original,false),
        (json!({"op":"selection","polygon":[[15989,989],[15995,989],[15995,995],[15989,995]],"mode":"add"}),[15989,989,15995,995],[15989,989,15995,995],false),
    ] {
        let commands = [json!({"op":"selection","rect":original}),command];
        let prepared = worker::prepare(capture(&state,&commands),&session,&|| Ok(())).unwrap();
        let previous = prepared.preview().metadata().selection_previous().unwrap();
        assert_eq!(previous.bounds(), original);
        assert_eq!(previous.origin(), [15990, 990]);
        assert_eq!([previous.mask().width(), previous.mask().height()], [4, 4]);
        let current = prepared.preview().metadata().selection_coverage().unwrap();
        let [left, top, right, bottom] = frame;
        assert_eq!(current.origin(), [left, top]);
        assert_eq!([current.mask().width(), current.mask().height()], [(right - left) as u32, (bottom - top) as u32]);
        assert_eq!(prepared.preview().metadata().selection(),Some(if inverted { [0,0,16000,1001] } else { area }));
        for y in 987..998 {
            for x in 15987..15999 {
                let inside = x >= area[0] && x < area[2] && y >= area[1] && y < area[3];
                assert_eq!(sample(prepared.preview(),x,y),if inside != inverted { 255 } else { 0 });
            }
        }
        assert_eq!(sample(prepared.preview(),0,0),if inverted { 255 } else { 0 });
        prepared.abort(&session,&|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(),baseline);
        assert_eq!(state.history_lengths(),[0,0]);
        assert!(state.native().unwrap().metadata().selection().is_none());
    }
    drop((state,session,runtime,connection));
}
