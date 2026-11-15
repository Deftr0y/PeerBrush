//! Streaming scalar bounds and exact pixel-boundary contours. Workspace and
//! retained contour capacities remain in the caller's shared resident ledger.
use super::backed::ContourOwner;
use crate::{
    large_document::{Error, ResidentBudget, Result, ScratchBytes},
    raster::TILE,
};
use std::sync::Arc;

const MAX_EDGES: usize = 500_000;
pub(super) struct Contours {
    width: u32,
    height: u32,
    next: u32,
    bounds: [i32; 4],
    edges: ScratchBytes,
    visited: ScratchBytes,
    previous: ScratchBytes,
    count: usize,
    pool: ResidentBudget,
    failed: bool,
}
fn point(bytes: &[u8], at: usize) -> (u16, u16) {
    (
        u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap()),
        u16::from_be_bytes(bytes[at + 2..at + 4].try_into().unwrap()),
    )
}
fn direction(a: (u16, u16), b: (u16, u16)) -> Result<u8> {
    match (
        i32::from(b.0) - i32::from(a.0),
        i32::from(b.1) - i32::from(a.1),
    ) {
        (1, 0) => Ok(0),
        (0, 1) => Ok(1),
        (-1, 0) => Ok(2),
        (0, -1) => Ok(3),
        _ => Err(Error::Corrupt),
    }
}
fn collinear(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    (b[0] - a[0]) * (c[1] - b[1]) == (b[1] - a[1]) * (c[0] - b[0])
}

