//! Exact hidden-channel support, with bounded physical tile pins.
use super::Work;
use crate::{
    large_document::{DiskBudget, ResidentBudget, Result},
    raster::{Surface, TILE},
};
use std::collections::{btree_map::Entry, BTreeMap};

pub(crate) fn source_bounds(
    source: &Surface,
    pool: &ResidentBudget,
    disk: &DiskBudget,
    work: &mut Work,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<Option<[i32; 4]>, String> {
    data_bounds(source, pool, disk, work, check, true)
}

/// Folder pivots use visible current alpha support, unlike original-source
/// support, which includes editable RGB underneath transparent pixels.
pub(crate) fn content_bounds(
    source: &Surface,
    pool: &ResidentBudget,
    disk: &DiskBudget,
    work: &mut Work,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<Option<[i32; 4]>, String> {
    data_bounds(source, pool, disk, work, check, false)
}

/// Current alpha support for the same roots/hidden descendants as resident gizmos.
pub(crate) fn tree_bounds(
    document: &crate::engine::native::NativeDocument,
    roots: &[String],
    work: &mut Work,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<Option<[i32; 4]>, String> {
    check().map_err(|e| e.to_string())?;
    let bytes = document
        .metadata()
        .layers()
        .try_fold(1024usize, |n, layer| {
            layer
                .id()
                .len()
                .checked_mul(3)
                .and_then(|v| v.checked_add(256))
                .and_then(|v| n.checked_add(v))
                .ok_or("Transform tree metadata exceeds admission")
        })?;
    let _charge = document
        .budget()
        .reserve_bytes(bytes, 64 * 1024 * 1024)
        .map_err(|e| e.to_string())?;
    for root in roots {
        if !document.metadata().layers().any(|layer| layer.id() == root) {
            return Err("Transform target no longer exists".into());
        }
    }
    let ids = crate::transform::tree_ids_view(document.into(), roots);
    let mut bounds: Option<[i32; 4]> = None;
    for layer in document.metadata().layers() {
        check().map_err(|e| e.to_string())?;
        work.claim(1)?;
        if !ids.iter().any(|id| id == layer.id()) || matches!(layer.kind(), "group" | "adjustment")
        {
            continue;
        }
        let pixels = layer.pixels();
        let b = content_bounds(
            document.source(pixels.slot())?,
            document.budget(),
            document.disk_budget(),
            work,
            check,
        )?
        .unwrap_or([0, 0, pixels.width() as i32, pixels.height() as i32]);
        let [x, y] = layer.origin();
        let mut translated = [0; 4];
        for (i, offset) in [x, y, x, y].into_iter().enumerate() {
            translated[i] = b[i]
                .checked_add(offset)
                .ok_or("Transform bounds exceed document coordinates")?;
        }
        bounds = Some(bounds.map_or(translated, |a| {
            [
                a[0].min(translated[0]),
                a[1].min(translated[1]),
                a[2].max(translated[2]),
                a[3].max(translated[3]),
            ]
        }));
    }
    check().map_err(|e| e.to_string())?;
    Ok(bounds)
}

fn data_bounds(
    source: &Surface,
    pool: &ResidentBudget,
    disk: &DiskBudget,
    work: &mut Work,
    check: &dyn Fn() -> Result<()>,
    hidden: bool,
) -> std::result::Result<Option<[i32; 4]>, String> {
    check().map_err(|e| e.to_string())?;
    if !source.uses_budget(pool) || !source.uses_disk_budget(disk) {
        return Err("Transform support source differs from shared admission".into());
    }
    let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let stride = usize::from(source.depth() / 8) * 4;
    let bytes = source.physical_tiles().count() * 128;
    let _index = pool
        .reserve_bytes(bytes, 512 * 1024)
        .map_err(|e| e.to_string())?;
    let mut support = BTreeMap::new();
    for physical in source.physical_tiles() {
        check().map_err(|e| e.to_string())?;
        let (tx, ty) = physical.coordinate();
        let left = tx * TILE;
        let top = ty * TILE;
        let width = (source.width() - left).min(TILE);
        let height = (source.height() - top).min(TILE);
        work.claim(1)?;
        let tile = source
            .prepare_tile((tx, ty), &mut || check())
            .map_err(|e| e.to_string())?;
        let bytes = tile
            .bytes()
            .ok_or("Present native support tile has no admitted bytes")?;
        // Identical bytes can have different support at a clipped canvas edge.
        let key = (physical.reference().digest(), width, height);
        let local = match support.entry(key) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                *entry.insert(tile_bounds(bytes, [width, height], stride, hidden, work, check)?)
            }
        };
        if let Some([l, t, r, b]) = local {
            bounds[0] = bounds[0].min((left + l) as i32);
            bounds[1] = bounds[1].min((top + t) as i32);
            bounds[2] = bounds[2].max((left + r) as i32);
            bounds[3] = bounds[3].max((top + b) as i32);
        }
    }
    check().map_err(|e| e.to_string())?;
    Ok((bounds[0] < bounds[2] && bounds[1] < bounds[3]).then_some(bounds))
}

fn tile_bounds(
    bytes: &[u8],
    [width, height]: [u32; 2],
    stride: usize,
    hidden: bool,
    work: &mut Work,
    check: &dyn Fn() -> Result<()>,
) -> std::result::Result<Option<[u32; 4]>, String> {
    // RGB beneath zero alpha remains editable. Inspect both bytes of native16.
    let channel = if hidden { 0 } else { stride / 4 * 3 };
    let occupied = |pixel: &[u8]| pixel[channel..].iter().any(|&byte| byte != 0);
    let mut bounds = [width, height, 0, 0];
    for (y, row) in bytes
        .chunks_exact(TILE as usize * stride)
        .take(height as usize)
        .enumerate()
    {
        check().map_err(|e| e.to_string())?;
        let mut pixels = row[..width as usize * stride].chunks_exact(stride);
        let Some(first) = pixels.position(occupied) else {
            work.claim(u64::from(width))?;
            continue;
        };
        let last = pixels.rposition(occupied).map_or(first, |at| first + 1 + at);
        // The samples strictly between the two known endpoints were not scanned.
        work.claim(u64::from(width) - (last - first).saturating_sub(1) as u64)?;
        bounds[0] = bounds[0].min(first as u32);
        bounds[1] = bounds[1].min(y as u32);
        bounds[2] = bounds[2].max(last as u32 + 1);
        bounds[3] = y as u32 + 1;
    }
    Ok((bounds[0] < bounds[2]).then_some(bounds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        large_document::{Error, Limits, Store},
        raster::Raster,
    };
    use std::sync::Arc;

    #[test]
    fn repeated_tiles_keep_exact_support_at_large_canvas_edges() {
        use crate::{large_document::TileSpec, raster::backed::Plane};

        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(16 * 1024 * 1024, 4).unwrap();
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-native-repeated-support-{}", crate::engine::id()));
        let store = Store::create_with_disk_budget(
            &path,
            pool.clone(),
            disk.clone(),
            Limits {
                disk_bytes: 16 * 1024 * 1024,
                max_blocks: 4,
                max_resident_blocks: 4,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        let spec = TileSpec::new(TILE as u16, TILE as u16, 16).unwrap();
        let mut bytes = vec![0; spec.bytes()];
        bytes[7] = 1;
        bytes[TILE as usize * 8 - 1] = 1;
        bytes[spec.bytes() - 1] = 1;
        let reference = store.put(spec, &bytes, &mut || Ok(())).unwrap();
        let blocks = (0..63)
            .flat_map(|n| [((0, n), reference), ((n, 0), reference)])
            .chain([((62, 62), reference)])
            .collect();
        let source = Surface::from_plane(
            Plane::from_blocks(16000, 16000, 16, store.clone(), blocks).unwrap(),
        )
        .unwrap();
        let mut work = Work::new(160_000).unwrap();
        assert_eq!(
            content_bounds(&source, &pool, &disk, &mut work, &|| Ok(())).unwrap(),
            Some([0, 0, 15873, 15873])
        );
        assert_eq!(work.used(), 147_456 - 2 * (u64::from(TILE) - 2) + 126);
        drop(source);
        drop(store);
        assert_eq!(pool.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn support_matches_resident_hidden_channels_and_native_low_bits_without_padding() {
        for depth in [8, 16] {
            let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(16 * 1024 * 1024, 4).unwrap();
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("peerbrush-native-support-{}", crate::engine::id()));
            let store = Store::create_with_disk_budget(
                &path,
                pool.clone(),
                disk.clone(),
                Limits {
                    disk_bytes: 16 * 1024 * 1024,
                    max_blocks: 32,
                    max_resident_blocks: 8,
                    manifest_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            let mut input = Raster::new_depth(257, 259, depth);
            input.set16(9, 11, [0, if depth == 16 { 1 } else { 257 }, 0, 0]);
            input.set16(256, 258, [257, 3, 0, 1]);
            if depth == 16 {
                Arc::make_mut(input.samples16.get_mut(&(1, 1)).unwrap())[4] = 65535;
            } else {
                Arc::make_mut(input.tiles.get_mut(&(1, 1)).unwrap())[4] = 255;
            }
            let source = Surface::stage_raster(&input, store.clone(), &mut || Ok(())).unwrap();
            let mut work = Work::new(100_000).unwrap();
            assert_eq!(
                source_bounds(&source, &pool, &disk, &mut work, &|| Ok(())).unwrap(),
                input.source_bounds()
            );
            assert_eq!(work.used(), 256 * 256 + 3 + 2);
            let mut visible_work = Work::new(100_000).unwrap();
            assert_eq!(
                content_bounds(&source, &pool, &disk, &mut visible_work, &|| Ok(())).unwrap(),
                input.content_bounds()
            );
            assert_eq!(visible_work.used(), 256 * 256 + 3 + 2);
            assert!(
                source_bounds(&source, &pool, &disk, &mut Work::new(10).unwrap(), &|| Ok(
                    ()
                ))
                .is_err()
            );
            assert!(source_bounds(
                &source,
                &pool,
                &disk,
                &mut Work::new(100_000).unwrap(),
                &|| Err(Error::Cancelled)
            )
            .is_err());
            let empty = Surface::zero(16000, 16000, depth).unwrap();
            let mut work = Work::new(1).unwrap();
            assert_eq!(
                source_bounds(&empty, &pool, &disk, &mut work, &|| Ok(())).unwrap(),
                None
            );
            assert_eq!(work.used(), 0);
            drop(source);
            drop(store);
            assert_eq!(pool.used(), 0);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
