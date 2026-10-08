use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::{
    brush::{self, Session, Settings, TipKind},
    clipboard,
    engine::{Document, Engine, Layer},
    raster::{self, Raster},
};
use serde_json::json;

fn document(width: u32, height: u32) -> Document {
    let mut doc = Document::new(width, height).unwrap();
    doc.bit_depth = 16;
    for layer in &mut doc.layers {
        layer.pixels.promote16();
    }
    doc
}

#[test]
fn faint_native_strokes_and_incremental_sessions_preserve_sub_byte_samples_and_history() {
    let mut source = Raster::new_depth(64, 48, 16);
    for y in 0..48 {
        for x in 0..64 {
            source.set16(x, y, [10000 + x as u16, 20000 + y as u16, 30001, 50000]);
        }
    }
    let before = source.samples16.clone();
    let settings = Settings {
        radius: 4.,
        opacity: 0.001,
        ..Default::default()
    };
    let mut faint = source.clone();
    brush::paint(
        &mut faint,
        &[[30., 24.]],
        settings,
        [255, 0, 0, 255],
        false,
        None,
    )
    .unwrap();
    assert_eq!(
        faint.get16(30, 24)[3],
        50016,
        "faint stroke was quantized to an 8-bit alpha"
    );
    assert_eq!(faint.get16(0, 0), source.get16(0, 0));
    assert!(faint.tiles.is_empty());
    let points = [[8., 20.], [18., 24.], [29., 22.], [42., 30.]];
    let pressures = [0.2, 0.8, 0.4, 0.7];
    let polygon = [[4., 24.], [32., 5.], [60., 24.], [32., 43.]];
    for tip in [
        TipKind::Round,
        TipKind::Dry,
        TipKind::Chalk,
        TipKind::Grain,
        TipKind::Bristle,
    ] {
        for erase in [false, true] {
            let settings = Settings {
                radius: 6.,
                tip,
                flow: 0.27,
                opacity: 0.63,
                density: 0.72,
                taper_end: 9.,
                smoothing: 0.3,
                ..Default::default()
            };
            let mut session = Session::new(
                &source,
                settings,
                [200, 70, 40, 239],
                erase,
                None,
                Some(&polygon),
            )
            .unwrap();
            for count in [1, 2, 4, 2] {
                let mut expected = source.clone();
                brush::paint_with_pressure(
                    &mut expected,
                    &points[..count],
                    Some(&pressures[..count]),
                    settings,
                    [200, 70, 40, 239],
                    erase,
                    None,
                    Some(&polygon),
                )
                .unwrap();
                assert_eq!(
                    session
                        .update(&points[..count], Some(&pressures[..count]))
                        .unwrap()
                        .samples16,
                    expected.samples16
                );
            }
        }
    }
    assert_eq!(source.samples16, before);
}

#[test]
fn native_smudge_carries_precise_pigment_and_respects_polygon_selection() {
    let mut source = Raster::new_depth(48, 32, 16);
    for y in 0..32 {
        for x in 0..48 {
            source.set16(
                x,
                y,
                if x < 20 {
                    [50001 + x as u16, 2003, 701, 65535]
                } else {
                    [703, 2007, 50009 + x as u16, 65535]
                },
            );
        }
    }
    let before = source.samples16.clone();
    let settings = Settings {
        radius: 4.,
        wetness: 1.,
        pickup: 0.07,
        spacing: 0.15,
        ..Default::default()
    };
    let polygon = [[5., 16.], [24., 8.], [43., 16.], [24., 24.]];
    let points = [[10., 16.], [20., 16.], [35., 16.]];
    let mut a = source.clone();
    let mut b = source.clone();
    for raster in [&mut a, &mut b] {
        peerbrush::smudge::paint(
            raster,
            &points,
            None,
            settings,
            [255, 0, 0, 255],
            None,
            Some(&polygon),
        )
        .unwrap();
    }
    assert_eq!(a.samples16, b.samples16);
    assert!(a.get16(29, 16)[0] > source.get16(29, 16)[0]);
    assert_eq!(a.get16(29, 4), source.get16(29, 4));
    assert!(a.tiles.is_empty());
    assert_eq!(source.samples16, before);
}

