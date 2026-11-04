use peerbrush::{
    brush::{self, Session, Settings, TipKind},
    raster::Raster,
};

#[test]
fn changed_bounds_cover_native_pixels_including_shortened_and_reshaped_tails() {
    for depth in [8, 16] {
        for erase in [false, true] {
            for dynamic in [false, true] {
                let mut source = Raster::new_depth(420, 72, depth);
                for y in 0..72 {
                    for x in 0..420 {
                        source.set16(x, y, [12347, 23459, 34571, 45683]);
                    }
                }
                let settings = Settings {
                    radius: 6.,
                    flow: 0.4,
                    opacity: 0.7,
                    tip: TipKind::Grain,
                    smoothing: if dynamic { 0.4 } else { 0. },
                    taper_end: if dynamic { 50. } else { 0. },
                    ..Default::default()
                };
                let mut session =
                    Session::new(&source, settings, [231, 90, 40, 220], erase, None, None).unwrap();
                let points = [
                    [20., 36.],
                    [120., 36.],
                    [220., 40.],
                    [320., 40.],
                    [395., 38.],
                ];
                let pressures = [0.2, 0.4, 0.8, 0.6, 1.];
                let mut previous = source.clone();
                for count in [1, 2, 3, 5, 2, 5] {
                    let actual = session
                        .update(&points[..count], dynamic.then_some(&pressures[..count]))
                        .unwrap()
                        .clone();
                    let mut expected = source.clone();
                    brush::paint_with_pressure(
                        &mut expected,
                        &points[..count],
                        dynamic.then_some(&pressures[..count]),
                        settings,
                        [231, 90, 40, 220],
                        erase,
                        None,
                        None,
                    )
                    .unwrap();
                    assert_eq!(actual, expected);
                    let bounds = session.last_update_bounds();
                    for y in 0..72 {
                        for x in 0..420 {
                            if actual.get16(x, y) != previous.get16(x, y) {
                                let b = bounds.expect("changed pixel without a dirty region");
                                assert!(x >= b[0] && y >= b[1] && x < b[2] && y < b[3]);
                            }
                        }
                    }
                    let before_invalid = bounds;
                    assert!(session.update(&points[..count], Some(&[])).is_err());
                    assert_eq!(session.last_update_bounds(), before_invalid);
                    previous = actual;
                }
                assert_eq!(
                    source.get16(200, 30),
                    if depth == 16 {
                        [12347, 23459, 34571, 45683]
                    } else {
                        [12347, 23459, 34571, 45683]
                            .map(|v| peerbrush::raster::project16(v) as u16 * 257)
                    }
                );
            }
        }
    }
}

#[test]
fn appended_stroke_bounds_leave_earlier_coverage_out_of_the_redraw() {
    let source = Raster::new(512, 64);
    let mut session = Session::new(
        &source,
        Settings {
            radius: 4.,
            spacing: 0.25,
            ..Default::default()
        },
        [255; 4],
        false,
        None,
        None,
    )
    .unwrap();
    session.update(&[[10., 32.], [300., 32.]], None).unwrap();
    session
        .update(&[[10., 32.], [300., 32.], [400., 32.]], None)
        .unwrap();
    assert!(session.last_update_bounds().unwrap()[0] > 290);
    session.update(&[[10., 32.]], None).unwrap();
    assert!(
        session.last_update_bounds().unwrap()[2] >= 400,
        "removed tail must be restored"
    );
}

#[test]
fn incremental_sessions_are_exact_for_texture_pressure_erase_smoothing_and_taper() {
    let mut source = Raster::new(128, 64);
    for y in 0..64 {
        for x in 0..128 {
            source.set(x, y, [x as u8, y as u8, 90, 170]);
        }
    }
    let before = source.tiles.clone();
    let points = (0..8)
        .map(|i| [12. + i as f32 * 13., 32. + (i as f32 * 0.4).sin() * 9.])
        .collect::<Vec<_>>();
    let pressures = (0..8).map(|i| 0.2 + i as f32 * 0.1).collect::<Vec<_>>();
    let clip = Some([8, 8, 120, 60]);
    let polygon = [[8., 32.], [64., 8.], [120., 32.], [64., 60.]];
    for tip in [
        TipKind::Round,
        TipKind::Dry,
        TipKind::Chalk,
        TipKind::Grain,
        TipKind::Bristle,
    ] {
        for erase in [false, true] {
            for dynamic in [false, true] {
                let settings = Settings {
                    radius: 8.,
                    tip,
                    density: 0.65,
                    flow: 0.3,
                    opacity: 0.6,
                    seed: 37,
                    smoothing: if dynamic { 0.5 } else { 0. },
                    taper_start: if dynamic { 14. } else { 0. },
                    taper_end: if dynamic { 17. } else { 0. },
                    ..Default::default()
                };
                let mut session = Session::new(
                    &source,
                    settings,
                    [200, 90, 30, 230],
                    erase,
                    clip,
                    Some(&polygon),
                )
                .unwrap();
                for count in [1, 2, 4, 6, 8, 4] {
                    let pressure = dynamic.then_some(&pressures[..count]);
                    let mut expected = source.clone();
                    brush::paint_with_pressure(
                        &mut expected,
                        &points[..count],
                        pressure,
                        settings,
                        [200, 90, 30, 230],
                        erase,
                        clip,
                        Some(&polygon),
                    )
                    .unwrap();
                    assert_eq!(
                        session.update(&points[..count], pressure).unwrap().tiles,
                        expected.tiles,
                        "{tip:?}, erase {erase}, dynamic {dynamic}, count {count}"
                    );
                }
            }
        }
    }
    assert_eq!(source.tiles, before, "session changed shared history tiles");
}

