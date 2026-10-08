use peerbrush::{
    brush::{self, Settings, TipKind},
    engine::{Document, Engine, Scope},
    raster::Raster,
    smudge,
};
use serde_json::json;

fn stroke(settings: Settings) -> Raster {
    let mut raster = Raster::new(128, 64);
    brush::paint(
        &mut raster,
        &[[16., 32.], [112., 32.]],
        settings,
        [230, 80, 20, 255],
        false,
        None,
    )
    .unwrap();
    raster
}
fn visible(raster: &Raster) -> usize {
    raster.rgba().chunks_exact(4).filter(|p| p[3] > 0).count()
}

#[test]
fn textured_tips_have_distinct_repeatable_marks_and_density_controls_coverage() {
    let base = Settings {
        radius: 12.,
        flow: 0.35,
        opacity: 0.8,
        grain: 2.,
        density: 0.65,
        seed: 42,
        ..Default::default()
    };
    let round = stroke(base);
    let mut seen = vec![round.rgba()];
    for tip in [
        TipKind::Dry,
        TipKind::Chalk,
        TipKind::Grain,
        TipKind::Bristle,
    ] {
        let settings = Settings { tip, ..base };
        let actual = stroke(settings);
        assert!(visible(&actual) > 0, "empty {tip:?} stroke");
        assert_eq!(
            actual.tiles,
            stroke(settings).tiles,
            "nondeterministic {tip:?} stroke"
        );
        assert!(
            !seen.contains(&actual.rgba()),
            "{tip:?} duplicates another tip"
        );
        seen.push(actual.rgba());
        assert_ne!(
            actual.tiles,
            stroke(Settings {
                seed: 94,
                ..settings
            })
            .tiles,
            "seed does not vary {tip:?}"
        );
        let low = stroke(Settings {
            density: 0.2,
            ..settings
        });
        let high = stroke(Settings {
            density: 0.95,
            ..settings
        });
        assert!(
            visible(&low) < visible(&high),
            "density does not add {tip:?} coverage"
        );
        let mut clipped = Raster::new(128, 64);
        brush::paint(
            &mut clipped,
            &[[16., 32.], [112., 32.]],
            settings,
            [230, 80, 20, 255],
            false,
            Some([64, 0, 128, 64]),
        )
        .unwrap();
        for y in 0..64 {
            for x in 0..128 {
                assert_eq!(
                    clipped.get(x, y),
                    if x >= 64 { actual.get(x, y) } else { [0; 4] },
                    "texture changed when clipping {tip:?}"
                );
            }
        }
    }
}

#[test]
fn explicit_pressure_changes_size_and_caps_opacity_and_can_be_disabled() {
    let settings = Settings {
        radius: 12.,
        flow: 0.8,
        ..Default::default()
    };
    let points = [[16., 32.], [112., 32.]];
    let mut low = Raster::new(128, 64);
    brush::paint_with_pressure(
        &mut low,
        &points,
        Some(&[0.25, 0.25]),
        settings,
        [255; 4],
        false,
        None,
        None,
    )
    .unwrap();
    assert!(
        (1..=64).contains(&low.get(64, 32)[3]),
        "pressure opacity did not cap at 25%"
    );
    assert_eq!(low.get(64, 39)[3], 0, "pressure size did not narrow tip");
    let mut full = Raster::new(128, 64);
    brush::paint(&mut full, &points, settings, [255; 4], false, None).unwrap();
    assert!(full.get(64, 39)[3] > 0);
    let mut disabled = Raster::new(128, 64);
    brush::paint_with_pressure(
        &mut disabled,
        &points,
        Some(&[0.25, 0.25]),
        Settings {
            pressure_size: false,
            pressure_opacity: false,
            ..settings
        },
        [255; 4],
        false,
        None,
        None,
    )
    .unwrap();
    assert_eq!(disabled.tiles, full.tiles);
    let (parsed, pressure) =
        brush::points_from_command(&json!({"points":[[1,2,0.2],[3,4,0.8]]})).unwrap();
    assert_eq!(parsed, vec![[1., 2.], [3., 4.]]);
    assert_eq!(pressure, Some(vec![0.2, 0.8]));
    assert!(brush::points_from_command(&json!({"points":[[1,2,0.2]],"pressures":[0.2]})).is_err());
    assert!(brush::points_from_command(&json!({"points":[[1,2]],"pressures":[1.1]})).is_err());
}

