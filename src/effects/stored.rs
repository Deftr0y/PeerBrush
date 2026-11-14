//! Complete native effect stages in operation-owned storage. Global style inputs
//! are computed once; cropped reads never substitute for full-layer statistics.
use super::{alpha::AlphaReduction, field, patterns, regional, styles, Effect};
use crate::{
    large_document::{ByteRole, ScratchBytes},
    raster::backed::{Region, MAX_REGION_PIXELS},
    workspace::source_runtime::{SessionBytes, SourceSession},
};
use std::{cell::RefCell, sync::Arc};
type Result<T> = std::result::Result<T, String>;
type Check<'a> = &'a mut dyn FnMut() -> Result<()>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub size: [u32; 2],
    pub origin: [i32; 2],
}
impl Frame {
    fn crop(self, bounds: [i32; 4]) -> Option<Self> {
        let [left, top, right, bottom] = bounds.map(i64::from);
        let [x, y] = self.origin.map(i64::from);
        let left = left.max(x);
        let top = top.max(y);
        let right = right.min(x + i64::from(self.size[0]));
        let bottom = bottom.min(y + i64::from(self.size[1]));
        (left < right && top < bottom).then(|| Self {
            origin: [left as i32, top as i32],
            size: [(right - left) as u32, (bottom - top) as u32],
        })
    }

    fn expand(self, halo: i32) -> [i32; 4] {
        let [x, y] = self.origin;
        [x.saturating_sub(halo), y.saturating_sub(halo),
            x.saturating_add(self.size[0] as i32).saturating_add(halo),
            y.saturating_add(self.size[1] as i32).saturating_add(halo)]
    }

