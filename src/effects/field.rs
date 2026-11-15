//! Legacy f64 smoothing keeps its full-origin addition/subtraction order.
//! A streamed prefix and bounded row rings avoid a full-canvas scalar buffer.
use super::regional::RegionalImage;
use crate::workspace::source_runtime::{SessionScalar, SourceSession};
use crate::{
    large_document::{Reservation, ResidentBudget},
    raster::backed::{Region, MAX_REGION_PIXELS},
};
use std::{collections::VecDeque, sync::Arc};

/// One complete smoothing pass retained for every tile of a captured stage.
/// The source session and stage binding must survive until explicit retirement.
pub(crate) struct SessionField<'a> {
    completed: SessionScalar<'a>,
    emission: RowEmission,
    frame: [u32; 2],
    origin: [i32; 2],
}
impl SessionField<'_> {
    pub(crate) fn tile(
        &self,
        session: &SourceSession,
        stage_binding: [u8; 32],
        bounds: [i32; 4],
        check: Check<'_>,
    ) -> Result<PreparedField, String> {
        self.completed.check_session(session)?;
        let local = |i: usize| bounds[i].checked_sub(self.origin[i % 2])
            .ok_or("Style field tile coordinate overflow");
        let mut tile = prepare_owned(
            self.completed.scalar(),
            &self.emission,
            stage_binding,
            [local(0)?, local(1)?, local(2)?, local(3)?],
            &session.resident(),
            &session.disk(),
            &mut || {
                self.completed.check()?;
                check()
            },
        )?;
        tile.frame = self.frame;
        tile.bounds = bounds;
        Ok(tile)
    }
    pub(crate) fn retire(self, cleanup: Check<'_>) -> Result<(), String> {
        self.completed.retire(cleanup)
    }
}
pub(crate) fn prepare_session<'a>(
    session: &'a SourceSession,
    stage_binding: [u8; 32],
    frame: [u32; 2],
    bounds: [i32; 4],
    radii: &[u32],
    depth: u16,
    check: Check<'_>,
    mut prepare_source: impl FnMut(Region, Check<'_>) -> Result<RegionalImage, String>,
) -> Result<SessionField<'a>, String> {
    validate(frame, radii, depth)?;
    let size = footprint_size(frame, bounds)?;
    if stage_binding == [0; 32] {
        return Err("Style field lacks its captured stage binding".into());
    }
    check()?;
    let spec =
        crate::large_document::ScalarSpec::new(size[0], size[1]).map_err(|e| e.to_string())?;
    let mut scratch = session.scalar_temporary(spec)?;
    let check = std::cell::RefCell::new(check);
    let emission = emit_checked(
        frame,
        bounds,
        radii,
        depth,
        &session.resident(),
        &mut || {
            session.check()?;
            (*check.borrow_mut())()
        },
        |region| prepare_source(region, &mut **check.borrow_mut()),
        |y, bytes, guard| scratch.append_rows(y, bytes, guard),
    );
    let emission = match emission {
        Ok(result) => result,
        Err(error) => {
            return match scratch.abort(&mut || Ok(())) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; style field cleanup: {cleanup}")),
            };
        }
    };
    // Finish the private seal under source admission, then observe cancellation.
    // Interrupting its consuming finish would discard the explicit retire owner.
    let completed = scratch.finish(stage_binding, &mut || session.check())?;
    if let Err(error) = (*check.borrow_mut())() {
        return match completed.retire(&mut || Ok(())) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}; style field cleanup: {cleanup}")),
        };
    }
    if completed.scalar().value_sha256() != emission.sha256() {
        let error = "Completed style field differs from its emitted values";
        return match completed.retire(&mut || Ok(())) {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(format!("{error}; style field cleanup: {cleanup}")),
        };
    }
    Ok(SessionField {
        completed,
        emission,
        frame,
        origin: [bounds[0], bounds[1]],
    })
}

struct Row {
    values: Vec<f64>,
    // Fields drop in declaration order: release the allocation before capacity.
    _charge: Reservation,
}
impl Row {
    fn new(length: usize, budget: &ResidentBudget) -> Result<Self, String> {
        let bytes = length.checked_mul(8).ok_or("Style field length overflow")?;
        let charge = budget
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(length)
            .map_err(|_| "Style row allocation failed")?;
        values.resize(length, 0.);
        Ok(Self {
            values,
            _charge: charge,
        })
    }
}
type Check<'a> = &'a mut dyn FnMut() -> Result<(), String>;
trait Rows {
    fn shape(&self) -> [usize; 2];
    fn next(&mut self, check: Check<'_>) -> Result<Option<Row>, String>;
}
type Stream<'a> = Box<dyn Rows + 'a>;

fn pipeline<'a>(
    frame: [u32; 2],
    shape: [usize; 2],
    radii: &[u32],
    depth: u16,
    budget: &'a ResidentBudget,
    prepare_source: impl FnMut(Region) -> Result<RegionalImage, String> + 'a,
) -> Result<Stream<'a>, String> {
    let mut stream: Stream<'a> = Box::new(Source {
        shape,
        depth,
        y: 0,
        budget,
        prepare: prepare_source,
        block: None,
    });
    for &radius in radii.iter().filter(|v| **v != 0) {
        stream = Box::new(Horizontal::new(
            stream,
            radius as usize,
            frame[0] as usize,
            budget,
        )?);
        stream = Box::new(Vertical::new(
            stream,
            radius as usize,
            frame[1] as usize,
            budget,
        )?);
    }
    Ok(stream)
}

