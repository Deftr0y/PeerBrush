//! Shared tetrahedral interpolation for embedded RGB lookup tables.
pub(crate) const MAX_GRID: usize = 33;

pub(crate) struct Clut {
    grid: [usize; 3],
    strides: [usize; 3],
    values: Box<[f64]>,
}

impl Clut {
    pub(super) fn icc_grid(&self, swap_input: bool) -> [usize; 3] {
        let mut grid = self.grid;
        if swap_input {
            grid.swap(0, 2);
        }
        grid
    }

    pub(super) fn icc_nodes(&self, swap: [bool; 2]) -> impl Iterator<Item = f32> + '_ {
        let [r, g, b] = self.icc_grid(swap[0]);
        (0..r * g * b).flat_map(move |index| {
            let mut coordinate = [index / (g * b), index / b % g, index % b];
            if swap[0] {
                coordinate.swap(0, 2);
            }
            let start: usize = coordinate
                .into_iter()
                .zip(self.strides)
                .map(|(i, stride)| i * stride)
                .sum();
            let mut rgb = [
                self.values[start],
                self.values[start + 1],
                self.values[start + 2],
            ];
            if swap[1] {
                rgb.swap(0, 2);
            }
            rgb.map(|channel| channel as f32)
        })
    }
    pub(crate) fn new(grid: [usize; 3], values: Box<[f64]>, red_fastest: bool) -> Self {
        let [r, g, b] = grid;
        let strides = if red_fastest {
            [3, r * 3, r * g * 3]
        } else {
            [g * b * 3, b * 3, 3]
        };
        debug_assert_eq!(values.len(), r * g * b * 3);
        Self {
            grid,
            strides,
            values,
        }
    }

    pub(crate) fn sample(&self, rgb: [f64; 3]) -> [f64; 3] {
        let positions: [(usize, f64); 3] =
            std::array::from_fn(|i| grid_position(rgb[i], self.grid[i]));
        let corner0: usize = positions
            .iter()
            .zip(self.strides)
            .map(|(&(low, _), stride)| low * stride)
            .sum();
        let mut order = std::array::from_fn(|i| (positions[i].1, self.strides[i]));
        // Each tetrahedron follows descending fractional coordinates.
        if order[0].0 < order[1].0 {
            order.swap(0, 1);
        }
        if order[1].0 < order[2].0 {
            order.swap(1, 2);
        }
        if order[0].0 < order[1].0 {
            order.swap(0, 1);
        }
        let [(first, step1), (second, step2), (third, step3)] = order;
        let corner1 = corner0 + step1;
        let corner2 = corner1 + step2;
        let corner3 = corner2 + step3;
        std::array::from_fn(|i| {
            let v0 = self.values[corner0 + i];
            let v1 = self.values[corner1 + i];
            let v2 = self.values[corner2 + i];
            let v3 = self.values[corner3 + i];
            v0 + (v1 - v0) * first + (v2 - v1) * second + (v3 - v2) * third
        })
    }
}

pub(super) fn grid_position(value: f64, size: usize) -> (usize, f64) {
    let position = value.clamp(0., 1.) * (size - 1) as f64;
    let low = (position as usize).min(size - 2);
    (low, position - low as f64)
}
