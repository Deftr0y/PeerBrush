//! Source-bound opening of supported images as independent native-depth documents.
use crate::{
    engine::Document, file_ownership::Ownership, file_safety::Version, image_import,
    loading::Control,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub fn photoshop(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("psd") || s.eq_ignore_ascii_case("psb"));
    if extension {
        return true;
    }
    // Older working sources were accepted by signature. Keep their save/export
    // protection even when a filename is misleading; the PSD decoder validates
    // the complete source again under ownership and version guards.
    use std::io::Read;
    let mut signature = [0; 4];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut signature))
        .is_ok()
        && signature == *b"8BPS"
}

pub struct Prepared {
    pub path: PathBuf,
    pub encoded: image_import::Encoded,
    pub(crate) ownership: Arc<Ownership>,
    pub(crate) version: Version,
}
impl Prepared {
    pub fn read(path: &Path, project: &str, control: &Control) -> Result<Self, String> {
        control.progress("Reading image", 0, 1)?;
        let ownership = Ownership::open(path, project)?;
        let path = ownership.path.clone();
        let version = crate::file_safety::version_with_check(&path, || control.check())?;
        let encoded = image_import::Encoded::file(&path)?;
        let prepared = Self {
            path,
            encoded,
            ownership,
            version,
        };
        prepared.validate(control)?;
        Ok(prepared)
    }
    pub(crate) fn validate(&self, control: &Control) -> Result<(), String> {
        control.check()?;
        self.ownership.prepare(&self.path)?;
        if crate::file_safety::version_with_check(&self.path, || control.check())? != self.version {
            return Err("Image file changed while opening; current work preserved".into());
        }
        Ok(())
    }
    pub(crate) fn document(&self, options: &Value, control: &Control) -> Result<Document, String> {
        control.progress("Decoding image", 0, 1)?;
        let prepared = self
            .encoded
            .prepare_with_control(options, image_import::BUDGET, control)?;
        control.check()?;
        let pixels = prepared.pixels;
        let mut doc = Document::new_depth(pixels.width, pixels.height, pixels.depth)?;
        doc.name = self
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into();
        doc.layers[0].name = self
            .path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into();
        doc.layers[0].pixels = pixels;
        doc.layers[0].source = prepared.source.map(Into::into);
        doc.srgb_tagged = true;
        control.preview(&doc.layers[0].pixels, None)?;
        control.progress("Image decoded", 1, 1)?;
        Ok(doc)
    }
}