fn validate(frame: [u32; 2], radii: &[u32], depth: u16) -> Result<(), String> {
    if frame.iter().any(|v| *v == 0 || *v > 16000)
        || ![8, 16].contains(&depth)
        || radii.len() > 4
        || radii.iter().any(|v| *v > 64)
    {
        return Err("Invalid style field frame or smoothing support".into());
    }
    Ok(())
}

fn prefix_size(frame: [u32; 2], bounds: [i32; 4], radii: &[u32]) -> [usize; 2] {
    let halo = radii.iter().map(|&radius| radius as usize).sum::<usize>();
    std::array::from_fn(|axis| (bounds[axis + 2] as usize + halo).min(frame[axis] as usize))
}

struct Source<'a, F> {
    shape: [usize; 2],
    depth: u16,
    y: usize,
    budget: &'a ResidentBudget,
    prepare: F,
    block: Option<RegionalImage>,
}
impl<F: FnMut(Region) -> Result<RegionalImage, String>> Rows for Source<'_, F> {
    fn shape(&self) -> [usize; 2] {
        self.shape
    }
    fn next(&mut self, check: Check<'_>) -> Result<Option<Row>, String> {
        if self.y == self.shape[1] {
            return Ok(None);
        }
        check()?;
        if self
            .block
            .as_ref()
            .is_none_or(|b| self.y as i32 >= b.bounds()[3])
        {
            self.block = None;
            let rows = (MAX_REGION_PIXELS / self.shape[0]).max(1);
            let bounds = [
                0,
                self.y as i32,
                self.shape[0] as i32,
                (self.y + rows).min(self.shape[1]) as i32,
            ];
            let block = (self.prepare)(Region::new(bounds).map_err(|e| e.to_string())?)?;
            if block.bounds() != bounds
                || block.depth() != self.depth
                || !block.uses_budget(self.budget)
            {
                return Err("Prepared alpha source differs from its requested native strip".into());
            }
            self.block = Some(block);
        }
        let mut row = Row::new(self.shape[0], self.budget)?;
        let block = self
            .block
            .as_ref()
            .ok_or("Prepared alpha source is missing")?;
        for (x, value) in row.values.iter_mut().enumerate() {
            *value = f64::from(block.pixel16(x as i32, self.y as i32)?[3]) / 65535.;
        }
        check()?;
        self.y += 1;
        Ok(Some(row))
    }
}

struct Horizontal<'a> {
    input: Stream<'a>,
    shape: [usize; 2],
    radius: usize,
    budget: &'a ResidentBudget,
}
impl<'a> Horizontal<'a> {
    fn new(
        input: Stream<'a>,
        radius: usize,
        physical_width: usize,
        budget: &'a ResidentBudget,
    ) -> Result<Self, String> {
        let mut shape = input.shape();
        if shape[0] < physical_width {
            shape[0] = shape[0]
                .checked_sub(radius)
                .filter(|v| *v > 0)
                .ok_or("Style prefix omits required horizontal support")?;
        }
        Ok(Self {
            input,
            shape,
            radius,
            budget,
        })
    }
}
impl Rows for Horizontal<'_> {
    fn shape(&self) -> [usize; 2] {
        self.shape
    }
    fn next(&mut self, check: Check<'_>) -> Result<Option<Row>, String> {
        let Some(source) = self.input.next(check)? else {
            return Ok(None);
        };
        check()?;
        let mut output = Row::new(self.shape[0], self.budget)?;
        let r = self.radius;
        let mut sum = source.values[..=r.min(source.values.len() - 1)]
            .iter()
            .sum::<f64>();
        let span = (r * 2 + 1) as f64;
        for x in 0..output.values.len() {
            output.values[x] = sum / span;
            if x + 1 == output.values.len() {
                break;
            }
            if x >= r {
                sum -= source.values[x - r];
            }
            if x + r + 1 < source.values.len() {
                sum += source.values[x + r + 1];
            }
        }
        Ok(Some(output))
    }
}

