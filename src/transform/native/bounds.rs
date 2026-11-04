//! Exact hidden-channel support, with bounded physical tile pins.
use super::Work;
use crate::{
    large_document::{DiskBudget, ResidentBudget, Result},
    raster::{Surface, TILE},
};

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
    for physical in source.physical_tiles() {
        check().map_err(|e| e.to_string())?;
        let (tx, ty) = physical.coordinate();
        let left = tx * TILE;
        let top = ty * TILE;
        let width = (source.width() - left).min(TILE);
        let height = (source.height() - top).min(TILE);
        work.claim(u64::from(width) * u64::from(height))?;
        let tile = source
            .prepare_tile((tx, ty), &mut || check())
            .map_err(|e| e.to_string())?;
        let bytes = tile
            .bytes()
            .ok_or("Present native support tile has no admitted bytes")?;
        for y in 0..height {
            check().map_err(|e| e.to_string())?;
            for x in 0..width {
                let at = (y * TILE + x) as usize * stride;
                // Red/green/blue under zero alpha remain editable source data;
                // native16 low bits must not pass through a display projection.
                let first = if hidden { at } else { at + stride / 4 * 3 };
                if bytes[first..at + stride].iter().any(|v| *v != 0) {
                    let x = (left + x) as i32;
                    let y = (top + y) as i32;
                    bounds = [
                        bounds[0].min(x),
                        bounds[1].min(y),
                        bounds[2].max(x + 1),
                        bounds[3].max(y + 1),
                    ];
                }
            }
        }
    }
    check().map_err(|e| e.to_string())?;
    Ok((bounds[0] < bounds[2] && bounds[1] < bounds[3]).then_some(bounds))
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
            assert_eq!(work.used(), 256 * 256 + 3);
            let mut visible_work = Work::new(100_000).unwrap();
            assert_eq!(
                content_bounds(&source, &pool, &disk, &mut visible_work, &|| Ok(())).unwrap(),
                input.content_bounds()
            );
            assert_eq!(visible_work.used(), 256 * 256 + 3);
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