    fn region_from(self, other: Self, region: Region) -> Result<Region> {
        let mut bounds = region.bounds();
        for (i, edge) in bounds.iter_mut().enumerate() {
            let axis = i % 2;
            *edge = i32::try_from(i64::from(*edge) + i64::from(other.origin[axis]) - i64::from(self.origin[axis]))
                .map_err(|_| "Stored stage coordinate overflow")?;
        }
        Region::new(bounds).map_err(|e| e.to_string())
    }
}
pub(crate) struct Image<'a> {
    bytes: SessionBytes<'a>,
    frame: Frame,
    depth: u16,
    binding: [u8; 32],
}
impl<'a> Image<'a> {
    pub(crate) fn frame(&self) -> Frame {
        self.frame
    }
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
    pub(crate) fn build(
        session: &'a SourceSession,
        frame: Frame,
        depth: u16,
        binding: [u8; 32],
        check: Check<'_>,
        mut render: impl FnMut(Region, Check<'_>) -> Result<regional::RegionalImage>,
    ) -> Result<Self> {
        regional::Frame::new(frame.size[0], frame.size[1])?;
        if ![8, 16].contains(&depth) || binding == [0; 32] {
            return Err(
                "Stored effect pixels require native precision and a captured binding".into(),
            );
        }
        check()?;
        let length = u64::from(frame.size[0]) * u64::from(frame.size[1]) * u64::from(depth / 2);
        let mut temporary = session.byte_temporary(length, ByteRole::StraightNativeRgba)?;
        let written = (|| {
            for region in bands(frame.size)? {
                session.check()?;
                check()?;
                let pixels = render(region, check)?;
                if pixels.bounds() != region.bounds()
                    || pixels.depth() != depth
                    || !pixels.uses_budget(&session.resident())
                {
                    return Err(
                        "Stored effect output differs from its admitted native stripe".into(),
                    );
                }
                temporary.append(pixels.bytes(), check)?;
            }
            check()
        })();
        if let Err(error) = written {
            return Err(combine(error, temporary.abort(&mut || Ok(()))));
        }
        // A private complete seal can be retired on cancellation; a consumed,
        // interrupted finish leaves only the persistent quarantine inventory.
        let bytes = temporary.finish(binding, &mut || session.check())?;
        if let Err(error) = check() {
            return Err(combine(error, bytes.retire(&mut || Ok(()))));
        }
        Ok(Self {
            bytes,
            frame,
            depth,
            binding,
        })
    }
    pub(crate) fn read(
        &self,
        session: &SourceSession,
        region: Region,
        check: Check<'_>,
    ) -> Result<regional::RegionalImage> {
        self.bytes.check_session(session)?;
        if self.bytes.bytes().binding() != self.binding
            || self.bytes.bytes().role() != ByteRole::StraightNativeRgba
        {
            return Err("Stored effect pixels lost their captured binding".into());
        }
        let [l, t, r, b] = region.bounds();
        if l < 0 || t < 0 || r > self.frame.size[0] as i32 || b > self.frame.size[1] as i32 {
            return Err("Stored effect read exceeds its native frame".into());
        }
        let stride = usize::from(self.depth / 2);
        let length = region.pixels() * stride;
        let mut pixels =
            ScratchBytes::new(&session.resident(), length, length).map_err(|e| e.to_string())?;
        let row_stride = u64::from(self.frame.size[0]) * stride as u64;
        let offset = t as u64 * row_stride + l as u64 * stride as u64;
        let mut interrupted = None;
        // The namespace is checked around the batch. Every newly loaded
        // chunk comes from the held file and must match its sealed hash.
        self.bytes
            .bytes()
            .read_strided_into(offset, (r - l) as usize * stride, row_stride, &mut pixels, &mut || {
                check().map_err(|error| {
                    interrupted = Some(error);
                    crate::large_document::Error::Cancelled
                })
            })
            .map_err(|e| interrupted.unwrap_or_else(|| e.to_string()))?;
        self.bytes.check()?;
        check()?;
        regional::RegionalImage::from_native(region, self.depth, pixels.freeze())
    }
    pub(crate) fn retire(self, check: Check<'_>) -> Result<()> {
        self.bytes.retire(check)
    }

    pub(crate) fn read_document(
        &self, session: &SourceSession, region: Region, check: Check<'_>,
    ) -> Result<regional::RegionalImage> {
        let bounds = region.bounds();
        let local = Region::new(std::array::from_fn(|i| bounds[i] - self.frame.origin[i % 2]))
            .map_err(|e| e.to_string())?;
        self.read(session, local, check)?.rebase(region)
    }
}

pub(crate) fn filter_stack<'a>(
    session: &'a SourceSession,
    binding: [u8; 32],
    canvas: [u32; 2],
    bounds: [i32; 4],
    depth: u16,
    filters: &[Effect],
    check: Check<'_>,
    mut source: impl FnMut(Region, Check<'_>) -> Result<regional::RegionalImage>,
) -> Result<Image<'a>> {
    let frame = regional::Frame::new(canvas[0], canvas[1])?;
    if bounds != frame.expand(bounds, 0) || bounds[0] >= bounds[2] || bounds[1] >= bounds[3]
        || filters.len() > crate::filters::LIMIT || binding == [0; 32] || ![8, 16].contains(&depth)
    {
        return Err("Invalid stored filter frame or stack".into());
    }
    let active = filters.iter().enumerate().filter(|(_, f)| f.enabled && f.weight > 0.);
    let mut footprints = [[0; 4]; crate::filters::LIMIT];
    let mut needed = bounds;
    // Each retained stage covers exactly the input halo of its successor.
    for (index, filter) in active.clone().rev() {
        check()?;
        footprints[index] = needed;
        needed = frame.expand(needed, regional::reach(&filter.kind, &filter.settings)?);
    }
    let full = Frame { size: canvas, origin: [0, 0] };
    let mut current: Option<Image<'a>> = None;
    for (index, filter) in active {
        let [l, t, r, b] = footprints[index];
        let output = Frame { size: [(r - l) as u32, (b - t) as u32], origin: [l, t] };
        let previous = current.take();
        let next = one_stage(
            session, stage_binding(binding, index, output, depth), full, output, depth, filter,
            check, |region, guard| {
                if let Some(image) = &previous {
                    image.read_document(session, region, guard)
                } else {
                    source(region, guard)
                }
            },
        );
        let cleanup = previous.map_or(Ok(()), |image| image.retire(&mut || Ok(())));
        current = Some(settle(next, cleanup)?);
    }
    current.ok_or_else(|| "Stored filter stack has no active stages".into())
}

