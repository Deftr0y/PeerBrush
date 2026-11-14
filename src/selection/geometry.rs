//! Shared scan conversion for resident and native geometric selections.
use serde_json::Value;

pub(crate) struct Geometry<'a> {
    kind: &'a str,
    area: [i32; 4],
    points: Option<&'a [[f32; 2]]>,
}
impl<'a> Geometry<'a> {
    pub(crate) fn new(
        canvas: [u32; 2],
        c: &'a Value,
        points: Option<&'a [[f32; 2]]>,
    ) -> Result<Self, String> {
        let kind = c["kind"].as_str().unwrap_or("rectangle");
        if ![
            "rectangle",
            "ellipse",
            "lasso",
            "polygon",
            "magnetic",
            "row",
            "column",
            "wand",
            "quick",
            "object",
        ]
        .contains(&kind)
        {
            return Err("Unknown selection kind".into());
        }
        if points.is_none() && ["lasso", "polygon", "magnetic"].contains(&kind) {
            return Err("Lasso selections need polygon points".into());
        }
        let mut area = if c["rect"].is_null() {
            [0, 0, canvas[0] as i32, canvas[1] as i32]
        } else {
            let values = c["rect"]
                .as_array()
                .filter(|v| v.len() == 4)
                .ok_or("Selection rectangle needs four integer coordinates")?;
            let mut area = [0; 4];
            for (out, value) in area.iter_mut().zip(values) {
                *out = value
                    .as_i64()
                    .filter(|v| v.unsigned_abs() <= 100000)
                    .ok_or("Invalid selection rectangle coordinates")?
                    as i32;
            }
            area
        };
        if let Some(points) = points {
            area = super::bounds(points)?;
        }
        Ok(Self { kind, area, points })
    }
    pub(crate) fn points(&self) -> usize {
        self.points.map_or(0, <[_]>::len)
    }
    pub(crate) fn bounds(&self) -> [i32; 4] {
        self.area
    }
    /// The caller admits the row and one crossing per polygon edge before use.
    pub(crate) fn row(&self, y: u32, row: &mut [u8], crossings: &mut Vec<f32>) {
        self.row_at(0, y as i32, row, crossings);
    }
    pub(crate) fn row_at(&self, x: i32, y: i32, row: &mut [u8], crossings: &mut Vec<f32>) {
        row.fill(0);
        let end = x + row.len() as i32;
        let area = self.area;
        if y < area[1] || y >= area[3] {
            return;
        }
        if let Some(points) = self.points {
            crossings.clear();
            let cy = y as f32 + 0.5;
            for (a, b) in points
                .iter()
                .zip(points.iter().cycle().skip(1))
                .take(points.len())
            {
                if (a[1] > cy) != (b[1] > cy) {
                    crossings.push(a[0] + (b[0] - a[0]) * (cy - a[1]) / (b[1] - a[1]));
                }
            }
            crossings.sort_by(f32::total_cmp);
            for pair in crossings.chunks_exact(2) {
                let left = ((pair[0] - 0.5).ceil() as i32).max(x);
                let right = ((pair[1] - 0.5).floor() as i32).min(end - 1);
                if left <= right {
                    row[(left - x) as usize..=(right - x) as usize].fill(255);
                }
            }
            return;
        }
        let left = area[0].max(x);
        let right = area[2].min(end);
        if left >= right {
            return;
        }
        if self.kind != "ellipse" {
            row[(left - x) as usize..(right - x) as usize].fill(255);
            return;
        }
        let rx = (area[2] - area[0]) as f32 / 2.;
        let ry = (area[3] - area[1]) as f32 / 2.;
        let y_squared = ((y as f32 + 0.5 - area[1] as f32 - ry) / ry).powi(2);
        let span = &mut row[(left - x) as usize..(right - x) as usize];
        for (column, value) in (left..right).zip(span) {
            if ((column as f32 + 0.5 - area[0] as f32 - rx) / rx).powi(2) + y_squared <= 1. {
                *value = 255;
            }
        }
    }
}
