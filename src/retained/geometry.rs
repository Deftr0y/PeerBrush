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
}