#[test]
fn native_fill_blends_original_words_and_creates_native_folder_children() {
    let mut doc = document(24, 20);
    let id = doc.layers[0].id.clone();
    doc.layers[0]
        .pixels
        .set16(10, 10, [10001, 30003, 50005, 50007]);
    let mut engine = Engine::new();
    engine.doc = doc;
    engine
        .edit(
            "human",
            &[json!({"op":"paint.fill","layer":id,"rect":[9,9,12,12],"color":[80,160,240,1]})],
            None,
            None,
            "Fill",
        )
        .unwrap();
    assert_eq!(
        engine.doc.layers[0].pixels.get16(10, 10),
        raster::blend16(
            [10001, 30003, 50005, 50007],
            [80 * 257, 160 * 257, 240 * 257, 257],
            1.,
            "normal"
        )
    );
    engine.undo("human").unwrap();
    assert_eq!(
        engine.doc.layers[0].pixels.get16(10, 10),
        [10001, 30003, 50005, 50007]
    );
    let mut group = Layer::new("Folder", "group", 24, 20);
    group.pixels.promote16();
    let gid = group.id.clone();
    engine.doc.layers.insert(0, group);
    engine
        .edit(
            "human",
            &[json!({"op":"paint.fill","layer":gid,"color":[80,160,240,255]})],
            None,
            None,
            "Fill folder",
        )
        .unwrap();
    let child = engine
        .doc
        .layers
        .iter()
        .find(|l| l.parent.as_deref() == Some(gid.as_str()))
        .unwrap();
    assert_eq!(child.pixels.depth, 16);
    assert_eq!(
        child.pixels.get16(10, 10),
        [80 * 257, 160 * 257, 240 * 257, 65535]
    );
}

#[test]
fn native_png_placement_resize_and_selection_clipboard_round_trip_without_narrowing() {
    let mut engine = Engine::new();
    engine.doc = document(16, 12);
    let target = engine.doc.layers[0].id.clone();
    let words = vec![10001, 30003, 40007, 65535, 20009, 31011, 41013, 65535];
    let encoded = STANDARD.encode(raster::png16(2, 1, &words).unwrap());
    engine
        .edit(
            "human",
            &[json!({"op":"image.place","layer":target,"png":encoded,"x":4,"y":5})],
            None,
            None,
            "Place",
        )
        .unwrap();
    let placed = engine.doc.layers[0].id.clone();
    assert_eq!(engine.doc.layers[0].pixels.rgba16(), words);
    engine.doc.selection = Some([4, 5, 6, 6]);
    let copy = clipboard::copy_document_pixels(&engine.doc, &placed, false, false).unwrap();
    assert_eq!(copy.samples16.as_ref().unwrap(), &words);
    engine
        .edit(
            "human",
            &[copy.command(&placed).unwrap()],
            None,
            None,
            "Paste",
        )
        .unwrap();
    let pasted = engine
        .doc
        .layers
        .iter()
        .find(|l| l.name == "Pasted image")
        .unwrap();
    assert_eq!(pasted.pixels.depth, 16);
    assert_eq!(pasted.pixels.rgba16(), words);
    let encoded = STANDARD.encode(raster::png16(2, 1, &words).unwrap());
    engine
        .edit(
            "human",
            &[json!({"op":"image.place","layer":target,"png":encoded,"rect":[0,0,3,1]})],
            None,
            None,
            "Resample",
        )
        .unwrap();
    let resized = engine
        .doc
        .layers
        .iter()
        .find(|layer| layer.name == "Placed image" && layer.pixels.width == 3)
        .unwrap();
    let middle = resized.pixels.get16(1, 0);
    assert_eq!(middle, [15005, 30507, 40510, 65535]);
    assert!(resized.pixels.tiles.is_empty());
}