#[test]
fn size_and_opacity_taper_use_distance_instead_of_input_event_count() {
    let settings = Settings {
        radius: 10.,
        spacing: 0.1,
        taper_start: 24.,
        taper_end: 24.,
        ..Default::default()
    };
    let mut sparse = Raster::new(128, 64);
    let mut dense = sparse.clone();
    brush::paint(
        &mut sparse,
        &[[10., 32.], [110., 32.]],
        settings,
        [255; 4],
        false,
        None,
    )
    .unwrap();
    let points = (0..=10)
        .map(|i| [10. + i as f32 * 10., 32.])
        .collect::<Vec<_>>();
    brush::paint(&mut dense, &points, settings, [255; 4], false, None).unwrap();
    assert_eq!(sparse.tiles, dense.tiles, "taper depends on event sampling");
    assert!(sparse.get(14, 32)[3] < sparse.get(60, 32)[3]);
    assert!(sparse.get(106, 32)[3] < sparse.get(60, 32)[3]);
    assert_eq!(sparse.get(14, 39)[3], 0);
    assert!(sparse.get(60, 39)[3] > 0);
    let mut dot = Raster::new(16, 16);
    brush::paint(&mut dot, &[[8., 8.]], settings, [255; 4], false, None).unwrap();
    assert!(visible(&dot) > 0, "taper made a single click invisible");
}

fn pigment_canvas() -> Raster {
    let mut raster = Raster::new(64, 32);
    for y in 0..32 {
        for x in 0..64 {
            raster.set(
                x,
                y,
                if x < 32 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                },
            );
        }
    }
    raster
}

#[test]
fn smudge_carries_existing_color_is_repeatable_and_preserves_selection_and_history() {
    let original = pigment_canvas();
    let original_bytes = original.rgba();
    let mut actual = original.clone();
    let mut repeat = original.clone();
    let settings = Settings {
        radius: 6.,
        opacity: 0.6,
        flow: 0.4,
        wetness: 0.8,
        pickup: 0.1,
        ..Default::default()
    };
    let points = [[16., 16.], [54., 16.]];
    let clip = Some([8, 8, 56, 24]);
    let polygon = [[8., 16.], [32., 8.], [56., 16.], [32., 24.]];
    for raster in [&mut actual, &mut repeat] {
        smudge::paint(
            raster,
            &points,
            None,
            settings,
            [0; 4],
            clip,
            Some(&polygon),
        )
        .unwrap();
    }
    assert_eq!(actual.tiles, repeat.tiles);
    let mixed = actual.get(36, 16);
    assert!(
        mixed[0] > 0 && mixed[2] > 0,
        "smudge did not carry red into blue: {mixed:?}"
    );
    assert_eq!(
        original.rgba(),
        original_bytes,
        "shared history raster changed"
    );
    let mut changed = 0;
    for y in 0..32 {
        for x in 0..64 {
            let inside =
                ((x as f32 + 0.5 - 32.) / 24.).abs() + ((y as f32 + 0.5 - 16.) / 8.).abs() <= 1.;
            if !inside {
                assert_eq!(
                    actual.get(x, y),
                    original.get(x, y),
                    "unselected pixel changed at {x},{y}"
                );
            }
            changed += usize::from(actual.get(x, y) != original.get(x, y));
        }
    }
    assert!(changed > 0);
}

