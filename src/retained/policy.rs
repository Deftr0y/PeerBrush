//! Native metadata calculations shared by transform preparation, without pixels.
use serde_json::Value;

/// Preserve Source::transformed's separate f32 affine arithmetic. The command's
/// whole-layer coordinate admission runs before this metadata calculation.
pub(crate) fn source_matrix(
    before_origin: [i32; 2],
    before_frame: [u32; 2],
    after_origin: [i32; 2],
    matrix: [f32; 6],
    command: &Value,
) -> Result<[f32; 6], String> {
    source_matrix_with_pivot(
        before_origin,
        before_frame,
        after_origin,
        matrix,
        command,
        None,
    )
}

pub(super) fn source_matrix_with_pivot(
    before_origin: [i32; 2],
    before_frame: [u32; 2],
    after_origin: [i32; 2],
    matrix: [f32; 6],
    command: &Value,
    default_pivot: Option<[f64; 2]>,
) -> Result<[f32; 6], String> {
    if crate::projective::parse(command)?.is_some() || crate::warp::parse(command)?.is_some() {
        return Err("Rasterize the editable source at its native precision before Distort, Perspective or Warp".into());
    }
    if command["op"] == "move" {
        return Ok(matrix);
    }
    let n = |key: &str, default: f32| command[key].as_f64().map(|v| v as f32).unwrap_or(default);
    let (mut sin, mut cos) = n("angle", 0.).to_radians().sin_cos();
    if sin.abs() < 0.000001 {
        sin = 0.;
    }
    if cos.abs() < 0.000001 {
        cos = 0.;
    }
    let (sx, sy) = crate::transform::scales(command)?;
    let (sx, sy) = (sx as f32, sy as f32);
    let px = command["pivot"][0]
        .as_f64()
        .map(|v| v as f32)
        .unwrap_or_else(|| {
            default_pivot.map_or(
                before_origin[0] as f32 + before_frame[0] as f32 * 0.5,
                |p| p[0] as f32,
            )
        });
    let py = command["pivot"][1]
        .as_f64()
        .map(|v| v as f32)
        .unwrap_or_else(|| {
            default_pivot.map_or(
                before_origin[1] as f32 + before_frame[1] as f32 * 0.5,
                |p| p[1] as f32,
            )
        });
    let [a, b, c, d] = crate::transform::skewed(
        [cos * sx, sin * sx, -sin * sy, cos * sy].map(f64::from),
        command,
    )?
    .map(|v| v as f32);
    let [dx, dy] = crate::transform::translation(command)?.map(|v| v as f32);
    let mut delta = [
        a,
        b,
        c,
        d,
        px + a * (before_origin[0] as f32 - px) + c * (before_origin[1] as f32 - py)
            - after_origin[0] as f32,
        py + b * (before_origin[0] as f32 - px) + d * (before_origin[1] as f32 - py)
            - after_origin[1] as f32,
    ];
    if dx != 0. {
        delta[4] += dx;
    }
    if dy != 0. {
        delta[5] += dy;
    }
    let [a, b, c, d, x, y] = matrix;
    let [e, f, g, h, u, v] = delta;
    Ok([
        e * a + g * b,
        f * a + h * b,
        e * c + g * d,
        f * c + h * d,
        e * x + g * y + u,
        f * x + h * y + v,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::Layer,
        source::{Content, Source},
    };
    use serde_json::json;

    #[test]
    fn editable_source_matrix_matches_actual_legacy_transform_f32_bits() {
        for depth in [8, 16] {
            let source = Source {
                width: 9,
                height: 11,
                matrix: [0.87, 0.13, -0.07, 1.11, 2.25, -1.5],
                content: Content::Shape {
                    shape: "rectangle".into(),
                    bounds: [1., 2., 7., 9.],
                    points: Vec::new(),
                    closed: true,
                    fill: [10001, 20003, 30007, 65535],
                    stroke: [0; 4],
                    stroke_width: 0.,
                },
            };
            let mut before = Layer::new("Affine source", "paint", 17, 19);
            before.pixels = source.render(17, 19, depth).unwrap();
            before.source = Some(source.clone().into());
            before.x = -7;
            before.y = 13;
            for command in [
                json!({"op":"move","dx":3,"dy":-4}),
                json!({"op":"transform","angle":90}),
                json!({"op":"transform","angle":-23.125,"scale_x":0.73,"scale_y":1.17,"mirror_x":true,"dx":2.75,"dy":-3.25,"fractional_translation":true}),
                json!({"op":"transform","angle":45.5,"skew_x":17.25,"skew_y":-9.5,"pivot":[3.125,-8.75],"dx":3.5,"dy":-4.5}),
            ] {
                let mut after = before.clone();
                after.x = -11;
                after.y = 8;
                crate::source::transformed(&before, &mut after, &command).unwrap();
                let actual = source_matrix(
                    [before.x, before.y],
                    [before.pixels.width, before.pixels.height],
                    [after.x, after.y],
                    source.matrix,
                    &command,
                )
                .unwrap();
                assert_eq!(
                    actual.map(f32::to_bits),
                    after.source.as_ref().unwrap().matrix.map(f32::to_bits)
                );
                assert_eq!(after.pixels.depth, depth);
            }
        }
    }
}
