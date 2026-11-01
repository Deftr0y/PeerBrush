//! Bounded native transform sampling. Output remains provisional until the
//! common source/session and engine publication checks admit it.
use crate::{
    large_document::{DiskBudget, Error, PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
    raster::{
        backed::{Region, MAX_REGION_PIXELS},
        Pixel, Pixel16, Surface, SurfaceKey, SurfaceRegion, TILE,
    },
    retained::geometry,
};
#[path = "native/bounds.rs"]
mod bounds;
pub(crate) use bounds::{content_bounds, source_bounds};
#[path = "native/output.rs"]
mod output;
pub(crate) use output::{write_fill, write_plane};

enum Kind {
    Affine { matrix: [f64; 6], determinant: f64 },
    Projective(crate::projective::Map),
    Warp(crate::warp::Placement),
}

/// Geometry only. Original source slots remain in their captured document.
pub(crate) struct Mapping {
    input: [u32; 2],
    depth: u16,
    matrix: [f64; 6],
    kind: Kind,
    // Own the bounded prepared Warp metadata until every mapping is finished.
    _geometry: Option<Reservation>,
    geometry_pool: Option<ResidentBudget>,
}
impl Mapping {
    pub(crate) fn retained(
        width: u32,
        height: u32,
        depth: u16,
        matrix: [f64; 6],
        projective: Option<[f64; 9]>,
        warps: &[crate::warp::Stage],
        pool: &ResidentBudget,
    ) -> Result<Self, String> {
        if warps.len() > crate::warp::MAX_STAGES {
            return Err("Retained Warp stage limit exceeded".into());
        }
        let charge = if warps.is_empty() {
            None
        } else {
            Some(
                pool.reserve_bytes(64 * 1024, 64 * 1024)
                    .map_err(|e| e.to_string())?,
            )
        };
        let matrix = matrix.map(geometry::exact);
        geometry::validate(width, height, depth, depth, matrix, projective, warps)?;
        let kind = if !warps.is_empty() {
            Kind::Warp(crate::warp::Placement::new(
                warps,
                geometry::placement(matrix, projective)?,
            )?)
        } else if projective.is_some() {
            Kind::Projective(
                crate::projective::Map::affine_matrix([1., 0., 0., 1., -0.5, -0.5])
                    .compose(geometry::placement(matrix, projective)?.inverse()?)?,
            )
        } else {
            let [a, b, c, d, _, _] = matrix;
            Kind::Affine {
                matrix,
                determinant: a * d - b * c,
            }
        };
        Ok(Self {
            input: [width, height],
            depth,
            matrix,
            kind,
            geometry_pool: charge.as_ref().map(|_| pool.clone()),
            _geometry: charge,
        })
    }
    pub(crate) fn matrix(&self) -> [f64; 6] {
        self.matrix
    }
    fn position(&self, x: u32, y: u32) -> [f32; 2] {
        match &self.kind {
            Kind::Affine {
                matrix: [a, b, c, d, tx, ty],
                determinant,
            } => {
                let (xx, yy) = (x as f64 + 0.5 - tx, y as f64 + 0.5 - ty);
                [
                    geometry::exact((d * xx - c * yy) / determinant - 0.5) as f32,
                    geometry::exact((-b * xx + a * yy) / determinant - 0.5) as f32,
                ]
            }
            Kind::Projective(map) => map.sample([x as f64 + 0.5, y as f64 + 0.5]),
            Kind::Warp(map) => map.sample([x as f64 + 0.5, y as f64 + 0.5]),
        }
    }
    fn identity(&self) -> bool {
        matches!(&self.kind,Kind::Affine{matrix,..} if *matrix==crate::retained::IDENTITY)
    }
}

/// Caller-gated geometry work, including repeated footprint subdivision scans.
pub(crate) struct Work {
    used: u64,
    maximum: u64,
}
impl Work {
    pub(crate) fn new(maximum: u64) -> Result<Self, String> {
        if maximum == 0 {
            return Err("Transform requires explicit work admission".into());
        }
        Ok(Self { used: 0, maximum })
    }
    pub(crate) fn used(&self) -> u64 {
        self.used
    }
    fn step(&mut self) -> Result<(), String> {
        self.claim(1)
    }
    pub(crate) fn claim(&mut self, amount: u64) -> Result<(), String> {
        let used = self
            .used
            .checked_add(amount)
            .filter(|used| *used <= self.maximum);
        let Some(used) = used else {
            return Err("Native transform mapping work budget exceeded".into());
        };
        self.used = used;
        Ok(())
    }
}

pub(crate) struct Tile {
    source: SurfaceKey,
    coordinate: (u32, u32),
    frame: [u32; 2],
    depth: u16,
    bytes: PinnedBytes,
    preserve_empty: bool,
}
impl Tile {
    pub(crate) fn source(&self) -> SurfaceKey {
        self.source
    }
    pub(crate) fn coordinate(&self) -> (u32, u32) {
        self.coordinate
    }
    pub(crate) fn frame(&self) -> [u32; 2] {
        self.frame
    }
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
    pub(crate) fn bytes(&self) -> &PinnedBytes {
        &self.bytes
    }
    pub(crate) fn into_bytes(self) -> PinnedBytes {
        self.bytes
    }
}

/// Full canonical native tile, with zero edge padding for a resampled frame.
/// Exact identity retains the original physical padding as the old clone path.
pub(crate) fn render_tile(
    source: &Surface,
    mapping: &Mapping,
    frame: [u32; 2],
    coordinate: (u32, u32),
    fill: Option<Pixel>,
    pool: &ResidentBudget,
    disk: &DiskBudget,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Tile, String> {
    check().map_err(|e| e.to_string())?;
    if frame.iter().any(|n| *n == 0 || *n > 16000)
        || coordinate.0 >= frame[0].div_ceil(TILE)
        || coordinate.1 >= frame[1].div_ceil(TILE)
        || mapping.input != [source.width(), source.height()]
        || mapping.depth != source.depth()
        || !source.uses_budget(pool)
        || !source.uses_disk_budget(disk)
        || mapping
            .geometry_pool
            .as_ref()
            .is_some_and(|p| !p.same_pool(pool))
    {
        return Err("Transform source, frame or shared admission differs".into());
    }
    let size = (TILE * TILE * 4) as usize * usize::from(source.depth() / 8);
    if fill.is_none() && mapping.identity() && frame == mapping.input {
        // This path copies complete physical padding too. It must consume the
        // same operation allowance even though no coordinate map is evaluated.
        work.claim(u64::from(TILE) * u64::from(TILE))?;
        let tile = source
            .prepare_tile(coordinate, &mut || check())
            .map_err(|e| e.to_string())?;
        let preserve_empty = !tile.is_known_zero();
        let bytes = tile
            .copy_to_scratch(pool)
            .map_err(|e| e.to_string())?
            .freeze();
        check().map_err(|e| e.to_string())?;
        return Ok(Tile {
            source: source.key(),
            coordinate,
            frame,
            depth: source.depth(),
            bytes,
            preserve_empty,
        });
    }
    let mut output = ScratchBytes::new(pool, size, size).map_err(|e| e.to_string())?;
    let left = coordinate.0 * TILE;
    let top = coordinate.1 * TILE;
    let region = [
        left,
        top,
        (left + TILE).min(frame[0]),
        (top + TILE).min(frame[1]),
    ];
    let mut positions = ScratchBytes::new(
        pool,
        (region[2] - region[0]) as usize * (region[3] - region[1]) as usize * 8,
        512 * 1024,
    )
    .map_err(|e| e.to_string())?;
    render(
        source,
        mapping,
        region,
        coordinate,
        fill,
        pool,
        work,
        check,
        &mut positions,
        &mut output,
    )?;
    check().map_err(|e| e.to_string())?;
    Ok(Tile {
        source: source.key(),
        coordinate,
        frame,
        depth: source.depth(),
        bytes: output.freeze(),
        preserve_empty: false,
    })
}

fn coord(bytes: &[u8], index: usize) -> [f32; 2] {
    let at = index * 8;
    [
        f32::from_bits(u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())),
        f32::from_bits(u32::from_be_bytes(
            bytes[at + 4..at + 8].try_into().unwrap(),
        )),
    ]
}
fn outside([x, y]: [f32; 2], source: &Surface) -> bool {
    x <= -1. || y <= -1. || x >= source.width() as f32 || y >= source.height() as f32
}
fn footprint(
    source: &Surface,
    mapping: &Mapping,
    region: [u32; 4],
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
    positions: &mut [u8],
) -> Result<Option<[i32; 4]>, String> {
    let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let mut index = 0;
    for y in region[1]..region[3] {
        check().map_err(|e| e.to_string())?;
        for x in region[0]..region[2] {
            work.step()?;
            let p = mapping.position(x, y);
            if p.iter().any(|v| !v.is_finite()) {
                return Err("Transform produced a nonfinite sample coordinate".into());
            }
            for (c, v) in p.iter().enumerate() {
                let at = index * 8 + c * 4;
                positions[at..at + 4].copy_from_slice(&v.to_bits().to_be_bytes());
            }
            index += 1;
            if outside(p, source) {
                continue;
            }
            let ix = p[0].floor() as i32;
            let iy = p[1].floor() as i32;
            let last = if p[0].fract() == 0. && p[1].fract() == 0. {
                0
            } else {
                1
            };
            let l = ix.max(0);
            let t = iy.max(0);
            let r = (ix + last + 1).min(source.width() as i32);
            let b = (iy + last + 1).min(source.height() as i32);
            if l < r && t < b {
                bounds = [
                    bounds[0].min(l),
                    bounds[1].min(t),
                    bounds[2].max(r),
                    bounds[3].max(b),
                ];
            }
        }
    }
    Ok((bounds[0] < bounds[2] && bounds[1] < bounds[3]).then_some(bounds))
}

#[allow(clippy::too_many_arguments)]
fn render(
    source: &Surface,
    mapping: &Mapping,
    region: [u32; 4],
    tile: (u32, u32),
    fill: Option<Pixel>,
    pool: &ResidentBudget,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
    positions: &mut [u8],
    output: &mut [u8],
) -> Result<(), String> {
    let footprint = footprint(source, mapping, region, work, check, positions)?;
    let pixels = footprint
        .map(|b| (b[2] - b[0]) as usize * (b[3] - b[1]) as usize)
        .unwrap_or(0);
    if fill.is_none() && pixels > MAX_REGION_PIXELS {
        return split(
            source, mapping, region, tile, fill, pool, work, check, positions, output,
        );
    }
    let view = if fill.is_none() {
        match footprint {
            Some(bounds) => match source
                .prepare_region(Region::new(bounds).map_err(|e| e.to_string())?, &mut || {
                    check()
                }) {
                Ok(view) => Some(view),
                Err(Error::Budget) if region[2] - region[0] > 1 || region[3] - region[1] > 1 => {
                    return split(
                        source, mapping, region, tile, fill, pool, work, check, positions, output,
                    )
                }
                Err(error) => return Err(error.to_string()),
            },
            None => None,
        }
    } else {
        None
    };
    let mut index = 0;
    for y in region[1]..region[3] {
        check().map_err(|e| e.to_string())?;
        for x in region[0]..region[2] {
            let p = coord(positions, index);
            index += 1;
            let pixel = sample(source, view.as_ref(), p, fill)?;
            let at = ((y - tile.1 * TILE) * TILE + x - tile.0 * TILE) as usize
                * 4
                * usize::from(source.depth() / 8);
            if source.depth() == 16 {
                for (out, v) in output[at..at + 8].chunks_exact_mut(2).zip(pixel) {
                    out.copy_from_slice(&v.to_be_bytes());
                }
            } else {
                for (out, v) in output[at..at + 4].iter_mut().zip(pixel) {
                    *out = (v / 257) as u8;
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn split(
    source: &Surface,
    mapping: &Mapping,
    region: [u32; 4],
    tile: (u32, u32),
    fill: Option<Pixel>,
    pool: &ResidentBudget,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
    positions: &mut [u8],
    output: &mut [u8],
) -> Result<(), String> {
    let w = region[2] - region[0];
    let h = region[3] - region[1];
    if w == 1 && h == 1 {
        return Err("Single transform sample exceeds native footprint admission".into());
    }
    let mut first = region;
    let mut second = region;
    if w >= h {
        let mid = region[0] + w / 2;
        first[2] = mid;
        second[0] = mid;
    } else {
        let mid = region[1] + h / 2;
        first[3] = mid;
        second[1] = mid;
    }
    render(
        source, mapping, first, tile, fill, pool, work, check, positions, output,
    )?;
    render(
        source, mapping, second, tile, fill, pool, work, check, positions, output,
    )
}

fn get(source: &Surface, view: Option<&SurfaceRegion>, x: i32, y: i32) -> Result<Pixel16, String> {
    if x < 0 || y < 0 || x >= source.width() as i32 || y >= source.height() as i32 {
        return Ok([0; 4]);
    }
    view.ok_or("Missing prepared transform source footprint")?
        .pixel16(x, y)
        .map_err(|e| e.to_string())
}
fn sample(
    source: &Surface,
    view: Option<&SurfaceRegion>,
    p: [f32; 2],
    fill: Option<Pixel>,
) -> Result<Pixel16, String> {
    let [x, y] = p;
    if outside(p, source) {
        return Ok([0; 4]);
    }
    if source.depth() == 8 && fill.is_some() {
        return Ok(
            if x >= -0.5
                && y >= -0.5
                && x < source.width() as f32 - 0.5
                && y < source.height() as f32 - 0.5
            {
                fill.unwrap().map(|v| u16::from(v) * 257)
            } else {
                [0; 4]
            },
        );
    }
    if fill.is_none() && x.fract() == 0. && y.fract() == 0. {
        return get(source, view, x as i32, y as i32);
    }
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    if source.depth() == 16 {
        let (fx, fy) = (f64::from(x - x.floor()), f64::from(y - y.floor()));
        let mut sum = [0.0f64; 4];
        let mut weight = 0.;
        for (dx, dy, w) in [
            (0, 0, (1. - fx) * (1. - fy)),
            (1, 0, fx * (1. - fy)),
            (0, 1, (1. - fx) * fy),
            (1, 1, fx * fy),
        ] {
            if fill.is_some() {
                if ix + dx >= 0
                    && iy + dy >= 0
                    && ix + dx < source.width() as i32
                    && iy + dy < source.height() as i32
                {
                    weight += w;
                }
                continue;
            }
            let p = get(source, view, ix + dx, iy + dy)?;
            let a = f64::from(p[3]) / 65535.;
            for c in 0..3 {
                sum[c] += f64::from(p[c]) * a * w;
            }
            sum[3] += a * w;
        }
        if let Some(color) = fill {
            return Ok(if weight <= 0. {
                [0; 4]
            } else {
                [
                    u16::from(color[0]) * 257,
                    u16::from(color[1]) * 257,
                    u16::from(color[2]) * 257,
                    (f64::from(u16::from(color[3]) * 257) * weight).round() as u16,
                ]
            });
        }
        if sum[3] <= 0. {
            return Ok([0; 4]);
        }
        Ok([
            (sum[0] / sum[3]).round().clamp(0., 65535.) as u16,
            (sum[1] / sum[3]).round().clamp(0., 65535.) as u16,
            (sum[2] / sum[3]).round().clamp(0., 65535.) as u16,
            (sum[3] * 65535.).round().clamp(0., 65535.) as u16,
        ])
    } else {
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let mut sum = [0.0f32; 4];
        for (dx, dy, w) in [
            (0, 0, (1. - fx) * (1. - fy)),
            (1, 0, fx * (1. - fy)),
            (0, 1, (1. - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let p = get(source, view, ix + dx, iy + dy)?.map(crate::raster::project16);
            let a = f32::from(p[3]) / 255.;
            for c in 0..3 {
                sum[c] += f32::from(p[c]) * a * w;
            }
            sum[3] += a * w;
        }
        if sum[3] <= 0.00001 {
            return Ok([0; 4]);
        }
        Ok([
            (sum[0] / sum[3]).round() as u8,
            (sum[1] / sum[3]).round() as u8,
            (sum[2] / sum[3]).round() as u8,
            (sum[3] * 255.).round() as u8,
        ]
        .map(|v| u16::from(v) * 257))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{Limits, Store},
        raster::Raster,
    };
    use std::sync::Arc;

    #[test]
    fn identity_plane_retains_explicit_empty_tile_presence_like_legacy_clone() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let (store, _, path) = fixture(&pool);
            let mut input = Raster::new_depth(3, 5, depth);
            if depth == 16 {
                input
                    .samples16
                    .insert((0, 0), Arc::new(vec![0; (TILE * TILE * 4) as usize]));
            } else {
                input
                    .tiles
                    .insert((0, 0), Arc::new(vec![0; (TILE * TILE * 4) as usize]));
            }
            let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
            let mapping =
                Mapping::retained(3, 5, depth, crate::retained::IDENTITY, None, &[], &pool)
                    .unwrap();
            let output = write_plane(
                &source,
                &mapping,
                [3, 5],
                &store,
                &mut Work::new(1_000_000).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(source.physical_tiles().count(), 1);
            assert_eq!(output.physical_tiles().count(), 1);
            assert!(!output.is_known_zero());
            assert!(output
                .prepare_tile((0, 0), &mut || Ok(()))
                .unwrap()
                .bytes()
                .unwrap()
                .iter()
                .all(|b| *b == 0));
            drop(output);
            drop(source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn generated_fill_originals_match_native_set_and_physical_padding() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let (store, _, path) = fixture(&pool);
            for color in [[31, 127, 243, 0], [13, 79, 211, 127], [0; 4]] {
                let output = write_fill(
                    [257, 3],
                    depth,
                    color,
                    &store,
                    &mut Work::new(1_000_000).unwrap(),
                    &|| Ok(()),
                )
                .unwrap();
                let mut expected = Raster::new_depth(257, 3, depth);
                for y in 0..3 {
                    for x in 0..257 {
                        expected.set(x, y, color);
                    }
                }
                for x in 0..2 {
                    let bytes = output
                        .prepare_tile((x, 0), &mut || Ok(()))
                        .unwrap()
                        .copy_to_scratch(&pool)
                        .unwrap()
                        .freeze();
                    assert_tile(
                        &Tile {
                            source: output.key(),
                            coordinate: (x, 0),
                            frame: [257, 3],
                            depth,
                            bytes,
                            preserve_empty: false,
                        },
                        &expected,
                    );
                }
            }
            assert!(write_fill(
                [257, 3],
                depth,
                [31, 127, 243, 0],
                &store,
                &mut Work::new(1).unwrap(),
                &|| Ok(())
            )
            .is_err());
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn whole_plane_output_preserves_native_tiles_and_retires_zero_and_refused_candidates() {
        use crate::large_document::FreshFragment;
        for depth in [8, 16] {
            let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let (store, disk, path) = fixture(&pool);
            let input = pixels(257, 3, depth);
            let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
            let original_key = source.key();
            for (matrix, frame) in [
                (crate::retained::IDENTITY, [257, 3]),
                ([1., 0., 0., 1., 0.25, 0.5], [263, 5]),
            ] {
                let output_path =
                    std::fs::canonicalize(std::env::temp_dir())
                        .unwrap()
                        .join(format!(
                            "peerbrush-transform-output-{}",
                            crate::engine::id()
                        ));
                let fragment = FreshFragment::create(
                    &output_path,
                    pool.clone(),
                    disk.clone(),
                    Limits {
                        disk_bytes: 8 * 1024 * 1024,
                        max_blocks: 16,
                        max_resident_blocks: 4,
                        manifest_bytes: 65536,
                    },
                )
                .unwrap();
                let mapping = Mapping::retained(257, 3, depth, matrix, None, &[], &pool).unwrap();
                let output = write_plane(
                    &source,
                    &mapping,
                    frame,
                    fragment.store(),
                    &mut Work::new(1_000_000).unwrap(),
                    &|| Ok(()),
                )
                .unwrap();
                let mut old = crate::retained::Original::from(&input);
                old.matrix = matrix;
                let expected = old.render(frame[0], frame[1]).unwrap();
                for y in 0..frame[1].div_ceil(TILE) {
                    for x in 0..frame[0].div_ceil(TILE) {
                        let bytes = output
                            .prepare_tile((x, y), &mut || Ok(()))
                            .unwrap()
                            .copy_to_scratch(&pool)
                            .unwrap()
                            .freeze();
                        assert_tile(
                            &Tile {
                                source: source.key(),
                                coordinate: (x, y),
                                frame,
                                depth,
                                bytes,
                                preserve_empty: false,
                            },
                            &expected,
                        );
                    }
                }
                assert_eq!(source.key(), original_key);
                assert_eq!(
                    source
                        .prepare_region(Region::new([256, 2, 257, 3]).unwrap(), &mut || Ok(()))
                        .unwrap()
                        .pixel16(256, 2)
                        .unwrap(),
                    input.get16(256, 2)
                );
                drop(output);
                fragment.abort(&mut || Ok(())).unwrap();
                std::fs::remove_dir(output_path).unwrap();
            }
            let output_path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("peerbrush-transform-zero-{}", crate::engine::id()));
            let fragment = FreshFragment::create(
                &output_path,
                pool.clone(),
                disk.clone(),
                Limits {
                    disk_bytes: 8 * 1024 * 1024,
                    max_blocks: 16,
                    max_resident_blocks: 4,
                    manifest_bytes: 65536,
                },
            )
            .unwrap();
            let mapping =
                Mapping::retained(257, 3, depth, crate::retained::IDENTITY, None, &[], &pool)
                    .unwrap();
            let baseline = pool.used();
            assert!(write_plane(
                &source,
                &mapping,
                [257, 3],
                fragment.store(),
                &mut Work::new(1).unwrap(),
                &|| Ok(())
            )
            .is_err());
            assert_eq!(pool.used(), baseline);
            let zero = Surface::zero(257, 3, depth).unwrap();
            let output = write_plane(
                &zero,
                &mapping,
                [257, 3],
                fragment.store(),
                &mut Work::new(1_000_000).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            assert!(output.is_known_zero());
            assert_eq!(output.physical_tiles().count(), 0);
            drop(output);
            fragment.abort(&mut || Ok(())).unwrap();
            std::fs::remove_dir(output_path).unwrap();
            drop(source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    fn fixture(pool: &ResidentBudget) -> (Store, DiskBudget, std::path::PathBuf) {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "peerbrush-native-transform-{}",
                crate::engine::id()
            ));
        let disk = DiskBudget::new(64 * 1024 * 1024, 4).unwrap();
        let store = Store::create_with_disk_budget(
            &path,
            pool.clone(),
            disk.clone(),
            Limits {
                disk_bytes: 64 * 1024 * 1024,
                max_blocks: 128,
                max_resident_blocks: 16,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        (store, disk, path)
    }
    fn pixels(w: u32, h: u32, depth: u16) -> Raster {
        let mut r = Raster::new_depth(w, h, depth);
        for y in 0..h {
            for x in 0..w {
                r.set16(
                    x as i32,
                    y as i32,
                    [
                        (x * 1931 + y * 911 + 17) as u16,
                        (x * 577 + y * 1319 + 129) as u16,
                        (x * 421 + y * 2377 + 32769) as u16,
                        if (x + y) % 7 == 0 {
                            0
                        } else {
                            (x * 1733 + y * 1471 + 1) as u16
                        },
                    ],
                );
            }
        }
        r
    }
    fn assert_tile(tile: &Tile, expected: &Raster) {
        let stride = usize::from(expected.depth / 8) * 4;
        let key = tile.coordinate();
        for y in 0..TILE {
            for x in 0..TILE {
                let at = ((y * TILE + x) * 4) as usize;
                let expected = if expected.depth == 16 {
                    expected
                        .samples16
                        .get(&key)
                        .map_or([0; 4], |words| words[at..at + 4].try_into().unwrap())
                } else {
                    expected.tiles.get(&key).map_or([0; 4], |bytes| {
                        std::array::from_fn(|c| u16::from(bytes[at + c]) * 257)
                    })
                };
                let at = ((y * TILE + x) as usize) * stride;
                let actual = if tile.depth() == 16 {
                    std::array::from_fn(|c| {
                        u16::from_be_bytes([tile.bytes()[at + c * 2], tile.bytes()[at + c * 2 + 1]])
                    })
                } else {
                    std::array::from_fn(|c| u16::from(tile.bytes()[at + c]) * 257)
                };
                assert_eq!(
                    actual,
                    expected,
                    "native{} tile {:?} at{x},{y}",
                    tile.depth(),
                    key
                );
            }
        }
    }

    #[test]
    fn affine_projective_and_retained_warp_tiles_match_actual_old_native8_and16_operators() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
            let (store, disk, path) = fixture(&pool);
            let mut input = pixels(257, 259, depth);
            // Identity's old clone path retains physical hidden edge padding.
            if depth == 16 {
                Arc::make_mut(input.samples16.get_mut(&(1, 1)).unwrap())[4] = 54321;
            } else {
                Arc::make_mut(input.tiles.get_mut(&(1, 1)).unwrap())[4] = 219;
            }
            let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
            let angle = 0.17f64;
            let (sin, cos) = angle.sin_cos();
            let affine = [cos * 0.83, sin * 0.83, -sin * 1.07, cos * 1.07, 7.25, -2.5];
            let projective = Some([1., 0.03, 0.5, -0.02, 1., 1.25, 0.0007, -0.0003, 1.]);
            let grid = crate::warp::Grid::preset([-1., -1., 258., 260.], "arc", 0.13).unwrap();
            let warps = vec![crate::warp::Stage {
                before: crate::projective::Map::IDENTITY.matrix(),
                grid,
            }];
            for (matrix, projective, warps, frame) in [
                (crate::retained::IDENTITY, None, Vec::new(), [257, 259]),
                (affine, None, Vec::new(), [271, 263]),
                ([-1., 0., 0., 1., 257., 0.], None, Vec::new(), [257, 259]),
                (
                    crate::retained::IDENTITY,
                    projective,
                    Vec::new(),
                    [271, 263],
                ),
                (crate::retained::IDENTITY, None, warps, [271, 283]),
            ] {
                let mapping =
                    Mapping::retained(257, 259, depth, matrix, projective, &warps, &pool).unwrap();
                let mut original = crate::retained::Original::from(&input);
                original.matrix = matrix;
                original.projective = projective;
                // The immutable reference and current geometry use the same
                // wire metadata, without sharing their mapping implementation.
                original.warps =
                    serde_json::from_value(serde_json::to_value(&warps).unwrap()).unwrap();
                let expected = original.render(frame[0], frame[1]).unwrap();
                for ty in 0..frame[1].div_ceil(TILE) {
                    for tx in 0..frame[0].div_ceil(TILE) {
                        let actual = render_tile(
                            &source,
                            &mapping,
                            frame,
                            (tx, ty),
                            None,
                            &pool,
                            &disk,
                            &mut Work::new(2_000_000).unwrap(),
                            &|| Ok(()),
                        )
                        .unwrap();
                        assert_eq!(actual.source(), source.key());
                        assert_eq!(actual.frame(), frame);
                        assert_tile(&actual, &expected);
                    }
                }
                let before = input.get16(256, 258);
                let view = source
                    .prepare_region(Region::new([256, 258, 257, 259]).unwrap(), &mut || Ok(()))
                    .unwrap();
                assert_eq!(view.pixel16(256, 258).unwrap(), before);
            }
            drop(source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn oversized_mapped_footprint_splits_without_full_source_materialization_or_arithmetic_changes()
    {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let (store, disk, path) = fixture(&pool);
            let input = pixels(1024, 1024, depth);
            let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
            let mapping = Mapping::retained(
                1024,
                1024,
                depth,
                [0.05, 0., 0., 0.05, 0., 0.],
                None,
                &[],
                &pool,
            )
            .unwrap();
            let mut original = crate::retained::Original::from(&input);
            original.matrix = mapping.matrix();
            let expected = original.render(33, 31).unwrap();
            let mut work = Work::new(100_000).unwrap();
            let actual = render_tile(
                &source,
                &mapping,
                [33, 31],
                (0, 0),
                None,
                &pool,
                &disk,
                &mut work,
                &|| Ok(()),
            )
            .unwrap();
            assert!(work.used() > 33 * 31);
            assert_tile(&actual, &expected);
            drop(actual);
            drop(source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn native_fill_edges_keep_depth_specific_old_sampling_and_alpha_rounding() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let (store, disk, path) = fixture(&pool);
            let input = pixels(3, 5, depth);
            let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
            let mapping =
                Mapping::retained(3, 5, depth, [1., 0., 0., 1., 0.25, 0.5], None, &[], &pool)
                    .unwrap();
            let fill = Some([200, 31, 77, 129]);
            let expected =
                super::super::resample(&input, 7, 9, fill, |x, y| mapping.position(x, y));
            let actual = render_tile(
                &source,
                &mapping,
                [7, 9],
                (0, 0),
                fill,
                &pool,
                &disk,
                &mut Work::new(1000).unwrap(),
                &|| Ok(()),
            )
            .unwrap();
            assert_tile(&actual, &expected);
            drop(actual);
            drop(source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn cancellation_work_pool_and_foreign_source_failures_return_no_tile() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let (store, disk, path) = fixture(&pool);
        let input = pixels(17, 19, 16);
        let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
        let mapping =
            Mapping::retained(17, 19, 16, [1., 0., 0., 1., 0.25, 0.5], None, &[], &pool).unwrap();
        for stop in [0, 5, 40] {
            let calls = std::cell::Cell::new(0);
            assert!(render_tile(
                &source,
                &mapping,
                [23, 25],
                (0, 0),
                None,
                &pool,
                &disk,
                &mut Work::new(1000).unwrap(),
                &|| {
                    calls.set(calls.get() + 1);
                    if calls.get() > stop {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                }
            )
            .is_err());
        }
        assert!(render_tile(
            &source,
            &mapping,
            [23, 25],
            (0, 0),
            None,
            &pool,
            &disk,
            &mut Work::new(10).unwrap(),
            &|| Ok(())
        )
        .is_err());
        assert!(render_tile(
            &source,
            &mapping,
            [23, 25],
            (0, 0),
            None,
            &ResidentBudget::new(pool.limit()).unwrap(),
            &disk,
            &mut Work::new(1000).unwrap(),
            &|| Ok(())
        )
        .is_err());
        assert!(render_tile(
            &source,
            &mapping,
            [23, 25],
            (0, 0),
            None,
            &pool,
            &DiskBudget::new(disk.limit(), 4).unwrap(),
            &mut Work::new(1000).unwrap(),
            &|| Ok(())
        )
        .is_err());
        let (foreign_store, _, foreign_path) = fixture(&pool);
        let tile = source.prepare_tile((0, 0), &mut || Ok(())).unwrap();
        let bytes = tile.copy_to_scratch(&pool).unwrap();
        let reference = foreign_store
            .put(
                crate::large_document::TileSpec::new(TILE as u16, TILE as u16, 16).unwrap(),
                &bytes,
                &mut || Ok(()),
            )
            .unwrap();
        let foreign = Surface::from_plane(
            crate::raster::backed::Plane::from_blocks(
                17,
                19,
                16,
                store.clone(),
                std::collections::BTreeMap::from([((0, 0), reference)]),
            )
            .unwrap(),
        )
        .unwrap();
        // A real backing admission error cannot become a transparent pixel.
        assert!(render_tile(
            &foreign,
            &mapping,
            [23, 25],
            (0, 0),
            None,
            &pool,
            &disk,
            &mut Work::new(1000).unwrap(),
            &|| Ok(())
        )
        .is_err());
        let unchanged = source
            .prepare_region(Region::new([0, 0, 17, 19]).unwrap(), &mut || Ok(()))
            .unwrap();
        for y in 0..19 {
            for x in 0..17 {
                assert_eq!(unchanged.pixel16(x, y).unwrap(), input.get16(x, y));
            }
        }
        drop(unchanged);
        drop(foreign);
        drop(bytes);
        drop(tile);
        drop(foreign_store);
        drop(source);
        drop(store);
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(foreign_path).unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }
}