struct Vertical<'a> {
    input: Stream<'a>,
    shape: [usize; 2],
    radius: usize,
    budget: &'a ResidentBudget,
    rows: VecDeque<Row>,
    first: usize,
    loaded: usize,
    y: usize,
    sum: Option<Row>,
}
impl<'a> Vertical<'a> {
    fn new(
        input: Stream<'a>,
        radius: usize,
        physical_height: usize,
        budget: &'a ResidentBudget,
    ) -> Result<Self, String> {
        let mut shape = input.shape();
        if shape[1] < physical_height {
            shape[1] = shape[1]
                .checked_sub(radius)
                .filter(|v| *v > 0)
                .ok_or("Style prefix omits required vertical support")?;
        }
        let mut rows = VecDeque::new();
        rows.try_reserve_exact(radius * 2 + 1)
            .map_err(|_| "Style row-ring allocation failed")?;
        Ok(Self {
            input,
            shape,
            radius,
            budget,
            rows,
            first: 0,
            loaded: 0,
            y: 0,
            sum: None,
        })
    }
    fn load(&mut self, through: usize, check: Check<'_>) -> Result<(), String> {
        while self.loaded <= through {
            let row = self
                .input
                .next(check)?
                .ok_or("Style field ended before required vertical support")?;
            if row.values.len() != self.shape[0] {
                return Err("Style field row width changed".into());
            }
            self.rows.push_back(row);
            self.loaded += 1;
        }
        Ok(())
    }
}
impl Rows for Vertical<'_> {
    fn shape(&self) -> [usize; 2] {
        self.shape
    }
    fn next(&mut self, check: Check<'_>) -> Result<Option<Row>, String> {
        if self.y == self.shape[1] {
            return Ok(None);
        }
        check()?;
        let source_height = self.input.shape()[1];
        if self.sum.is_none() {
            self.load(self.radius.min(source_height - 1), check)?;
            let mut sum = Row::new(self.shape[0], self.budget)?;
            for row in &self.rows {
                for (s, v) in sum.values.iter_mut().zip(&row.values) {
                    *s += v;
                }
            }
            self.sum = Some(sum);
        }
        let mut output = Row::new(self.shape[0], self.budget)?;
        let span = (self.radius * 2 + 1) as f64;
        for (out, sum) in output
            .values
            .iter_mut()
            .zip(&self.sum.as_ref().ok_or("Style sum is missing")?.values)
        {
            *out = *sum / span;
        }
        if self.y + 1 < self.shape[1] {
            if self.y >= self.radius {
                let remove = self.y - self.radius;
                let row = self
                    .rows
                    .get(remove - self.first)
                    .ok_or("Style row ring lost its live input")?;
                for (sum, v) in self
                    .sum
                    .as_mut()
                    .ok_or("Style sum is missing")?
                    .values
                    .iter_mut()
                    .zip(&row.values)
                {
                    *sum -= v;
                }
                while self.first <= remove {
                    self.rows.pop_front();
                    self.first += 1;
                }
            }
            let add = self.y + self.radius + 1;
            if add < source_height {
                self.load(add, check)?;
                let row = self
                    .rows
                    .get(add - self.first)
                    .ok_or("Style row ring lost its next input")?;
                for (sum, v) in self
                    .sum
                    .as_mut()
                    .ok_or("Style sum is missing")?
                    .values
                    .iter_mut()
                    .zip(&row.values)
                {
                    *sum += v;
                }
            }
        }
        self.y += 1;
        Ok(Some(output))
    }
}

/// Derived scalar samples only. The caller separately binds this in-memory
/// object to its exact scene, stage, source identities and final cancellation.
#[derive(Clone)]
pub(crate) struct PreparedField {
    frame: [u32; 2],
    bounds: [i32; 4],
    values: Arc<Row>,
}
impl PreparedField {
    pub(crate) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.values._charge.uses_budget(pool)
    }
    pub(crate) fn frame(&self) -> [u32; 2] {
        self.frame
    }
    pub(crate) fn bounds(&self) -> [i32; 4] {
        self.bounds
    }
    pub(crate) fn value(&self, x: i32, y: i32) -> Result<f64, String> {
        if x < 0 || y < 0 || x >= self.frame[0] as i32 || y >= self.frame[1] as i32 {
            return Ok(0.);
        }
        if x < self.bounds[0] || y < self.bounds[1] || x >= self.bounds[2] || y >= self.bounds[3] {
            return Err("Style field sample is outside its prepared footprint".into());
        }
        Ok(self.values.values[(y - self.bounds[1]) as usize
            * (self.bounds[2] - self.bounds[0]) as usize
            + (x - self.bounds[0]) as usize])
    }
    pub(crate) fn sample(&self, x: f64, y: f64) -> Result<f64, String> {
        if !x.is_finite() || !y.is_finite() {
            return Err("Style field coordinate must be finite".into());
        }
        let ix = x.floor() as i32;
        let iy = y.floor() as i32;
        let fx = x - f64::from(ix);
        let fy = y - f64::from(iy);
        Ok(self.value(ix, iy)? * (1. - fx) * (1. - fy)
            + self.value(ix + 1, iy)? * fx * (1. - fy)
            + self.value(ix, iy + 1)? * (1. - fx) * fy
            + self.value(ix + 1, iy + 1)? * fx * fy)
    }
}

