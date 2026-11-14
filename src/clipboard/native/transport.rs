use super::*;
use crate::{
    clipboard::{Image, ImageIdentity, Provider},
    large_document::ResidentBudget,
    psd::memory36::MetadataCharge,
};
use std::ops::Deref;

pub(in crate::clipboard) fn layout(width: u32, height: u32, depth: u16) -> Result<(usize, usize), String> {
    crate::raster::CanvasLimits::Native.check(width, height)?;
    if ![8, 16].contains(&depth) { return Err("Clipboard color depth must be 8 or 16 bits".into()); }
    let channels = (width as usize).checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4)).ok_or("Clipboard pixel size overflow")?;
    let retained = channels.checked_mul(if depth == 16 { 3 } else { 1 })
        .filter(|n| *n <= 256 * 1024 * 1024)
        .ok_or("Clipboard pixels exceed the 256 MiB transport budget")?;
    // A successful cut must remain decodable by our bitmap paste path. Its
    // working-memory estimate is larger than the retained clipboard image.
    crate::image_import::check_budget_with_limits(
        width, height, depth, crate::image_import::BUDGET, crate::raster::CanvasLimits::Native,
    )?;
    Ok((channels, retained))
}

/// The bitmap and its original words keep their admission until the clipboard
/// replaces them. Cloning this owner must never authorize a deep image clone.
pub(crate) struct CapturedImage {
    image: Image,
    admission: Option<(ResidentBudget, Arc<MetadataCharge>)>,
}
impl From<Image> for CapturedImage {
    fn from(image: Image) -> Self {
        Self {
            image,
            admission: None,
        }
    }
}
impl Deref for CapturedImage {
    type Target = Image;
    fn deref(&self) -> &Image {
        &self.image
    }
}
impl CapturedImage {
    pub(crate) fn stage_native(
        &self,
        document: &NativeDocument,
        session: &SourceSession,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<StagedOutput, String> {
        check()?;
        self.validate()?;
        let depth = if self.samples16.is_some() { 16 } else { 8 };
        let _input = if self.admission.as_ref().is_some_and(|(budget, _)| {
            budget.same_pool(document.budget())
        }) {
            None
        } else {
            let input_bytes = self.bytes.capacity()
                + self.samples16.as_ref().map_or(0, |words| words.capacity() * 2);
            Some(MetadataCharge::reserve(document.budget(), input_bytes + 4096, check)?)
        };
        let channels = self.width as usize * 4;
        let mut row = ScratchBytes::new(document.budget(), channels * 2, channels * 2)
            .map_err(|e| e.to_string())?;
        transaction::stage(document, session, check, |store, guard| {
            let mut writer = NativeRows::new(self.width, self.height, depth, store.clone())
                .map_err(|e| e.to_string())?;
            for y in 0..self.height {
                check()?;
                let start = y as usize * channels;
                if let Some(words) = &self.samples16 {
                    for (out, &word) in row.chunks_exact_mut(2).zip(&words[start..start + channels]) {
                        out.copy_from_slice(&word.to_be_bytes());
                    }
                } else {
                    for (out, &byte) in row.chunks_exact_mut(2).zip(&self.bytes[start..start + channels]) {
                        out.copy_from_slice(&(u16::from(byte) * 257).to_be_bytes());
                    }
                }
                writer.row(y, &row, &mut || guard()).map_err(|e| e.to_string())?;
            }
            writer.finish(&mut || guard()).map_err(|e| e.to_string())
        })
    }

    pub(in crate::clipboard) fn write(
        &self,
        clipboard: &mut dyn Provider,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<ImageIdentity>, String> {
        check()?;
        self.image.validate()?;
        clipboard.write_captured_image(self, check)
    }
    #[cfg(windows)]
    pub(in crate::clipboard) fn budget(&self) -> Option<&ResidentBudget> {
        self.admission.as_ref().map(|(budget, _)| budget)
    }
    pub(in crate::clipboard) fn admit_transport(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<Arc<MetadataCharge>>, String> {
        check()?;
        // Allow for growing PNG output, DIB/TIFF staging and platform copies.
        // This covers transport buffers, not memory owned by the OS clipboard.
        self.admission
            .as_ref()
            .map(|(budget, _)| {
                let raw = self
                    .samples16
                    .as_ref()
                    .map_or(self.bytes.len(), |v| v.len() * 2);
                let bytes = raw
                    .checked_mul(8)
                    .and_then(|n| {
                        self.bytes
                            .len()
                            .checked_mul(4)
                            .and_then(|b| n.checked_add(b))
                    })
                    .and_then(|n| n.checked_add(16 * 1024 * 1024))
                    .ok_or("Clipboard transport admission overflow")?;
                MetadataCharge::reserve(budget, bytes, check)
            })
            .transpose()
    }

    pub(crate) fn prepare_resident(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(crate::raster::Raster, Option<Arc<MetadataCharge>>), String> {
        use crate::raster::{CanvasLimits, TILE};
        check()?;
        CanvasLimits::Resident.check(self.width, self.height)?;
        let charge = self.admission.as_ref()
            .map(|(budget, _)| {
                let tiles = self.width.div_ceil(TILE) as usize * self.height.div_ceil(TILE) as usize;
                let stride = if self.samples16.is_some() { 8 } else { 4 };
                let bytes = tiles * ((TILE * TILE) as usize * stride + 256) + 4096;
                MetadataCharge::reserve(budget, bytes, check)
            })
            .transpose()?;
        let raster = self.image.resident_raster()?;
        check()?;
        Ok((raster, charge))
    }
}

impl StagedCopy {
    pub(in crate::clipboard) fn retire_into_transport(
        self,
        session: &impl Session,
        budget: &ResidentBudget,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<CapturedImage, String> {
        let result = self.transport(budget, check);
        let cleanup = self.abort(session);
        match (result, cleanup) {
            (Ok(image), Ok(())) => Ok(image),
            (Err(reason), Ok(())) => Err(reason),
            (Ok(_), Err(cleanup)) => Err(format!("Clipboard source retirement: {cleanup}")),
            (Err(reason), Err(cleanup)) => {
                Err(format!("{reason}; clipboard source retirement: {cleanup}"))
            }
        }
    }
    pub(in crate::clipboard) fn transport(
        &self,
        budget: &ResidentBudget,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<CapturedImage, String> {
        check()?;
        let source = self.output.surface();
        if !self.budget.same_pool(budget) || !source.uses_budget(budget) {
            return Err("Clipboard transport uses another source admission".into());
        }
        let (width, height, depth) = (source.width(), source.height(), source.depth());
        let (channels, retained) = layout(width, height, depth)?;
        let charge = MetadataCharge::reserve(budget, retained + 4096, check)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(channels)
            .map_err(|_| "Clipboard display allocation refused")?;
        bytes.resize(channels, 0);
        let mut samples16 = if depth == 16 {
            let mut words = Vec::new();
            words
                .try_reserve_exact(channels)
                .map_err(|_| "Clipboard native allocation refused")?;
            words.resize(channels, 0u16);
            Some(words)
        } else {
            None
        };
        if bytes.capacity() + samples16.as_ref().map_or(0, |words| words.capacity() * 2)
            > charge.bytes()
        {
            return Err("Clipboard allocations exceed their admission".into());
        }
        // One tile pin at a time; large clipboard rows do not pin their full width.
        for ty in 0..height.div_ceil(TILE) {
            for tx in 0..width.div_ceil(TILE) {
                check()?;
                let mut failure = None;
                let tile = source
                    .prepare_tile((tx, ty), &mut || {
                        check().map_err(|reason| {
                            failure = Some(reason);
                            Error::Cancelled
                        })
                    })
                    .map_err(|error| failure.unwrap_or_else(|| error.to_string()))?;
                let Some(input) = tile.bytes() else { continue };
                for y in 0..TILE.min(height - ty * TILE) {
                    check()?;
                    for x in 0..TILE.min(width - tx * TILE) {
                        let at = ((y * TILE + x) * 4) as usize;
                        let out = (((ty * TILE + y) * width + tx * TILE + x) * 4) as usize;
                        if let Some(words) = &mut samples16 {
                            for c in 0..4 {
                                let value = u16::from_be_bytes([
                                    input[(at + c) * 2],
                                    input[(at + c) * 2 + 1],
                                ]);
                                words[out + c] = value;
                                bytes[out + c] = crate::raster::project16(value);
                            }
                        } else {
                            bytes[out..out + 4].copy_from_slice(&input[at..at + 4]);
                        }
                    }
                }
            }
        }
        check()?;
        Ok(CapturedImage {
            image: Image {
                width,
                height,
                bytes,
                samples16,
                origin: Some(self.origin()),
            },
            admission: Some((budget.clone(), charge)),
        })
    }
}
