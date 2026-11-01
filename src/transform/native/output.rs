//! Complete provisional native output. The owning caller must seal or retire
//! its registered fragment; this function grants no publication authority.
use super::*;
use crate::{large_document::Store, raster::backed::Plane};
use std::collections::BTreeMap;

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
    let stride = usize::from(depth / 8) * 4;
    for y in 0..frame[1].div_ceil(TILE) {
        for x in 0..frame[0].div_ceil(TILE) {
            check().map_err(|e| e.to_string())?;
            work.claim(u64::from(TILE) * u64::from(TILE))?;
            tile.fill(0);
            for py in 0..TILE.min(frame[1] - y * TILE) {
                check().map_err(|e| e.to_string())?;
                for px in 0..TILE.min(frame[0] - x * TILE) {
                    let at = ((py * TILE + px) as usize) * stride;
                    if depth == 16 {
                        for (out, value) in tile[at..at + 8].chunks_exact_mut(2).zip(color) {
                            out.copy_from_slice(&(u16::from(value) * 257).to_be_bytes());
                        }
                    } else {
                        tile[at..at + 4].copy_from_slice(&color);
                    }
                }
            }
            blocks.insert(
                (x, y),
                store
                    .put(spec, &tile, &mut || check())
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    drop(tile);
    check().map_err(|e| e.to_string())?;
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
    check().map_err(|e| e.to_string())?;
    if frame.iter().any(|n| *n == 0 || *n > 16000) {
        return Err("Invalid native transform output frame".into());
    }
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Native transform output has no shared disk admission")?;
    if !source.uses_budget(&pool) || !source.uses_disk_budget(&disk) {
        return Err("Native transform output uses another source admission".into());
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
    for y in 0..frame[1].div_ceil(TILE) {
        for x in 0..frame[0].div_ceil(TILE) {
            check().map_err(|e| e.to_string())?;
            let tile = render_tile(
                source,
                mapping,
                frame,
                (x, y),
                None,
                &pool,
                &disk,
                work,
                check,
            )?;
            if tile.preserve_empty || tile.bytes().iter().any(|b| *b != 0) {
                let reference = store
                    .put(spec, tile.bytes(), &mut || check())
                    .map_err(|e| e.to_string())?;
                blocks.insert((x, y), reference);
            }
        }
    }
    check().map_err(|e| e.to_string())?;
    let output = if blocks.is_empty() {
        drop(blocks);
        Surface::zero(frame[0], frame[1], source.depth()).map_err(|e| e.to_string())?
    } else {
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
