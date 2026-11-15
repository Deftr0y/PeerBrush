//! Provisional standard-channel decoding into native tiles. The shared
//! Photoshop parser still owns feature and layer metadata admission.
use super::{row_stream::Source, structural::Layer};
use crate::{
    large_document::{Error, ScratchBytes, Store},
    raster::{Raster, Surface},
    selection::NativeRows,
};
use std::{
    collections::BTreeMap,
    io::{Read, Seek},
    sync::Arc,
};

pub(super) struct DirectLayers<'a> {
    pub(super) store: Store,
    pub(super) check: &'a dyn Fn() -> Result<(), String>,
    pub(super) control: &'a crate::loading::Control,
    pub(super) color: BTreeMap<String, Surface>,
    pub(super) masks: BTreeMap<String, Surface>,
    pub(super) mask_originals: BTreeMap<String, Surface>,
    pub(super) metadata_charge: Option<Arc<super::memory36::MetadataCharge>>,
    pub(super) interpreted_charges: Vec<crate::large_document::Reservation>,
}

impl DirectLayers<'_> {
    pub(super) fn color<R: Read + Seek>(
        &mut self,
        source: &Source<R>,
        record: &Layer,
        id: &str,
        raster: &Raster,
        require_binary_alpha: bool,
    ) -> Result<bool, String> {
        let check = self.check;
        let cancel = || self.control.check();
        let pool = self.store.resident_budget();
        let width = raster.width as usize;
        let sample_bytes = usize::from(raster.depth / 8);
        let mut streams = std::array::from_fn::<_, 4, _>(|_| None);
        for channel in &record.channels {
            let slot = match channel.id {
                -1 => 3,
                c @ 0..=2 => c as usize,
                _ => continue,
            };
            let encoding = channel
                .encoding
                .as_ref()
                .ok_or("Missing Photoshop layer encoding")?;
            if streams[slot].is_some()
                || encoding.row_bytes != (width * sample_bytes) as u64
                || encoding.rows != u64::from(raster.height)
            {
                return Err("Backed Photoshop channel geometry differs".into());
            }
            streams[slot] = Some((encoding, source.rows_with_checks(encoding, check, &cancel)?));
        }
        if streams.iter().all(Option::is_none) {
            return Ok(false);
        }
        let mut channel = ScratchBytes::new(&pool, width * sample_bytes, width * sample_bytes)
            .map_err(|e| e.to_string())?;
        let pixel_bytes = 4 * sample_bytes;
        let mut rgba = ScratchBytes::new(&pool, width * pixel_bytes, width * pixel_bytes)
            .map_err(|e| e.to_string())?;
        if streams[3].is_none() {
            for pixel in rgba.chunks_exact_mut(pixel_bytes) {
                pixel[3 * sample_bytes..].fill(255);
            }
        }
        let mut output = NativeRows::new(
            raster.width,
            raster.height,
            raster.depth,
            self.store.clone(),
        )
        .map_err(|e| e.to_string())?;
        // Reserve required row and stripe storage first. Leave headroom for
        // tile publication before adding optional per-channel read windows.
        if pool.limit().saturating_sub(pool.used()) >= 2 * 1024 * 1024 {
            for (_, rows) in streams.iter_mut().flatten() {
                rows.prefetch(&pool);
            }
        }
        let mut guarded = || check().map_err(|_| Error::Cancelled);
        for y in 0..raster.height {
            check()?;
            for (slot, stream) in streams.iter_mut().enumerate() {
                let component = slot * sample_bytes..(slot + 1) * sample_bytes;
                if let Some((_, rows)) = stream {
                    if !rows.read_row(&mut channel)? {
                        return Err("Missing backed Photoshop color row".into());
                    }
                    if require_binary_alpha && slot == 3
                        && channel.chunks_exact(sample_bytes).any(|word| !matches!(word, [0] | [255] | [0, 0] | [255, 255]))
                    {
                        return Err(super::STROKE_ALPHA_PROTECTION.into());
                    }
                    for (pixel, sample) in rgba
                        .chunks_exact_mut(pixel_bytes)
                        .zip(channel.chunks_exact(sample_bytes))
                    {
                        pixel[component.clone()].copy_from_slice(sample);
                    }
                }
            }
            stage_row(&mut output, y, &rgba, sample_bytes, &mut guarded)?;
        }
        for (encoding, mut rows) in streams.into_iter().flatten() {
            rows.finish()?.check(source, encoding, check)?;
        }
        let surface = output.finish(&mut guarded).map_err(|e| e.to_string())?;
        if self.color.insert(id.to_owned(), surface).is_some() {
            return Err("Duplicate backed Photoshop layer".into());
        }
        Ok(true)
    }

    pub(super) fn mask<R: Read + Seek>(
        &mut self,
        source: &Source<R>,
        encoding: &super::structural::Encoding,
        id: &str,
        raster: &Raster,
        offset: [i32; 2],
    ) -> Result<(), String> {
        let check = self.check;
        let cancel = || self.control.check();
        let pool = self.store.resident_budget();
        let sample_bytes = usize::from(raster.depth / 8);
        let pixel_bytes = sample_bytes * 4;
        let row_bytes =
            usize::try_from(encoding.row_bytes).map_err(|_| "Photoshop mask row overflow")?;
        let width = (row_bytes / sample_bytes) as i64;
        let mut channel =
            ScratchBytes::new(&pool, row_bytes, row_bytes).map_err(|e| e.to_string())?;
        let rgba_bytes = raster.width as usize * pixel_bytes;
        let mut rgba =
            ScratchBytes::new(&pool, rgba_bytes, rgba_bytes).map_err(|e| e.to_string())?;
        let mut stream = source.rows_with_checks(encoding, check, &cancel)?;
        let mut output = NativeRows::new(
            raster.width,
            raster.height,
            raster.depth,
            self.store.clone(),
        )
        .map_err(|e| e.to_string())?;
        let mut original = raster
            .retained
            .as_ref()
            .map(|original| -> Result<_, String> {
                let source = &original.pixels;
                if i64::from(source.width) != width || u64::from(source.height) != encoding.rows
                    || source.depth != raster.depth
                {
                    return Err("Backed Photoshop mask original geometry differs".into());
                }
                let output = NativeRows::new(
                    source.width,
                    source.height,
                    source.depth,
                    self.store.clone(),
                )
                .map_err(|e| e.to_string())?;
                let bytes = source.width as usize * pixel_bytes;
                let rgba = ScratchBytes::new(&pool, bytes, bytes).map_err(|e| e.to_string())?;
                Ok((output, rgba))
            })
            .transpose()?;
        if pool.limit().saturating_sub(pool.used()) >= 2 * 1024 * 1024 {
            stream.prefetch(&pool);
        }
        let left = (-i64::from(offset[0])).clamp(0, width) as usize;
        let right = (i64::from(raster.width) - i64::from(offset[0])).clamp(0, width) as usize;
        let mut next = 0;
        let mut guarded = || check().map_err(|_| Error::Cancelled);
        let mut read = |y, channel: &mut [u8], guard: &mut dyn FnMut() -> Result<(), Error>| {
            if !stream.read_row(channel)? {
                return Err("Missing backed Photoshop mask row".to_owned());
            }
            if let Some((output, rgba)) = original.as_mut() {
                for (pixel, sample) in rgba.chunks_exact_mut(pixel_bytes).zip(channel.chunks_exact(sample_bytes)) {
                    write_mask_sample(pixel, sample);
                }
                stage_row(output, y, rgba, sample_bytes, guard)?;
            }
            Ok(())
        };
        for y in 0..raster.height {
            check()?;
            let sy = i64::from(y) - i64::from(offset[1]);
            if left >= right || sy < 0 || sy as u64 >= encoding.rows {
                output.zero_rows(1, &mut guarded).map_err(|error| error.to_string())?;
                continue;
            }
            while next <= sy as u64 {
                read(next as u32, &mut channel, &mut guarded)?;
                next += 1;
            }
            let start = (left as i64 + i64::from(offset[0])) as usize * pixel_bytes;
            let end = start + (right - left) * pixel_bytes;
            rgba[..start].fill(0);
            rgba[end..].fill(0);
            let samples = &channel[left * sample_bytes..right * sample_bytes];
            for (pixel, sample) in rgba[start..end].chunks_exact_mut(pixel_bytes)
                .zip(samples.chunks_exact(sample_bytes))
            {
                write_mask_sample(pixel, sample);
            }
            stage_row(&mut output, y, &rgba, sample_bytes, &mut guarded)?;
        }
        while next < encoding.rows {
            read(next as u32, &mut channel, &mut guarded)?;
            next += 1;
        }
        stream.finish()?.check(source, encoding, check)?;
        let surface = output.finish(&mut guarded).map_err(|e| e.to_string())?;
        if self.masks.insert(id.to_owned(), surface).is_some() {
            return Err("Duplicate backed Photoshop mask".into());
        }
        if let Some((original, _)) = original {
            let surface = original.finish(&mut guarded).map_err(|e| e.to_string())?;
            if self.mask_originals.insert(id.to_owned(), surface).is_some() {
                return Err("Duplicate backed Photoshop mask original".into());
            }
        }
        Ok(())
    }
}

fn write_mask_sample(pixel: &mut [u8], sample: &[u8]) {
    let (rgb, alpha) = pixel.split_at_mut(3 * sample.len());
    for channel in rgb.chunks_exact_mut(sample.len()) {
        channel.copy_from_slice(sample);
    }
    alpha.fill(255);
}

fn stage_row(
    output: &mut NativeRows,
    y: u32,
    rgba: &[u8],
    sample_bytes: usize,
    check: &mut dyn FnMut() -> Result<(), Error>,
) -> Result<(), String> {
    if sample_bytes == 1 {
        output.row8(y, rgba, check)
    } else {
        output.row(y, rgba, check)
    }.map_err(|error| error.to_string())
}
