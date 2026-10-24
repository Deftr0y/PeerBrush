//! Installed document fonts. Native paths and owned font bytes never enter sources.
use ab_glyph::{Font, FontArc, FontRef, FontVec};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex, OnceLock},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

const FILE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const FACE_LIMIT: usize = 8192;
const FILE_FACES: u32 = 256;
const METADATA_BYTES: usize = 8 * 1024 * 1024;
const QUEUE: usize = 8;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TextFont {
    Bundled(String),
    Installed(InstalledFont),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledFont {
    pub family: String,
    pub postscript: String,
    pub style: String,
    pub weight: u16,
    pub stretch: u16,
    pub index: u32,
    pub sha256: String,
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.chars().any(|c| c.is_control() || "/\\:".contains(c))
}
impl InstalledFont {
    pub fn validate(&self) -> Result<(), FontError> {
        if !name(&self.family)
            || !name(&self.postscript)
            || !["normal", "italic", "oblique"].contains(&self.style.as_str())
            || !(1..=1000).contains(&self.weight)
            || !(1..=9).contains(&self.stretch)
            || self.index >= FILE_FACES
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(FontError::InvalidReference);
        }
        Ok(())
    }
}
impl TextFont {
    pub fn validate(&self) -> Result<(), FontError> {
        match self {
            Self::Bundled(value) if ["regular", "medium", "semibold"].contains(&value.as_str()) => {
                Ok(())
            }
            Self::Installed(value) => value.validate(),
            _ => Err(FontError::InvalidReference),
        }
    }
    pub fn bundled_name(&self) -> Option<&str> {
        match self {
            Self::Bundled(name) => Some(name),
            Self::Installed(_) => None,
        }
    }
    pub fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Bundled(name) => Cow::Owned(format!("Ubuntu Sans {name}")),
            Self::Installed(font) => Cow::Owned(format!(
                "{} · {} · {}",
                font.family, font.style, font.weight
            )),
        }
    }
}
impl From<&str> for TextFont {
    fn from(value: &str) -> Self {
        Self::Bundled(value.into())
    }
}
impl From<String> for TextFont {
    fn from(value: String) -> Self {
        Self::Bundled(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FontError {
    Loading,
    Busy,
    Missing,
    Changed,
    Unavailable,
    CatalogLimit,
    FileTooLarge,
    Unsupported,
    Variable,
    ColorOrBitmap,
    InvalidReference,
}
impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Loading => "The installed font is loading; retry when it is ready",
            Self::Busy => "Font loading is busy; retry shortly",
            Self::Missing => "The exact font is missing; retain the saved appearance or explicitly choose a replacement",
            Self::Changed => "The installed font differs from the saved font; explicitly choose a replacement",
            Self::Unavailable => "The native font catalog or local font data is unavailable",
            Self::CatalogLimit => "The native font catalog exceeds the supported bounds; it is not complete",
            Self::FileTooLarge => "This font exceeds the 32 MiB document-font limit",
            Self::Unsupported => "This font format or simulated face is unsupported; explicitly choose another font",
            Self::Variable => "Variable font instances are unsupported; explicitly choose a static font",
            Self::ColorOrBitmap => "Color, bitmap and SVG font glyphs are unsupported; explicitly choose an outline font",
            Self::InvalidReference => "Invalid document-font reference",
        })
    }
}
impl std::error::Error for FontError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum CatalogStatus {
    Loading,
    Ready,
    Unavailable(FontError),
}
#[derive(Clone, Debug, Serialize)]
pub struct Face {
    pub family: String,
    pub postscript: String,
    pub font: Option<InstalledFont>,
    pub error: Option<FontError>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Catalog {
    pub generation: u64,
    pub status: CatalogStatus,
    pub faces: Arc<[Face]>,
}

struct NativeFace {
    family: String,
    postscript: String,
    path: Option<PathBuf>,
    index: Option<u32>,
    error: Option<FontError>,
}
fn push_native(
    rows: &mut Vec<NativeFace>,
    bytes: &mut usize,
    face: NativeFace,
) -> Result<(), FontError> {
    *bytes = bytes
        .checked_add(
            face.family.len()
                + face.postscript.len()
                + face.path.as_ref().map_or(0, |p| p.as_os_str().len())
                + 128,
        )
        .ok_or(FontError::CatalogLimit)?;
    if rows.len() >= FACE_LIMIT || *bytes > METADATA_BYTES {
        return Err(FontError::CatalogLimit);
    }
    rows.push(face);
    Ok(())
}
struct Available {
    font: InstalledFont,
    path: PathBuf,
}
struct Loaded {
    font: InstalledFont,
    data: FontArc,
    bytes: usize,
}
enum Work {
    Discover(u64),
    Load(u64, InstalledFont),
}
struct State {
    catalog: Catalog,
    available: Vec<Available>,
    cache: VecDeque<Loaded>,
    pending: HashSet<InstalledFont>,
    failed: HashMap<InstalledFont, FontError>,
}
impl State {
    fn remember(&mut self, entry: Loaded) {
        while self.cache.iter().map(|f| f.bytes).sum::<usize>() + entry.bytes > CACHE_BYTES {
            self.cache.pop_front();
        }
        self.cache.push_back(entry);
    }
}
struct Service {
    state: Arc<Mutex<State>>,
    tx: mpsc::SyncSender<Work>,
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn read_owned(path: &Path) -> Result<Vec<u8>, FontError> {
    if !path.is_absolute() {
        return Err(FontError::Unavailable);
    }
    let file = File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            FontError::Missing
        } else {
            FontError::Unavailable
        }
    })?;
    let metadata = file.metadata().map_err(|_| FontError::Unavailable)?;
    if !metadata.is_file() {
        return Err(FontError::Unavailable);
    }
    if metadata.len() > FILE_BYTES as u64 {
        return Err(FontError::FileTooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FontError::Unavailable)?;
    if bytes.len() > FILE_BYTES {
        return Err(FontError::FileTooLarge);
    }
    Ok(bytes)
}
fn word(bytes: &[u8], offset: usize) -> Result<u32, FontError> {
    Ok(u32::from_be_bytes(
        bytes
            .get(offset..offset.checked_add(4).ok_or(FontError::Unsupported)?)
            .ok_or(FontError::Unsupported)?
            .try_into()
            .map_err(|_| FontError::Unsupported)?,
    ))
}
fn tables(bytes: &[u8], index: u32) -> Result<Vec<([u8; 4], &[u8])>, FontError> {
    let offset = if bytes.starts_with(b"ttcf") {
        let count = word(bytes, 8)?;
        if count == 0 || count > FILE_FACES || index >= count {
            return Err(FontError::Unsupported);
        }
        word(bytes, 12 + index as usize * 4)? as usize
    } else if index == 0 {
        0
    } else {
        return Err(FontError::Unsupported);
    };
    let header = bytes
        .get(offset..offset.checked_add(12).ok_or(FontError::Unsupported)?)
        .ok_or(FontError::Unsupported)?;
    if ![b"\x00\x01\x00\x00".as_slice(), b"OTTO", b"true"].contains(&&header[..4]) {
        return Err(FontError::Unsupported);
    }
    let count = u16::from_be_bytes(header[4..6].try_into().unwrap()) as usize;
    if count == 0 || count > 256 {
        return Err(FontError::Unsupported);
    }
    let mut tags = Vec::with_capacity(count);
    for i in 0..count {
        let start = offset
            .checked_add(12 + i * 16)
            .ok_or(FontError::Unsupported)?;
        let record = bytes
            .get(start..start.checked_add(16).ok_or(FontError::Unsupported)?)
            .ok_or(FontError::Unsupported)?;
        let begin = word(record, 8)? as usize;
        let size = word(record, 12)? as usize;
        let end = begin.checked_add(size).ok_or(FontError::Unsupported)?;
        let data = bytes.get(begin..end).ok_or(FontError::Unsupported)?;
        tags.push((record[..4].try_into().unwrap(), data));
    }
    Ok(tags)
}
fn table_tags(bytes: &[u8], index: u32) -> Result<Vec<[u8; 4]>, FontError> {
    Ok(tables(bytes, index)?
        .into_iter()
        .map(|(tag, _)| tag)
        .collect())
}
fn metadata_budget(bytes: &[u8]) -> Result<(), FontError> {
    let count = if bytes.starts_with(b"ttcf") {
        word(bytes, 8)?
    } else {
        1
    };
    if count == 0 || count > FILE_FACES {
        return Err(FontError::Unsupported);
    }
    let mut budget = 0usize;
    for index in 0..count {
        for (tag, data) in tables(bytes, index)? {
            if tag != *b"name" {
                continue;
            }
            let header = data.get(..6).ok_or(FontError::Unsupported)?;
            let names = u16::from_be_bytes(header[2..4].try_into().unwrap()) as usize;
            if names > 256 {
                return Err(FontError::Unsupported);
            }
            let offset = u16::from_be_bytes(header[4..6].try_into().unwrap()) as usize;
            for record in data
                .get(6..6 + names * 12)
                .ok_or(FontError::Unsupported)?
                .chunks_exact(12)
            {
                let length = u16::from_be_bytes(record[8..10].try_into().unwrap()) as usize;
                let begin =
                    offset + u16::from_be_bytes(record[10..12].try_into().unwrap()) as usize;
                if length > 1024 || begin + length > data.len() {
                    return Err(FontError::Unsupported);
                }
                // Bound decoded strings and alias vectors before the parser allocates them.
                budget += length * 3 + 64;
                if budget > METADATA_BYTES {
                    return Err(FontError::Unsupported);
                }
            }
        }
    }
    Ok(())
}
fn supported(bytes: &[u8], index: u32) -> Result<(), FontError> {
    let tags = table_tags(bytes, index)?;
    if tags.contains(b"fvar") {
        return Err(FontError::Variable);
    }
    if tags.iter().any(|tag| {
        [
            *b"COLR", *b"CPAL", *b"CBDT", *b"CBLC", *b"sbix", *b"SVG ", *b"EBDT",
        ]
        .contains(tag)
    }) {
        return Err(FontError::ColorOrBitmap);
    }
    if !tags.contains(b"glyf") && !tags.contains(b"CFF ") {
        return Err(FontError::Unsupported);
    }
    Ok(())
}
fn metrics(font: &impl Font) -> Result<(), FontError> {
    if !font.height_unscaled().is_finite()
        || font.height_unscaled() <= 0.
        || !font.ascent_unscaled().is_finite()
        || !font.descent_unscaled().is_finite()
        || font
            .units_per_em()
            .is_none_or(|units| !units.is_finite() || units <= 0.)
    {
        return Err(FontError::Unsupported);
    }
    Ok(())
}
fn reference(
    face: &fontdb::FaceInfo,
    bytes: &[u8],
    digest: &str,
) -> Result<InstalledFont, FontError> {
    supported(bytes, face.index)?;
    metrics(
        &FontRef::try_from_slice_and_index(bytes, face.index)
            .map_err(|_| FontError::Unsupported)?,
    )?;
    let font = InstalledFont {
        family: face
            .families
            .first()
            .ok_or(FontError::Unsupported)?
            .0
            .clone(),
        postscript: face.post_script_name.clone(),
        style: match face.style {
            fontdb::Style::Normal => "normal",
            fontdb::Style::Italic => "italic",
            fontdb::Style::Oblique => "oblique",
        }
        .into(),
        weight: face.weight.0,
        stretch: face.stretch.to_number(),
        index: face.index,
        sha256: digest.into(),
    };
    font.validate()?;
    Ok(font)
}
struct Inspected {
    index: u32,
    postscript: String,
    font: Result<InstalledFont, FontError>,
}
fn inspect_file(path: &Path) -> Result<Vec<Inspected>, FontError> {
    let bytes = read_owned(path)?;
    // Bound collection allocation before fontdb parses its advertised face count.
    metadata_budget(&bytes)?;
    let digest = sha256(&bytes);
    let mut db = fontdb::Database::new();
    db.load_font_data(bytes);
    Ok(db
        .faces()
        .map(|face| Inspected {
            index: face.index,
            postscript: face.post_script_name.clone(),
            font: db
                .with_face_data(face.id, |bytes, _| reference(face, bytes, &digest))
                .unwrap_or(Err(FontError::Unsupported)),
        })
        .collect())
}
fn discover(mut native: Vec<NativeFace>) -> Result<(Arc<[Face]>, Vec<Available>), FontError> {
    if native.len() > FACE_LIMIT {
        return Err(FontError::CatalogLimit);
    }
    let mut faces = Vec::new();
    let mut available = Vec::new();
    let mut seen = HashSet::new();
    // Keep only one parsed file's metadata, sharing collection work without
    // retaining an unbounded database or rereading one file for every face.
    native
        .sort_by(|a, b| (&a.path, a.index, &a.postscript).cmp(&(&b.path, b.index, &b.postscript)));
    let mut cached_path = None;
    let mut inspected = Err(FontError::Unavailable);
    let mut metadata = 0usize;
    for entry in native {
        if !name(&entry.family) || (!entry.postscript.is_empty() && !name(&entry.postscript)) {
            return Err(FontError::Unavailable);
        }
        let result = if let Some(error) = entry.error {
            Err(error)
        } else if let Some(path) = &entry.path {
            if cached_path.as_ref() != Some(path) {
                inspected = inspect_file(path);
                cached_path = Some(path.clone());
            }
            match &inspected {
                Ok(faces) => {
                    let mut found = faces.iter().filter(|face| {
                        entry.index.is_none_or(|i| face.index == i)
                            && (entry.postscript.is_empty() || face.postscript == entry.postscript)
                    });
                    let result = found
                        .next()
                        .map_or(Err(FontError::Unsupported), |face| face.font.clone());
                    if found.next().is_some() {
                        Err(FontError::Unsupported)
                    } else {
                        result
                    }
                }
                Err(error) => Err(*error),
            }
        } else {
            Err(FontError::Unavailable)
        };
        let mut row = Face {
            family: entry.family,
            postscript: entry.postscript,
            font: None,
            error: None,
        };
        match result {
            Ok(font) => {
                if !seen.insert(font.clone()) {
                    continue;
                }
                row.family = font.family.clone();
                row.postscript = font.postscript.clone();
                row.font = Some(font.clone());
                available.push(Available {
                    font,
                    path: entry.path.unwrap(),
                });
            }
            Err(error) => row.error = Some(error),
        }
        metadata += row.family.len()
            + row.postscript.len()
            + row.font.as_ref().map_or(0, |f| {
                f.family.len() + f.postscript.len() + f.style.len() + 64
            })
            + 128;
        if metadata > METADATA_BYTES {
            return Err(FontError::CatalogLimit);
        }
        faces.push(row);
    }
    faces.sort_by(|a, b| (&a.family, &a.postscript).cmp(&(&b.family, &b.postscript)));
    Ok((faces.into(), available))
}
impl Service {
    fn start(enumerate: impl Fn() -> Result<Vec<NativeFace>, FontError> + Send + 'static) -> Self {
        let state = Arc::new(Mutex::new(State {
            catalog: Catalog {
                generation: 1,
                status: CatalogStatus::Loading,
                faces: Arc::from([]),
            },
            available: vec![],
            cache: VecDeque::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
        }));
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let worker_state = state.clone();
        let spawned = std::thread::Builder::new()
            .name("PeerBrush document fonts".into())
            .spawn(move || {
                let mut next = Some(Work::Discover(1));
                loop {
                    let work = match next.take().or_else(|| rx.recv().ok()) {
                        Some(work) => work,
                        None => break,
                    };
                    match work {
                        Work::Discover(generation) => {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    enumerate().and_then(discover)
                                }))
                                .unwrap_or(Err(FontError::Unavailable));
                            let Ok(mut state) = worker_state.lock() else {
                                break;
                            };
                            if state.catalog.generation != generation {
                                continue;
                            }
                            match result {
                                Ok((faces, available)) => {
                                    state.catalog.faces = faces;
                                    state.available = available;
                                    state.catalog.status = CatalogStatus::Ready;
                                }
                                Err(error) => {
                                    state.catalog.status = CatalogStatus::Unavailable(error)
                                }
                            }
                        }
                        Work::Load(generation, font) => {
                            let path = worker_state.lock().ok().and_then(|s| {
                                (s.catalog.generation == generation)
                                    .then(|| {
                                        s.available
                                            .iter()
                                            .find(|f| f.font == font)
                                            .map(|f| f.path.clone())
                                    })
                                    .flatten()
                            });
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    path.ok_or(FontError::Missing).and_then(|path| {
                                        let bytes = read_owned(&path)?;
                                        if sha256(&bytes) != font.sha256 {
                                            return Err(FontError::Changed);
                                        }
                                        supported(&bytes, font.index)?;
                                        let size = bytes.len();
                                        let data: FontArc =
                                            FontVec::try_from_vec_and_index(bytes, font.index)
                                                .map_err(|_| FontError::Unsupported)?
                                                .into();
                                        metrics(&data)?;
                                        Ok((data, size))
                                    })
                                }))
                                .unwrap_or(Err(FontError::Unavailable));
                            let Ok(mut state) = worker_state.lock() else {
                                break;
                            };
                            if state.catalog.generation != generation {
                                continue;
                            }
                            state.pending.remove(&font);
                            match result {
                                Ok((data, bytes)) => {
                                    state.remember(Loaded { font, data, bytes });
                                }
                                Err(error) => {
                                    state.failed.insert(font, error);
                                }
                            }
                        }
                    }
                }
            });
        if spawned.is_err() {
            state.lock().unwrap().catalog.status =
                CatalogStatus::Unavailable(FontError::Unavailable);
        }
        Self { state, tx }
    }
    fn catalog(&self) -> Catalog {
        self.state
            .lock()
            .map(|s| s.catalog.clone())
            .unwrap_or(Catalog {
                generation: 0,
                status: CatalogStatus::Unavailable(FontError::Unavailable),
                faces: Arc::from([]),
            })
    }
    fn snapshot(&self, font: &InstalledFont) -> Result<FontArc, FontError> {
        font.validate()?;
        let mut state = self.state.lock().map_err(|_| FontError::Unavailable)?;
        match state.catalog.status {
            CatalogStatus::Loading => return Err(FontError::Loading),
            CatalogStatus::Unavailable(error) => return Err(error),
            CatalogStatus::Ready => {}
        }
        if let Some(index) = state.cache.iter().position(|f| f.font == *font) {
            let entry = state.cache.remove(index).unwrap();
            let result = entry.data.clone();
            state.cache.push_back(entry);
            return Ok(result);
        }
        if let Some(error) = state.failed.get(font) {
            return Err(*error);
        }
        if !state.available.iter().any(|f| f.font == *font) {
            if let Some(error) = state.catalog.faces.iter().find_map(|row| {
                (row.postscript == font.postscript)
                    .then_some(row.error)
                    .flatten()
            }) {
                return Err(error);
            }
            return Err(
                if state
                    .available
                    .iter()
                    .any(|f| f.font.postscript == font.postscript && f.font.index == font.index)
                {
                    FontError::Changed
                } else {
                    FontError::Missing
                },
            );
        }
        if state.pending.contains(font) {
            return Err(FontError::Loading);
        }
        let generation = state.catalog.generation;
        self.tx
            .try_send(Work::Load(generation, font.clone()))
            .map_err(queue_error)?;
        state.pending.insert(font.clone());
        Err(FontError::Loading)
    }
    fn refresh(&self) -> Result<(), FontError> {
        let mut state = self.state.lock().map_err(|_| FontError::Unavailable)?;
        if state.catalog.status == CatalogStatus::Loading {
            return Err(FontError::Loading);
        }
        let generation = state
            .catalog
            .generation
            .checked_add(1)
            .ok_or(FontError::Unavailable)?;
        self.tx
            .try_send(Work::Discover(generation))
            .map_err(queue_error)?;
        state.catalog = Catalog {
            generation,
            status: CatalogStatus::Loading,
            faces: Arc::from([]),
        };
        state.available.clear();
        state.cache.clear();
        state.pending.clear();
        state.failed.clear();
        Ok(())
    }
}
fn queue_error(error: mpsc::TrySendError<Work>) -> FontError {
    match error {
        mpsc::TrySendError::Full(_) => FontError::Busy,
        mpsc::TrySendError::Disconnected(_) => FontError::Unavailable,
    }
}
fn native_faces() -> Result<Vec<NativeFace>, FontError> {
    #[cfg(windows)]
    {
        windows::enumerate()
    }
    #[cfg(target_os = "macos")]
    {
        macos::enumerate()
    }
    #[cfg(target_os = "linux")]
    {
        linux::enumerate()
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        Err(FontError::Unavailable)
    }
}
fn service() -> &'static Service {
    static SERVICE: OnceLock<Service> = OnceLock::new();
    SERVICE.get_or_init(|| Service::start(native_faces))
}
/// Nonblocking metadata view. A ready catalog includes unsupported rows explicitly.
pub fn catalog() -> Catalog {
    service().catalog()
}
pub fn refresh() -> Result<(), FontError> {
    service().refresh()
}
/// The three legacy aliases retain exactly their original bytes and face indices.
pub fn bundled(name: &str) -> Result<FontRef<'static>, FontError> {
    let bytes: &'static [u8] = match name {
        "regular" => include_bytes!("../assets/fonts/UbuntuSans-Regular.ttf"),
        "medium" => include_bytes!("../assets/fonts/UbuntuSans-Medium.ttf"),
        "semibold" => include_bytes!("../assets/fonts/UbuntuSans-SemiBold.ttf"),
        _ => return Err(FontError::InvalidReference),
    };
    FontRef::try_from_slice(bytes).map_err(|_| FontError::Unsupported)
}
/// Cache-only on the caller. Missing snapshots queue bounded background work.
pub fn snapshot(font: &TextFont) -> Result<FontArc, FontError> {
    font.validate()?;
    #[cfg(test)]
    if let TextFont::Installed(installed) = font {
        if let Some(result) = tests::override_snapshot(installed) {
            return result;
        }
    }
    match font {
        TextFont::Installed(font) => service().snapshot(font),
        TextFont::Bundled(name) => {
            static FONTS: OnceLock<[Result<FontArc, FontError>; 3]> = OnceLock::new();
            let fonts = FONTS.get_or_init(|| {
                ["regular", "medium", "semibold"].map(|name| bundled(name).map(FontArc::new))
            });
            fonts[["regular", "medium", "semibold"]
                .iter()
                .position(|n| *n == name)
                .ok_or(FontError::InvalidReference)?]
            .clone()
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::test_fonts;