pub(crate) fn prepare(
    frame: [u32; 2],
    bounds: [i32; 4],
    radii: &[u32],
    depth: u16,
    budget: &ResidentBudget,
    check: Check<'_>,
    prepare_source: impl FnMut(Region) -> Result<RegionalImage, String>,
) -> Result<PreparedField, String> {
    validate(frame, radii, depth)?;
    if bounds[0] < 0
        || bounds[1] < 0
        || bounds[0] >= bounds[2]
        || bounds[1] >= bounds[3]
        || bounds[2] > frame[0] as i32
        || bounds[3] > frame[1] as i32
    {
        return Err("Invalid style field frame, support or output geometry".into());
    }
    check()?;
    let prefix = prefix_size(frame, bounds, radii);
    let mut stream = pipeline(frame, prefix, radii, depth, budget, prepare_source)?;
    let shape = stream.shape();
    if shape[0] < bounds[2] as usize || shape[1] < bounds[3] as usize {
        return Err("Style prefix does not cover its declared output".into());
    }
    let width = (bounds[2] - bounds[0]) as usize;
    let mut output = Row::new(width * (bounds[3] - bounds[1]) as usize, budget)?;
    for y in 0..bounds[3] as usize {
        let row = stream
            .next(check)?
            .ok_or("Style field ended before its declared output")?;
        if y >= bounds[1] as usize {
            let at = (y - bounds[1] as usize) * width;
            output.values[at..at + width]
                .copy_from_slice(&row.values[bounds[0] as usize..bounds[2] as usize]);
        }
    }
    drop(stream);
    check()?;
    Ok(PreparedField {
        frame,
        bounds,
        values: Arc::new(output),
    })
}

/// This token attests the complete rows handed to an owned scalar writer. It
/// does not authenticate a document source or authorize scratch publication.
pub(crate) struct RowEmission {
    frame: [u32; 2],
    sha256: [u8; 32],
}
impl RowEmission {
    pub(crate) fn frame(&self) -> [u32; 2] {
        self.frame
    }
    pub(crate) fn sha256(&self) -> [u8; 32] {
        self.sha256
    }
}

/// Run once per captured operation/stage and retain its completed owned scalar
/// scratch for all later tiles. Repeating a full-origin prefix per tile is not
/// the large-document rendering route.
pub(crate) fn emit_full(
    frame: [u32; 2],
    radii: &[u32],
    depth: u16,
    budget: &ResidentBudget,
    check: Check<'_>,
    prepare_source: impl FnMut(Region) -> Result<RegionalImage, String>,
    mut append_row: impl FnMut(u32, &[u8]) -> Result<(), String>,
) -> Result<RowEmission, String> {
    emit_checked(
        frame,
        full_bounds(frame),
        radii,
        depth,
        budget,
        check,
        prepare_source,
        |y, bytes, _| append_row(y, bytes),
    )
}

fn full_bounds(frame: [u32; 2]) -> [i32; 4] {
    [0, 0, frame[0] as i32, frame[1] as i32]
}

fn footprint_size(frame: [u32; 2], [left, top, right, bottom]: [i32; 4]) -> Result<[u32; 2], String> {
    if left < 0 || top < 0 || left >= right || top >= bottom
        || right > frame[0] as i32 || bottom > frame[1] as i32
    {
        return Err("Invalid style field footprint".into());
    }
    Ok([(right - left) as u32, (bottom - top) as u32])
}

