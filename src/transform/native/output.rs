//! Complete provisional native output. The owning caller must seal or retire
//! its registered fragment; this function grants no publication authority.
use super::*;
use crate::{large_document::Store, raster::backed::Plane};
use std::collections::{btree_map::Entry, BTreeMap};

const COPY_TILE: u8 = 2;

/// Materialize a newly authored fill original with the same native conversion
/// and physical edge padding as Raster::set, without a resident full frame.
pub(crate) fn write_fill(
    frame: [u32; 2],
    depth: u16,
    color: Pixel,
    store: &Store,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Surface, String> {
    check().map_err(|e| e.to_string())?;
    if frame.iter().any(|n| *n == 0 || *n > 16000)
        || ![8, 16].contains(&depth)
        || store.disk_budget().is_none()
    {
        return Err("Invalid native fill source admission".into());
    }
    if color == [0; 4] {
        return Surface::zero(frame[0], frame[1], depth).map_err(|e| e.to_string());
    }
    let pool = store.resident_budget();
    let count = u64::from(frame[0].div_ceil(TILE)) * u64::from(frame[1].div_ceil(TILE));
    let size = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(512))
        .and_then(|n| n.checked_add(4096))
        .ok_or("Native fill index overflow")?;
    let index = pool.reserve_bytes(size, size).map_err(|e| e.to_string())?;
    let mut blocks = BTreeMap::new();
    let spec = crate::large_document::TileSpec::new(TILE as u16, TILE as u16, depth)
        .map_err(|e| e.to_string())?;
    let mut tile =
        ScratchBytes::new(&pool, spec.bytes(), spec.bytes()).map_err(|e| e.to_string())?;
    let mut words = [0; 8];
    let pixel: &[u8] = if depth == 16 {
        for (out, value) in words.chunks_exact_mut(2).zip(color) {
            out.copy_from_slice(&(u16::from(value) * 257).to_be_bytes());
        }
        &words
    } else {
        &color
    };
    let row_bytes = TILE as usize * pixel.len();
    // Full tile, right edge, bottom edge and corner share immutable blocks.
    let mut shapes = [[None; 2]; 2];
    for y in 0..frame[1].div_ceil(TILE) {
        let height = TILE.min(frame[1] - y * TILE);
        for x in 0..frame[0].div_ceil(TILE) {
            check().map_err(|e| e.to_string())?;
            work.claim(1)?;
            let width = TILE.min(frame[0] - x * TILE);
            let shape = &mut shapes[usize::from(height != TILE)][usize::from(width != TILE)];
            let reference = if let Some(reference) = *shape {
                reference
            } else {
                work.claim(u64::from(TILE) * u64::from(TILE))?;
                tile.fill(0);
                let (first, rest) = tile.split_at_mut(row_bytes);
                for out in first[..width as usize * pixel.len()].chunks_exact_mut(pixel.len()) {
                    out.copy_from_slice(pixel);
                }
                for row in rest.chunks_exact_mut(row_bytes).take(height as usize - 1) {
                    check().map_err(|e| e.to_string())?;
                    row.copy_from_slice(first);
                }
                let reference = store
                    .put(spec, &tile, &mut || check())
                    .map_err(|e| e.to_string())?;
                *shape = Some(reference);
                reference
            };
            blocks.insert((x, y), reference);
        }
    }
    drop(tile);
    check().map_err(|e| e.to_string())?;
    store.check_writable_owner().map_err(|e| e.to_string())?;
    let output = Surface::from_plane(
        Plane::from_blocks(frame[0], frame[1], depth, store.clone(), blocks)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    drop(index);
    check().map_err(|e| e.to_string())?;
    Ok(output)
}

pub(crate) fn write_plane(
    source: &Surface,
    mapping: &Mapping,
    frame: [u32; 2],
    store: &Store,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Surface, String> {
    write_plane_guarded(source, mapping, frame, store, work, check, check)
}

pub(crate) fn write_plane_guarded(
    source: &Surface,
    mapping: &Mapping,
    frame: [u32; 2],
    store: &Store,
    work: &mut Work,
    active: &dyn Fn() -> crate::large_document::Result<()>,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Surface, String> {
    let checks = Checks { active, owner: check };
    check().map_err(|e| e.to_string())?;
    if frame.iter().any(|n| *n == 0 || *n > 16000) {
        return Err("Invalid native transform output frame".into());
    }
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Native transform output has no shared disk admission")?;
    mapping.check_source(source, &pool, &disk)?;
    if source.is_known_zero() {
        check().map_err(|e| e.to_string())?;
        return Surface::zero(frame[0], frame[1], source.depth()).map_err(|e| e.to_string());
    }
    let count = u64::from(frame[0].div_ceil(TILE)) * u64::from(frame[1].div_ceil(TILE));
    let bytes = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(512))
        .and_then(|n| n.checked_add(4096))
        .ok_or("Native transform output index overflow")?;
    // Keep this provisional B-tree admission through permanent descriptor
    // conversion. Its payload drops before the reservation on every failure.
    let index = pool
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut blocks = BTreeMap::new();
    let spec = crate::large_document::TileSpec::new(TILE as u16, TILE as u16, source.depth())
        .map_err(|e| e.to_string())?;
    let columns = frame[0].div_ceil(TILE);
    let tile_edge = f64::from(TILE);
    let translation = match (&mapping.kind, mapping.matrix) {
        (Kind::Affine { .. }, [1., 0., 0., 1., tx, ty])
            if tx % tile_edge == 0. && ty % tile_edge == 0. =>
        {
            Some([(tx / tile_edge) as i32, (ty / tile_edge) as i32])
        }
        _ => None,
    };
    let selected = match &mapping.kind {
        Kind::Affine { .. } if integral_orthogonal(mapping.matrix) => {
            Some(occupied_output_tiles(
                source,
                mapping,
                frame,
                translation.is_some(),
                &pool,
                work,
                check,
            )?)
        }
        Kind::Affine { .. } => {
            Some(mapped_output_tiles(source, mapping, frame, &pool, work, check)?)
        }
        Kind::Projective(_) | Kind::Warp(_) if source.physical_tiles().len()
            < (source.width().div_ceil(TILE) * source.height().div_ceil(TILE)) as usize =>
        {
            // Planning reads the captured tile index without opening backing files.
            let tiles = mapped_output_tiles(source, mapping, frame, &pool, work, active)?;
            check().map_err(|e| e.to_string())?;
            Some(tiles)
        }
        Kind::Projective(_) | Kind::Warp(_) => None,
    };
    // Keep copies in the destination owner so a prior provisional source can
    // retire independently. Both indexes fit the provisional per-tile charge.
    let mut copied = BTreeMap::new();
    let identity = mapping.identity() && frame == mapping.input;
    for y in 0..frame[1].div_ceil(TILE) {
        for x in 0..columns {
            let mode = selected
                .as_ref()
                .map_or(1, |tiles| tiles[(y * columns + x) as usize]);
            if mode == 0 {
                continue;
            }
            check().map_err(|e| e.to_string())?;
            if mode == COPY_TILE {
                let [dx, dy] = translation.ok_or("Missing aligned native transform offset")?;
                let coordinate = ((x as i32 - dx) as u32, (y as i32 - dy) as u32);
                let tile = source
                    .physical_tile(coordinate)
                    .ok_or("Missing aligned native source tile")?;
                // Charge the verified tile transfer without charging per-pixel mapping.
                work.claim(u64::from(TILE))?;
                let pinned = tile
                    .owner()
                    .pin(tile.reference(), &mut || check())
                    .map_err(|e| e.to_string())?;
                let digest = tile.reference().digest();
                let reference = match copied.entry(digest) {
                    Entry::Occupied(entry) => *entry.get(),
                    Entry::Vacant(entry) => {
                        let reference = if identity || pinned.bytes().iter().any(|&byte| byte != 0) {
                            Some(
                                store
                                    .put_guarded(
                                        spec,
                                        pinned.bytes(),
                                        &mut || active(),
                                        &mut || check(),
                                    )
                                    .map_err(|e| e.to_string())?,
                            )
                        } else {
                            None
                        };
                        *entry.insert(reference)
                    }
                };
                if let Some(reference) = reference {
                    blocks.insert((x, y), reference);
                }
                continue;
            }
            let tile = render_tile_guarded(
                source,
                mapping,
                frame,
                (x, y),
                None,
                &pool,
                &disk,
                work,
                checks,
            )?;
            if tile.preserve_empty || tile.bytes().iter().any(|b| *b != 0) {
                let reference = store
                    .put_guarded(spec, tile.bytes(), &mut || active(), &mut || check())
                    .map_err(|e| e.to_string())?;
                blocks.insert((x, y), reference);
            }
        }
    }
    drop(copied);
    check().map_err(|e| e.to_string())?;
    let output = if blocks.is_empty() {
        drop(blocks);
        Surface::zero(frame[0], frame[1], source.depth()).map_err(|e| e.to_string())?
    } else {
        store.check_writable_owner().map_err(|e| e.to_string())?;
        Surface::from_plane(
            Plane::from_blocks(frame[0], frame[1], source.depth(), store.clone(), blocks)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
    };
    drop(index);
    check().map_err(|e| e.to_string())?;
    Ok(output)
}

fn occupied_output_tiles(
    source: &Surface,
    mapping: &Mapping,
    frame: [u32; 2],
    aligned_translation: bool,
    pool: &ResidentBudget,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<ScratchBytes, String> {
    let columns = frame[0].div_ceil(TILE);
    let count = (columns * frame[1].div_ceil(TILE)) as usize;
    let mut selected = ScratchBytes::new(pool, count, count).map_err(|e| e.to_string())?;
    let [a, b, c, d, tx, ty] = mapping.matrix;
    let identity = mapping.identity() && frame == mapping.input;
    let mut samples = 0;
    for tile in source.physical_tiles() {
        check().map_err(|e| e.to_string())?;
        work.claim(1)?;
        let (x, y) = tile.coordinate();
        let (l, t) = (f64::from(x * TILE), f64::from(y * TILE));
        let (r, bottom) = (
            f64::from(((x + 1) * TILE).min(source.width())),
            f64::from(((y + 1) * TILE).min(source.height())),
        );
        let map = |x, y| [a * x + c * y + tx, b * x + d * y + ty];
        let [p, q] = [map(l, t), map(r, bottom)];
        let first = std::array::from_fn::<_, 2, _>(|i| {
            p[i].min(q[i]).clamp(0., f64::from(frame[i])) as u32
        });
        let end = std::array::from_fn::<_, 2, _>(|i| {
            p[i].max(q[i]).clamp(0., f64::from(frame[i])) as u32
        });
        if first[0] >= end[0] || first[1] >= end[1] {
            continue;
        }
        for y in first[1] / TILE..end[1].div_ceil(TILE) {
            for x in first[0] / TILE..end[0].div_ceil(TILE) {
                let marked = &mut selected[(y * columns + x) as usize];
                if *marked == 0 {
                    // A shifted partial tile can contain old padding. Only a full
                    // tile or an exact same-frame identity can copy all its bytes.
                    if aligned_translation
                        && (identity
                            || (r - l == f64::from(TILE)
                                && bottom - t == f64::from(TILE)
                                && end[0] - first[0] == TILE
                                && end[1] - first[1] == TILE))
                    {
                        *marked = COPY_TILE;
                        samples += u64::from(TILE);
                        continue;
                    }
                    *marked = 1;
                    let w = (frame[0] - x * TILE).min(TILE);
                    let h = (frame[1] - y * TILE).min(TILE);
                    samples += orthogonal_work(w, h);
                }
            }
        }
    }
    work.checked_total(samples)?;
    Ok(selected)
}
fn mapped_output_tiles(
    source: &Surface,
    mapping: &Mapping,
    frame: [u32; 2],
    pool: &ResidentBudget,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<ScratchBytes, String> {
    let columns = frame[0].div_ceil(TILE);
    let rows = frame[1].div_ceil(TILE);
    let count = (columns * rows) as usize;
    let mut selected = ScratchBytes::new(pool, count, count).map_err(|e| e.to_string())?;
    let physical_tiles = source.physical_tiles().len() as u64;
    let mut samples = 0;
    for y in 0..rows {
        for x in 0..columns {
            check().map_err(|e| e.to_string())?;
            let left = x * TILE;
            let top = y * TILE;
            let right = (left + TILE).min(frame[0]);
            let bottom = (top + TILE).min(frame[1]);
            let region = [left, top, right, bottom];
            let bounds = match &mapping.kind {
                Kind::Affine { .. } => {
                    let Some(bounds) = affine_footprint(source, mapping, region, work)? else {
                        continue;
                    };
                    Some(bounds)
                }
                Kind::Projective(inverse) => {
                    work.claim(4)?;
                    inverse.sampling_bounds(region.map(f64::from)).map(super::bilinear_bounds)
                }
                Kind::Warp(inverse) => {
                    work.claim(4 * inverse.sample_cost())?;
                    match inverse.inverse_bounds(region.map(f64::from)) {
                        Some(None) => continue,
                        Some(Some(bounds)) => Some(super::bilinear_bounds(bounds)),
                        None => None,
                    }
                }
            };
            if let Some(bounds) = bounds {
                let [l, t, r, b] = [
                    bounds[0].clamp(0, source.width() as i32),
                    bounds[1].clamp(0, source.height() as i32),
                    bounds[2].clamp(0, source.width() as i32),
                    bounds[3].clamp(0, source.height() as i32),
                ].map(|v| v as u32);
                if l >= r || t >= b {
                    continue;
                }
                let probes = u64::from((r.div_ceil(TILE) - l / TILE) * (b.div_ceil(TILE) - t / TILE));
                work.claim(probes.min(physical_tiles))?;
                if !source.has_physical_tiles(bounds) {
                    continue;
                }
            }
            selected[(y * columns + x) as usize] = 1;
            samples += u64::from(right - left) * u64::from(bottom - top) * mapping.sample_cost();
            work.checked_total(samples)?;
        }
    }
    Ok(selected)
}
