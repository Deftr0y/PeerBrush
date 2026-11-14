use super::*;
use crate::engine::{Document,scene::{CapturedScene,LegacyTileCache},native::worker::tests::Fixture};

#[test]
fn streamed_edge_guides_keep_exact_samples_above_the_complete_preview_limit() {
    for depth in [8,16] {
        let fixture = Fixture::new();
        let (runtime,connection) = fixture.runtime_with_budgets(256*1024*1024,128*1024*1024);
        let mut original = Document::new_depth(1,1,depth).unwrap();
        original.width = 16000;
        original.height = 4200;
        original.layers[0].pixels = crate::raster::Raster::new_depth(16000,4200,depth);
        original.layers[0].pixels.set16(15999,4199,[65535,0,0,65535]);
        let cache = LegacyTileCache::new(&runtime.resident(),32).unwrap();
        let mut staged = CapturedScene::from_parts(original,"large-guide".into(),vec![],None,None)
            .stage(&runtime,&cache,&|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let baseline = runtime.disk().used().unwrap();
        let target = native.metadata().layer(0).unwrap().id();
        for frame in [[-1,0,15999,4200],[1,0,16001,4200],[0,0,16001,4200]] {
            assert!(stage(&native,&session,frame,Some(target),&|| Ok(())).is_err());
            assert_eq!(runtime.disk().used().unwrap(),baseline);
        }
        let guide = stage(&native,&session,[0,0,16000,4200],Some(target),&|| Ok(())).unwrap();
        assert_eq!((guide.surface().width(),guide.surface().height(),guide.surface().depth()),(16000,4200,8));
        assert_eq!(guide.surface().physical_tiles().count(), 1);
        let empty = guide.surface().prepare_region(Region::new([0,0,2,2]).unwrap(),&mut || Ok(())).unwrap();
        assert_eq!(empty.pixel(1,1).unwrap(), [0; 4]);
        drop(empty);
        let view = guide.surface().prepare_region(Region::new([15998,4199,16000,4200]).unwrap(),&mut || Ok(())).unwrap();
        assert_eq!(view.pixel(15999,4199).unwrap(),[255,0,0,255]);
        assert_eq!(view.pixel(15998,4199).unwrap(),[0,0,0,0]);
        drop(view);
        guide.abort(&session,&|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(),baseline);
        let guide = stage_layer_frame(
            &native, &session, [15998, 4198, 16002, 4202], target, &|| Ok(()),
        ).unwrap();
        assert_eq!((guide.surface().width(), guide.surface().height()), (4, 4));
        let view = guide.surface().prepare_region(Region::new([0, 0, 4, 4]).unwrap(), &mut || Ok(())).unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let expected = if [x, y] == [1, 1] { [255, 0, 0, 255] } else { [0; 4] };
                assert_eq!(view.pixel(x, y).unwrap(), expected);
            }
        }
        drop(view);
        guide.abort(&session, &|| session.cleanup_check()).unwrap();
        let empty = stage_layer_frame(&native, &session, [-8, -7, -4, -3], target, &|| Ok(())).unwrap();
        assert!(empty.surface().is_known_zero());
        assert_eq!((empty.surface().width(), empty.surface().height()), (4, 4));
        empty.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        drop((native,staged.seals,session,cache,runtime,connection));
    }
}
