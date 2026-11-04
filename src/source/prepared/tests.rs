use super::*;
use crate::{fonts::prepared::FontOwner, source::TextLayout};
fn text(layout: TextLayout) -> Source36 {
    Source36 {
        width: 103,
        height: 67,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content36::Text {
            text: "AV office\nÀé Wj".into(),
            layout,
            font: "regular".into(),
            size: 23.125,
            line_height: 0.875,
            letter_spacing: -1.75,
            align: "center".into(),
            color: [12501, 23003, 34507, 43871],
        }
        .into(),
    }
}
fn compare(source: &Source36, matrix: [f32; 6], frame: [u32; 2]) {
    let pool = ResidentBudget::new(256 * 1024 * 1024).unwrap();
    let font = match source.definition() {
        Content36::Text { font, .. } => Some(Arc::new(
            FontOwner::load(font, &pool, &mut || Ok(())).unwrap(),
        )),
        _ => None,
    };
    let prepared =
        PreparedSource::new_transformed(source, matrix, font, &pool, &mut || Ok(())).unwrap();
    let mut legacy: crate::source::Source =
        serde_json::from_value(serde_json::to_value(source).unwrap()).unwrap();
    legacy.matrix = matrix;
    for depth in [8, 16] {
        let expected = legacy.render(frame[0], frame[1], depth).unwrap();
        for y in (0..frame[1] as i32).step_by(13) {
            for x in (0..frame[0] as i32).step_by(17) {
                let region = Region::new([
                    x,
                    y,
                    (x + 17).min(frame[0] as i32),
                    (y + 13).min(frame[1] as i32),
                ])
                .unwrap();
                let result = prepared
                    .render_region(frame, region, depth, &pool, &mut || Ok(()))
                    .unwrap();
                assert!(result.uses_budget(&pool));
                for yy in y..region.bounds()[3] {
                    for xx in x..region.bounds()[2] {
                        assert_eq!(
                            result.pixel16(xx, yy).unwrap(),
                            expected.get16(xx, yy),
                            "depth{depth} at({xx},{yy}) matrix{matrix:?}"
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn analytic_shape_and_cubic_regions_keep_original_native_grid_and_affine_sampler() {
    for shape in ["rectangle", "ellipse", "path"] {
        let source = Source36 {
            width: 39,
            height: 35,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content36::Shape {
                shape: shape.into(),
                bounds: [2.125, 3.375, 34.75, 30.625],
                points: vec![
                    [2.125, 3.375],
                    [34.75, 8.625],
                    [30.125, 30.625],
                    [7.875, 27.25],
                ],
                closed: true,
                fill: [12345, 23457, 45679, 43871],
                stroke: [54321, 32109, 10987, 49153],
                stroke_width: 2.375,
            }
            .into(),
        };
        for matrix in [
            [1., 0., 0., 1., 0., 0.],
            [0.9375, 0.125, -0.0625, 1.125, 7.25, 0.625],
        ] {
            compare(&source, matrix, [47, 41]);
        }
    }
    let source = Source36 {
        width: 39,
        height: 35,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content36::Bezier {
            nodes: vec![
                crate::path::Anchor {
                    point: [3.25, 4.125],
                    in_handle: None,
                    out_handle: Some([15.875, -1.25]),
                    smooth: false,
                },
                crate::path::Anchor {
                    point: [33.5, 28.75],
                    in_handle: Some([37.125, 3.5]),
                    out_handle: None,
                    smooth: false,
                },
                crate::path::Anchor {
                    point: [5.5, 29.875],
                    in_handle: None,
                    out_handle: None,
                    smooth: false,
                },
            ],
            closed: true,
            fill: [12345, 23457, 45679, 43871],
            stroke: [54321, 32109, 10987, 49153],
            stroke_width: 2.375,
        }
        .into(),
    };
    compare(
        &source,
        [0.9375, 0.125, -0.0625, 1.125, 7.25, 0.625],
        [47, 41],
    );
}
#[test]
fn native_text_keeps_global_alignment_glyph_phase_and_per_glyph_depth_projection() {
    for layout in [TextLayout::Legacy, TextLayout::Shaped] {
        for matrix in [
            [1., 0., 0., 1., 0., 0.],
            [1., 0., 0., 1., 3.25, -2.75],
            [0.9375, 0.125, -0.0625, 1.125, 7.25, 0.625],
            [0., 1., -1., 0., 80., -5.],
        ] {
            let s = text(layout);
            compare(&s, matrix, [111, 83]);
            assert_eq!(s.matrix, [1., 0., 0., 1., 0., 0.]);
        }
    }
}
#[test]
fn transformed_text_splits_wide_actual_source_support_without_changing_pixels() {
    let pool = ResidentBudget::new(256 * 1024 * 1024).unwrap();
    let mut source = text(TextLayout::Shaped);
    source.width = 800;
    source.height = 800;
    source.matrix = [0.35355338, 0.35355338, -0.35355338, 0.35355338, 300., 0.];
    let font = Arc::new(FontOwner::load(&"regular".into(), &pool, &mut || Ok(())).unwrap());
    let p = PreparedSource::new(&source, Some(font), &pool, &mut || Ok(())).unwrap();
    let region = Region::new([100, 100, 400, 400]).unwrap();
    let legacy: crate::source::Source =
        serde_json::from_value(serde_json::to_value(&source).unwrap()).unwrap();
    for depth in [8, 16] {
        let expected = legacy.render(800, 800, depth).unwrap();
        let pixels = p
            .render_region([800, 800], region, depth, &pool, &mut || Ok(()))
            .unwrap();
        for y in 100..400 {
            for x in 100..400 {
                assert_eq!(
                    pixels.pixel16(x, y).unwrap(),
                    expected.get16(x, y),
                    "depth{depth} at({x},{y})"
                );
            }
        }
    }
    let before = pool.used();
    let mut rows = 0;
    let result = p.render_region([800, 800], region, 16, &pool, &mut || {
        rows += 1;
        if rows > 350 {
            Err("wide text canceled".into())
        } else {
            Ok(())
        }
    });
    assert_eq!(result.err().unwrap(), "wide text canceled");
    assert_eq!(pool.used(), before);
}
#[test]
fn svg_small_full_origin_projection_preserves_clip_gradient_and_affine_phase() {
    for svg in [
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="103" height="67"><rect x="5.125" y="3.25" width="80.75" height="55.5" rx="8.125" fill="#d0a047" fill-opacity=".65" stroke="#6ba4e2" stroke-width="3.375"/></svg>"##,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="103" height="67"><defs><linearGradient id="g" gradientTransform="rotate(23)"><stop stop-color="#315bad"/><stop offset="1" stop-color="#f0ac57" stop-opacity=".3"/></linearGradient><clipPath id="c"><ellipse cx="47.5" cy="33.25" rx="43.5" ry="28.125"/></clipPath></defs><g opacity=".73" clip-path="url(#c)"><path fill="url(#g)" d="M-4 2 Q75 -14 102 44 L80 70 L5 60Z"/><circle cx="60" cy="32" r="22.5" fill="#ba1e37" opacity=".57"/></g></svg>"##,
    ] {
        for matrix in [
            [1., 0., 0., 1., 0., 0.],
            [0.9375, 0.125, -0.0625, 1.125, 7.25, 0.625],
        ] {
            compare(
                &Source36 {
                    width: 103,
                    height: 67,
                    matrix,
                    content: Content36::Svg { svg: svg.into() }.into(),
                },
                matrix,
                [111, 83],
            );
        }
    }
}
#[test]
fn source_cancellation_pool_and_output_bounds_preserve_charge_lifetime() {
    let pool = ResidentBudget::new(256 * 1024 * 1024).unwrap();
    let s = text(TextLayout::Legacy);
    let font = Arc::new(FontOwner::load(&"regular".into(), &pool, &mut || Ok(())).unwrap());
    let baseline = pool.used();
    let p = PreparedSource::new(&s, Some(font.clone()), &pool, &mut || Ok(())).unwrap();
    let before = pool.used();
    let region = Region::new([0, 0, 20, 20]).unwrap();
    assert_eq!(
        p.render_region([103, 67], region, 16, &pool, &mut || Err(
            "vector canceled".into()
        ))
        .err()
        .unwrap(),
        "vector canceled"
    );
    assert_eq!(pool.used(), before);
    assert!(p
        .render_region(
            [103, 67],
            region,
            16,
            &ResidentBudget::new(pool.limit()).unwrap(),
            &mut || Ok(())
        )
        .is_err());
    assert!(p
        .render_region(
            [103, 67],
            Region::new([-1, 0, 20, 20]).unwrap(),
            16,
            &pool,
            &mut || Ok(())
        )
        .is_err());
    assert!(p
        .render_region([103, 67], region, 32, &pool, &mut || Ok(()))
        .is_err());
    drop(p);
    assert_eq!(pool.used(), baseline);
    drop(font);
    assert_eq!(pool.used(), 0);
}
#[test]
fn large_analytic_metadata_uses_only_bounded_region_and_large_svg_fails_closed() {
    let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
    let frame = [16000, 16000];
    let s = Source36 {
        width: 16000,
        height: 16000,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content36::Shape {
            shape: "ellipse".into(),
            bounds: [15000.125, 15001.375, 15015.75, 15014.25],
            points: vec![],
            closed: true,
            fill: [12345, 23457, 45679, 43871],
            stroke: [54321, 32109, 10987, 49153],
            stroke_width: 2.375,
        }
        .into(),
    };
    let p = PreparedSource::new(&s, None, &pool, &mut || Ok(())).unwrap();
    let pixels = p
        .render_region(
            frame,
            Region::new([14998, 14999, 15018, 15017]).unwrap(),
            16,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
    assert_eq!(pixels.bytes().len(), 20 * 18 * 8);
    assert!(pixels
        .pixel16(15008, 15008)
        .unwrap()
        .iter()
        .any(|v| v % 257 != 0));
    drop((pixels, p));
    assert_eq!(pool.used(), 0);
    let s=Source36{content:Content36::Svg{svg:r#"<svg xmlns="http://www.w3.org/2000/svg" width="16000" height="16000"><rect width="16000" height="16000"/></svg>"#.into()}.into(),..s};
    let p = PreparedSource::new(&s, None, &pool, &mut || Ok(())).unwrap();
    let before = pool.used();
    assert!(p
        .render_region(
            frame,
            Region::new([0, 0, 16, 16]).unwrap(),
            16,
            &pool,
            &mut || Ok(())
        )
        .err()
        .unwrap()
        .contains("global-phase streaming renderer"));
    assert_eq!(pool.used(), before);
}