#[test]
fn wet_paint_load_can_deposit_pigment_but_empty_smudge_stays_sparse() {
    let mut empty = Raster::new(64, 32);
    let settings = Settings {
        radius: 5.,
        pickup: 0.,
        ..Default::default()
    };
    smudge::paint(
        &mut empty,
        &[[10., 16.], [54., 16.]],
        None,
        settings,
        [255, 80, 20, 255],
        None,
        None,
    )
    .unwrap();
    assert!(empty.tiles.is_empty());
    smudge::paint(
        &mut empty,
        &[[10., 16.], [54., 16.]],
        None,
        Settings {
            load: 1.,
            ..settings
        },
        [255, 80, 20, 255],
        None,
        None,
    )
    .unwrap();
    let pixel = empty.get(24, 16);
    assert!(pixel[3] > 0);
    assert_eq!(&pixel[..3], &[255, 80, 20]);
}

#[test]
fn invalid_pressure_settings_and_excessive_smudge_work_reject_before_mutation() {
    let mut raster = pigment_canvas();
    let before = raster.tiles.clone();
    assert!(brush::paint_with_pressure(
        &mut raster,
        &[[1., 1.]],
        Some(&[]),
        Settings::default(),
        [255; 4],
        false,
        None,
        None
    )
    .is_err());
    assert_eq!(raster.tiles, before);
    assert!(smudge::paint(
        &mut raster,
        &[[1., 1.]],
        None,
        Settings {
            grain: 0.,
            ..Default::default()
        },
        [255; 4],
        None,
        None
    )
    .is_err());
    assert_eq!(raster.tiles, before);
    let mut big = Raster::new(4096, 4096);
    big.set(4, 4, [255; 4]);
    let before = big.tiles.clone();
    let points = (0..400)
        .map(|i| {
            if i % 2 == 0 {
                [512., 512.]
            } else {
                [3584., 3584.]
            }
        })
        .collect::<Vec<_>>();
    assert!(smudge::paint(
        &mut big,
        &points,
        None,
        Settings {
            radius: 512.,
            spacing: 2.,
            ..Default::default()
        },
        [255; 4],
        None,
        None
    )
    .unwrap_err()
    .contains("work budget"));
    assert_eq!(big.tiles, before);
}

#[test]
fn painting_and_smudge_share_preview_commit_undo_and_lock_rules() {
    for op in ["paint", "smudge"] {
        let mut engine = Engine::new();
        engine.doc = Document::new(64, 32).unwrap();
        engine.doc.layers[0].pixels = pigment_canvas();
        let target = engine.doc.layers[0].id.clone();
        let command = json!({"op":op,"layer":target,"points":[[16,16,0.3],[54,16,0.9]],"radius":6,"tip":"bristle","density":0.75,"seed":9,"taper_start":5,"taper_end":8,"color":[20,240,120,255],"load":0.2});
        let before = engine.doc.clone();
        let preview = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
        engine
            .edit("human", &[command.clone()], None, None, "Painting")
            .unwrap();
        assert_eq!(
            engine.doc.layers[0].pixels.tiles, preview.layers[0].pixels.tiles,
            "{op} preview differs from commit"
        );
        assert_ne!(
            engine.doc.layers[0].pixels.tiles,
            before.layers[0].pixels.tiles
        );
        engine.undo("human").unwrap();
        assert_eq!(
            engine.doc.layers[0].pixels.tiles,
            before.layers[0].pixels.tiles
        );
        engine.redo("human").unwrap();
        assert_eq!(
            engine.doc.layers[0].pixels.tiles,
            preview.layers[0].pixels.tiles
        );
        engine.doc.layers[0].locked = true;
        assert!(engine
            .edit("human", &[command.clone()], None, None, "Painting")
            .is_err());
        engine.doc.layers[0].locked = false;
        engine
            .reserve("artist", "Painting", vec![Scope::layer(&target)])
            .unwrap();
        assert!(engine
            .edit("human", &[command], None, None, "Painting")
            .unwrap_err()
            .contains("Reserved"));
    }
}
