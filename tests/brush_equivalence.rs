//! Byte-for-byte comparison against the original brush implementation, across tile borders.
use peerbrush::{
    brush::{self, Settings},
    raster::{blend, Pixel, Raster, TILE},
};
fn reference_coverage(settings: Settings, dx: f32, dy: f32) -> f32 {
    let (sin, cos) = settings.angle.to_radians().sin_cos();
    let u = (dx * cos + dy * sin) / settings.radius;
    let v = (-dx * sin + dy * cos) / (settings.radius * settings.roundness);
    let distance = (u * u + v * v).sqrt();
    let gradient = if distance > 0.0001 {
        (u / settings.radius).hypot(v / (settings.radius * settings.roundness)) / distance
    } else {
        1.0 / settings.radius
    };
    let edge = (1.0 - settings.hardness).max(gradient);
    ((1.0 - distance + gradient * 0.5) / edge).clamp(0.0, 1.0)
}

fn reference_paint(
    raster: &mut Raster,
    points: &[[f32; 2]],
    settings: Settings,
    color: Pixel,
    erase: bool,
    clip: Option<[i32; 4]>,
) -> Result<(), String> {
    if points.is_empty()
        || points.len() > 10000
        || points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 100000.0)
    {
        return Err("Invalid stroke points".into());
    }
    if settings.opacity == 0.0 || settings.flow == 0.0 || color[3] == 0 {
        return Ok(());
    }
    let clip = clip.unwrap_or([0, 0, raster.width as i32, raster.height as i32]);
    let clip = [
        clip[0].max(0),
        clip[1].max(0),
        clip[2].min(raster.width as i32),
        clip[3].min(raster.height as i32),
    ];
    let step = (settings.radius * 2.0 * settings.spacing).max(0.5);
    let mut dabs = vec![points[0]];
    let mut previous = points[0];
    let mut until_next = step;
    for (index, &point) in points.iter().enumerate().skip(1) {
        let weight = if index + 1 == points.len() {
            1.0
        } else {
            1.0 - settings.smoothing * 0.9
        };
        let target = [
            previous[0] + (point[0] - previous[0]) * weight,
            previous[1] + (point[1] - previous[1]) * weight,
        ];
        let dx = target[0] - previous[0];
        let dy = target[1] - previous[1];
        let distance = dx.hypot(dy);
        let mut along = until_next;
        while along <= distance {
            dabs.push([
                previous[0] + dx * along / distance,
                previous[1] + dy * along / distance,
            ]);
            if dabs.len() > 16000 {
                return Err("Stroke is too long; split it into shorter strokes".into());
            }
            along += step;
        }
        until_next = along - distance;
        previous = target;
    }
    // Reject pathological work before allocating or editing. Clipping also skips off-canvas dabs.
    let bounds = |p: [f32; 2]| {
        [
            (p[0] - settings.radius - 1.0).floor() as i32,
            (p[1] - settings.radius - 1.0).floor() as i32,
            (p[0] + settings.radius + 1.0).ceil() as i32,
            (p[1] + settings.radius + 1.0).ceil() as i32,
        ]
    };
    let mut work = 0u64;
    for &p in &dabs {
        let b = bounds(p);
        work += (b[2].min(clip[2]) - b[0].max(clip[0])).max(0) as u64
            * (b[3].min(clip[3]) - b[1].max(clip[1])).max(0) as u64;
        if work > 250_000_000 {
            return Err(
                "Stroke exceeds the work budget; use shorter strokes or wider spacing".into(),
            );
        }
    }
    let mut coverage = Raster::new(raster.width, raster.height);
    for p in dabs {
        let b = bounds(p);
        for y in b[1].max(clip[1])..b[3].min(clip[3]) {
            for x in b[0].max(clip[0])..b[2].min(clip[2]) {
                let alpha =
                    reference_coverage(settings, x as f32 + 0.5 - p[0], y as f32 + 0.5 - p[1])
                        * settings.flow;
                if alpha <= 0.0 {
                    continue;
                }
                let old = coverage.get(x, y)[3] as f32 / 255.0;
                coverage.set(
                    x,
                    y,
                    [0, 0, 0, ((old + alpha * (1.0 - old)) * 255.0).round() as u8],
                );
            }
        }
    }
    for (&(tx, ty), tile) in &coverage.tiles {
        for (i, p) in tile.chunks_exact(4).enumerate() {
            if p[3] == 0 {
                continue;
            }
            let x = (tx * TILE + i as u32 % TILE) as i32;
            let y = (ty * TILE + i as u32 / TILE) as i32;
            let alpha = p[3] as f32 / 255.0 * settings.opacity;
            let old = raster.get(x, y);
            let mut src = color;
            src[3] = (color[3] as f32 * alpha).round() as u8;
            raster.set(
                x,
                y,
                if erase {
                    [
                        old[0],
                        old[1],
                        old[2],
                        (old[3] as f32 * (1.0 - src[3] as f32 / 255.0)).round() as u8,
                    ]
                } else {
                    blend(old, src, 1.0, "normal")
                },
            );
        }
    }
    Ok(())
}