pub(crate) fn stack<'a>(
    session: &'a SourceSession,
    binding: [u8; 32],
    source_frame: Frame,
    canvas: [u32; 2],
    depth: u16,
    effects: &[Effect],
    crop: Option<[i32; 4]>,
    mut prepare_liquify: impl FnMut(
        usize,
        regional::Frame,
        Check<'_>,
    ) -> Result<crate::liquify::regional::Mapping>,
    check: Check<'_>,
    mut source: impl FnMut(Region, Check<'_>) -> Result<regional::RegionalImage>,
) -> Result<Image<'a>> {
    regional::Frame::new(canvas[0], canvas[1])?;
    regional::Frame::new(source_frame.size[0], source_frame.size[1])?;
    if effects.len() > 32 || binding == [0; 32] || ![8, 16].contains(&depth) {
        return Err("Invalid stored native effect stack".into());
    }
    for effect in effects {
        super::validate_weight(effect.weight)?;
    }
    let active = effects.iter().enumerate().filter(|(_, e)| e.enabled && e.weight > 0.);
    let mut frames = [source_frame; 32];
    let mut frame = source_frame;
    let mut expanded = false;
    for (index, effect) in active.clone() {
        if !expanded && styles::contains(&effect.kind) {
            frame = Frame {
                size: canvas,
                origin: [0, 0],
            };
            expanded = true;
        }
        frames[index] = frame;
    }
    let mut outputs = frames;
    let mut needed = crop;
    // Each successor keeps its original frame. Only the pixels retained from
    // its predecessor shrink, and global fields still see the full input.
    for (index, effect) in active.clone().rev() {
        let Some(output) = needed.and_then(|bounds| frames[index].crop(bounds)) else { break; };
        check()?;
        outputs[index] = output;
        needed = regional::local_change_reach(&effect.kind, &effect.settings)?
            .map(|halo| output.expand(halo));
    }

    let mut current: Option<Image<'a>> = None;
    let mut frame = source_frame;
    for (index, effect) in active {
        let input = frame;
        frame = frames[index];
        let output_frame = outputs[index];
        let stage_binding = stage_binding(binding, index, output_frame, depth);
        let previous = current.take();
        let output = (|| {
            let mut read = |region, guard: Check<'_>| {
                aligned(
                    session,
                    input,
                    frame,
                    depth,
                    region,
                    guard,
                    |source_region, guard| {
                        let Some(image) = &previous else { return source(source_region, guard); };
                        if image.frame() == input {
                            return image.read(session, source_region, guard);
                        }
                        let document = Region::new(std::array::from_fn(|i| {
                            source_region.bounds()[i] + input.origin[i % 2]
                        })).map_err(|e| e.to_string())?;
                        image.read_document(session, document, guard)?.rebase(source_region)
                    },
                )
            };
            if effect.kind == "liquify" {
                let mapping = prepare_liquify(
                    index,
                    regional::Frame::new(frame.size[0], frame.size[1])?,
                    check,
                )?;
                liquify_stage(
                    session,
                    stage_binding,
                    frame,
                    output_frame,
                    depth,
                    effect.weight,
                    &mapping,
                    check,
                    &mut read,
                )
            } else {
                one_stage(
                    session,
                    stage_binding,
                    frame,
                    output_frame,
                    depth,
                    effect,
                    check,
                    &mut read,
                )
            }
        })();
        let cleanup = previous.map_or(Ok(()), |image| image.retire(&mut || Ok(())));
        current = Some(settle(output, cleanup)?);
    }
    match current {
        Some(image) => Ok(image),
        None => {
            let output = crop.and_then(|bounds| frame.crop(bounds)).unwrap_or(frame);
            Image::build(session, output, depth, stage_binding(binding, effects.len(), output, depth),
                check, |region, check| {
                    let input = frame.region_from(output, region)?;
                    let image = source(input, check)?;
                    if image.bounds() != input.bounds() {
                        return Err("Stored source differs from its admitted native window".into());
                    }
                    image.rebase(region)
                })
        }
    }
}

