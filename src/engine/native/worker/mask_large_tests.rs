use super::*;

#[test]
fn path_mask_on_a_large_canvas_keeps_its_local_target_frame() {
    let fixture = Fixture::new();
    let (runtime,connection) = fixture.runtime_with_budgets(96*1024*1024,128*1024*1024);
    let mut original = Document::new_depth(4,4,16).unwrap();
    original.width = 16000;
    original.height = 1001;
    let target = original.layers[0].id.clone();
    original.layers[0].x = 15990;
    original.layers[0].y = 990;
    crate::path::create(&mut original,&json!({"closed":true,"frame":[15990,990,15994,994],
        "nodes":[{"point":[15990,990]},{"point":[15994,990]},{"point":[15994,994]},{"point":[15990,994]}]})).unwrap();
    let path = original.layers[0].id.clone();
    let cache = LegacyTileCache::new(&runtime.resident(),32).unwrap();
    let mut staged = CapturedScene::from_parts(original,"large-path-mask".into(),vec![],None,None)
        .stage(&runtime,&cache,&|| Ok(())).unwrap();
    let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let state = DocumentAuthority::from_native(native,4).unwrap();
    let baseline = runtime.disk().used().unwrap();
    let prepared = worker::prepare(capture(&state,&[json!({"op":"path.to_mask","path":path,"layer":target,"feather":1})]),&session,&|| Ok(())).unwrap();
    let layer = prepared.preview().metadata().layers().find(|layer| layer.id() == target).unwrap();
    let mask = layer.mask().unwrap();
    let output = prepared.preview().source(mask.steps().last().unwrap().pixels().slot()).unwrap();
    assert_eq!((output.width(),output.height(),output.depth()),(4,4,16));
    let view = output.prepare_region(crate::raster::backed::Region::new([0,0,4,4]).unwrap(),&mut || Ok(())).unwrap();
    for y in 0..4 {
        for x in 0..4 {
            let word = match ([0, 3].contains(&x), [0, 3].contains(&y)) {
                (true, true) => 113 * 257,
                (true, false) | (false, true) => 170 * 257,
                (false, false) => u16::MAX,
            };
            assert_eq!(view.pixel16(x,y).unwrap(), [word, word, word, u16::MAX]);
        }
    }
    drop(view);
    prepared.abort(&session,&|| session.cleanup_check()).unwrap();
    assert_eq!(runtime.disk().used().unwrap(),baseline);
    assert_eq!(state.history_lengths(),[0,0]);
    drop((state,staged.seals,session,cache,runtime,connection));
}

