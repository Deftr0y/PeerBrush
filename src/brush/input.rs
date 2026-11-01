//! Charged local geometry derived from one immutable native input generation.
use super::{native::PreparedBrush, Settings};
use crate::{
    edit_bounds::input::{InputBinding, RasterInput},
    large_document::Reservation,
    retouch::native::PreparedRetouch,
    smudge::native::PreparedSmudge,
};

pub(crate) struct LocalStroke {
    binding: InputBinding,
    points: Vec<[f32; 2]>,
    pressures: Option<Vec<f32>>,
    polygon: Option<Vec<[f32; 2]>>,
    clip: Option<[i32; 4]>,
    // Keep admission until the actual Vec capacities above have been released.
    _charge: Reservation,
}
impl LocalStroke {
    pub(crate) fn new(
        input: &RasterInput<'_>,
        points_document: &[[f32; 2]],
        pressures: Option<&[f32]>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if points_document.is_empty()
            || points_document.len() > 10000
            || points_document
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 100000.)
            || pressures.is_some_and(|p| {
                p.len() != points_document.len()
                    || p.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
            })
        {
            return Err("Invalid native document-space stroke".into());
        }
        let selection = input.selection()?;
        let polygon = selection.polygon();
        let point_bytes = points_document
            .len()
            .checked_add(polygon.map_or(0, |p| p.len()))
            .and_then(|n| n.checked_mul(std::mem::size_of::<[f32; 2]>()))
            .ok_or("Native local stroke footprint overflow")?;
        let admitted = point_bytes
            .checked_add(pressures.map_or(0, std::mem::size_of_val))
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_add(128))
            .ok_or("Native local stroke footprint overflow")?;
        let charge = input
            .pool()
            .reserve_bytes(admitted, admitted)
            .map_err(|e| e.to_string())?;
        let shift = input.origin();
        let copy = |source: &[[f32; 2]]| -> Result<Vec<[f32; 2]>, String> {
            let mut values = Vec::new();
            values
                .try_reserve_exact(source.len())
                .map_err(|_| "Native local stroke allocation failed")?;
            for p in source {
                values.push([p[0] - shift[0] as f32, p[1] - shift[1] as f32]);
            }
            Ok(values)
        };
        let points = copy(points_document)?;
        let polygon = polygon.map(copy).transpose()?;
        let pressures = pressures
            .map(|source| {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(source.len())
                    .map_err(|_| "Native pressure allocation failed")?;
                values.extend_from_slice(source);
                Ok::<_, String>(values)
            })
            .transpose()?;
        let actual = points
            .capacity()
            .checked_add(polygon.as_ref().map_or(0, |p| p.capacity()))
            .and_then(|n| n.checked_mul(std::mem::size_of::<[f32; 2]>()))
            .and_then(|n| {
                pressures.as_ref().map_or(Some(n), |p| {
                    p.capacity()
                        .checked_mul(std::mem::size_of::<f32>())
                        .and_then(|b| n.checked_add(b))
                })
            })
            .ok_or("Native stroke capacity overflow")?;
        if actual > admitted {
            return Err("Native stroke allocator exceeded its admission".into());
        }
        check()?;
        Ok(Self {
            binding: input.binding(),
            points,
            pressures,
            polygon,
            clip: input.local_clip()?,
            _charge: charge,
        })
    }
    pub(crate) fn binding(&self) -> InputBinding {
        self.binding
    }
    pub(crate) fn brush<'a>(
        &'a self,
        input: &RasterInput<'_>,
        mut settings: Settings,
        color: [u8; 4],
        erase: bool,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedBrush<'a>, String> {
        input.validate(self.binding)?;
        settings.texture_origin = input.origin();
        PreparedBrush::new(
            input.shape()[0],
            input.shape()[1],
            input.depth(),
            &self.points,
            self.pressures.as_deref(),
            settings,
            input.color(color),
            erase,
            self.clip,
            self.polygon.as_deref(),
            input.pool(),
            check,
        )
    }
    pub(crate) fn retouch<'a>(
        &'a self,
        input: &RasterInput<'_>,
        settings: Settings,
        offset_document: [f32; 2],
        heal_radius: Option<u32>,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedRetouch<'a>, String> {
        input.validate(self.binding)?;
        PreparedRetouch::new(
            input.shape()[0],
            input.shape()[1],
            input.depth(),
            &self.points,
            self.pressures.as_deref(),
            settings,
            input.origin(),
            offset_document,
            heal_radius,
            self.clip,
            self.polygon.as_deref(),
            input.pool(),
            check,
        )
    }
    pub(crate) fn smudge<'a>(
        &'a self,
        input: &RasterInput<'_>,
        mut settings: Settings,
        color: [u8; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<PreparedSmudge<'a>, String> {
        input.validate(self.binding)?;
        settings.texture_origin = input.origin();
        PreparedSmudge::new(
            input.shape()[0],
            input.shape()[1],
            input.depth(),
            &self.points,
            self.pressures.as_deref(),
            settings,
            input.color(color),
            self.clip,
            self.polygon.as_deref(),
            input.pool(),
            check,
        )
    }
}