fn liquify_stage<'a>(
    session: &'a SourceSession,
    binding: [u8; 32],
    frame: Frame,
    output_frame: Frame,
    depth: u16,
    weight: f32,
    mapping: &crate::liquify::regional::Mapping,
    check: Check<'_>,
    mut source: impl FnMut(Region, Check<'_>) -> Result<regional::RegionalImage>,
) -> Result<Image<'a>> {
    mapping.validate_frame(
        regional::Frame::new(frame.size[0], frame.size[1])?,
        &session.resident(),
    )?;
    Image::build(session, output_frame, depth, binding, check, |stripe, check| {
        let [left, top, right, bottom] = frame.region_from(output_frame, stripe)?.bounds();
        let stride = usize::from(depth / 2);
        let length = stripe.pixels() * stride;
        let mut output =
            ScratchBytes::new(&session.resident(), length, length).map_err(|e| e.to_string())?;
        // A local deformation must not expand a full-width 16000px stripe's
        // source envelope. Each output tile declares its own inverse footprint.
        for y in (top..bottom).step_by(crate::raster::TILE as usize) {
            for x in (left..right).step_by(crate::raster::TILE as usize) {
                let region = Region::new([
                    x,
                    y,
                    (x + crate::raster::TILE as i32).min(right),
                    (y + crate::raster::TILE as i32).min(bottom),
                ])
                .map_err(|e| e.to_string())?;
                // Includes the declared footprint list and its bounded native image index.
                let _index = session
                    .resident()
                    .reserve_bytes(65536, 65536)
                    .map_err(|e| e.to_string())?;
                let regions = mapping.source_regions(region, check)?;
                let mut input = Vec::new();
                input
                    .try_reserve_exact(regions.len())
                    .map_err(|_| "Liquify source index allocation refused")?;
                let mut read = |region, check: Check<'_>| -> Result<regional::RegionalImage> {
                    let image = source(region, check)?;
                    if image.bounds() != region.bounds()
                        || image.depth() != depth
                        || !image.uses_budget(&session.resident())
                    {
                        return Err(
                            "Liquify input differs from its admitted native footprint".into()
                        );
                    }
                    Ok(image)
                };
                for region in regions {
                    input.push(read(region, check)?);
                }
                let before = if weight < 1. {
                    Some(read(region, check)?)
                } else {
                    None
                };
                let bytes = crate::render::region::native(
                    region,
                    depth,
                    &session.resident(),
                    check,
                    |x, y| {
                        let after = mapping.sample(x, y, depth, |sx, sy| {
                            let index = input.partition_point(|image| image.bounds()[3] <= sy);
                            input
                                .get(index)
                                .ok_or("Liquify sample has no prepared source")?
                                .pixel16(sx, sy)
                        })?;
                        let Some(before) = &before else {
                            return Ok(after);
                        };
                        let before = before.pixel16(x, y)?;
                        Ok(if depth == 16 {
                            super::weighted_pixel(before, after, weight)
                        } else {
                            super::weighted_pixel(
                                before.map(|v| u16::from(crate::raster::project16(v))),
                                after.map(|v| u16::from(crate::raster::project16(v))),
                                weight,
                            )
                            .map(|v| v * 257)
                        })
                    },
                )?;
                let bounds = region.bounds();
                let row_bytes = (bounds[2] - bounds[0]) as usize * stride;
                for row in bounds[1]..bounds[3] {
                    let destination = ((row - top) as usize * (right - left) as usize
                        + (bounds[0] - left) as usize)
                        * stride;
                    let source = (row - bounds[1]) as usize * row_bytes;
                    output[destination..destination + row_bytes]
                        .copy_from_slice(&bytes[source..source + row_bytes]);
                }
            }
        }
        regional::RegionalImage::from_native(stripe, depth, output.freeze())
    })
}