#[test]
fn large_native_masks_keep_selection_source_words_and_transaction_guards() {
    for depth in [8,16] {
        for (height,op) in [(16000,"mask.from_selection"),(1001,"mask.refine")] {
            let fixture = Fixture::new();
            let (runtime,connection) = fixture.runtime_with_budgets(128*1024*1024,128*1024*1024);
            let mut original = Document::new_depth(1,1,depth).unwrap();
            original.width = 16000;
            original.height = height;
            let point = [15997,height as i32-3];
            let layer = &mut original.layers[0];
            let target = layer.id.clone();
            layer.pixels = Raster::new_depth(16000,height,depth);
            layer.pixels.set16(point[0],point[1],[12347,23459,34571,49153]);
            let mut mask = Raster::new_depth(16000,height,depth);
            mask.set16(point[0],point[1],[12347,12347,12347,65535]);
            let original_word = mask.get16(point[0],point[1])[0];
            layer.mask = Some(Mask { enabled:false,linked:false,cache_key:crate::engine::id(),steps:vec![
                MaskStep { id:crate::engine::id(),kind:"fill".into(),enabled:true,weight:1.,value:0.,settings:json!(null),pixels:Raster::new_depth(1,1,depth) },
                MaskStep { id:crate::engine::id(),kind:"paint".into(),enabled:true,weight:1.,value:0.,settings:json!(null),pixels:mask },
            ]});
            let mut selected = Raster::new(1,1);
            selected.set(0,0,[137;4]);
            original.selection = Some([point[0],point[1],point[0]+1,point[1]+1]);
            original.selection_coverage = Some(crate::selection::Coverage {
                mask:selected,bounds:original.selection.unwrap(),origin:point,contours:vec![],
            });
            let cache = LegacyTileCache::new(&runtime.resident(),32).unwrap();
            let mut staged = CapturedScene::from_parts(original,"large-mask".into(),vec![],None,None)
                .stage(&runtime,&cache,&|| Ok(())).unwrap();
            let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
            let session = staged.session.take().unwrap();
            let mut state = DocumentAuthority::from_native(native.clone(),4).unwrap();
            let command = json!({"op":op,"layer":target});
            let baseline = runtime.disk().used().unwrap();
            let completed = crate::selection::regional_masks::tests::CompletedTiles::observe();
            let failed = worker::prepare(capture(&state,&[command.clone()]),&session,&|| {
                if completed.count() > 0 { Err("large mask canceled after tile".into()) } else { Ok(()) }
            }).err().unwrap();
            assert!(failed.reason.contains("large mask canceled after tile"));
            assert!(failed.cleanup_error.is_none());
            assert_eq!(completed.count(),1);
            drop(completed);
            assert_eq!(runtime.disk().used().unwrap(),baseline);
            assert_eq!(state.history_lengths(),[0,0]);
            let prepared = worker::prepare(capture(&state,&[command]),&session,&|| Ok(())).unwrap();
            let after = prepared.preview();
            let layer = after.metadata().layer(0).unwrap();
            let old = native.metadata().layer(0).unwrap();
            assert_eq!(after.source(layer.pixels().slot()).unwrap().key(),native.source(old.pixels().slot()).unwrap().key());
            let mask = layer.mask().unwrap();
            assert!(!mask.enabled() && !mask.linked());
            assert_eq!(mask.steps().len(),3);
            for (step,previous) in mask.steps().zip(old.mask().unwrap().steps()) {
                assert_eq!(after.source(step.pixels().slot()).unwrap().key(),native.source(previous.pixels().slot()).unwrap().key());
            }
            let output = after.source(mask.steps().last().unwrap().pixels().slot()).unwrap();
            let region = crate::raster::backed::Region::new([point[0]-1,point[1],point[0]+1,point[1]+1]).unwrap();
            let view = output.prepare_region(region,&mut || Ok(())).unwrap();
            let value = if op == "mask.from_selection" { 137*257 } else { original_word };
            assert_eq!(view.pixel16(point[0],point[1]).unwrap(),[value,value,value,65535]);
            assert_eq!(view.pixel16(point[0]-1,point[1]).unwrap(),[0,0,0,65535]);
            if op == "mask.from_selection" {
                let first = output.physical_tile((0, 0)).unwrap().reference();
                assert_eq!(first, output.physical_tile((1, 1)).unwrap().reference());
                assert_ne!(first, output.physical_tile((62, 62)).unwrap().reference());
            }
            assert_eq!(after.metadata().selection(),native.metadata().selection());
            assert_eq!(after.source(after.metadata().selection_coverage().unwrap().mask().slot()).unwrap().key(),
                native.source(native.metadata().selection_coverage().unwrap().mask().slot()).unwrap().key());
            drop(view);
            let (base,history,delta) = prepared.into_parts();
            let committed = history.prepare_commit(&base,delta,HistoryRequest {
                actor:"human",label:"Large mask",task:None,gesture:None,
            },&|| Ok(())).unwrap();
            state.commit_native(gate(&[]),committed).unwrap().retire(&session,&|| session.cleanup_check()).unwrap();
            for direction in [Direction::Undo,Direction::Redo] {
                let moved = state.native_history().unwrap().prepare_move(state.native().unwrap(),direction,"human",&|| Ok(())).unwrap();
                drop(state.move_native(gate(&[]),moved).unwrap());
                assert_eq!(state.native().unwrap().metadata().layer(0).unwrap().mask().unwrap().steps().len(),
                    if matches!(direction,Direction::Undo) { 2 } else { 3 });
            }
            drop((state,native,staged.seals,session,cache,runtime,connection));
        }
    }
}
