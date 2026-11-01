//! Worker font snapshots with payload-owned shared admission. No FontArc is
//! inserted into, or borrowed from, the presentation snapshot cache.
use super::{FontError, TextFont, FILE_BYTES};
use crate::large_document::{Reservation, ResidentBudget};
use ab_glyph::{Font, FontArc, FontVec, GlyphId};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::Path,
};
#[path = "prepared/outline.rs"]
mod outline;
#[path = "prepared/shaping.rs"]
mod shaping;

pub(crate) struct FontOwner {
    font: FontArc,
    reference: TextFont,
    _data: Option<Reservation>,
    _metadata: Reservation,
}
impl FontOwner {
    /// Loading stays outside Engine/registry locks. Installed font paths come
    /// only from the actual OS catalog; saved definitions contain no path.
    pub(crate) fn load(
        reference: &TextFont,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        reference.validate().map_err(|e| e.to_string())?;
        let metadata = pool
            .reserve_bytes(1024 * 1024, 1024 * 1024)
            .map_err(|e| e.to_string())?;
        match reference {
            TextFont::Bundled(name) => {
                let bytes = super::bundled_bytes(name).map_err(|e| e.to_string())?;
                outline::preparse(bytes, 0, check)?;
                let font = FontArc::new(
                    ab_glyph::FontRef::try_from_slice(bytes)
                        .map_err(|_| FontError::Unsupported.to_string())?,
                );
                super::metrics(&font).map_err(|e| e.to_string())?;
                check()?;
                Ok(Self {
                    font,
                    reference: reference.clone(),
                    _data: None,
                    _metadata: metadata,
                })
            }
            TextFont::Installed(installed) => {
                let path = {
                    let state = super::service()
                        .state
                        .try_lock()
                        .map_err(|_| FontError::Busy.to_string())?;
                    match state.catalog.status {
                        super::CatalogStatus::Ready => {}
                        super::CatalogStatus::Loading => return Err(FontError::Loading.to_string()),
                        super::CatalogStatus::Unavailable(error) => return Err(error.to_string()),
                    }
                    state
                        .available
                        .iter()
                        .find(|item| item.font == *installed)
                        .map(|item| item.path.clone())
                        .ok_or_else(|| FontError::Missing.to_string())?
                };
                if !path.is_absolute() || path.as_os_str().len() > 32768 {
                    return Err(FontError::Unavailable.to_string());
                }
                let path =
                    fs::canonicalize(path).map_err(|_| FontError::Unavailable.to_string())?;
                if path.as_os_str().len() > 32768 {
                    return Err(FontError::Unavailable.to_string());
                }
                let mut file = open(&path)?;
                let before = file
                    .metadata()
                    .map_err(|_| FontError::Unavailable.to_string())?;
                let length = usize::try_from(before.len())
                    .map_err(|_| FontError::FileTooLarge.to_string())?;
                if length == 0 || length > FILE_BYTES {
                    return Err(FontError::FileTooLarge.to_string());
                }
                let identity = crate::file_safety::identity(&file)?;
                let modified = before
                    .modified()
                    .map_err(|_| FontError::Unavailable.to_string())?;
                let data = pool
                    .reserve_bytes(length * 2, length * 2)
                    .map_err(|e| e.to_string())?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(length)
                    .map_err(|_| "Font byte allocation failed")?;
                if bytes.capacity() > length * 2 {
                    return Err("Font byte capacity exceeded admission".into());
                }
                bytes.resize(length, 0);
                for part in bytes.chunks_mut(64 * 1024) {
                    check()?;
                    file.read_exact(part)
                        .map_err(|_| FontError::Changed.to_string())?;
                }
                let mut extra = [0];
                if file
                    .read(&mut extra)
                    .map_err(|_| FontError::Changed.to_string())?
                    != 0
                {
                    return Err(FontError::Changed.to_string());
                }
                let after = file
                    .metadata()
                    .map_err(|_| FontError::Changed.to_string())?;
                let named = open(&path)?;
                if after.len() != before.len()
                    || after.modified().ok() != Some(modified)
                    || crate::file_safety::identity(&file)? != identity
                    || crate::file_safety::identity(&named)? != identity
                    || super::sha256(&bytes) != installed.sha256
                {
                    return Err(FontError::Changed.to_string());
                }
                check()?;
                super::supported(&bytes, installed.index).map_err(|e| e.to_string())?;
                outline::preparse(&bytes, installed.index, check)?;
                let font: FontArc = FontVec::try_from_vec_and_index(bytes, installed.index)
                    .map_err(|_| FontError::Unsupported.to_string())?
                    .into();
                super::metrics(&font).map_err(|e| e.to_string())?;
                check()?;
                Ok(Self {
                    font,
                    reference: reference.clone(),
                    _data: Some(data),
                    _metadata: metadata,
                })
            }
        }
    }
    pub(crate) fn font(&self) -> &impl Font {
        &self.font
    }
    pub(crate) fn matches(&self, reference: &TextFont) -> bool {
        self.reference == *reference
    }
    pub(crate) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self._metadata.uses_budget(pool) && self._data.as_ref().is_none_or(|r| r.uses_budget(pool))
    }
    pub(crate) fn preflight_shaping(
        &self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let index = match &self.reference {
            TextFont::Bundled(_) => 0,
            TextFont::Installed(f) => f.index,
        };
        let face = ttf_parser::Face::parse(self.font.font_data(), index)
            .map_err(|_| "Invalid immutable font face")?;
        shaping::preflight(&face, check)
    }
    /// The parser is the same pinned one used by ab_glyph. Count its actual
    /// emitted outline events before ab_glyph can grow an infallible curve Vec.
    pub(crate) fn preflight_glyph(
        &self,
        id: GlyphId,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let index = match &self.reference {
            TextFont::Bundled(_) => 0,
            TextFont::Installed(f) => f.index,
        };
        outline::preflight(self.font.font_data(), index, id.0, check)
    }
}

fn open(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|_| FontError::Unavailable.to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| FontError::Unavailable.to_string())?;
    if !metadata.is_file() {
        return Err(FontError::Unavailable.to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(FontError::Unavailable.to_string());
        }
    }
    Ok(file)
}
