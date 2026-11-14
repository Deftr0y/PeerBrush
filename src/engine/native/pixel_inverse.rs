//! Native three-way task pixels. Only immutable prepared inputs are sampled;
//! the result grants no staging, publication or history authority.
use super::*;
use crate::large_document::PinnedBytes;

pub(crate) struct PixelInverse<'a> {
    before: &'a Surface,
    after: &'a Surface,
    current: &'a Surface,
    pool: &'a ResidentBudget,
}
pub(crate) enum TileInverse {
    Unchanged,
    Merged {
        pixels: PinnedBytes,
        changed: [u32; 4],
    },
    Conflict {
        changed: [u32; 4],
        conflicts: [u32; 4],
    },
}
impl<'a> PixelInverse<'a> {
    pub(crate) fn new(
        before: &'a Surface,
        after: &'a Surface,
        current: &'a Surface,
        pool: &'a ResidentBudget,
        disk: &DiskBudget,
    ) -> Result<Self> {
        let frame = |surface: &Surface| (surface.width(), surface.height(), surface.depth());
        if frame(before) != frame(after) || frame(after) != frame(current) {
            return Err("Native task raster frame or precision changed".into());
        }
        for surface in [before, after, current] {
            if !surface.uses_budget(pool) || !surface.uses_disk_budget(disk) {
                return Err("Native task source uses another admission".into());
            }
        }
        Ok(Self {
            before,
            after,
            current,
            pool,
        })
    }
    /// Union of physically changed before/after descriptors, without a full-grid
    /// allocation or source reads. Equal content in distinct Stores is equal.
    pub(crate) fn coordinates(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let mut before = self.before.physical_tiles().peekable();
        let mut after = self.after.physical_tiles().peekable();
        std::iter::from_fn(move || loop {
            let left = before.peek().map(|tile| {
                (
                    tile.coordinate(),
                    tile.reference().spec(),
                    tile.reference().digest(),
                )
            });
            let right = after.peek().map(|tile| {
                (
                    tile.coordinate(),
                    tile.reference().spec(),
                    tile.reference().digest(),
                )
            });
            match (left, right) {
                (None, None) => return None,
                (Some(left), Some(right)) if left.0 == right.0 => {
                    before.next();
                    after.next();
                    if left != right {
                        return Some(left.0);
                    }
                }
                (Some(left), Some(right)) if left.0 < right.0 => {
                    before.next();
                    return Some(left.0);
                }
                (Some(_), Some(right)) => {
                    after.next();
                    return Some(right.0);
                }
                (Some(left), None) => {
                    before.next();
                    return Some(left.0);
                }
                (None, Some(right)) => {
                    after.next();
                    return Some(right.0);
                }
            }
        })
    }
    /// Every read is fallible before sampling. A conflict carries no candidate
    /// pixels. Hidden RGB, native low bits and the current physical padding are
    /// preserved exactly; no rendered projection or alpha-only comparison occurs.
    pub(crate) fn tile(
        &self,
        coordinate: (u32, u32),
        check: &dyn Fn() -> Result<()>,
    ) -> Result<TileInverse> {
        check()?;
        let mut read_check = || check().map_err(|_| crate::large_document::Error::Cancelled);
        let before = self
            .before
            .prepare_tile(coordinate, &mut read_check)
            .map_err(|e| e.to_string())?;
        let after = self
            .after
            .prepare_tile(coordinate, &mut read_check)
            .map_err(|e| e.to_string())?;
        let current = self
            .current
            .prepare_tile(coordinate, &mut read_check)
            .map_err(|e| e.to_string())?;
        let channels = if self.before.depth() == 16 { 8 } else { 4 };
        let (left, top) = (coordinate.0 * 256, coordinate.1 * 256);
        let (right, bottom) = (
            (left + 256).min(self.before.width()),
            (top + 256).min(self.before.height()),
        );
        let mut changed = None;
        let mut conflicts = None;
        let zero = [0u8; 8];
        for y in top..bottom {
            if (y - top) % 16 == 0 {
                check()?;
            }
            for x in left..right {
                let at = ((y - top) as usize * 256 + (x - left) as usize) * channels;
                let b = pixel(before.bytes(), &zero, at, channels);
                let a = pixel(after.bytes(), &zero, at, channels);
                if b != a {
                    extend(&mut changed, x, y);
                    if pixel(current.bytes(), &zero, at, channels) != a {
                        extend(&mut conflicts, x, y);
                    }
                }
            }
        }
        check()?;
        let Some(changed) = changed else {
            return Ok(TileInverse::Unchanged);
        };
        if let Some(conflicts) = conflicts {
            return Ok(TileInverse::Conflict { changed, conflicts });
        }
        let mut pixels = current
            .copy_to_scratch(self.pool)
            .map_err(|e| e.to_string())?;
        for y in top..bottom {
            if (y - top) % 16 == 0 {
                check()?;
            }
            for x in left..right {
                let at = ((y - top) as usize * 256 + (x - left) as usize) * channels;
                let b = pixel(before.bytes(), &zero, at, channels);
                let a = pixel(after.bytes(), &zero, at, channels);
                if b != a {
                    pixels[at..at + channels].copy_from_slice(b);
                }
            }
        }
        check()?;
        Ok(TileInverse::Merged {
            pixels: pixels.freeze(),
            changed,
        })
    }
}
fn pixel<'a>(bytes: Option<&'a [u8]>, zero: &'a [u8; 8], at: usize, channels: usize) -> &'a [u8] {
    bytes.map_or(&zero[..channels], |bytes| &bytes[at..at + channels])
}
fn extend(bounds: &mut Option<[u32; 4]>, x: u32, y: u32) {
    *bounds = Some(match *bounds {
        Some([l, t, r, b]) => [l.min(x), t.min(y), r.max(x + 1), b.max(y + 1)],
        None => [x, y, x + 1, y + 1],
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{Document, Engine, History, Scope},
        large_document::{Limits, Store},
        raster::{backed::Plane, Raster},
    };
    struct Fixture {
        store: Option<Store>,
        pool: ResidentBudget,
        disk: DiskBudget,
        path: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(8 * 1024 * 1024, 8).unwrap();
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("peerbrush-task-pixels-{}", crate::engine::id()));
            let store = Store::create_with_disk_budget(
                &path,
                pool.clone(),
                disk.clone(),
                Limits {
                    disk_bytes: 8 * 1024 * 1024,
                    max_blocks: 32,
                    max_resident_blocks: 8,
                    manifest_bytes: 64 * 1024,
                },
            )
            .unwrap();
            Self {
                store: Some(store),
                pool,
                disk,
                path,
            }
        }
        fn stage(&self, raster: &Raster) -> Surface {
            if raster.tiles.is_empty() && raster.samples16.is_empty() {
                return Surface::zero(raster.width, raster.height, raster.depth).unwrap();
            }
            Surface::from_plane(
                Plane::from_resident(raster, self.store.as_ref().unwrap().clone(), &mut || Ok(()))
                    .unwrap(),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(self.store.take());
            assert_eq!(self.pool.used(), 0);
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }
    fn paint(raster: &mut Raster, x: i32, values: [u16; 4]) {
        if raster.depth == 16 {
            raster.set16(x, 0, values);
        } else {
            raster.set(x, 0, values.map(|value| (value / 257) as u8));
        }
    }
    fn source(depth: u16) -> (Document, Document, Document) {
        let before = Document::new_depth(8, 8, depth).unwrap();
        let mut after = before.clone();
        after.revision = 1;
        paint(&mut after.layers[0].pixels, 0, [12345, 23456, 34567, 65535]);
        let mut current = after.clone();
        current.revision = 2;
        paint(&mut current.layers[0].pixels, 1, [4444, 5555, 6666, 7777]);
        let pad = (200 * 256 + 200) * 4;
        if depth == 16 {
            Arc::make_mut(current.layers[0].pixels.samples16.get_mut(&(0, 0)).unwrap())[pad] =
                54321;
        } else {
            Arc::make_mut(current.layers[0].pixels.tiles.get_mut(&(0, 0)).unwrap())[pad] = 211;
        }
        (before, after, current)
    }
    #[test]
    fn native_three_way_matches_legacy_task_undo_and_preserves_disjoint_human_pixels_and_padding() {
        for depth in [8, 16] {
            let fixture = Fixture::new();
            let (before, after, current) = source(depth);
            let before_surface = fixture.stage(&before.layers[0].pixels);
            let after_surface = fixture.stage(&after.layers[0].pixels);
            let current_surface = fixture.stage(&current.layers[0].pixels);
            let mut legacy = Engine::new();
            legacy.resident_state_mut().expect("resident fixture").document = current.clone();
            legacy.resident_state_mut().expect("resident fixture").undo = vec![History {
                before: before.clone(),
                after: after.clone(),
                actor: "agent".into(),
                label: "Task".into(),
                task: Some("task".into()),
                scopes: vec![Scope::layer(&before.layers[0].id)],
                gesture: None,
                reverted: vec![],
            }];
            legacy.undo_task("human", "agent", "task").unwrap();
            let expected = fixture.stage(&legacy.resident_state().expect("resident fixture").document.layers[0].pixels);
            let inverse = PixelInverse::new(
                &before_surface,
                &after_surface,
                &current_surface,
                &fixture.pool,
                &fixture.disk,
            )
            .unwrap();
            assert_eq!(inverse.coordinates().collect::<Vec<_>>(), vec![(0, 0)]);
            let TileInverse::Merged { pixels, changed } = inverse.tile((0, 0), &|| Ok(())).unwrap()
            else {
                panic!("expected three-way merge")
            };
            assert_eq!(changed, [0, 0, 1, 1]);
            assert_eq!(
                &pixels[..],
                expected
                    .prepare_tile((0, 0), &mut || Ok(()))
                    .unwrap()
                    .bytes()
                    .unwrap()
            );
            assert_eq!(
                current.layers[0].pixels.get16(1, 0),
                legacy.resident_state().expect("resident fixture").document.layers[0].pixels.get16(1, 0)
            );
            assert_eq!(
                &pixels[..if depth == 16 { 8 } else { 4 }],
                vec![0u8; if depth == 16 { 8 } else { 4 }]
            );
        }
    }
    #[test]
    fn conflicts_cancellation_wrong_pool_and_changed_frames_have_no_candidate_pixels() {
        let fixture = Fixture::new();
        let (before, after, mut current) = source(16);
        current.layers[0]
            .pixels
            .set16(0, 0, [12346, 23456, 34567, 65535]);
        let before = fixture.stage(&before.layers[0].pixels);
        let after = fixture.stage(&after.layers[0].pixels);
        let current = fixture.stage(&current.layers[0].pixels);
        let inverse =
            PixelInverse::new(&before, &after, &current, &fixture.pool, &fixture.disk).unwrap();
        let TileInverse::Conflict { changed, conflicts } =
            inverse.tile((0, 0), &|| Ok(())).unwrap()
        else {
            panic!("expected low-bit conflict")
        };
        assert_eq!(changed, [0, 0, 1, 1]);
        assert_eq!(conflicts, [0, 0, 1, 1]);
        assert!(inverse.tile((0, 0), &|| Err("cancelled".into())).is_err());
        let foreign = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        assert!(PixelInverse::new(&before, &after, &current, &foreign, &fixture.disk).is_err());
        let frame = Surface::zero(4, 8, 16).unwrap();
        assert!(PixelInverse::new(&before, &after, &frame, &fixture.pool, &fixture.disk).is_err());
        assert!(inverse.tile((1, 0), &|| Ok(())).is_err());
    }
    #[test]
    fn late_cancellation_releases_cow_scratch_and_native_source_changes_fail_closed() {
        let fixture = Fixture::new();
        let (before, after, current) = source(16);
        let before = fixture.stage(&before.layers[0].pixels);
        let after = fixture.stage(&after.layers[0].pixels);
        let current = fixture.stage(&current.layers[0].pixels);
        let inverse =
            PixelInverse::new(&before, &after, &current, &fixture.pool, &fixture.disk).unwrap();
        let reached_copy = std::cell::Cell::new(false);
        let retained = fixture.disk.used().unwrap();
        let descriptors = fixture.pool.payload_used();
        assert!(inverse
            .tile((0, 0), &|| {
                if fixture.pool.payload_used() >= descriptors + 3 * 256 * 256 * 8 {
                    reached_copy.set(true);
                    return Err("cancelled after COW".into());
                }
                Ok(())
            })
            .is_err());
        assert!(reached_copy.get());
        assert_eq!(fixture.pool.payload_used(), descriptors);
        assert_eq!(fixture.disk.used().unwrap(), retained);
        let path = fixture.path.join("blocks.pbt");
        let original = std::fs::metadata(&path).unwrap().len();
        let changed = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_len(0));
        if changed.is_ok() {
            assert!(inverse.tile((0, 0), &|| Ok(())).is_err());
        } else {
            assert_eq!(std::fs::metadata(&path).unwrap().len(), original);
        }
        assert_eq!(fixture.pool.payload_used(), descriptors);
    }
}
