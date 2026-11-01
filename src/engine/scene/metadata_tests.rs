use super::*;
use crate::engine::{
    native::{
        CommandDescriptor, Direction, HistoryRequest, NativeDocument, NativeHistory, Publication,
    },
    scene::RasterCollector,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};

fn png_asset(encoded: usize) -> String {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        let mut table = [0u32; 256];
        for (index, value) in table.iter_mut().enumerate() {
            let mut crc = index as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xedb88320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
            *value = crc;
        }
        let mut crc = !0u32;
        for &byte in kind.iter().chain(data) {
            crc = table[((crc ^ u32::from(byte)) & 255) as usize] ^ (crc >> 8);
        }
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(
        &mut bytes,
        b"IHDR",
        &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0],
    );
    // Exact zlib stored block for filter0 and RGBA(10,20,30,255).
    chunk(
        &mut bytes,
        b"IDAT",
        &[
            0x78, 1, 1, 5, 0, 250, 255, 0, 10, 20, 30, 255, 1, 164, 1, 60,
        ],
    );
    let raw = encoded / 4 * 3;
    let padding = vec![0; raw - bytes.len() - 24];
    chunk(&mut bytes, b"npAD", &padding);
    chunk(&mut bytes, b"IEND", &[]);
    let text = STANDARD.encode(&bytes);
    assert_eq!(text.len(), encoded);
    text
}
struct Zero(Vec<Surface>);
impl RasterCollector for Zero {
    fn resident(&mut self, r: &Raster) -> Result<RasterRef36> {
        self.0
            .push(Surface::zero(r.width, r.height, r.depth).unwrap());
        Ok(RasterRef36 {
            id: self.0.len() as u32,
            width: r.width,
            height: r.height,
            depth: r.depth,
            retained: None,
        })
    }
    fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
        Err("No source substitution in metadata ownership fixture".into())
    }
}
fn styled(pool: &ResidentBudget) -> (NativeDocument, Vec<u8>) {
    let asset = png_asset(4 * 1024 * 1024 - 512);
    let mut document = Document::new_depth(4, 2, 16).unwrap();
    document.icc_profile = Some(Arc::new(crate::color_profile::srgb_profile().to_vec()));
    for index in 0..8 {
        let mut settings = crate::effects::defaults("pattern_overlay");
        settings["pattern"] = serde_json::json!("bitmap");
        settings["bitmap_png"] = serde_json::json!(asset);
        for (key, value) in crate::effects::styles::bitmap_defaults()
            .as_object()
            .unwrap()
        {
            if key != "bitmap_png" {
                settings[key] = value.clone();
            }
        }
        crate::effects::validate("pattern_overlay", &settings).unwrap();
        document.layers[0].effects.push(crate::effects::Effect {
            id: format!("pattern{index}"),
            kind: "pattern_overlay".into(),
            enabled: true,
            weight: 1.,
            settings: Arc::new(settings),
        });
    }
    crate::effects::patterns::validate_document(&document).unwrap();
    let mut collector = Zero(vec![]);
    let metadata = Document36::capture(&document, &mut collector).unwrap();
    let wire = serde_json::to_vec(&metadata).unwrap();
    // The parser envelope is deliberately broad. Runtime adoption must replace
    // it with fresh headers and the actual physical asset owners.
    let owner = MetadataCharge::reserve(pool, 256 * 1024 * 1024, &|| Ok(())).unwrap();
    let metadata = Admitted::from_parts(Arc::new(metadata), owner);
    let pending = PendingScene::new(
        metadata,
        RasterRoster::new(collector.0).unwrap(),
        pool.clone(),
        DiskBudget::new(1024 * 1024, 8).unwrap(),
    )
    .unwrap();
    (NativeDocument::from_pending(pending).unwrap(), wire)
}
#[test]
fn valid_styled_history_keeps_one_asset_set_with_fresh_headers_across_200_edits() {
    let pool = ResidentBudget::new(768 * 1024 * 1024).unwrap();
    let (mut current, wire) = styled(&pool);
    assert_eq!(serde_json::to_vec(&current.metadata()).unwrap(), wire);
    drop(wire);
    let initial = pool.used();
    assert!(
        initial < 40 * 1024 * 1024,
        "broad parse envelope must have been released"
    );
    let settings = current
        .metadata()
        .layer(0)
        .unwrap()
        .effects()
        .next()
        .unwrap()
        .settings() as *const serde_json::Value;
    let profile = current.metadata().profile().unwrap().as_ptr();
    let layer = current.metadata().layer(0).unwrap().id().to_owned();
    let mut history = NativeHistory::new(&pool, 200).unwrap();
    for index in 0..200 {
        let descriptor = CommandDescriptor::from_normalized(
            &current,
            current.metadata().revision(),
            &[serde_json::json!({"op":"layer.update","layer":layer,"visible":index%2==0})],
            &|| Ok(()),
        )
        .unwrap();
        let delta = current
            .capture_descriptor(descriptor, "human", &[], 1, false, true)
            .unwrap()
            .visibility(&layer, index % 2 == 0, &|| Ok(()))
            .unwrap();
        let commit = history
            .snapshot()
            .prepare_commit(
                &current,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Visibility",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        drop(
            history
                .commit(
                    &mut current,
                    Publication {
                        actor: "human",
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    commit,
                )
                .unwrap(),
        );
        assert_eq!(
            settings,
            current
                .metadata()
                .layer(0)
                .unwrap()
                .effects()
                .next()
                .unwrap()
                .settings() as *const serde_json::Value
        );
        assert_eq!(profile, current.metadata().profile().unwrap().as_ptr());
        assert!(
            pool.used() < initial + 16 * 1024 * 1024,
            "unchanged assets must not be copied or repeatedly charged"
        );
    }
    assert_eq!(current.metadata().revision(), 200);
    assert_eq!(history.undo_len(), 200);
    for _ in 0..20 {
        let undo = history
            .snapshot()
            .prepare_move(&current, Direction::Undo, "human", &|| Ok(()))
            .unwrap();
        drop(
            history
                .move_cursor(
                    &mut current,
                    Publication {
                        actor: "human",
                        leases: &[],
                        now: 1,
                        closed: false,
                        write_available: true,
                    },
                    undo,
                )
                .unwrap(),
        );
        assert_eq!(profile, current.metadata().profile().unwrap().as_ptr());
    }
    drop(history);
    drop(current);
    assert_eq!(pool.used(), 0);
}
#[test]
fn runtime_header_copy_preserves_wire_and_rejects_foreign_unbound_or_cancelled_ownership() {
    let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    let mut document = Document::new(4, 2).unwrap();
    document.selection = Some([0, 0, 2, 2]);
    document.selection_polygon = Some(vec![[0., 0.], [2., 0.], [0., 2.]]);
    let mut collector = Zero(vec![]);
    let wire = Document36::capture(&document, &mut collector).unwrap();
    assert!(wire.copy_runtime(&pool, &|| Ok(())).is_err());
    let expected = serde_json::to_vec(&wire).unwrap();
    let old = MetadataCharge::reserve(&pool, 1024 * 1024, &|| Ok(())).unwrap();
    let admitted = Admitted::from_parts(Arc::new(wire), old)
        .rehome_unique(|v, _| v.bind_runtime(&pool, &|| Ok(())))
        .unwrap();
    let retained = pool.used();
    assert!(retained < 64 * 1024);
    let copied = admitted.copy_runtime(&pool, &|| Ok(())).unwrap();
    assert_eq!(serde_json::to_vec(&*copied).unwrap(), expected);
    assert!(admitted.warnings.same_allocation(&copied.warnings));
    let other = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    assert!(admitted.copy_runtime(&other, &|| Ok(())).is_err());
    assert_eq!(other.used(), 0);
    assert!(admitted
        .copy_runtime(&pool, &|| Err("cancelled".into()))
        .is_err());
    drop(copied);
    assert_eq!(pool.used(), retained);
    let refusal = pool
        .reserve_bytes(pool.limit() - pool.used() - 1, pool.limit())
        .unwrap();
    assert!(admitted.copy_runtime(&pool, &|| Ok(())).is_err());
    drop(refusal);
    assert_eq!(pool.used(), retained);
    drop(admitted);
    assert_eq!(pool.used(), 0);
}