impl Contours {
    pub fn new(width: u32, height: u32, pool: &ResidentBudget) -> Result<Self> {
        if width == 0 || height == 0 || width > 16000 || height > 16000 {
            return Err(Error::Invalid("Invalid selection contour source shape"));
        }
        Ok(Self {
            width,
            height,
            next: 0,
            bounds: [i32::MAX, i32::MAX, i32::MIN, i32::MIN],
            edges: ScratchBytes::new(pool, MAX_EDGES * 8, MAX_EDGES * 8)?,
            visited: ScratchBytes::new(pool, MAX_EDGES.div_ceil(8), MAX_EDGES.div_ceil(8))?,
            previous: ScratchBytes::new(pool, width as usize, width as usize)?,
            count: 0,
            pool: pool.clone(),
            failed: false,
        })
    }
    fn edge(&mut self, a: (u32, u32), b: (u32, u32)) -> Result<()> {
        if self.count == MAX_EDGES {
            return Err(Error::Invalid(
                "Selection boundary exceeds 500000 edges; simplify the region",
            ));
        }
        let at = self.count * 8;
        for (i, value) in [a.0, a.1, b.0, b.1].into_iter().enumerate() {
            self.edges[at + i * 2..at + i * 2 + 2].copy_from_slice(&(value as u16).to_be_bytes());
        }
        self.count += 1;
        Ok(())
    }
    pub fn row(
        &mut self,
        y: u32,
        values: &[u8],
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<()> {
        let invalid = self.failed
            || y != self.next
            || y >= self.height
            || values.len() != self.width as usize;
        self.failed = true;
        if invalid {
            return Err(Error::Invalid(
                "Selection contour rows differ from their source",
            ));
        }
        let zero = [0; TILE as usize];
        for (column, samples) in values.chunks(TILE as usize).enumerate() {
            if column * TILE as usize % 4096 == 0 {
                check()?;
            }
            let start = column * TILE as usize;
            if samples == &zero[..samples.len()]
                && self.previous[start..start + samples.len()] == zero[..samples.len()]
            {
                continue;
            }
            for (i, &value) in samples.iter().enumerate() {
                let x = (start + i) as u32;
                let inside = value >= 128;
                let above = self.previous[x as usize] >= 128;
                if value > 0 {
                    self.bounds = [
                        self.bounds[0].min(x as i32),
                        self.bounds[1].min(y as i32),
                        self.bounds[2].max(x as i32 + 1),
                        self.bounds[3].max(y as i32 + 1),
                    ];
                }
                if inside && !above {
                    self.edge((x, y), (x + 1, y))?;
                }
                if above && !inside {
                    self.edge((x + 1, y), (x, y))?;
                }
                if inside && (x == 0 || values[x as usize - 1] < 128) {
                    self.edge((x, y + 1), (x, y))?;
                }
                if inside && (x + 1 == self.width || values[x as usize + 1] < 128) {
                    self.edge((x + 1, y), (x + 1, y + 1))?;
                }
            }
        }
        self.previous.copy_from_slice(values);
        self.next += 1;
        check()?;
        self.failed = false;
        Ok(())
    }
    fn swap(&mut self, a: usize, b: usize) {
        for i in 0..8 {
            self.edges.swap(a * 8 + i, b * 8 + i);
        }
    }
    fn sift(&mut self, mut root: usize, end: usize) {
        loop {
            let child = root * 2 + 1;
            if child >= end {
                break;
            }
            let right = child + 1;
            let greatest = if right < end
                && self.edges[right * 8..right * 8 + 8] > self.edges[child * 8..child * 8 + 8]
            {
                right
            } else {
                child
            };
            if self.edges[root * 8..root * 8 + 8] >= self.edges[greatest * 8..greatest * 8 + 8] {
                break;
            }
            self.swap(root, greatest);
            root = greatest;
        }
    }
    fn sort(&mut self, check: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        for at in (0..self.count / 2).rev() {
            if at % 1024 == 0 {
                check()?;
            }
            self.sift(at, self.count);
        }
        for end in (1..self.count).rev() {
            if end % 1024 == 0 {
                check()?;
            }
            self.swap(0, end);
            self.sift(0, end);
        }
        Ok(())
    }
    fn seen(&self, index: usize) -> bool {
        self.visited[index / 8] & (1 << (index % 8)) != 0
    }
    fn next_edge(&self, vertex: (u16, u16), incoming: u8) -> Result<usize> {
        let mut low = 0;
        let mut high = self.count;
        while low < high {
            let mid = (low + high) / 2;
            if point(&self.edges, mid * 8) < vertex {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let mut best = None;
        for index in low..self.count {
            let a = point(&self.edges, index * 8);
            if a != vertex {
                break;
            }
            if self.seen(index) {
                continue;
            }
            let dir = direction(a, point(&self.edges, index * 8 + 4))?;
            let rank = match (dir + 4 - incoming) % 4 {
                1 => 0,
                0 => 1,
                3 => 2,
                _ => 3,
            };
            if best.is_none_or(|(score, _)| rank < score) {
                best = Some((rank, index));
            }
        }
        best.map(|(_, index)| index).ok_or(Error::Corrupt)
    }
    pub fn finish(
        mut self,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<([i32; 4], Arc<ContourOwner>)> {
        if self.failed || self.next != self.height {
            return Err(Error::Invalid("Selection contour source did not complete"));
        }
        for x in 0..self.width {
            if x % 4096 == 0 {
                check()?;
            }
            if self.previous[x as usize] >= 128 {
                self.edge((x + 1, self.height), (x, self.height))?;
            }
        }
        if self.count == 0 {
            check()?;
            return Ok((
                if self.bounds[0] == i32::MAX {
                    [0; 4]
                } else {
                    self.bounds
                },
                ContourOwner::empty(&self.pool)?,
            ));
        }
        self.sort(check)?;
        let mut owner = ContourOwner::peak(&self.pool)?;
        for first in 0..self.count {
            if self.seen(first) {
                continue;
            }
            check()?;
            let start = point(&self.edges, first * 8);
            let mut current = first;
            let mut line = Vec::new();
            let mut length = 0;
            loop {
                if self.seen(current) {
                    return Err(Error::Corrupt);
                }
                self.visited[current / 8] |= 1 << (current % 8);
                length += 1;
                if length > self.count {
                    return Err(Error::Corrupt);
                }
                if length % 1024 == 0 {
                    check()?;
                }
                let a = point(&self.edges, current * 8);
                let b = point(&self.edges, current * 8 + 4);
                let p = [f32::from(a.0), f32::from(a.1)];
                if line.len() >= 2 && collinear(line[line.len() - 2], line[line.len() - 1], p) {
                    let last = line.len() - 1;
                    line[last] = p;
                } else {
                    line.try_reserve(1).map_err(|_| Error::Budget)?;
                    line.push(p);
                }
                if b == start {
                    break;
                }
                current = self.next_edge(b, direction(a, b)?)?;
            }
            while line.len() >= 3 && collinear(line[line.len() - 2], line[line.len() - 1], line[0])
            {
                line.pop();
            }
            while line.len() >= 3 && collinear(line[line.len() - 1], line[0], line[1]) {
                line.remove(0);
            }
            if line.len() < 4 {
                return Err(Error::Corrupt);
            }
            owner.push(line)?;
        }
        check()?;
        let bounds = if self.bounds[0] == i32::MAX {
            [0; 4]
        } else {
            self.bounds
        };
        Ok((bounds, owner.freeze()?))
    }
}