fn emit_checked(
    frame: [u32; 2],
    bounds: [i32; 4],
    radii: &[u32],
    depth: u16,
    budget: &ResidentBudget,
    check: Check<'_>,
    prepare_source: impl FnMut(Region) -> Result<RegionalImage, String>,
    mut append_row: impl FnMut(u32, &[u8], Check<'_>) -> Result<(), String>,
) -> Result<RowEmission, String> {
    validate(frame, radii, depth)?;
    let size = footprint_size(frame, bounds)?;
    let [left, top, right, bottom] = bounds.map(|value| value as usize);
    check()?;
    let mut stream = pipeline(
        frame,
        prefix_size(frame, bounds, radii),
        radii,
        depth,
        budget,
        prepare_source,
    )?;
    let [width, height] = stream.shape();
    if width < right || height < bottom {
        return Err("Style prefix does not cover its declared output".into());
    }
    let length = size[0] as usize * 8;
    let mut bytes = crate::large_document::ScratchBytes::new(budget, length, length)
        .map_err(|e| e.to_string())?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for y in 0..bottom {
        check()?;
        let row = stream
            .next(check)?
            .ok_or("Style field ended before its declared output")?;
        if row.values.len() != width {
            return Err("Style emitted row width changed".into());
        }
        let retained = if y >= top { left..right } else { 0..0 };
        if row.values[..retained.start].iter().chain(&row.values[retained.end..])
            .any(|value| !value.is_finite())
        {
            return Err("Style field generated a non-finite scalar".into());
        }
        if !retained.is_empty() {
            for (value, out) in row.values[retained].iter().zip(bytes.chunks_exact_mut(8)) {
                if !value.is_finite() {
                    return Err("Style field generated a non-finite scalar".into());
                }
                out.copy_from_slice(&value.to_bits().to_be_bytes());
            }
            append_row((y - top) as u32, &bytes, check)?;
            digest.update(&bytes);
        }
        check()?;
    }
    if bottom == height && stream.next(check)?.is_some() {
        return Err("Style field emitted excess source rows".into());
    }
    drop(stream);
    check()?;
    let mut sha256 = [0; 32];
    sha256.copy_from_slice(digest.finish().as_ref());
    Ok(RowEmission { frame: size, sha256 })
}

/// Fill an already admitted, exclusive derived scratch. On failure its owner
/// remains with the caller for explicit, session-admitted abort; dropping it
/// must not be mistaken for returning persistent disk quota.
pub(crate) fn append_to_owned(
    scratch: &mut crate::large_document::ScalarScratch,
    radii: &[u32],
    depth: u16,
    budget: &ResidentBudget,
    disk: &crate::large_document::DiskBudget,
    check: Check<'_>,
    prepare_source: impl FnMut(Region) -> Result<RegionalImage, String>,
) -> Result<RowEmission, String> {
    if !scratch.uses_budgets(budget, disk) {
        return Err("Style field scratch belongs to another resource pool".into());
    }
    let frame = scratch.frame();
    emit_checked(
        frame,
        full_bounds(frame),
        radii,
        depth,
        budget,
        check,
        prepare_source,
        |y, bytes, check| storage_checked(check, |probe| scratch.append_rows(y, bytes, probe)),
    )
}

fn storage_checked<T>(
    check: Check<'_>,
    operation: impl FnOnce(
        &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> crate::large_document::Result<T>,
) -> Result<T, String> {
    let mut interrupted = None;
    let result = operation(&mut || {
        check().map_err(|message| {
            interrupted = Some(message);
            crate::large_document::Error::Cancelled
        })
    });
    result.map_err(|error| interrupted.unwrap_or_else(|| error.to_string()))
}

/// Read only the requested tile plus declared support from a completed field.
/// The caller retains the complete owner for reuse and explicit retirement.
/// Binding, frame and complete-emission SHA must all match before any read.
pub(crate) fn prepare_owned(
    completed: &crate::large_document::CompletedScalar,
    emission: &RowEmission,
    stage_binding: [u8; 32],
    bounds: [i32; 4],
    budget: &ResidentBudget,
    disk: &crate::large_document::DiskBudget,
    check: Check<'_>,
) -> Result<PreparedField, String> {
    let frame = emission.frame();
    if completed.frame() != frame
        || completed.stage_binding() != stage_binding
        || completed.value_sha256() != emission.sha256()
        || !completed.uses_budgets(budget, disk)
    {
        return Err(
            "Completed style field does not match its captured stage, values or resource pool"
                .into(),
        );
    }
    if bounds[0] < 0
        || bounds[1] < 0
        || bounds[0] >= bounds[2]
        || bounds[1] >= bounds[3]
        || bounds[2] > frame[0] as i32
        || bounds[3] > frame[1] as i32
    {
        return Err("Invalid completed style field footprint".into());
    }
    let width = (bounds[2] - bounds[0]) as usize;
    let height = (bounds[3] - bounds[1]) as usize;
    let length = width
        .checked_mul(height)
        .ok_or("Style field footprint overflow")?;
    if length > MAX_REGION_PIXELS {
        return Err("Completed style field footprint exceeds the regional limit".into());
    }
    check()?;
    let mut output = Row::new(length, budget)?;
    let mut y = 0;
    while y < height {
        let rows = (height - y).min(256);
        let bytes = width * rows * 8;
        let mut encoded = crate::large_document::ScratchBytes::new(budget, bytes, bytes)
            .map_err(|e| e.to_string())?;
        storage_checked(check, |probe| {
            completed.read_rect(
                bounds[0] as u32,
                bounds[1] as u32 + y as u32,
                width as u32,
                rows as u32,
                &mut encoded,
                probe,
            )
        })?;
        for (value, bytes) in output.values[y * width..(y + rows) * width]
            .iter_mut()
            .zip(encoded.chunks_exact(8))
        {
            *value = f64::from_be_bytes(bytes.try_into().unwrap());
        }
        y += rows;
    }
    check()?;
    Ok(PreparedField {
        frame,
        bounds,
        values: Arc::new(output),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{effects::styles, large_document::ScratchBytes};
    #[test]
    fn owned_complete_fields_reuse_exact_values_and_refuse_foreign_stage_pool_or_sha() {
        use crate::large_document::{DiskBudget, ScalarScratch, ScalarSpec};
        let root =
            std::env::temp_dir().join(format!("peerbrush-style-field-{}", uuid::Uuid::new_v4()));
        let frame = [129, 333];
        let spec = ScalarSpec::new(frame[0], frame[1]).unwrap();
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let mut scratch = ScalarScratch::create(&root, spec, budget.clone(), disk.clone()).unwrap();
        assert!(append_to_owned(
            &mut scratch,
            &[3, 3, 3],
            16,
            &ResidentBudget::new(budget.limit()).unwrap(),
            &disk,
            &mut || Ok(()),
            |_| panic!("foreign pool must be refused before reading native source"),
        )
        .is_err());
        let emission = append_to_owned(
            &mut scratch,
            &[3, 3, 3],
            16,
            &budget,
            &disk,
            &mut || Ok(()),
            |r| source(r, 16, &budget),
        )
        .unwrap();
        let completed = scratch.finish([43; 32], &mut || Ok(())).unwrap();
        // The immutable scalar index retains its own shared-pool charge.
        let scalar_retained = budget.used();
        assert_eq!(completed.value_sha256(), emission.sha256());
        for bounds in [[49, 43, 83, 88], [0, 0, 1, 333], [109, 313, 129, 333]] {
            let expected = prepare(
                frame,
                bounds,
                &[3, 3, 3],
                16,
                &budget,
                &mut || Ok(()),
                |r| source(r, 16, &budget),
            )
            .unwrap();
            let actual = prepare_owned(
                &completed,
                &emission,
                [43; 32],
                bounds,
                &budget,
                &disk,
                &mut || Ok(()),
            )
            .unwrap();
            for y in bounds[1]..bounds[3] {
                for x in bounds[0]..bounds[2] {
                    assert_eq!(
                        actual.value(x, y).unwrap().to_bits(),
                        expected.value(x, y).unwrap().to_bits()
                    );
                }
            }
        }
        assert_eq!(budget.used(), scalar_retained);
        let bounds = [49, 43, 83, 88];
        assert!(prepare_owned(
            &completed,
            &emission,
            [44; 32],
            bounds,
            &budget,
            &disk,
            &mut || Ok(())
        )
        .is_err());
        let wrong_sha = RowEmission {
            frame,
            sha256: [0; 32],
        };
        assert!(prepare_owned(
            &completed,
            &wrong_sha,
            [43; 32],
            bounds,
            &budget,
            &disk,
            &mut || Ok(())
        )
        .is_err());
        assert!(prepare_owned(
            &completed,
            &emission,
            [43; 32],
            bounds,
            &budget,
            &DiskBudget::new(spec.encoded_bytes(), 1).unwrap(),
            &mut || Ok(())
        )
        .is_err());
        let canceled = prepare_owned(
            &completed,
            &emission,
            [43; 32],
            bounds,
            &budget,
            &disk,
            &mut || Err("Captured operation revision changed".into()),
        )
        .err()
        .unwrap();
        assert_eq!(canceled, "Captured operation revision changed");
        assert_eq!(budget.used(), scalar_retained);
        completed.retire(&mut || Ok(())).unwrap();
        assert_eq!(budget.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn failed_or_canceled_source_keeps_owned_scalar_until_separate_admitted_abort() {
        use crate::large_document::{DiskBudget, ScalarScratch, ScalarSpec};
        for cancellation in [false, true] {
            let frame = [1024, 260];
            let spec = ScalarSpec::new(frame[0], frame[1]).unwrap();
            let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
            let root = std::env::temp_dir()
                .join(format!("peerbrush-style-field-{}", uuid::Uuid::new_v4()));
            let mut scratch =
                ScalarScratch::create(&root, spec, pool.clone(), disk.clone()).unwrap();
            let canceled = std::cell::Cell::new(false);
            let mut blocks = 0;
            let result = append_to_owned(
                &mut scratch,
                &[3, 3, 3],
                16,
                &pool,
                &disk,
                &mut || {
                    if canceled.get() {
                        Err("Captured document revision changed".into())
                    } else {
                        Ok(())
                    }
                },
                |region| {
                    blocks += 1;
                    if blocks == 2 {
                        if cancellation {
                            canceled.set(true);
                        } else {
                            return Err("Native source read failed".into());
                        }
                    }
                    source(region, 16, &pool)
                },
            );
            assert_eq!(
                result.err().unwrap(),
                if cancellation {
                    "Captured document revision changed"
                } else {
                    "Native source read failed"
                }
            );
            assert_eq!(blocks, 2);
            let held_bytes = std::fs::metadata(root.join("field.pbf")).unwrap().len();
            assert!(held_bytes > 64 && held_bytes < spec.encoded_bytes());
            assert_eq!(disk.used().unwrap(), spec.encoded_bytes());
            // In production this callback holds the cleanup/session permit;
            // it is intentionally separate from the canceled producer check.
            let mut cleanup_checks = 0;
            scratch
                .abort(&mut || {
                    cleanup_checks += 1;
                    Ok(())
                })
                .unwrap();
            assert!(cleanup_checks > 0);
            assert_eq!(pool.used(), 0);
            assert_eq!(disk.used().unwrap(), 0);
            std::fs::remove_dir(root).unwrap();
        }
    }
    fn alpha(x: u32, y: u32, depth: u16) -> u16 {
        let value = if (x + y) % 7 == 0 {
            0
        } else {
            21001 + (x * 173 + y * 379) % 44535
        };
        if depth == 8 {
            (value / 257 * 257) as u16
        } else {
            value as u16
        }
    }
    fn source(
        region: Region,
        depth: u16,
        budget: &ResidentBudget,
    ) -> Result<RegionalImage, String> {
        let length = region.pixels() * if depth == 16 { 8 } else { 4 };
        let mut bytes = ScratchBytes::new(budget, length, length).map_err(|e| e.to_string())?;
        let [left, top, right, bottom] = region.bounds();
        let mut at = 0;
        for y in top..bottom {
            for x in left..right {
                let a = alpha(x as u32, y as u32, depth);
                if depth == 16 {
                    for v in [12347, 33559, 51237, a] {
                        bytes[at..at + 2].copy_from_slice(&v.to_be_bytes());
                        at += 2;
                    }
                } else {
                    bytes[at..at + 4].copy_from_slice(&[37, 91, 173, (a / 257) as u8]);
                    at += 4;
                }
            }
        }
        RegionalImage::from_native(region, depth, bytes.freeze())
    }
    #[test]
    fn streamed_fields_keep_every_full_origin_f64_bit_at_edges_and_inside() {
        let frame = [129, 133];
        for depth in [8, 16] {
            for radii in [
                vec![0, 0, 0],
                vec![1, 2, 3],
                vec![3, 3, 3],
                vec![22, 22, 22],
                vec![3, 3, 3, 5],
            ] {
                let mut expected: Vec<f64> = (0..frame[1])
                    .flat_map(|y| {
                        (0..frame[0]).map(move |x| f64::from(alpha(x, y, depth)) / 65535.)
                    })
                    .collect();
                for &r in &radii {
                    styles::smooth(
                        &mut expected,
                        frame[0] as usize,
                        frame[1] as usize,
                        r as usize,
                    )
                    .unwrap();
                }
                for bounds in [[49, 43, 83, 88], [0, 0, 17, 19], [109, 113, 129, 133]] {
                    let budget = ResidentBudget::new(32 * 1024 * 1024).unwrap();
                    let field =
                        prepare(frame, bounds, &radii, depth, &budget, &mut || Ok(()), |r| {
                            source(r, depth, &budget)
                        })
                        .unwrap();
                    for y in bounds[1]..bounds[3] {
                        for x in bounds[0]..bounds[2] {
                            assert_eq!(
                                field.value(x, y).unwrap().to_bits(),
                                expected[y as usize * frame[0] as usize + x as usize].to_bits(),
                                "{depth} {radii:?} ({x},{y})"
                            );
                        }
                    }
                    assert_eq!(field.value(-1, 0).unwrap(), 0.);
                    assert!(field.value(90, 90).is_err());
                    let bytes =
                        (bounds[2] - bounds[0]) as usize * (bounds[3] - bounds[1]) as usize * 8;
                    assert_eq!(budget.used(), bytes);
                    let other = field.clone();
                    drop(field);
                    assert_eq!(budget.used(), bytes);
                    drop(other);
                    assert_eq!(budget.used(), 0);
                }
            }
        }
    }
    #[test]
    fn foreign_pool_or_incomplete_source_cannot_publish_a_field() {
        let budget = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let foreign = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        assert!(prepare(
            [129, 133],
            [49, 43, 83, 88],
            &[3, 3, 3],
            16,
            &budget,
            &mut || Ok(()),
            |r| source(r, 16, &foreign)
        )
        .is_err());
        assert_eq!(budget.used(), 0);
        assert_eq!(foreign.used(), 0);
        let mut calls = 0;
        assert!(prepare(
            [129, 133],
            [49, 43, 83, 88],
            &[3, 3, 3],
            16,
            &budget,
            &mut || Ok(()),
            |r| {
                calls += 1;
                if calls == 1 {
                    Err("synthetic native read fault".into())
                } else {
                    source(r, 16, &budget)
                }
            }
        )
        .is_err());
        assert_eq!(budget.used(), 0);
        assert!(prepare(
            [129, 133],
            [49, 43, 83, 88],
            &[3, 3, 3],
            16,
            &ResidentBudget::new(1).unwrap(),
            &mut || Ok(()),
            |r| source(r, 16, &budget)
        )
        .is_err());
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn complete_row_emission_matches_frozen_full_field_and_refuses_partial_writes() {
        let frame = [129, 133];
        let radii = [3, 3, 3, 5];
        let budget = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let mut expected: Vec<f64> = (0..frame[1])
            .flat_map(|y| (0..frame[0]).map(move |x| f64::from(alpha(x, y, 16)) / 65535.))
            .collect();
        for r in radii {
            styles::smooth(
                &mut expected,
                frame[0] as usize,
                frame[1] as usize,
                r as usize,
            )
            .unwrap();
        }
        let mut rows = 0;
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        let result = emit_full(
            frame,
            &radii,
            16,
            &budget,
            &mut || Ok(()),
            |r| source(r, 16, &budget),
            |y, bytes| {
                assert_eq!(y, rows);
                for (x, b) in bytes.chunks_exact(8).enumerate() {
                    assert_eq!(
                        u64::from_be_bytes(b.try_into().unwrap()),
                        expected[y as usize * frame[0] as usize + x].to_bits()
                    );
                }
                context.update(bytes);
                rows += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result.frame(), frame);
        assert_eq!(result.sha256().as_slice(), context.finish().as_ref());
        assert_eq!(rows, frame[1]);
        assert_eq!(budget.used(), 0);
        let mut written = 0;
        assert!(emit_full(
            frame,
            &radii,
            16,
            &budget,
            &mut || Ok(()),
            |r| source(r, 16, &budget),
            |_, _| {
                written += 1;
                if written == 3 {
                    Err("Synthetic scalar append failure".into())
                } else {
                    Ok(())
                }
            }
        )
        .is_err());
        assert_eq!(written, 3);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    #[ignore = "Opt-in full 16000-square operator/storage traversal; not editor or total RAM qualification"]
    fn full_16000_native16_stream_uses_one_owned_scalar_pass_and_bounded_replay() {
        use crate::large_document::{DiskBudget, ScalarScratch, ScalarSpec};
        let frame = [16000, 16000];
        let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let spec = ScalarSpec::new(frame[0], frame[1]).unwrap();
        let disk = DiskBudget::new(spec.encoded_bytes(), 1).unwrap();
        let root =
            std::env::temp_dir().join(format!("peerbrush-style-field-{}", uuid::Uuid::new_v4()));
        let mut scratch = ScalarScratch::create(&root, spec, pool.clone(), disk.clone()).unwrap();
        let mut pixels = 0usize;
        let mut next_y = 0;
        let emission = append_to_owned(
            &mut scratch,
            &[3, 3, 3],
            16,
            &pool,
            &disk,
            &mut || Ok(()),
            |r| {
                let b = r.bounds();
                assert_eq!(b[0], 0);
                assert_eq!(b[2], 16000);
                assert_eq!(b[1], next_y);
                next_y = b[3];
                pixels += r.pixels();
                source(r, 16, &pool)
            },
        )
        .unwrap();
        assert_eq!(pixels, 256_000_000);
        assert_eq!(next_y, 16000);
        let completed = scratch.finish([77; 32], &mut || Ok(())).unwrap();
        assert_eq!(completed.value_sha256(), emission.sha256());
        for bounds in [
            [0, 0, 128, 128],
            [7911, 8123, 8039, 8251],
            [15872, 15872, 16000, 16000],
        ] {
            let mut settings = crate::effects::defaults("outer_glow");
            settings["size"] = serde_json::json!(9.);
            let effect = crate::effects::Effect {
                id: "full-traversal-style".into(),
                kind: "outer_glow".into(),
                enabled: true,
                weight: 0.73,
                settings: Arc::new(settings),
            };
            let plan = crate::effects::regional::Plan::new(
                crate::effects::regional::Frame::new(16000, 16000).unwrap(),
                Region::new(bounds).unwrap(),
                16,
                &[effect],
            )
            .unwrap();
            let (_, support, radii) = plan.field_request(0).unwrap();
            assert_eq!(radii, [3, 3, 3]);
            let field = prepare_owned(
                &completed,
                &emission,
                [77; 32],
                support,
                &pool,
                &disk,
                &mut || Ok(()),
            )
            .unwrap();
            for y in bounds[1]..bounds[3] {
                for x in bounds[0]..bounds[2] {
                    let value = field.value(x, y).unwrap();
                    assert!(value.is_finite() && (0. ..=1.).contains(&value));
                }
            }
            let styled = plan
                .render(&pool, &[None], &[Some(&field)], &mut || Ok(()), |x, y| {
                    Ok([12347, 33559, 51237, alpha(x as u32, y as u32, 16)])
                })
                .unwrap();
            assert!(styled
                .pixel16(bounds[0], bounds[1])
                .unwrap()
                .iter()
                .any(|v| v % 257 != 0));
        }
        let digest = emission
            .sha256()
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect::<String>();
        completed.retire(&mut || Ok(())).unwrap();
        assert_eq!(pool.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
        std::fs::remove_dir(root).unwrap();
        println!("PASS full dense procedural native16 operator/storage traversal frame=16000x16000 pixels={pixels} shared_resident_cap_bytes={} scalar_payload_bytes={} emission_sha256={digest}; no live editor/Surface/history/GPU/PB108/total RAM qualification",pool.limit(),spec.payload_bytes());
    }
}
