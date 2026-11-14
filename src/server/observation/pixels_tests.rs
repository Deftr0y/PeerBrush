use super::{fixture, pixels::Pixels};
use crate::{raster::Raster, selection::Coverage};
use std::io::Read;
#[test]
fn native_preview_selection_and_streamed_png_match_actual_legacy_pixels() {
    let mut comparisons = 0;
    for depth in [8, 16] {
        for soft in [false, true] {
            let (w, h) = if soft { (513, 259) } else { (19, 13) };
            let mut doc = fixture::document(depth, w, h);
            doc.selection = Some([2, 1, 7, 6]);
            if soft {
                let mut mask = Raster::new(w + 7, h + 5);
                for y in 0..mask.height as i32 {
                    for x in 0..mask.width as i32 {
                        let value = ((x * 53 + y * 17 + 31) % 256) as u8;
                        mask.set(x, y, [value, 0, 0, 255]);
                    }
                }
                doc.selection_coverage = Some(Coverage {
                    mask,
                    bounds: doc.selection.unwrap(),
                    origin: [-3, -2],
                    contours: vec![],
                });
            } else {
                doc.selection_polygon = Some(vec![[2., 1.], [7., 2.], [6., 6.], [2., 5.]]);
                doc.selection =
                    crate::selection::bounds(doc.selection_polygon.as_ref().unwrap()).ok();
            }
            let original = serde_json::to_value(&doc).unwrap();
            let (native, scene, path) = fixture::capture(&doc);
            let pool = native.budget().clone();
            for (rect, edge) in [(None, 8192), (Some([1, 2, w as i32 - 1, h as i32]), 48)] {
                for mode in ["mask", "overlay", "cutout"] {
                    let (w, h, mut expected, area) = doc.preview(rect, edge, None, false).unwrap();
                    crate::selection::display::apply_region(&doc, w, h, &mut expected, area, mode);
                    let frame = crate::render::native_preview::test_frame_selection(
                        &scene,
                        rect,
                        edge,
                        None,
                        false,
                        Some(mode),
                        &mut || Ok(()),
                    )
                    .unwrap();
                    let pixels = Pixels::new(frame);
                    let (png, tag) = pixels.encode(&pool, &mut || Ok(())).unwrap();
                    assert_eq!(
                        (tag.key().width, tag.key().height, tag.key().rect),
                        (w, h, area)
                    );
                    let mut encoded = Vec::new();
                    png.reader().read_to_end(&mut encoded).unwrap();
                    let image = image::load_from_memory(&encoded).unwrap().to_rgba8();
                    assert_eq!(
                        image.as_raw(),
                        &expected,
                        "depth{depth} soft{soft} mode{mode} edge{edge}"
                    );
                    comparisons += 1;
                }
            }
            assert_eq!(serde_json::to_value(&doc).unwrap(), original);
            drop(scene);
            drop(native);
            assert_eq!(pool.used(), 0);
            fixture::cleanup(&path);
        }
    }
    assert_eq!(comparisons, 24);
}
#[test]
fn selection_and_png_cancel_or_admission_errors_keep_exact_source_and_owner_baselines() {
    let mut doc = fixture::document(16, 31, 23);
    doc.selection = Some([1, 1, 30, 22]);
    let (native, scene, path) = fixture::capture(&doc);
    let pool = native.budget().clone();
    let source = native
        .source(native.metadata().layer(0).unwrap().pixels().slot())
        .unwrap()
        .key();
    let frame =
        crate::render::native_preview::test_frame(&scene, None, 48, None, false, &mut || Ok(()))
            .unwrap();
    drop(frame);
    let baseline = pool.used();
    let failure = crate::render::native_preview::test_frame_selection(
        &scene,
        None,
        48,
        None,
        false,
        Some("mask"),
        &mut || Err("selection source lease ended".into()),
    )
    .err()
    .unwrap();
    assert_eq!(failure, "selection source lease ended");
    assert_eq!(pool.used(), baseline);
    let frame =
        crate::render::native_preview::test_frame(&scene, None, 48, None, false, &mut || Ok(()))
            .unwrap();
    let pixels = Pixels::new(frame);
    assert!(pixels
        .encode(&pool, &mut || Err("PNG response canceled".into()))
        .is_err());
    assert_eq!(pool.used(), baseline);
    assert_eq!(
        native
            .source(native.metadata().layer(0).unwrap().pixels().slot())
            .unwrap()
            .key(),
        source
    );
    // Occupying all remaining bytes refuses actual output preparation. No
    // resource failure is converted into an empty/transparent response.
    let available = pool.limit() - pool.used();
    let mut guards = Vec::new();
    let mut left = available;
    while left > 0 {
        let n = left.min(64 * 1024 * 1024);
        guards.push(pool.reserve_bytes(n, n).unwrap());
        left -= n;
    }
    assert!(
        crate::render::native_preview::test_frame(&scene, None, 48, None, false, &mut || Ok(()))
            .is_err()
    );
    assert_eq!(pool.used(), pool.limit());
    drop(guards);
    assert_eq!(pool.used(), baseline);
    drop(scene);
    drop(native);
    assert_eq!(pool.used(), 0);
    fixture::cleanup(&path);
}