fn patterned_raster() -> Raster {
    let mut r = Raster::new(530, 410);
    for y in 0..410 {
        for x in 0..530 {
            if (x + 2 * y) % 7 < 3 {
                r.set(
                    x,
                    y,
                    [
                        (x % 251) as u8,
                        (y % 241) as u8,
                        ((x + y) % 239) as u8,
                        ((3 * x + y) % 256) as u8,
                    ],
                );
            }
        }
    }
    r
}
#[test]
fn tiled_brush_matches_original_pixels_and_preserves_history_source() {
    let source = patterned_raster();
    let original_bytes = source.rgba();
    let strokes = [
        vec![
            [-8.7, 6.3],
            [252.25, 254.75],
            [269.2, 258.8],
            [541.0, 422.0],
        ],
        vec![
            [260.3, 251.6],
            [240.2, 270.8],
            [280.6, 250.1],
            [240.2, 270.8],
        ],
        vec![[519.7, 7.2]],
    ];
    for case in 0..48 {
        let settings = Settings {
            radius: [0.5, 3.75, 32.0, 64.5][case % 4],
            hardness: [0.0, 0.35, 0.72, 1.0][(case / 2) % 4],
            opacity: [0.0, 0.27, 0.73, 1.0][(case / 3) % 4],
            flow: [0.03, 0.4, 1.0][(case / 2) % 3],
            spacing: [0.03, 0.15, 1.4][case % 3],
            roundness: [0.05, 0.42, 1.0][case % 3],
            angle: [-179.5, -48.0, 0.0, 35.2, 90.0][case % 5],
            smoothing: [0.0, 0.37, 1.0][(case / 3) % 3],
        };
        let clip = (case % 3 == 0).then_some([245, 12, 510, 389]);
        let color = [233, 82, 19, [0, 71, 193, 255][(case / 2) % 4]];
        let points = &strokes[case % strokes.len()];
        let initial = if case % 3 == 0 {
            Raster::new(530, 410)
        } else {
            source.clone()
        };
        let mut actual = initial.clone();
        let mut expected = initial.clone();
        brush::paint(&mut actual, points, settings, color, case % 2 == 1, clip).unwrap();
        reference_paint(&mut expected, points, settings, color, case % 2 == 1, clip).unwrap();
        assert_eq!(actual.tiles, expected.tiles, "case {case}");
    }
    assert_eq!(
        source.rgba(),
        original_bytes,
        "shared history tiles were modified"
    );
}

#[test]
fn rejected_brush_work_never_changes_target_tiles() {
    let mut actual = patterned_raster();
    let original = actual.tiles.clone();
    let points = [[0.0, 0.0], [100000.0, 100000.0]];
    let too_many_dabs = Settings {
        radius: 0.5,
        spacing: 0.01,
        ..Default::default()
    };
    assert!(brush::paint(&mut actual, &points, too_many_dabs, [255; 4], false, None).is_err());
    assert_eq!(actual.tiles, original);
    let mut large = Raster::new(4096, 4096);
    large.set(4, 4, [233, 82, 19, 255]);
    let original = large.tiles.clone();
    let points = (0..400)
        .map(|i| {
            if i % 2 == 0 {
                [512.0, 512.0]
            } else {
                [3584.0, 3584.0]
            }
        })
        .collect::<Vec<_>>();
    let too_much_work = Settings {
        radius: 512.0,
        spacing: 2.0,
        ..Default::default()
    };
    let result = brush::paint(&mut large, &points, too_much_work, [255; 4], false, None);
    assert!(result.unwrap_err().contains("work budget"));
    assert_eq!(large.tiles, original);
}

#[test]
fn invisible_stroke_does_not_allocate_destination_tiles() {
    let mut r = Raster::new(512, 512);
    let settings = Settings {
        radius: 10.0,
        opacity: 0.0001,
        ..Default::default()
    };
    brush::paint(&mut r, &[[255.0, 255.0]], settings, [255; 4], false, None).unwrap();
    assert!(r.tiles.is_empty());
    brush::paint(
        &mut r,
        &[[255.0, 255.0]],
        Settings::default(),
        [255; 4],
        true,
        None,
    )
    .unwrap();
    assert!(r.tiles.is_empty());
}

#[test]
fn early_tip_regions_match_original_coverage_across_supported_extremes() {
    for radius in [0.5, 1.0, 2.25, 64.0, 512.0] {
        for roundness in [0.05, 0.15, 0.65, 1.0] {
            for hardness in [0.0, 0.02, 0.5, 0.99, 1.0] {
                for angle in [-180.0, -37.0, 83.3] {
                    let settings = Settings {
                        radius,
                        roundness,
                        hardness,
                        angle,
                        ..Default::default()
                    };
                    let (sin, cos) = angle.to_radians().sin_cos();
                    for direction in 0..8 {
                        let theta = direction as f32 * std::f32::consts::TAU / 8.0;
                        for distance in [
                            0.0, 0.00005, 0.02, 0.1, 0.3, 0.5, 0.8, 0.99, 1.0, 1.001, 1.01, 1.1,
                            1.5, 3.0, 12.0,
                        ] {
                            let u = distance * theta.cos() * radius;
                            let v = distance * theta.sin() * radius * roundness;
                            let dx = u * cos - v * sin;
                            let dy = u * sin + v * cos;
                            assert_eq!(settings.coverage(dx,dy), reference_coverage(settings,dx,dy), "radius {radius}, roundness {roundness}, hardness {hardness}, angle {angle}, distance {distance}");
                        }
                    }
                }
            }
        }
    }
}
