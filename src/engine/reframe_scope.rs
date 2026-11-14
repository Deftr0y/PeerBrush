//! Physical mask reframes can change pixels outside the selected transform.
pub(crate) fn widen(
    area: [i32; 4],
    origin: [i32; 2],
    shape: [u32; 2],
    linked: bool,
    steps: impl IntoIterator<Item = [u32; 2]>,
) -> [i32; 4] {
    let frame = bounds(origin, shape);
    let expanded =
        area[0] < frame[0] || area[1] < frame[1] || area[2] > frame[2] || area[3] > frame[3];
    let mut affected = frame;
    let mut reframe = false;
    for step in steps {
        // A linked transform collectively rerenders every step when even one
        // current plane differs from the final color frame. Retained originals
        // can then reveal samples anywhere inside that frame. Expansion also
        // reframes untouched steps, whose old extent can be cropped.
        reframe |= expanded || (linked && step != shape);
        affected = union(affected, bounds(origin, step));
    }
    if reframe {
        union(area, affected)
    } else {
        area
    }
}
fn bounds(origin: [i32; 2], shape: [u32; 2]) -> [i32; 4] {
    [
        origin[0],
        origin[1],
        (i64::from(origin[0]) + i64::from(shape[0])).min(i64::from(i32::MAX)) as i32,
        (i64::from(origin[1]) + i64::from(shape[1])).min(i64::from(i32::MAX)) as i32,
    ]
}
pub(crate) fn union(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collective_linked_reframe_reserves_revealed_and_cropped_mask_extents() {
        assert_eq!(
            widen([4, 6, 9, 12], [2, 3], [17, 19], true, [[7, 9], [31, 29]]),
            [2, 3, 33, 32]
        );
        assert_eq!(
            widen([4, 6, 9, 12], [2, 3], [17, 19], true, [[17, 19]; 2]),
            [4, 6, 9, 12]
        );
        assert_eq!(
            widen([4, 6, 9, 12], [2, 3], [17, 19], false, [[7, 9], [31, 29]]),
            [4, 6, 9, 12]
        );
    }
    #[test]
    fn expansion_covers_untouched_mask_reframing_without_widening_ordinary_planes() {
        assert_eq!(
            widen([-2, 6, 9, 12], [2, 3], [17, 19], false, [[31, 29]]),
            [-2, 3, 33, 32]
        );
        assert_eq!(
            widen([-2, 6, 9, 12], [2, 3], [17, 19], true, []),
            [-2, 6, 9, 12]
        );
    }
}
