//! Private texture candidate fed by projected stripes in document order.
use super::{tiles::{self, Rows}, *};
use crate::render::native_preview::MissingColumns;
use std::{collections::VecDeque, ops::Range};

pub(super) struct PanTiles<'a> {
    layout: tiles::Layout,
    previous: &'a tiles::Textures,
    matches: Vec<Option<usize>>,
}
impl PanTiles<'_> {
    fn tile(&self, index: usize) -> Option<&tiles::TextureTile> {
        self.matches[index].map(|prior| &self.previous.tiles[prior])
    }
}
impl MissingColumns for PanTiles<'_> {
    fn stripe_end(&self, [start, stop]: [u32; 2]) -> u32 {
        let [cw, ch] = self.layout.cell;
        let columns = self.layout.size[0].div_ceil(cw) as usize;
        let first = start.saturating_sub(1) / ch + 1;
        let last = (self.layout.size[1] - 1) / ch;
        let mut end = stop;
        for band in first..=last {
            let boundary = band * ch;
            if boundary - 1 >= end { break; }
            let index = band as usize * columns;
            let adjacent = self.matches[index - columns..index].iter().zip(&self.matches[index..index + columns]);
            // The lower band first samples its top halo one row before the
            // boundary. The upper band still samples the row at the boundary.
            if boundary - 1 > start && adjacent.clone().any(|(upper, lower)| upper.is_some() && lower.is_none()) {
                end = boundary - 1;
                break;
            }
            if (start + 1..end).contains(&(boundary + 1))
                && adjacent.clone().any(|(upper, lower)| upper.is_none() && lower.is_some())
            {
                end = boundary + 1;
            }
        }
        end
    }
    fn visit(&self, rows: [u32; 2], render: &mut dyn FnMut(Range<u32>) -> Result<()>) -> Result<()> {
        let [cw, ch] = self.layout.cell;
        let columns = self.layout.size[0].div_ceil(cw) as usize;
        let first = (rows[0].saturating_sub(1) / ch) as usize;
        let last = (rows[1].min(self.layout.size[1] - 1) / ch) as usize;
        let mut span: Option<Range<u32>> = None;
        for column in 0..columns {
            let needed = (first..=last).any(|band| {
                let index = band * columns + column;
                let area = self.layout.area(index).unwrap();
                self.matches[index].is_none() && area.outer[1] < rows[1] && rows[0] < area.outer[3]
            });
            if !needed { continue; }
            let [left, _, right, _] = self.layout.area(column).unwrap().outer;
            if let Some(current) = span.as_mut().filter(|current| current.end >= left) {
                current.end = right;
            } else if let Some(old) = span.replace(left..right) {
                render(old)?;
            }
        }
        if let Some(span) = span { render(span)?; }
        Ok(())
    }
}

struct Buffer {
    stripes: VecDeque<Stripe>,
    width: u32,
    received: u32,
    _index: Reservation,
}
type Stripe = ([u32; 2], StripePixels);

fn stripe_slots(layout: &tiles::Layout) -> usize {
    // Before the next push, the unfinished band holds at most one stripe per
    // received row, including its filtering neighbors. The push completes the
    // band or adds a stripe within that same row bound.
    layout.size[1].min(layout.cell[1] + 2) as usize
}

pub(super) fn stripe_index_bytes(layout: &tiles::Layout) -> usize {
    stripe_slots(layout) * std::mem::size_of::<Stripe>() + 128
}