fn one_stage<'a>(
    session: &'a SourceSession,
    binding: [u8; 32],
    frame: Frame,
    output_frame: Frame,
    depth: u16,
    effect: &Effect,
    check: Check<'_>,
    mut source: impl FnMut(Region, Check<'_>) -> Result<regional::RegionalImage>,
) -> Result<Image<'a>> {
    let bitmap = if patterns::active(&effect.kind, &effect.settings) {
        Some(Arc::new(patterns::PreparedBitmap::from_png(
            effect.settings["bitmap_png"]
                .as_str()
                .ok_or("Bitmap pattern requires bitmap_png")?,
            &session.resident(),
            check,
        )?))
    } else {
        None
    };
    let effects = std::slice::from_ref(effect);
    let bitmaps = [bitmap];
    let plan = |region| {
        regional::Plan::new_prepared(
            regional::Frame::new(frame.size[0], frame.size[1])?,
            region,
            depth,
            effects,
            &session.resident(),
            &bitmaps,
        )
    };
    let first = plan(Region::new([0, 0, 1, 1]).map_err(|e| e.to_string())?)?;
    let has_halo = first.input_bounds() != first.output_bounds();
    let field = if first.global_field_stages().next().is_some() {
        let output = output_frame.expand(0);
        let logical = std::array::from_fn(|i| output[i] - frame.origin[i % 2]);
        let (_, bounds, radii) = first.field_request_for(0, logical)?;
        Some(field::prepare_session(
            session,
            binding,
            frame.size,
            bounds,
            &radii,
            depth,
            check,
            |region, check| source(region, check),
        )?)
    } else {
        None
    };
    let result = (|| {
        let alpha = if first.global_alpha_stages().next().is_some() {
            let mut reduction = AlphaReduction::new(
                regional::Frame::new(frame.size[0], frame.size[1])?,
                depth,
                &session.resident(),
            )?;
            while let Some(region) = reduction.next_region()? {
                reduction.visit(&source(region, check)?, check)?;
            }
            Some(reduction.finish(check)?)
        } else {
            None
        };
        let mut render = |region, guard: Check<'_>| {
            let plan = plan(region)?;
            let field = field
                .as_ref()
                .map(|field| field.tile(session, binding, plan.field_request(0)?.1, guard))
                .transpose()?;
            let [l, t, r, b] = plan.input_bounds();
            let rows = (MAX_REGION_PIXELS / (r - l) as usize) as i32;
            let mut image: Option<regional::RegionalImage> = None;
            let guard = RefCell::new(guard);
            plan.render(
                &session.resident(),
                &[alpha],
                &[field.as_ref()],
                &mut || (*guard.borrow_mut())(),
                |x, y| {
                    if image
                        .as_ref()
                        .is_none_or(|image| y < image.bounds()[1] || y >= image.bounds()[3])
                    {
                        drop(image.take());
                        let top = t + (y - t) / rows * rows;
                        let region = Region::new([l, top, r, (top + rows).min(b)])
                            .map_err(|e| e.to_string())?;
                        let next = source(region, &mut **guard.borrow_mut())?;
                        if next.bounds() != region.bounds()
                            || next.depth() != depth
                            || !next.uses_budget(&session.resident())
                        {
                            return Err(
                                "Effect input differs from its captured native stripe".into()
                            );
                        }
                        image = Some(next);
                    }
                    image
                        .as_ref()
                        .ok_or("Missing stored effect input")?
                        .pixel16(x, y)
                },
            )
        };
        Image::build(session, output_frame, depth, binding, check, |region, guard| {
            let window = frame.region_from(output_frame, region)?;
            let image = if has_halo {
                regional::render_band(window, depth, &session.resident(), guard, &mut render)?
            } else {
                render(window, guard)?
            };
            image.rebase(region)
        })
    })();
    settle(
        result,
        field.map_or(Ok(()), |field| field.retire(&mut || Ok(()))),
    )
}