#[test]
fn unchanged_prefixes_only_add_new_dabs_and_invalid_updates_preserve_the_session() {
    let source = Raster::new(64, 64);
    let settings = Settings {
        radius: 4.,
        spacing: 0.25,
        ..Default::default()
    };
    let mut session = Session::new(&source, settings, [255; 4], false, None, None).unwrap();
    session.update(&[[10., 32.], [20., 32.]], None).unwrap();
    assert_eq!(session.last_update_dabs(), 6);
    let before = session
        .update(&[[10., 32.], [20., 32.], [30., 32.]], None)
        .unwrap()
        .clone();
    assert_eq!(session.last_update_dabs(), 5);
    assert!(session.update(&[[10., 32.]], Some(&[])).is_err());
    assert_eq!(
        session
            .update(&[[10., 32.], [20., 32.], [30., 32.]], None)
            .unwrap()
            .tiles,
        before.tiles
    );
    assert_eq!(session.last_update_dabs(), 0);
}

#[test]
fn reshaped_tails_replay_only_contributing_tiles_and_preserve_exact_native_coverage() {
    let points = (0..70)
        .map(|i| [28. + i as f32 * 24., 44. + (i as f32 * 0.3).sin() * 12.])
        .collect::<Vec<_>>();
    let pressures = (0..70)
        .map(|i| 0.35 + (i % 7) as f32 * 0.08)
        .collect::<Vec<_>>();
    let clip = Some([11, 9, 2020, 86]);
    let polygon = [[12., 10.], [2010., 15.], [1970., 82.], [16., 86.]];
    for depth in [8, 16] {
        let mut source = Raster::new_depth(2048, 96, depth);
        for y in (0..96).step_by(7) {
            for x in (0..2048).step_by(17) {
                source.set16(x, y, [12347, 23459, 34571, 45683]);
            }
        }
        let original = source.clone();
        for erase in [false, true] {
            for taper_end in [0., 18.] {
                let settings = Settings {
                    radius: 6.,
                    spacing: 0.4,
                    flow: 0.31,
                    opacity: 0.63,
                    smoothing: 0.35,
                    taper_end,
                    tip: TipKind::Grain,
                    texture_origin: [-73, 15],
                    ..Default::default()
                };
                let mut session = Session::new(
                    &source,
                    settings,
                    [231, 90, 40, 220],
                    erase,
                    clip,
                    Some(&polygon),
                )
                .unwrap();
                let before = session
                    .update(&points[..50], Some(&pressures[..50]))
                    .unwrap()
                    .clone();
                for count in [51, 70, 65, 70, 1, 70] {
                    let actual = session
                        .update(&points[..count], Some(&pressures[..count]))
                        .unwrap()
                        .clone();
                    if count == 51 {
                        assert!(
                            session.last_update_dabs() < 100,
                            "unchanged prefix must not be painted again"
                        );
                        assert!(session.last_update_bounds().unwrap()[0] > 900);
                        assert!(
                            if depth == 16 {
                                std::sync::Arc::ptr_eq(
                                    &before.samples16[&(0, 0)],
                                    &actual.samples16[&(0, 0)],
                                )
                            } else {
                                std::sync::Arc::ptr_eq(
                                    &before.tiles[&(0, 0)],
                                    &actual.tiles[&(0, 0)],
                                )
                            },
                            "unchanged prefix tiles must stay shared"
                        );
                    }
                    let mut expected = source.clone();
                    brush::paint_with_pressure(
                        &mut expected,
                        &points[..count],
                        Some(&pressures[..count]),
                        settings,
                        [231, 90, 40, 220],
                        erase,
                        clip,
                        Some(&polygon),
                    )
                    .unwrap();
                    assert_eq!(
                        actual, expected,
                        "native{depth}, erase {erase}, taper {taper_end}, count {count}"
                    );
                }
            }
        }
        assert_eq!(source, original);
    }
}

#[test]
fn invisible_large_strokes_match_regular_paint_without_allocating_tiles() {
    let source = Raster::new(4096, 4096);
    let points = [[512., 2048.], [3584., 2048.]];
    for (opacity, flow, alpha) in [(0.0, 1.0, 255), (1.0, 0.0, 255), (1.0, 1.0, 0)] {
        let settings = Settings {
            radius: 512.,
            spacing: 0.01,
            opacity,
            flow,
            ..Default::default()
        };
        let color = [170, 90, 30, alpha];
        let mut expected = source.clone();
        brush::paint(&mut expected, &points, settings, color, false, None).unwrap();
        let mut session = Session::new(&source, settings, color, false, None, None).unwrap();
        assert_eq!(session.update(&points, None).unwrap().tiles, expected.tiles);
        assert!(expected.tiles.is_empty());
    }
}
