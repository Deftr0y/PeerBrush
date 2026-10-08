use peerbrush::{
    brush::{self, Session, Settings, TipKind},
    raster::Raster,
};

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