impl Rows for Buffer {
    fn retire_before(&mut self, row: u32) {
        while self.stripes.front().is_some_and(|(rows, _)| rows[1] <= row) {
            self.stripes.pop_front();
        }
    }
    fn row(&self, y: u32) -> Result<&[u8]> {
        self.columns(y, self.width, 0..self.width)
    }
    fn columns(&self, y: u32, width: u32, columns: Range<u32>) -> Result<&[u8]> {
        let (rows, bytes) = self.stripes.iter().find(|(rows, _)| rows[0] <= y && y < rows[1])
            .ok_or("Streamed preview row is unavailable")?;
        bytes.columns(y - rows[0], width, columns)
    }
}
pub(super) struct Stream<'a> {
    candidate: tiles::Tiles,
    layout: tiles::Layout,
    rows: Buffer,
    retirement: &'a Retirement,
    previous: Option<&'a tiles::Textures>,
    track_content: bool,
    converted: usize,
    uploaded: usize,
}
impl<'a> Stream<'a> {
    pub(super) fn new(width: u32, height: u32, side: usize, retirement: &'a Retirement) -> Result<Self> {
        Self::with_layout(tiles::Layout::new(width, height, side)?, retirement, None, false)
    }
    pub(super) fn for_upload(
        width: u32, height: u32, side: usize, retirement: &'a Retirement,
        previous: Option<&'a tiles::Textures>,
        track_content: bool,
    ) -> Result<Self> {
        Self::with_layout(tiles::Layout::streamed(width, height, side)?, retirement, previous, track_content)
    }
    fn with_layout(
        layout: tiles::Layout, retirement: &'a Retirement, previous: Option<&'a tiles::Textures>, track_content: bool,
    ) -> Result<Self> {
        let width = layout.size[0];
        let capacity = stripe_slots(&layout);
        let index = retirement.pool().reserve_bytes(stripe_index_bytes(&layout), 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut stripes = VecDeque::new();
        stripes.try_reserve_exact(capacity).map_err(|_| "Streamed preview index allocation refused")?;
        if stripes.capacity() > capacity {
            return Err("Streamed preview index exceeds admission".into());
        }
        Ok(Self {
            candidate: tiles::Tiles::allocate(&layout, retirement)?, layout,
            rows: Buffer { stripes, width, received: 0, _index: index }, retirement,
            previous, track_content, converted: 0, uploaded: 0,
        })
    }
    pub(super) fn push(
        &mut self, rows: [u32; 2], bytes: Option<StripePixels>,
        mut batch: Option<&mut upload::Batch<'_>>,
        pan: Option<&PanTiles<'_>>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        check()?;
        if rows[0] != self.rows.received || rows[1] <= rows[0] || rows[1] > self.layout.size[1] {
            return Err("Streamed preview stripe differs from its admitted frame".into());
        }
        if let Some(bytes) = bytes {
            if !bytes.matches(self.rows.width, rows[1] - rows[0], self.retirement.pool())
                || matches!(&bytes, StripePixels::Packed(_)) && (pan.is_none() || batch.is_none())
                || self.rows.stripes.len() == self.rows.stripes.capacity()
            {
                return Err("Streamed preview stripe differs from its admitted frame".into());
            }
            self.rows.stripes.push_back((rows, bytes));
        } else if pan.is_none() || batch.is_none() {
            return Err("Only a reused texture may omit preview rows".into());
        }
        self.rows.received = rows[1];
        while let Some(area) = self.layout.area(self.converted) {
            if area.outer[3] > self.rows.received { break; }
            check()?;
            self.rows.retire_before(area.outer[1]);
            let mut digest = None;
            let mut transparent = false;
            if let Some(batch) = &mut batch {
                if let Some(tile) = pan.and_then(|pan| pan.tile(self.converted)) {
                    batch.reuse(tile, area)?;
                    self.uploaded += 1;
                    self.converted += 1;
                    continue;
                }
                if self.track_content {
                    // The filtering halo can change even when the interior is identical.
                    let [left, top, right, bottom] = area.outer;
                    const ZERO_ROW: &[u8] = &[0; super::tiles::MAX_TEXTURE_EDGE * 4];
                    transparent = true;
                    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
                    for y in top..bottom {
                        check()?;
                        let row = self.rows.columns(y, self.rows.width, left..right)?;
                        hash.update(row);
                        transparent = transparent && row == &ZERO_ROW[..row.len()];
                    }
                    let hash = hash.finish().as_ref().try_into().unwrap();
                    digest = Some(hash);
                    if let Some(tile) = self.previous.and_then(|tiles| tiles.reusable(self.converted, area, hash)) {
                        // Position and UVs belong to the new crop.
                        batch.reuse(tile, area)?;
                        self.uploaded += 1;
                        self.converted += 1;
                        continue;
                    }
                    if transparent && batch.reuse_transparent(area)? {
                        self.uploaded += 1;
                        self.converted += 1;
                        continue;
                    }
                }
                let pixels = (area.outer[2] - area.outer[0]) as usize
                    * (area.outer[3] - area.outer[1]) as usize;
                batch.before_conversion(pixels, check)?;
            }
            let upload = tiles::Upload::prepare(
                area, self.rows.width, &self.rows, self.retirement, check,
            )?;
            if let Some(batch) = &mut batch {
                batch.push(upload, digest, transparent, check)?;
                if transparent {
                    batch.remember_transparent();
                }
                self.uploaded += 1;
            } else {
                self.candidate.uploads.push(upload);
            }
            self.converted += 1;
        }
        let keep = self.layout.area(self.converted)
            .map_or(self.layout.size[1], |area| area.outer[1]);
        self.rows.retire_before(keep);
        check()
    }
    pub(super) fn finish(self, check: &mut dyn FnMut() -> Result<()>) -> Result<tiles::Tiles> {
        check()?;
        if self.rows.received != self.layout.size[1] || self.converted != self.layout.count || self.uploaded != 0
            || !self.rows.stripes.is_empty()
        {
            return Err("Streamed preview candidate is incomplete".into());
        }
        Ok(self.candidate)
    }
    pub(super) fn tile_count(&self) -> usize { self.layout.count }
    pub(super) fn position_reuse(&self, old: [i32; 4], new: [i32; 4], damage: Option<[i32; 4]>) -> Option<PanTiles<'a>> {
        let previous = self.previous?;
        // The existing admitted tile index includes this per-tile lookup.
        let mut matches = Vec::new();
        matches.try_reserve_exact(self.layout.count).ok()?;
        if matches.capacity() > self.layout.count {
            return None;
        }
        let offset = [new[0] - old[0], new[1] - old[1]];
        matches.extend(self.layout.areas().map(|area| {
            let [left, top, right, bottom] = area.outer.map(|v| v as i32);
            if damage.is_some_and(|[l, t, r, b]| {
                left + new[0] < r && l < right + new[0] && top + new[1] < b && t < bottom + new[1]
            }) {
                None
            } else {
                previous.at_same_position(area, offset)
            }
        }));
        matches.iter().any(Option::is_some).then_some(PanTiles { layout: self.layout, previous, matches })
    }
    pub(super) fn finish_uploaded(
        self, handles: Vec<tiles::TextureTile>, check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<tiles::Textures> {
        check()?;
        if self.rows.received != self.layout.size[1] || self.converted != self.layout.count
            || self.uploaded != self.converted || !self.candidate.uploads.is_empty()
            || !self.rows.stripes.is_empty() || handles.len() != self.layout.count
            || handles.iter().zip(self.layout.areas()).any(|(tile, area)|
                tile.area.inner != area.inner || tile.area.outer != area.outer)
        {
            return Err("Uploaded preview candidate is incomplete".into());
        }
        self.candidate.with_textures(handles)
    }
}
