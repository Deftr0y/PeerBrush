//! Explicit preparation outside engine locks, followed by the ordinary atomic edit.
//! This bridge does not evict resident authority or admit larger live documents.
use super::{Document, Engine};
use crate::large_document::{PinnedBytes, Reservation, ScratchBytes, Store};
use crate::raster::{
    backed::{Plane, Region},
    Pixel16, Raster, TILE,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// A non-I/O cancellation guard that survives preparation and queued publication.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    fn check(&self) -> crate::large_document::Result<()> {
        if self.0.load(Ordering::Acquire) {
            Err(crate::large_document::Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Capturing this handle only clones resident descriptors; staging performs I/O.
pub struct Source {
    document: String,
    revision: u64,
    layer: String,
    raster: Raster,
    rect: [i32; 4],
    cancellation: Cancellation,
}

impl Source {
    /// The initial bridge deliberately covers exactly one complete aligned tile.
    /// Shifted/partial tiles would need separately admitted reframe/COW ownership.
    pub fn capture(engine: &Engine, target: &str, rect: [i32; 4]) -> Result<Self, String> {
        let doc = &engine.doc;
        if doc.read_only {
            return Err("Backing preparation requires an editable document".into());
        }
        let layer = doc
            .layers
            .iter()
            .find(|l| l.id == target)
            .ok_or("Unknown paint layer")?;
        crate::placement::unlocked(doc, target)?;
        if layer.kind != "paint"
            || layer.source.is_some()
            || layer.x != 0
            || layer.y != 0
            || layer.pixels.width != doc.width
            || layer.pixels.height != doc.height
            || layer.pixels.depth != doc.bit_depth
        {
            return Err(
                "Backing bridge requires an unshifted canvas-sized native paint layer".into(),
            );
        }
        let [x, y, right, bottom] = rect;
        if x < 0
            || y < 0
            || x % TILE as i32 != 0
            || y % TILE as i32 != 0
            || right.checked_sub(x) != Some(TILE as i32)
            || bottom.checked_sub(y) != Some(TILE as i32)
            || right > doc.width as i32
            || bottom > doc.height as i32
        {
            return Err("Backing bridge requires one complete aligned tile".into());
        }
        Ok(Self {
            document: doc.id.clone(),
            revision: doc.revision,
            layer: target.into(),
            raster: layer.pixels.clone(),
            rect,
            cancellation: Cancellation::default(),
        })
    }

    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }

    /// All callbacks, backing reads/writes and charged allocations occur here,
    /// after releasing the engine/root/registry locks used for capture.
    pub fn prepare(
        self,
        store: Store,
        check: &mut dyn FnMut() -> crate::large_document::Result<()>,
        mut transform: impl FnMut(Pixel16) -> Pixel16,
    ) -> Result<PreparedPixels, String> {
        let cancellation = self.cancellation.clone();
        let mut checked = || {
            cancellation.check()?;
            check()?;
            cancellation.check()
        };
        let check: &mut dyn FnMut() -> crate::large_document::Result<()> = &mut checked;
        let prepare = || -> crate::large_document::Result<PreparedPixels> {
            let plane = Plane::from_resident(&self.raster, store.clone(), check)?;
            let view = plane.pin(Region::new(self.rect)?, check)?;
            let stride = if self.raster.depth == 16 { 8 } else { 4 };
            let payload = TILE as usize * TILE as usize * stride;
            let budget = store.resident_budget();
            // One changed tile plus the soft-selection COW copy and conservative
            // third retained/reconciliation copy, plus the selection byte vector.
            // This temporary reservation is released after commit/refusal. Resulting
            // resident/history tiles stay under legacy admission, not this pool.
            // Legacy setters still allocate infallibly; no total-RAM claim is made.
            let temporary = 3 * payload + TILE as usize * TILE as usize;
            let cow = budget.reserve_bytes(temporary, temporary)?;
            let mut bytes = ScratchBytes::new(&budget, payload, payload)?;
            for y in self.rect[1]..self.rect[3] {
                check()?;
                for x in self.rect[0]..self.rect[2] {
                    let pixel = transform(view.pixel16(x, y)?);
                    let at = ((y - self.rect[1]) as usize * TILE as usize
                        + (x - self.rect[0]) as usize)
                        * stride;
                    if self.raster.depth == 16 {
                        for i in 0..4 {
                            bytes[at + i * 2..at + i * 2 + 2]
                                .copy_from_slice(&pixel[i].to_be_bytes());
                        }
                    } else {
                        for i in 0..4 {
                            bytes[at + i] = crate::raster::project16(pixel[i]);
                        }
                    }
                }
            }
            check()?;
            Ok(PreparedPixels {
                command: json!({"op":"pixels.replace","layer":self.layer,
                "rect":self.rect,"bit_depth":self.raster.depth,"document_id":self.document,
                "source_revision":self.revision}),
                bytes: bytes.freeze(),
                _cow: cow,
                cancellation: self.cancellation.clone(),
            })
        };
        let mut prepare = prepare;
        prepare().map_err(|e| e.to_string())
    }
}

/// Single-use typed payload. No Clone/serde and no exposed mutable command or bytes.
pub struct PreparedPixels {
    command: Value,
    bytes: PinnedBytes,
    _cow: Reservation,
    cancellation: Cancellation,
}

impl PreparedPixels {
    pub(super) fn validate(&self, doc: &Document, command: &Value) -> Result<(), String> {
        self.check_cancelled()?;
        if command != &self.command
            || command.get("rgba").is_some()
            || command["op"] != "pixels.replace"
            || command["mask"] == true
            || command["document_id"].as_str() != Some(doc.id.as_str())
            || command["source_revision"].as_u64() != Some(doc.revision)
            || command["bit_depth"].as_u64() != Some(doc.bit_depth as u64)
        {
            return Err("Prepared pixels do not match the exact source and command".into());
        }
        let rect = super::rect(&command["rect"]).ok_or("Invalid prepared rectangle")?;
        Source::capture_document(
            doc,
            command["layer"].as_str().ok_or("Missing prepared layer")?,
            rect,
        )?;
        Ok(())
    }
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(super) fn check_cancelled(&self) -> Result<(), String> {
        self.cancellation.check().map_err(|e| e.to_string())
    }
}

impl Source {
    fn capture_document(doc: &Document, target: &str, rect: [i32; 4]) -> Result<(), String> {
        let layer = doc
            .layers
            .iter()
            .find(|l| l.id == target)
            .ok_or("Unknown prepared layer")?;
        if layer.x != 0
            || layer.y != 0
            || layer.pixels.width != doc.width
            || layer.pixels.height != doc.height
            || layer.pixels.depth != doc.bit_depth
            || rect[2] > doc.width as i32
            || rect[3] > doc.height as i32
        {
            return Err("Prepared paint surface changed".into());
        }
        Ok(())
    }
}

impl Engine {
    /// No backing I/O or base64/JSON pixel materialization occurs under this lock.
    pub fn commit_backed_pixels(
        &mut self,
        actor: &str,
        pixels: PreparedPixels,
        task: Option<&str>,
        label: &str,
    ) -> Result<Value, String> {
        let command = pixels.command.clone();
        let revision = command["source_revision"].as_u64();
        self.edit_transaction(
            actor,
            &[command],
            revision,
            task,
            label,
            None,
            None,
            None,
            None,
            None,
            Some(pixels),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_document::{Error, Limits, ResidentBudget};
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    fn store(bytes: usize) -> (Store, std::path::PathBuf) {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-engine-backing-{}", super::super::id()));
        assert_eq!(
            root.parent().unwrap().canonicalize().unwrap(),
            root.parent().unwrap()
        );
        let store = Store::create(
            &root,
            ResidentBudget::new(bytes).unwrap(),
            Limits {
                disk_bytes: 32 * 1024 * 1024,
                max_blocks: 64,
                max_resident_blocks: 32,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        (store, root)
    }
    fn engine(depth: u16) -> Engine {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(512, 256, depth).unwrap();
        engine.doc.layers[0]
            .pixels
            .set16(2, 3, [12345, 22222, 33333, 65535]);
        engine.doc.layers[0]
            .pixels
            .set16(300, 3, [40000, 50000, 60000, 65535]);
        engine
    }
    fn prepare(engine: &Engine, store: Store) -> PreparedPixels {
        Source::capture(engine, &engine.doc.layers[0].id, [0, 0, 256, 256])
            .unwrap()
            .prepare(store, &mut || Ok(()), |_| [10001, 20002, 30003, 65535])
            .unwrap()
    }
    fn cleanup(store: Store, path: std::path::PathBuf) {
        let budget = store.resident_budget();
        assert_store_metadata_only(&store);
        drop(store);
        assert_eq!(budget.used(), 0);
        std::fs::remove_dir_all(path).unwrap();
    }
    fn assert_store_metadata_only(store: &Store) {
        assert_eq!(
            store.resident_budget().used(),
            store.retained_index_bytes().unwrap()
        );
    }

    #[test]
    fn typed_backing_matches_wire_native_depth_selection_history_and_undo() {
        for depth in [8, 16] {
            for soft in [false, true] {
                let (store, path) = store(16 * 1024 * 1024);
                let mut typed = engine(depth);
                typed.doc.selection = Some([1, 1, 120, 120]);
                if soft {
                    let mut mask = Raster::new(512, 256);
                    for y in 1..120 {
                        for x in 1..120 {
                            mask.set(x, y, [127; 4]);
                        }
                    }
                    typed.doc.selection_coverage = Some(crate::selection::Coverage {
                        mask,
                        bounds: [1, 1, 120, 120],
                        origin: [0, 0],
                        contours: vec![],
                    });
                }
                let original = typed.doc.layers[0].pixels.clone();
                typed.doc.layers[0].pixels.retained = Some(Box::new(crate::retained::Original {
                    pixels: Box::new(original),
                    matrix: crate::retained::IDENTITY,
                    projective: None,
                    warps: vec![],
                }));
                let mut wire = engine(depth);
                wire.doc = typed.doc.clone();
                let prepared = prepare(&typed, store.clone());
                let mut command = prepared.command.clone();
                let mut bytes = prepared.bytes.to_vec();
                if depth == 16 {
                    for pair in bytes.chunks_exact_mut(2) {
                        pair.swap(0, 1);
                    }
                }
                command["rgba"] = json!(STANDARD.encode(bytes));
                wire.edit("human", &[command], Some(0), None, "Native tile")
                    .unwrap();
                typed
                    .commit_backed_pixels("human", prepared, None, "Native tile")
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&typed.doc).unwrap(),
                    serde_json::to_value(&wire.doc).unwrap()
                );
                assert_eq!(typed.undo.len(), 1);
                assert_eq!(
                    typed.doc.layers[0].pixels.get16(300, 3),
                    wire.doc.layers[0].pixels.get16(300, 3)
                );
                typed.undo("human").unwrap();
                wire.undo("human").unwrap();
                assert_eq!(typed.doc.layers[0].pixels, wire.doc.layers[0].pixels);
                typed.redo("human").unwrap();
                wire.redo("human").unwrap();
                assert_eq!(typed.doc.layers[0].pixels, wire.doc.layers[0].pixels);
                assert_store_metadata_only(&store);
                cleanup(store, path);
            }
        }
    }

    #[test]
    fn prepared_source_changes_and_locks_refuse_atomically() {
        for case in 0..6 {
            let (store, path) = store(16 * 1024 * 1024);
            let mut e = engine(16);
            let prepared = prepare(&e, store.clone());
            match case {
                0 => e.doc.revision += 1,
                1 => e.doc.id = super::super::id(),
                2 => e.doc.layers[0].locked = true,
                3 => e.doc.read_only = true,
                4 => {
                    e.reserve("other", "Conflict", e.scopes(&prepared.command))
                        .unwrap();
                }
                _ => {
                    e.doc.layers[0].kind = "folder".into();
                }
            }
            let before = serde_json::to_value(&e.doc).unwrap();
            let mut wire = engine(16);
            wire.doc = e.doc.clone();
            wire.leases = e.leases.clone();
            let mut command = prepared.command.clone();
            let mut bytes = prepared.bytes.to_vec();
            for pair in bytes.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
            command["rgba"] = json!(STANDARD.encode(bytes));
            assert!(wire
                .edit("human", &[command], Some(0), None, "Refused")
                .is_err());
            assert_eq!(serde_json::to_value(&wire.doc).unwrap(), before);
            assert!(e
                .commit_backed_pixels("human", prepared, None, "Refused")
                .is_err());
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            assert!(e.undo.is_empty());
            assert_store_metadata_only(&store);
            cleanup(store, path);
        }
    }

    #[test]
    fn typed_payload_cannot_change_command_or_accept_wire_payload() {
        let (store, path) = store(16 * 1024 * 1024);
        let e = engine(16);
        let p = prepare(&e, store.clone());
        for key in ["layer", "rect", "bit_depth", "rgba", "op"] {
            let mut changed = p.command.clone();
            changed[key] = json!("ambiguous");
            assert!(p.validate(&e.doc, &changed).is_err());
        }
        drop(p);
        cleanup(store, path);
    }

    #[test]
    fn malformed_prepared_length_and_multi_command_batches_rollback() {
        let (store, path) = store(16 * 1024 * 1024);
        let mut e = engine(16);
        let before = serde_json::to_value(&e.doc).unwrap();
        let mut p = prepare(&e, store.clone());
        p.bytes = ScratchBytes::new(&store.resident_budget(), 8, 8)
            .unwrap()
            .freeze();
        assert!(e
            .commit_backed_pixels("human", p, None, "Malformed")
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        let p = prepare(&e, store.clone());
        let commands = [p.command.clone(), p.command.clone()];
        assert!(e
            .edit_transaction(
                "human",
                &commands,
                Some(0),
                None,
                "Ambiguous",
                None,
                None,
                None,
                None,
                None,
                Some(p)
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert!(e.undo.is_empty());
        assert_store_metadata_only(&store);
        cleanup(store, path);
    }

    #[test]
    fn live_admission_and_unsupported_tile_boundaries_stay_closed() {
        assert!(Document::new_depth(16000, 16000, 8).is_err());
        assert!(Document::new_depth(16000, 16000, 16).is_err());
        let mut e = engine(16);
        let target = e.doc.layers[0].id.clone();
        for rect in [
            [0, 0, 512, 256],
            [1, 0, 257, 256],
            [0, 0, 255, 256],
            [-256, 0, 0, 256],
        ] {
            assert!(Source::capture(&e, &target, rect).is_err());
        }
        e.doc.layers[0].x = 1;
        assert!(Source::capture(&e, &target, [0, 0, 256, 256]).is_err());
    }

    #[test]
    fn cancellation_survives_preparation_and_refuses_queued_publication() {
        let (store, path) = store(16 * 1024 * 1024);
        let mut e = engine(16);
        let source = Source::capture(&e, &e.doc.layers[0].id, [0, 0, 256, 256]).unwrap();
        let cancellation = source.cancellation();
        let prepared = source
            .prepare(store.clone(), &mut || Ok(()), |_| [10001; 4])
            .unwrap();
        let before = serde_json::to_value(&e.doc).unwrap();
        cancellation.cancel();
        assert!(e
            .commit_backed_pixels("human", prepared, None, "Cancelled")
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert!(e.undo.is_empty());
        assert_store_metadata_only(&store);
        cleanup(store, path);
    }

    #[test]
    fn cancellation_and_cow_budget_refuse_before_mutating_engine() {
        for case in 0..3 {
            let (store, path) = store(if case != 0 {
                16 * 1024 * 1024
            } else {
                1024 * 1024
            });
            let e = engine(16);
            let before = serde_json::to_value(&e.doc).unwrap();
            let source = Source::capture(&e, &e.doc.layers[0].id, [0, 0, 256, 256]).unwrap();
            let mut calls = 0;
            assert!(source
                .prepare(
                    store.clone(),
                    &mut || {
                        calls += 1;
                        if case == 1 || (case == 2 && calls == 200) {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    |p| p
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            assert_store_metadata_only(&store);
            cleanup(store, path);
        }
    }
}