fn aligned(
    session: &SourceSession,
    input: Frame,
    output: Frame,
    depth: u16,
    region: Region,
    check: Check<'_>,
    mut source: impl FnMut(Region, Check<'_>) -> Result<regional::RegionalImage>,
) -> Result<regional::RegionalImage> {
    let offset = [
        i64::from(input.origin[0]) - i64::from(output.origin[0]),
        i64::from(input.origin[1]) - i64::from(output.origin[1]),
    ];
    let [l, t, r, b] = region.bounds();
    let bounds = [
        (i64::from(l) - offset[0]).max(0),
        (i64::from(t) - offset[1]).max(0),
        (i64::from(r) - offset[0]).min(i64::from(input.size[0])),
        (i64::from(b) - offset[1]).min(i64::from(input.size[1])),
    ];
    let image = if bounds[0] < bounds[2] && bounds[1] < bounds[3] {
        let region = Region::new(bounds.map(|v| v as i32)).map_err(|e| e.to_string())?;
        let image = source(region, check)?;
        if image.bounds() != region.bounds()
            || image.depth() != depth
            || !image.uses_budget(&session.resident())
        {
            return Err("Stored stage source differs from its admitted native window".into());
        }
        Some(image)
    } else {
        None
    };
    let bytes =
        crate::render::region::native(region, depth, &session.resident(), check, |x, y| {
            let (x, y) = (i64::from(x) - offset[0], i64::from(y) - offset[1]);
            if x < 0 || y < 0 || x >= i64::from(input.size[0]) || y >= i64::from(input.size[1]) {
                Ok([0; 4])
            } else {
                image
                    .as_ref()
                    .ok_or("Missing expanded style input")?
                    .pixel16(x as i32, y as i32)
            }
        })?;
    regional::RegionalImage::from_native(region, depth, bytes)
}
fn bands(size: [u32; 2]) -> Result<impl Iterator<Item = Region>> {
    regional::Frame::new(size[0], size[1])?;
    let rows = (MAX_REGION_PIXELS / size[0] as usize) as u32;
    Ok((0..size[1]).step_by(rows as usize).map(move |y| {
        Region::new([0, y as i32, size[0] as i32, (y + rows).min(size[1]) as i32]).unwrap()
    }))
}
fn stage_binding(base: [u8; 32], index: usize, frame: Frame, depth: u16) -> [u8; 32] {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"PeerBrush derived native effect stage v1");
    hash.update(&base);
    hash.update(&(index as u64).to_be_bytes());
    for value in frame.size {
        hash.update(&value.to_be_bytes());
    }
    for value in frame.origin {
        hash.update(&value.to_be_bytes());
    }
    hash.update(&depth.to_be_bytes());
    hash.finish().as_ref().try_into().unwrap()
}
fn combine(error: String, cleanup: Result<()>) -> String {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => format!("{error}; stored effect cleanup: {cleanup}"),
    }
}
fn settle<'a>(output: Result<Image<'a>>, cleanup: Result<()>) -> Result<Image<'a>> {
    match (output, cleanup) {
        (result, Ok(())) => result,
        (Err(error), Err(cleanup)) => Err(combine(error, Err(cleanup))),
        (Ok(image), Err(error)) => Err(combine(error, image.retire(&mut || Ok(())))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::tests::Fixture;

    #[test]
    fn wide_stored_stroke_keeps_its_halo_across_window_edges() {
        for depth in [8, 16] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime_with_budgets(16 * 1024 * 1024, 64 * 1024 * 1024);
            let session = runtime.begin().unwrap();
            let budget = runtime.resident();
            let disk = runtime.disk().used().unwrap();
            let frame = Frame { size: [4097, 129], origin: [0, 0] };
            let effect = Effect {
                id: "stroke".into(),
                kind: "stroke".into(),
                enabled: true,
                weight: 1.,
                settings: serde_json::json!({
                    "size": 64., "position": "outside", "color": [1., 0., 0.], "opacity": 1.,
                }).into(),
            };
            let source = [12347, 23459, 34571, 65535].map(|word| {
                if depth == 16 { word } else { u16::from(crate::raster::project16(word)) * 257 }
            });
            let mut widest = 0;
            let image = stack(
                &session, [1; 32], frame, frame.size, depth, &[effect], None,
                |_, _, _| Err("Unexpected Liquify".into()),
                &mut || Ok(()),
                |region, check| {
                    let [left, _, right, _] = region.bounds();
                    widest = widest.max(right - left);
                    let bytes = crate::render::region::native(region, depth, &budget, check, |x, y| {
                        Ok(if (x, y) == (511, 64) { source } else { [0; 4] })
                    })?;
                    regional::RegionalImage::from_native(region, depth, bytes)
                },
            ).unwrap();
            assert!(widest <= 512 + 2 * (64 + 1));
            let pixels = image.read(
                &session, Region::new([510, 64, 577, 65]).unwrap(), &mut || Ok(()),
            ).unwrap();
            assert_eq!(pixels.pixel16(511, 64).unwrap(), source);
            for x in [510, 512] {
                assert_eq!(pixels.pixel16(x, 64).unwrap(), [65535, 0, 0, 65535]);
            }
            let half = if depth == 16 { 32768 } else { 128 * 257 };
            assert_eq!(pixels.pixel16(575, 64).unwrap(), [65535, 0, 0, half]);
            assert_eq!(pixels.pixel16(576, 64).unwrap(), [0; 4]);
            drop(pixels);
            image.retire(&mut || Ok(())).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
    }
}
