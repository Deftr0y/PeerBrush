//! Placement validation shared by resident and native retained-source owners.
use crate::{
    projective::Map,
    warp::{Placement, Stage},
};

pub(crate) fn exact(value: f64) -> f64 {
    if (value - value.round()).abs() < 1e-8 {
        value.round()
    } else {
        value
    }
}

pub(crate) fn placement(matrix: [f64; 6], projective: Option<[f64; 9]>) -> Result<Map, String> {
    let affine = Map::affine_matrix(matrix);
    projective
        .map(Map::from_matrix)
        .transpose()?
        .map_or(Ok(affine), |p| affine.compose(p))
}

/// Map logical source support using the unchanged resident placement arithmetic.
/// Source bounds, layout and prepared metadata admission belong to the caller.
pub(crate) fn bounds(
    source: [i32; 4],
    matrix: [f64; 6],
    projective: Option<[f64; 9]>,
    warps: &[Stage],
) -> Result<[f64; 4], String> {
    if !warps.is_empty() {
        return Placement::new(warps, placement(matrix, projective)?)?
            .bounds(source.map(f64::from));
    }
    if projective.is_some() {
        return placement(matrix, projective)?.bounds(source.map(f64::from));
    }
    let [a, b, c, d, tx, ty] = matrix;
    let ps = [
        [source[0], source[1]],
        [source[2], source[1]],
        [source[2], source[3]],
        [source[0], source[3]],
    ]
    .map(|[x, y]| {
        [
            exact(a * x as f64 + c * y as f64 + tx),
            exact(b * x as f64 + d * y as f64 + ty),
        ]
    });
    Ok([
        ps.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
        ps.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
        ps.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max),
        ps.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max),
    ])
}

/// Source layout/owner/metadata admission remains the caller's responsibility.
/// This checks no Raster, backing file or live document-size policy.
pub(crate) fn validate(
    width: u32,
    height: u32,
    depth: u16,
    expected_depth: u16,
    matrix: [f64; 6],
    projective: Option<[f64; 9]>,
    warps: &[Stage],
) -> Result<(), String> {
    let [a, b, c, d, _, _] = matrix;
    if width == 0
        || height == 0
        || width > 16000
        || height > 16000
        || ![8, 16].contains(&depth)
        || depth != expected_depth
        || matrix.iter().any(|v| !v.is_finite() || v.abs() > 1e8)
        || (a * d - b * c).abs() < 1e-12
    {
        return Err("Invalid retained transform source or placement".into());
    }
    if !warps.is_empty() {
        Placement::new(warps, placement(matrix, projective)?)?.bounds([
            -1.,
            -1.,
            width as f64 + 1.,
            height as f64 + 1.,
        ])?;
    } else if projective.is_some() {
        placement(matrix, projective)?.bounds([0., 0., width as f64, height as f64])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_keep_raw_placement_coefficients_and_full_logical_support() {
        let raw = [1. + 1e-9, 0., 0., 1., 0., 0.];
        let b = bounds([0, 0, 16000, 5], raw, None, &[]).unwrap();
        assert_eq!([b[0], b[1], b[3]], [0., 0., 5.]);
        assert!(b[2] > 16000.00001);
        assert_eq!(
            bounds([0, 0, 6, 7], [1., 0., 0., 1., 2., 3.], None, &[]).unwrap(),
            [2., 3., 8., 10.]
        );
        let projective = Some([1., 0., 2., 0., 1., 3., 0., 0., 1.]);
        assert_eq!(
            bounds([0, 0, 6, 7], crate::retained::IDENTITY, projective, &[]).unwrap(),
            [2., 3., 8., 10.]
        );
    }

    #[test]
    fn geometry_keeps_depth_determinant_horizon_and_warp_support_checks_without_live_caps() {
        assert!(validate(16000, 16000, 16, 16, crate::retained::IDENTITY, None, &[]).is_ok());
        for (w, h, depth, expected, matrix) in [
            (0, 17, 8, 8, crate::retained::IDENTITY),
            (17, 16001, 8, 8, crate::retained::IDENTITY),
            (17, 19, 16, 8, crate::retained::IDENTITY),
            (17, 19, 8, 8, [1., 0., 0., 0., 0., 0.]),
            (17, 19, 8, 8, [f64::NAN, 0., 0., 1., 0., 0.]),
        ] {
            assert!(validate(w, h, depth, expected, matrix, None, &[]).is_err());
        }
        assert!(validate(
            17,
            19,
            8,
            8,
            crate::retained::IDENTITY,
            Some([1., 0., 0., 0., 1., 0., 1., 0., -8.]),
            &[]
        )
        .is_err());
        let grid = crate::warp::Grid::identity([0., 0., 17., 19.]).unwrap();
        let stage = Stage {
            before: Map::IDENTITY.matrix(),
            grid,
        };
        assert!(validate(17, 19, 8, 8, crate::retained::IDENTITY, None, &[stage]).is_err());
        let grid = crate::warp::Grid::identity([-1., -1., 18., 20.]).unwrap();
        let stage = Stage {
            before: Map::IDENTITY.matrix(),
            grid,
        };
        assert!(validate(17, 19, 8, 8, crate::retained::IDENTITY, None, &[stage]).is_ok());
    }

    #[test]
    fn small_metadata_admission_matches_actual_resident_original_validation() {
        for depth in [8, 16] {
            let pixels = crate::raster::Raster::new_depth(17, 19, depth);
            let valid = Stage {
                before: Map::IDENTITY.matrix(),
                grid: crate::warp::Grid::identity([-1., -1., 18., 20.]).unwrap(),
            };
            let mut folded = valid.clone();
            folded.grid.points[4] = [40., 10.];
            for (matrix, projective, warps) in [
                (crate::retained::IDENTITY, None, Vec::new()),
                ([1., 0.2, 0.1, 0.8, 3.25, -7.5], None, Vec::new()),
                ([1., 0., 0., 0., 0., 0.], None, Vec::new()),
                (
                    crate::retained::IDENTITY,
                    Some([1., 0., 0., 0., 1., 0., 1., 0., -8.]),
                    Vec::new(),
                ),
                (crate::retained::IDENTITY, None, vec![valid]),
                (crate::retained::IDENTITY, None, vec![folded]),
            ] {
                let mut old = crate::retained::Original::from(&pixels);
                old.matrix = matrix;
                old.projective = projective;
                old.warps = serde_json::from_value(serde_json::to_value(&warps).unwrap()).unwrap();
                assert_eq!(
                    validate(17, 19, depth, depth, matrix, projective, &warps).is_ok(),
                    old.validate(depth).is_ok()
                );
            }
        }
    }
}
