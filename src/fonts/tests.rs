use super::*;
use ab_glyph::Font;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    MutexGuard,
};

struct Overrides {
    values: Vec<(InstalledFont, Result<FontArc, FontError>)>,
    calls: Arc<AtomicUsize>,
}
static SERIAL: Mutex<()> = Mutex::new(());
static OVERRIDES: Mutex<Option<Overrides>> = Mutex::new(None);
/// Exact-key test responses only; no environment override, paths or native registration.
pub(crate) struct TestFonts {
    _serial: MutexGuard<'static, ()>,
    calls: Arc<AtomicUsize>,
}
impl TestFonts {
    pub(crate) fn requests(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
impl Drop for TestFonts {
    fn drop(&mut self) {
        *OVERRIDES.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
pub(crate) fn test_fonts(values: Vec<(InstalledFont, Result<FontArc, FontError>)>) -> TestFonts {
    assert!(values.len() <= QUEUE);
    for (font, _) in &values {
        font.validate().unwrap();
    }
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let calls = Arc::new(AtomicUsize::new(0));
    *OVERRIDES.lock().unwrap_or_else(|e| e.into_inner()) = Some(Overrides {
        values,
        calls: calls.clone(),
    });
    TestFonts {
        _serial: serial,
        calls,
    }
}
pub(super) fn override_snapshot(font: &InstalledFont) -> Option<Result<FontArc, FontError>> {
    let state = OVERRIDES.lock().unwrap_or_else(|e| e.into_inner());
    let state = state.as_ref()?;
    let (_, result) = state.values.iter().find(|(key, _)| key == font)?;
    state.calls.fetch_add(1, Ordering::SeqCst);
    Some(result.clone())
}

fn wait_catalog(service: &Service) -> Catalog {
    wait_catalog_for(service, std::time::Duration::from_secs(10))
}
fn wait_catalog_for(service: &Service, timeout: std::time::Duration) -> Catalog {
    let end = std::time::Instant::now() + timeout;
    loop {
        let catalog = service.catalog();
        if catalog.status != CatalogStatus::Loading {
            return catalog;
        }
        assert!(
            std::time::Instant::now() < end,
            "Font discovery did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
fn wait_snapshot(service: &Service, font: &InstalledFont) -> Result<FontArc, FontError> {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match service.snapshot(font) {
            Err(FontError::Loading | FontError::Busy) => {}
            result => return result,
        }
        assert!(
            std::time::Instant::now() < end,
            "Synthetic snapshot did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
struct Fixture {
    directory: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("peerbrush-fonts-{}", crate::engine::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("synthetic 字体 static.ttf");
        let mut bytes = include_bytes!("../../assets/fonts/UbuntuSans-Regular.ttf").to_vec();
        // A private synthetic default-axis fixture if the bundled source is variable.
        // Production never edits, instantiates or registers an installed font.
        let count = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
        for i in 0..count {
            let start = 12 + i * 16;
            if &bytes[start..start + 4] == b"fvar" {
                bytes[start..start + 4].copy_from_slice(b"test");
            }
        }
        std::fs::write(&path, bytes).unwrap();
        Self { directory, path }
    }
    fn native(&self) -> NativeFace {
        NativeFace {
            family: "Synthetic installed font".into(),
            postscript: String::new(),
            path: Some(self.path.clone()),
            index: Some(0),
            error: None,
        }
    }
    fn service(&self) -> Service {
        let path = self.path.clone();
        Service::start(move || {
            Ok(vec![NativeFace {
                family: "Synthetic installed font".into(),
                postscript: String::new(),
                path: Some(path.clone()),
                index: Some(0),
                error: None,
            }])
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

#[test]
fn bundled_identity_json_and_pixels_remain_exact() {
    for name in ["regular", "medium", "semibold"] {
        let font = TextFont::from(name);
        assert_eq!(serde_json::to_value(&font).unwrap(), name);
        assert_eq!(
            serde_json::from_value::<TextFont>(serde_json::json!(name)).unwrap(),
            font
        );
        assert_eq!(font.bundled_name(), Some(name));
        assert!(
            snapshot(&font).unwrap().font_data() == bundled(name).unwrap().font_data(),
            "Bundled font bytes changed"
        );
    }
    assert_eq!(
        TextFont::from("unknown").validate(),
        Err(FontError::InvalidReference)
    );
}
#[test]
fn sha256_is_content_bound_and_matches_standard_vectors() {
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
#[test]
fn owned_snapshot_cache_and_changed_files_never_substitute() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let catalog = wait_catalog(&service);
    assert_eq!(catalog.status, CatalogStatus::Ready);
    let font = catalog.faces[0].font.clone().unwrap();
    let expected = std::fs::read(&fixture.path).unwrap();
    let first = wait_snapshot(&service, &font).unwrap();
    assert!(first.glyph_id('A').0 > 0);
    std::fs::write(&fixture.path, b"changed font").unwrap();
    assert!(
        wait_snapshot(&service, &font).unwrap().font_data() == first.font_data(),
        "Cache replaced an owned snapshot"
    );
    assert!(
        first.font_data() == expected,
        "Owned snapshot bytes changed"
    );
}
#[test]
fn reload_checks_original_hash_before_parsing_and_refresh_reports_missing() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let font = wait_catalog(&service).faces[0].font.clone().unwrap();
    std::fs::write(&fixture.path, b"changed font").unwrap();
    assert_eq!(
        wait_snapshot(&service, &font).unwrap_err(),
        FontError::Changed
    );
    service.refresh().unwrap();
    let catalog = wait_catalog(&service);
    assert_eq!(catalog.status, CatalogStatus::Ready);
    assert!(catalog.faces[0].font.is_none());
    assert_eq!(service.snapshot(&font).unwrap_err(), FontError::Missing);
}
#[test]
fn structural_reference_rejects_paths_unknown_fields_and_tampering() {
    let fixture = Fixture::new();
    let (faces, _) = discover(vec![fixture.native()]).unwrap();
    let font = faces[0].font.clone().unwrap();
    let mut value = serde_json::to_value(TextFont::Installed(font.clone())).unwrap();
    assert!(!value
        .to_string()
        .contains(&fixture.path.to_string_lossy().to_string()));
    value["path"] = serde_json::json!("/synthetic/private/font.ttf");
    assert!(serde_json::from_value::<TextFont>(value).is_err());
    for field in ["family", "postscript", "style", "sha256"] {
        let mut value = serde_json::to_value(&font).unwrap();
        value[field] = serde_json::json!("/synthetic/private/font.ttf");
        assert_eq!(
            serde_json::from_value::<InstalledFont>(value)
                .unwrap()
                .validate(),
            Err(FontError::InvalidReference)
        );
    }
    let mut invalid = font.clone();
    invalid.index = FILE_FACES;
    assert_eq!(invalid.validate(), Err(FontError::InvalidReference));
    let service = fixture.service();
    wait_catalog(&service);
    let mut changed = font;
    changed.sha256 = "0".repeat(64);
    assert_eq!(service.snapshot(&changed).unwrap_err(), FontError::Changed);
}
#[test]
fn discovery_failure_and_panic_are_explicit_never_complete() {
    for panic in [false, true] {
        let service = Service::start(move || {
            if panic {
                panic!("synthetic discovery failure");
            }
            Err(FontError::Unavailable)
        });
        assert_eq!(
            wait_catalog(&service).status,
            CatalogStatus::Unavailable(FontError::Unavailable)
        );
    }
    let service = Service::start(|| {
        Ok((0..=FACE_LIMIT)
            .map(|_| NativeFace {
                family: "Synthetic".into(),
                postscript: String::new(),
                path: None,
                index: None,
                error: None,
            })
            .collect())
    });
    assert_eq!(
        wait_catalog(&service).status,
        CatalogStatus::Unavailable(FontError::CatalogLimit)
    );
}
#[test]
fn malformed_collections_and_unsupported_tables_are_explicit() {
    let mut bytes = b"ttcf\x00\x01\x00\x00".to_vec();
    bytes.extend_from_slice(&(FILE_FACES + 1).to_be_bytes());
    assert_eq!(table_tags(&bytes, 0), Err(FontError::Unsupported));
    for (tag, reason) in [
        (*b"fvar", FontError::Variable),
        (*b"COLR", FontError::ColorOrBitmap),
        (*b"SVG ", FontError::ColorOrBitmap),
        (*b"sbix", FontError::ColorOrBitmap),
    ] {
        let mut bytes = b"\x00\x01\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00".to_vec();
        bytes.extend_from_slice(&tag);
        bytes.extend_from_slice(&[0; 12]);
        assert_eq!(supported(&bytes, 0), Err(reason));
    }
}
#[test]
fn owned_read_rejects_oversized_and_nonfile_inputs_before_allocating_data() {
    let fixture = Fixture::new();
    File::options()
        .write(true)
        .open(&fixture.path)
        .unwrap()
        .set_len(FILE_BYTES as u64 + 1)
        .unwrap();
    assert_eq!(read_owned(&fixture.path), Err(FontError::FileTooLarge));
    assert_eq!(read_owned(&fixture.directory), Err(FontError::Unavailable));
    assert_eq!(
        read_owned(Path::new("relative-font.ttf")),
        Err(FontError::Unavailable)
    );
}
#[test]
fn zero_font_height_and_excessive_name_metadata_are_unsupported() {
    let fixture = Fixture::new();
    let mut bytes = std::fs::read(&fixture.path).unwrap();
    let count = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
    for index in 0..count {
        let start = 12 + index * 16;
        let offset = word(&bytes, start + 8).unwrap() as usize;
        match &bytes[start..start + 4] {
            b"hhea" => bytes[offset + 4..offset + 8].fill(0),
            b"OS/2" => bytes[offset + 68..offset + 78].fill(0),
            _ => {}
        }
    }
    let font = FontRef::try_from_slice(&bytes).unwrap();
    assert_eq!(font.height_unscaled(), 0.);
    assert_eq!(metrics(&font), Err(FontError::Unsupported));
    std::fs::write(&fixture.path, bytes).unwrap();
    assert_eq!(
        discover(vec![fixture.native()]).unwrap().0[0].error,
        Some(FontError::Unsupported)
    );
    let mut oversized = b"\x00\x01\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00name".to_vec();
    oversized.extend_from_slice(&[0; 4]); // checksum
    oversized.extend_from_slice(&28u32.to_be_bytes());
    oversized.extend_from_slice(&6u32.to_be_bytes());
    oversized.extend_from_slice(&[0, 0, 1, 1, 0, 6]); // 257 name records
    assert_eq!(metadata_budget(&oversized), Err(FontError::Unsupported));
}
#[test]
fn discovery_is_async_and_unavailable_snapshots_do_not_do_caller_io() {
    let (release, wait) = mpsc::channel();
    let service = Service::start(move || {
        wait.recv().unwrap();
        Ok(vec![])
    });
    assert_eq!(service.catalog().status, CatalogStatus::Loading);
    let font = InstalledFont {
        family: "Synthetic".into(),
        postscript: "Synthetic-Regular".into(),
        style: "normal".into(),
        weight: 400,
        stretch: 5,
        index: 0,
        sha256: "0".repeat(64),
    };
    assert_eq!(service.snapshot(&font).unwrap_err(), FontError::Loading);
    assert_eq!(service.refresh(), Err(FontError::Loading));
    release.send(()).unwrap();
    assert_eq!(wait_catalog(&service).status, CatalogStatus::Ready);
    assert_eq!(service.snapshot(&font).unwrap_err(), FontError::Missing);
}
#[test]
fn bounded_queue_reports_busy_and_disconnection_without_pending_leaks() {
    let fixture = Fixture::new();
    let (faces, available) = discover(vec![fixture.native()]).unwrap();
    let font = faces[0].font.clone().unwrap();
    let (tx, rx) = mpsc::sync_channel(0);
    let service = Service {
        tx,
        state: Arc::new(Mutex::new(State {
            catalog: Catalog {
                generation: 1,
                status: CatalogStatus::Ready,
                faces,
            },
            available,
            cache: VecDeque::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
        })),
    };
    assert_eq!(service.snapshot(&font).unwrap_err(), FontError::Busy);
    assert!(service.state.lock().unwrap().pending.is_empty());
    drop(rx);
    assert_eq!(service.snapshot(&font).unwrap_err(), FontError::Unavailable);
    assert!(service.state.lock().unwrap().pending.is_empty());
}
#[test]
fn cache_evicts_least_recent_snapshot_with_bounded_accounting() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let original = wait_catalog(&service).faces[0].font.clone().unwrap();
    let mut first = original.clone();
    first.weight = 100;
    let mut second = original.clone();
    second.weight = 200;
    let mut third = original;
    third.weight = 300;
    let data = FontArc::new(bundled("regular").unwrap());
    {
        let mut state = service.state.lock().unwrap();
        // Synthetic accounting weights exercise eviction without large allocations.
        for font in [&first, &second] {
            state.remember(Loaded {
                font: font.clone(),
                data: data.clone(),
                bytes: FILE_BYTES,
            });
        }
    }
    assert!(service.snapshot(&first).is_ok());
    let mut state = service.state.lock().unwrap();
    state.remember(Loaded {
        font: third.clone(),
        data,
        bytes: FILE_BYTES,
    });
    assert_eq!(
        state.cache.iter().map(|f| f.bytes).sum::<usize>(),
        CACHE_BYTES
    );
    assert_eq!(
        state.cache.iter().map(|f| &f.font).collect::<Vec<_>>(),
        [&first, &third]
    );
    assert!(!state.cache.iter().any(|f| f.font == second));
}
#[test]
fn collection_face_index_is_retained_and_out_of_range_faces_reject() {
    let fixture = Fixture::new();
    let mut ttf = std::fs::read(&fixture.path).unwrap();
    while ttf.len() % 4 != 0 {
        ttf.push(0);
    }
    let mut collection = b"ttcf\x00\x01\x00\x00\x00\x00\x00\x02".to_vec();
    let offsets = [20u32, 20 + ttf.len() as u32];
    for offset in offsets {
        collection.extend_from_slice(&offset.to_be_bytes());
    }
    for offset in offsets {
        let mut face = ttf.clone();
        let tables = u16::from_be_bytes(face[4..6].try_into().unwrap()) as usize;
        for index in 0..tables {
            let start = 12 + index * 16 + 8;
            let relative = u32::from_be_bytes(face[start..start + 4].try_into().unwrap());
            face[start..start + 4].copy_from_slice(&(relative + offset).to_be_bytes());
        }
        collection.extend(face);
    }
    std::fs::write(&fixture.path, &collection).unwrap();
    let mut native = fixture.native();
    native.index = Some(1);
    let (faces, _) = discover(vec![native]).unwrap();
    let font = faces[0].font.clone().unwrap();
    assert_eq!(font.index, 1);
    assert_eq!(font.sha256, sha256(&collection));
    let data: FontArc = FontVec::try_from_vec_and_index(collection.clone(), font.index)
        .unwrap()
        .into();
    assert!(data.glyph_id('A').0 > 0);
    assert_eq!(table_tags(&collection, 2), Err(FontError::Unsupported));
}
#[test]
fn exact_test_responses_are_scoped_and_count_resolutions() {
    let fixture = Fixture::new();
    let (faces, _) = discover(vec![fixture.native()]).unwrap();
    let font = faces[0].font.clone().unwrap();
    let guard = test_fonts(vec![(
        font.clone(),
        Ok(FontArc::new(bundled("regular").unwrap())),
    )]);
    assert!(snapshot(&TextFont::Installed(font)).is_ok());
    assert_eq!(guard.requests(), 1);
}
#[test]
#[ignore = "Read-only native installed-font enumeration; select explicitly in native CI"]
fn native_enumeration_reports_exact_selectable_faces_without_paths() {
    let service = Service::start(native_faces);
    let catalog = wait_catalog_for(&service, std::time::Duration::from_secs(30));
    assert_eq!(catalog.status, CatalogStatus::Ready);
    assert!(!catalog.faces.is_empty());
    let font = catalog
        .faces
        .iter()
        .find_map(|face| face.font.clone())
        .expect("Native machine needs one supported static outline face");
    {
        let state = service.state.lock().unwrap();
        assert!(state.cache.is_empty());
        for face in &state.available {
            face.font.validate().unwrap();
            let encoded = serde_json::to_string(&face.font).unwrap();
            assert!(!encoded.contains(&face.path.to_string_lossy().to_string()));
        }
    }
    assert_eq!(service.snapshot(&font).unwrap_err(), FontError::Loading);
    let data = wait_snapshot(&service, &font)
        .expect("Native selected face could not load its pinned bytes");
    assert_eq!(sha256(data.font_data()), font.sha256);
    metrics(&data).unwrap();
    let selected = FontRef::try_from_slice_and_index(data.font_data(), font.index).unwrap();
    assert_eq!(data.units_per_em(), selected.units_per_em());
    assert_eq!(data.ascent_unscaled(), selected.ascent_unscaled());
    assert_eq!(data.descent_unscaled(), selected.descent_unscaled());
    assert_eq!(data.glyph_count(), selected.glyph_count());
    let cached = service.snapshot(&font).unwrap();
    assert!(
        std::ptr::eq(data.font_data().as_ptr(), cached.font_data().as_ptr()),
        "Cache did not retain the owned snapshot"
    );
}
