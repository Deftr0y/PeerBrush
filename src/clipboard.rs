//! Selection pixels and OS clipboard transport. Editing stays in the shared engine.
pub(crate) mod native;
pub(crate) mod paste;
mod png_transport;
use crate::{
    engine::{
        native::Cancellation,
        view::{DocumentSnapshot, DocumentView},
        Document, Layer,
    },
    raster::{CanvasLimits, Raster},
    server::Shared,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::mpsc};
const MAX_PASTE_FILES: usize = 16;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;

fn clip_copy_area(area: [i32; 4], width: u32, height: u32) -> Result<[i32; 4], String> {
    let area = [
        area[0].max(0), area[1].max(0), area[2].min(width as i32), area[3].min(height as i32),
    ];
    if area[0] >= area[2] || area[1] >= area[3] {
        return Err("Select an area inside the canvas to copy".into());
    }
    Ok(area)
}

fn copy_area(doc: &Document) -> Result<[i32; 4], String> {
    let area = doc.selection.unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
    clip_copy_area(area, doc.width, doc.height)
}

fn mask_cut_sample(
    mut pixel: crate::raster::Pixel16,
    coverage: f32,
    depth: u16,
) -> crate::raster::Pixel16 {
    if coverage <= 0. {
        return [0; 4];
    }
    // Match the erase operation's native alpha rounding. Retain the selected
    // paint step's RGB even at transparent pixels, including hidden values.
    let scale = if depth == 8 { 257 } else { 1 };
    pixel[3] = (f64::from(pixel[3] / scale) * f64::from(coverage)).round() as u16 * scale;
    pixel
}

#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    /// Native words retained internally and in platform PNG; bitmap fallback uses display bytes.
    pub samples16: Option<Vec<u16>>,
    pub origin: Option<[i32; 2]>,
}
impl Image {
    fn resident_raster(&self) -> Result<Raster, String> {
        self.validate()?;
        match &self.samples16 {
            Some(words) => Raster::from_rgba16(self.width, self.height, words),
            None => Raster::from_rgba(self.width, self.height, &self.bytes),
        }
    }

    fn decoder(decoder: impl image::ImageDecoder, budget: usize) -> Result<Self, String> {
        let decoded = crate::image_import::decoder(decoder, budget, CanvasLimits::Native)?;
        let (width, height) = (decoded.width(), decoded.height());
        let (bytes, samples16) = match decoded {
            image::DynamicImage::ImageRgba16(image) => {
                let words = image.into_raw();
                let bytes = words.iter().copied().map(crate::raster::project16).collect();
                (bytes, Some(words))
            }
            image::DynamicImage::ImageRgba8(image) => (image.into_raw(), None),
            _ => unreachable!("normalized decoder produces RGBA"),
        };
        let image = Self { width, height, bytes, samples16, origin: None };
        image.validate()?;
        Ok(image)
    }
    #[cfg(test)]
    fn from_raster(pixels: Raster) -> Self {
        Self {
            width: pixels.width,
            height: pixels.height,
            bytes: pixels.rgba(),
            samples16: (pixels.depth == 16).then(|| pixels.rgba16()),
            origin: None,
        }
    }
    fn png_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        use image::ImageDecoder;
        let profile = crate::image_import::prepared_png::validate_clipboard(&bytes)?;
        let mut limits = crate::image_import::limits();
        limits.max_image_width = Some(CanvasLimits::Native.edge());
        limits.max_image_height = Some(CanvasLimits::Native.edge());
        let mut decoder = image::codecs::png::PngDecoder::with_limits(
            std::io::Cursor::new(bytes), limits,
        ).map_err(|e| e.to_string())?;
        if profile && decoder.icc_profile().map_err(|e| e.to_string())?.is_none() {
            return Err("Declared clipboard PNG profile could not be decoded".into());
        }
        Self::decoder(decoder, crate::image_import::BUDGET)
    }
    fn mask_cut(doc: &Document, target: &str, step: Option<&str>) -> Result<Self, String> {
        let coverage = crate::selection::current(doc).ok_or("Select pixels before cutting")?;
        let [left, top, right, bottom] = clip_copy_area(coverage.bounds, doc.width, doc.height)?;
        let layer = doc
            .layers
            .iter()
            .find(|layer| layer.id == target)
            .ok_or("Layer no longer exists")?;
        let mask = layer.mask.as_ref().ok_or("Layer has no mask")?;
        let index = crate::mask::paint_step_index(mask, step)?;
        let raster = &mask.steps[index].pixels;
        let (width, height) = ((right - left) as u32, (bottom - top) as u32);
        let (samples, _) = native::transport_layout(width, height, raster.depth)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(samples)
            .map_err(|_| "Clipboard mask allocation refused")?;
        let mut samples16 = (raster.depth == 16).then(Vec::new);
        if let Some(words) = &mut samples16 {
            words
                .try_reserve_exact(samples)
                .map_err(|_| "Clipboard native mask allocation refused")?;
        }
        for y in top..bottom {
            for x in left..right {
                let pixel = mask_cut_sample(
                    raster.get16(x - layer.x, y - layer.y),
                    coverage.value(x, y),
                    raster.depth,
                );
                bytes.extend(pixel.map(crate::raster::project16));
                if let Some(words) = &mut samples16 {
                    words.extend(pixel);
                }
            }
        }
        Ok(Self {
            width, height, bytes, samples16, origin: Some([left, top]),
        })
    }

    pub fn copy(doc: &Document, target: &str, mask: bool, merged: bool) -> Result<Self, String> {
        Self::copy_pixels(doc, target, mask, merged, true)
    }

    pub(crate) fn copy_in_document(doc: &Document, target: &str, mask: bool) -> Result<Self, String> {
        Self::copy_pixels(doc, target, mask, false, false)
    }

    fn copy_pixels(
        doc: &Document, target: &str, mask: bool, merged: bool, display_color: bool,
    ) -> Result<Self, String> {
        let mask = mask && !merged;
        let area = copy_area(doc)?;
        if doc.bit_depth == 16 {
            return Self::copy16(doc, area, target, mask, merged);
        }
        let target_id = (!merged).then_some(target);
        let (width, height, mut bytes, area) = if display_color {
            doc.preview(Some(area), 8192, target_id, mask)?
        } else {
            doc.preview_native8(Some(area), 8192, target_id, mask)?
        };
        if !merged && !mask {
            let layer = doc
                .layers
                .iter()
                .find(|l| l.id == target)
                .ok_or("Layer no longer exists")?;
            let prepared = layer
                .mask
                .as_ref()
                .and_then(|m| m.prepare(layer.pixels.width, layer.pixels.height));
            for y in 0..height {
                for x in 0..width {
                    let alpha = &mut bytes[((y * width + x) * 4 + 3) as usize];
                    *alpha = (*alpha as f32
                        * layer.mask_value_prepared(
                            area[0] + x as i32 - layer.x,
                            area[1] + y as i32 - layer.y,
                            prepared.as_deref(),
                            false,
                        )
                        * layer.opacity)
                        .round() as u8;
                }
            }
        }
        // Resolve validity once; copied pixels are already cropped to the selection bounds.
        if let Some(coverage) = crate::selection::current(doc) {
            for y in 0..height {
                for x in 0..width {
                    let at = ((y * width + x) * 4) as usize;
                    let factor = coverage.value(area[0] + x as i32, area[1] + y as i32);
                    bytes[at + 3] = (bytes[at + 3] as f64 * factor as f64).round() as u8;
                    if bytes[at + 3] == 0 {
                        bytes[at..at + 4].fill(0);
                    }
                }
            }
        }
        Ok(Self {
            width,
            height,
            bytes,
            samples16: None,
            origin: Some([area[0], area[1]]),
        })
    }
    fn copy16(
        doc: &Document, area: [i32; 4], target: &str, mask: bool, merged: bool,
    ) -> Result<Self, String> {
        let (width, height, mut words, area) =
            crate::depth16::preview16(doc, Some(area), 8192, (!merged).then_some(target), mask)?;
        if !merged && !mask {
            let index = doc
                .layers
                .iter()
                .position(|layer| layer.id == target)
                .ok_or("Layer no longer exists")?;
            let layer = &doc.layers[index];
            let prepared = if layer.mask.as_ref().is_some_and(|mask| mask.enabled) {
                crate::depth16::mask::prepare(layer)?
            } else {
                None
            };
            for y in 0..height {
                for x in 0..width {
                    let factor = crate::depth16::mask::value(
                        layer,
                        area[0] + x as i32 - layer.x,
                        area[1] + y as i32 - layer.y,
                        prepared.as_deref(),
                        false,
                    );
                    let alpha = &mut words[((y * width + x) * 4 + 3) as usize];
                    *alpha = (*alpha as f64 * factor * layer.opacity as f64)
                        .round()
                        .clamp(0., 65535.) as u16;
                }
            }
        }
        if let Some(coverage) = crate::selection::current(doc) {
            for y in 0..height {
                for x in 0..width {
                    let at = ((y * width + x) * 4) as usize;
                    let factor = coverage.value(area[0] + x as i32, area[1] + y as i32);
                    words[at + 3] = (words[at + 3] as f64 * factor as f64).round() as u16;
                    if words[at + 3] == 0 {
                        words[at..at + 4].fill(0);
                    }
                }
            }
        }
        let bytes = words
            .iter()
            .copied()
            .map(crate::raster::project16)
            .collect();
        Ok(Self {
            width,
            height,
            bytes,
            samples16: Some(words),
            origin: Some([area[0], area[1]]),
        })
    }
    pub fn command(&self, context: &str) -> Result<Value, String> {
        let encoded = self.png()?;
        Ok(
            json!({"op":"image.paste","layer":context,"png":STANDARD.encode(encoded),"origin":self.origin}),
        )
    }
    fn validate(&self) -> Result<(), String> {
        let (channels, _) = native::transport_layout(self.width, self.height,
            if self.samples16.is_some() { 16 } else { 8 })?;
        if self.bytes.len() != channels || self.samples16.as_ref().is_some_and(|v| v.len() != channels) {
            return Err("Wrong clipboard image size".into());
        }
        Ok(())
    }
    fn png(&self) -> Result<Vec<u8>, String> {
        self.png_checked(&|| Ok(()))
    }
    fn png_checked(&self, check: &dyn Fn() -> Result<(), String>) -> Result<Vec<u8>, String> {
        png_transport::encode(self, None, png_transport::LIMIT, check)
    }
    #[cfg(any(target_os = "linux", test))]
    fn identified_png(&self, check: &dyn Fn() -> Result<(), String>) -> Result<Vec<u8>, String> {
        png_transport::encode(
            self, Some(*uuid::Uuid::new_v4().as_bytes()), png_transport::LIMIT, check,
        )
    }
    #[cfg(any(target_os = "macos", test))]
    fn display_tiff(&self) -> Result<Vec<u8>, String> {
        use tiff::{encoder::{colortype::RGB8, TiffEncoder}, tags::ExtraSamples};
        self.validate()?;
        let mut bytes = Vec::new();
        (|| {
            let mut encoder = TiffEncoder::new(std::io::Cursor::new(&mut bytes))?;
            let mut image = encoder.new_image::<RGB8>(self.width, self.height)?;
            image.extra_samples(&[ExtraSamples::UnassociatedAlpha])?;
            image.write_data(&self.bytes)
        })().map_err(|error: tiff::TiffError| error.to_string())?;
        Ok(bytes)
    }
}

/// Copy exact selected pixels for interface and agent callers, including rotated boundaries.
pub fn copy_document_pixels(
    doc: &Document,
    target: &str,
    mask: bool,
    merged: bool,
) -> Result<Image, String> {
    Image::copy(doc, target, mask, merged)
}

/// Called inside an Engine transaction so grouping, pixels and selection share one undo step.
pub fn paste(doc: &mut Document, command: &Value) -> Result<(), String> {
    paste_with_pixels(doc, command, || {
        crate::placement::decode(command, 8, crate::image_import::BUDGET)
    })
}

pub(crate) fn paste_with_pixels(
    doc: &mut Document,
    command: &Value,
    pixels: impl FnOnce() -> Result<Raster, String>,
) -> Result<(), String> {
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    let plan = paste::Plan::new(command, doc.layers.len(), |i| {
        let l = &doc.layers[i];
        paste::Layer {
            id: &l.id,
            parent: l.parent.as_deref(),
            kind: &l.kind,
            clip_to: l.clip_to.as_deref(),
            locked: l.locked,
        }
    })?;
    let mut pixels = pixels()?;
    let depth = doc.bit_depth.max(pixels.depth);
    if depth == 16 {
        pixels.promote16();
    }
    let origin = paste::origin(
        command,
        [doc.width, doc.height],
        [pixels.width, pixels.height],
    )?;
    let mut pasted = Layer::new("Pasted image", "paint", pixels.width, pixels.height);
    pasted.pixels = pixels;
    pasted.x = origin[0];
    pasted.y = origin[1];
    let mut draft = doc.clone();
    if depth == 16 && draft.bit_depth == 8 {
        draft.bit_depth = 16;
        draft.ensure_depth();
    }
    if let Some(base) = plan.wrap_base {
        let selected = &doc.layers[base];
        let mut folder = Layer::new(
            &format!("{} group", selected.name),
            "group",
            doc.width,
            doc.height,
        );
        folder.pixels = Raster::new_depth(doc.width, doc.height, depth);
        folder.blend = selected.blend.clone();
        draft.layers[base].blend = "normal".into();
        for l in &mut draft.layers {
            if l.parent == selected.parent
                && (l.id == selected.id || l.clip_to.as_deref() == Some(&selected.id))
            {
                l.parent = Some(folder.id.clone());
            }
        }
        pasted.parent = Some(folder.id.clone());
        draft.layers.insert(plan.index - 1, folder);
    } else {
        pasted.parent = plan.parent;
    }
    draft.layers.insert(plan.index, pasted);
    draft.selection = None;
    draft.selection_coverage = None;
    draft.selection_polygon = None;
    *doc = draft;
    Ok(())
}

pub enum Request {
    Copy {
        doc: Document,
        target: String,
        mask: bool,
        merged: bool,
    },
    CopyLayers {
        doc: Document,
        ids: Vec<String>,
    },
    Cut {
        shared: Shared,
        doc: Document,
        target: String,
        mask: bool,
        step: Option<String>,
    },
    CutLayers {
        shared: Shared,
        doc: Document,
        ids: Vec<String>,
    },
    Paste {
        shared: Shared,
        document: String,
        target: String,
        revision: u64,
    },
}
pub struct Reply {
    pub document: String,
    pub result: Result<String, String>,
    pub selected: Option<String>,
    pub selected_layers: Vec<String>,
}
enum Work {
    Edit(EditRequest),
    Dimensions {
        reply: mpsc::Sender<Result<[u32; 2], String>>,
        cancel: Cancellation,
    },
}
// Keep the public resident request API while workers accept immutable native owners.
pub(crate) enum EditRequest {
    Resident(Request),
    Paste {
        shared: Shared,
        doc: DocumentSnapshot,
        target: String,
    },
    Layers {
        doc: DocumentSnapshot,
        ids: Vec<String>,
        cut: Option<Shared>,
    },
    Pixels {
        shared: Shared,
        doc: DocumentSnapshot,
        target: String,
        mask: bool,
        merged: bool,
        cut: bool,
        step: Option<String>,
    },
}
impl EditRequest {
    fn document(&self) -> &str {
        match self {
            Self::Resident(request) => request.document(),
            Self::Layers { doc, .. } | Self::Pixels { doc, .. } | Self::Paste { doc, .. } => {
                doc.view().id()
            }
        }
    }
}
pub struct Worker {
    tx: mpsc::SyncSender<Work>,
    pub replies: mpsc::Receiver<Reply>,
}
impl Default for Worker {
    fn default() -> Self {
        Self::new(eframe::egui::Context::default())
    }
}
impl Worker {
    #[cfg(test)]
    pub(crate) fn memory(ctx: eframe::egui::Context) -> Self {
        Self::memory_images(ctx, vec![])
    }
    #[cfg(test)]
    pub(crate) fn memory_images(
        ctx: eframe::egui::Context,
        images: Vec<Result<Image, String>>,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Work>(1);
        let (reply_tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            let mut clipboard = tests::Fake::with_images(images);
            let mut session = Session::default();
            while let Ok(request) = rx.recv() {
                let request = match request {
                    Work::Edit(request) => request,
                    Work::Dimensions { reply, cancel } => {
                        let result = image_dimensions(&mut clipboard, &mut || cancel.check());
                        if reply.send(result).is_ok() {
                            ctx.request_repaint();
                        }
                        continue;
                    }
                };
                if reply_tx
                    .send(session.process_edit(&mut clipboard, request))
                    .is_err()
                {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self { tx, replies }
    }
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Work>(1);
        let (reply_tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            // Open on first use and retain ownership for Linux clipboard providers.
            let mut clipboard = None;
            let mut session = Session::default();
            loop {
                #[cfg(windows)]
                windows::pump();
                #[cfg(windows)]
                let request = match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                #[cfg(not(windows))]
                let request = match rx.recv() {
                    Ok(request) => request,
                    Err(_) => break,
                };
                if clipboard.is_none() {
                    clipboard = arboard::Clipboard::new().ok();
                }
                let request = match request {
                    Work::Edit(request) => request,
                    Work::Dimensions { reply, cancel } => {
                        let result = clipboard
                            .as_mut()
                            .ok_or_else(|| "The system clipboard is unavailable".to_owned())
                            .and_then(|clipboard| image_dimensions(clipboard, &mut || cancel.check()));
                        if reply.send(result).is_ok() {
                            ctx.request_repaint();
                        }
                        continue;
                    }
                };
                let document = request.document().to_owned();
                let reply = if let Some(clipboard) = clipboard.as_mut() {
                    session.process_edit(clipboard, request)
                } else {
                    Reply { document, result: Err("The system clipboard is unavailable. Retry after closing other clipboard operations".into()), selected: None, selected_layers: vec![] }
                };
                if reply_tx.send(reply).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self { tx, replies }
    }
    pub fn request(&self, request: Request) -> Result<(), String> {
        match request {
            Request::CopyLayers { doc, ids } => self.copy_layers(doc.into(), ids, None),
            Request::CutLayers { shared, doc, ids } => {
                self.copy_layers(doc.into(), ids, Some(shared))
            }
            Request::Cut {
                shared,
                doc,
                target,
                mask,
                step,
            } => self.copy_pixels(shared, doc.into(), target, mask, false, true, step),
            request => self.request_snapshot(EditRequest::Resident(request)),
        }
    }
    pub(crate) fn request_snapshot(&self, request: EditRequest) -> Result<(), String> {
        self.tx
            .try_send(Work::Edit(request))
            .map_err(|_| "Finish the current clipboard operation first".into())
    }
    pub(crate) fn copy_layers(
        &self,
        doc: DocumentSnapshot,
        ids: Vec<String>,
        cut: Option<Shared>,
    ) -> Result<(), String> {
        self.request_snapshot(EditRequest::Layers { doc, ids, cut })
    }
    pub(crate) fn copy_pixels(
        &self,
        shared: Shared,
        doc: DocumentSnapshot,
        target: String,
        mask: bool,
        merged: bool,
        cut: bool,
        step: Option<String>,
    ) -> Result<(), String> {
        self.request_snapshot(EditRequest::Pixels {
            shared,
            doc,
            target,
            mask,
            merged,
            cut,
            step,
        })
    }
    pub fn image_dimensions(&self) -> Result<mpsc::Receiver<Result<[u32; 2], String>>, String> {
        self.image_dimensions_until(Cancellation::default())
    }
    pub(crate) fn image_dimensions_until(
        &self,
        cancel: Cancellation,
    ) -> Result<mpsc::Receiver<Result<[u32; 2], String>>, String> {
        let (reply, receive) = mpsc::channel();
        self.tx
            .try_send(Work::Dimensions { reply, cancel })
            .map_err(|_| "Finish the current clipboard operation first".to_owned())?;
        Ok(receive)
    }
}

fn image_dimensions(
    provider: &mut impl Provider,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<[u32; 2], String> {
    check()?;
    let sequence = provider.sequence();
    let (image, _) = provider.read_image()?;
    check()?;
    let dimensions = match image {
        ClipboardImage::Bitmap(image) => {
            image.validate()?;
            [image.width, image.height]
        }
        ClipboardImage::Png(png) => crate::image_import::prepared_png::clipboard_dimensions(png.payload(), check)?,
    };
    if provider.sequence() != sequence {
        return Err("Clipboard changed while reading the image size; try again".into());
    }
    Ok(dimensions)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImageIdentity {
    Generation(u64),
    Png { digest: [u8; 32], generation: Option<u64> },
}
impl ImageIdentity {
    fn generation(self) -> Option<u64> {
        match self {
            Self::Generation(value) => Some(value),
            Self::Png { generation, .. } => generation,
        }
    }
}

enum ClipboardImage {
    Bitmap(Image),
    Png(ClipboardPng),
}

struct ClipboardPng {
    allocation: Vec<u8>,
    payload_len: usize,
}
impl From<Vec<u8>> for ClipboardPng {
    fn from(allocation: Vec<u8>) -> Self {
        let payload_len = allocation.len();
        Self { allocation, payload_len }
    }
}
impl ClipboardPng {
    fn payload(&self) -> &[u8] {
        &self.allocation[..self.payload_len]
    }
    fn into_payload(mut self) -> Vec<u8> {
        self.allocation.truncate(self.payload_len);
        self.allocation
    }
}

impl ClipboardImage {
    fn decode(self) -> Result<Image, String> {
        match self {
            Self::Bitmap(image) => Ok(image),
            Self::Png(png) => Image::png_bytes(png.into_payload()),
        }
    }
}

fn png_digest(bytes: &[u8]) -> [u8; 32] {
    ring::digest::digest(&ring::digest::SHA256, bytes).as_ref().try_into().unwrap()
}

// Native transport is replaceable in tests; safety tests never alter the real clipboard.
trait Provider {
    fn write_image(
        &mut self,
        image: &Image,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<ImageIdentity>, String>;
    fn write_captured_image(
        &mut self,
        image: &native::CapturedImage,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<ImageIdentity>, String> {
        let _transport = image.admit_transport(check)?;
        check()?;
        self.write_image(image, check)
    }
    fn read_image(&mut self) -> Result<(ClipboardImage, Option<ImageIdentity>), String>;
    fn write_text(&mut self, text: &str) -> Result<(), String>;
    fn read_text(&mut self) -> Result<String, String>;
    fn read_files(&mut self) -> Result<Vec<PathBuf>, String> {
        Ok(vec![])
    }
    fn sequence(&mut self) -> Option<u64> {
        None
    }
}
impl Provider for arboard::Clipboard {
    fn write_image(
        &mut self,
        image: &Image,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<ImageIdentity>, String> {
        image.validate()?;
        #[cfg(windows)]
        return windows::write_image(image, None, check);
        #[cfg(target_os = "macos")]
        return macos::write_image(image, check).map(Some);
        #[cfg(target_os = "linux")]
        return linux::write_image(self, image, check).map(Some);
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            // Paste commands use PNG even where the OS backend takes RGBA.
            // Check the actual encoded cap before replacing the clipboard/cutting.
            image.png_checked(check)?;
            check()?;
            self.set_image(arboard::ImageData {
                width: image.width as usize,
                height: image.height as usize,
                bytes: std::borrow::Cow::Borrowed(&image.bytes),
            })
            .map_err(|e| e.to_string())?;
            Ok(None)
        }
    }
    #[cfg(windows)]
    fn write_captured_image(
        &mut self,
        image: &native::CapturedImage,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Option<ImageIdentity>, String> {
        windows::write_image(image, image.budget(), check)
    }
    fn read_image(&mut self) -> Result<(ClipboardImage, Option<ImageIdentity>), String> {
        #[cfg(target_os = "linux")]
        return linux::read_image(self);
        #[cfg(windows)]
        if let Some(image) = windows::image()? {
            return Ok((image, None));
        }
        #[cfg(target_os = "macos")]
        if let Some(bytes) = macos::image()? {
            return Ok((ClipboardImage::Png(bytes.into()), None));
        }
        #[cfg(not(target_os = "linux"))]
        {
        let pixels = self
            .get_image()
            .map_err(|_| "No readable clipboard image is available".to_owned())?;
        let width = u32::try_from(pixels.width).map_err(|_| "Clipboard image is too large")?;
        let height = u32::try_from(pixels.height).map_err(|_| "Clipboard image is too large")?;
        native::transport_layout(width, height, 8)?;
        let image = Image {
            width,
            height,
            bytes: pixels.bytes.into_owned(),
            samples16: None,
            origin: None,
        };
        image.validate()?;
        Ok((ClipboardImage::Bitmap(image), None))
        }
    }
    fn write_text(&mut self, text: &str) -> Result<(), String> {
        #[cfg(windows)]
        return windows::write_text(text);
        #[cfg(not(windows))]
        self.set_text(text).map_err(|e| e.to_string())
    }
    fn read_text(&mut self) -> Result<String, String> {
        self.get_text().map_err(|e| e.to_string())
    }
    fn read_files(&mut self) -> Result<Vec<PathBuf>, String> {
        #[cfg(target_os = "linux")]
        return linux::read_files(self);
        #[cfg(target_os = "macos")]
        return macos::read_files();
        #[cfg(windows)]
        {
            match self.get().file_list() {
            Ok(files)=>Ok(files),
            Err(arboard::Error::ContentNotAvailable)=>Ok(vec![]),
            Err(e)=>Err(format!("Cannot read clipboard files: {e}. Retry after the other clipboard operation finishes")),
        }
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            Ok(vec![])
        }
    }
    fn sequence(&mut self) -> Option<u64> {
        #[cfg(windows)]
        {
            clipboard_win::seq_num().map(|s| s.get() as u64)
        }
        #[cfg(target_os = "macos")]
        return macos::sequence();
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            None
        }
    }
}
pub(crate) enum LayerSnapshot {
    Resident(crate::layer_clipboard::Layers),
    Native(native::layers::LayerCopyPlan),
}
struct LayerBuffer {
    marker: String,
    layers: LayerSnapshot,
}
#[derive(Default)]
struct Session {
    image: Option<native::CapturedImage>,
    image_identity: Option<ImageIdentity>,
    layers: Option<LayerBuffer>,
}
impl Session {
    fn check_cut_clipboard(&self, clipboard: &mut dyn Provider) -> Result<(), String> {
        let unchanged = match self.image_identity {
            Some(ImageIdentity::Png { digest, generation: None }) => {
                matches!(clipboard.read_image(), Ok((ClipboardImage::Png(png), _))
                    if png_digest(&png.allocation) == digest)
            }
            Some(identity) => clipboard.sequence() == identity.generation(),
            None => true,
        };
        if unchanged {
            Ok(())
        } else {
            Err("Clipboard changed before the cut could clear the original pixels".into())
        }
    }

    fn process_edit(&mut self, clipboard: &mut dyn Provider, request: EditRequest) -> Reply {
        match request {
            EditRequest::Resident(request) => self.process(clipboard, request),
            EditRequest::Paste {
                shared,
                doc,
                target,
            } => self.process_observed(
                clipboard,
                Request::Paste {
                    shared,
                    document: doc.view().id().to_owned(),
                    revision: doc.view().revision(),
                    target,
                },
                Some(doc.view()),
            ),
            EditRequest::Layers { doc, ids, cut } => Reply {
                document: doc.view().id().to_owned(),
                result: self.copy_layers(clipboard, doc.view(), ids, cut),
                selected: None,
                selected_layers: vec![],
            },
            EditRequest::Pixels {
                shared,
                doc,
                target,
                mask,
                merged,
                cut,
                step,
            } => match doc {
                DocumentSnapshot::Resident(doc) => Reply {
                    document: doc.id.clone(),
                    result: self.copy_resident_pixels(
                        clipboard, &doc, &target, mask, merged,
                        cut.then_some((&shared, step.as_deref())),
                    ),
                    selected: None,
                    selected_layers: vec![],
                },
                DocumentSnapshot::Native(doc) => Reply {
                    document: doc.metadata().id().to_owned(),
                    result: self.copy_native_pixels(
                        clipboard, shared, &doc, &target, mask, merged, cut, step,
                    ),
                    selected: None,
                    selected_layers: vec![],
                },
            },
        }
    }
    fn copy_resident_pixels(
        &mut self,
        clipboard: &mut dyn Provider,
        doc: &Document,
        target: &str,
        mask: bool,
        merged: bool,
        cut: Option<(&Shared, Option<&str>)>,
    ) -> Result<String, String> {
        if cut.is_some() && (merged || doc.selection.is_none()) {
            return Err("Select pixels from one layer to cut".into());
        }
        let [left, top, right, bottom] = copy_area(doc)?;
        native::transport_layout((right - left) as u32, (bottom - top) as u32, doc.bit_depth)?;
        let cut = cut.map(|(shared, step)| {
            (shared, step, json!({"op":"paint.clear_selection","layer":target,"mask":mask,"step":step,"document_id":doc.id,"source_revision":doc.revision}))
        });
        if let Some((_, _, command)) = &cut {
            crate::engine::Engine::preview_edits(doc.clone(), std::slice::from_ref(command))?;
        }
        let image = match cut.as_ref() {
            Some((_, step, _)) if mask => Image::mask_cut(doc, target, *step)?,
            _ => Image::copy(doc, target, mask, merged)?,
        };
        self.image_identity = clipboard.write_image(&image, &|| Ok(()))?;
        self.image = Some(image.into());
        self.layers = None;
        if let Some((shared, _, command)) = cut {
            self.check_cut_clipboard(clipboard)?;
            let reply = crate::server::edit_shared_observed(
                shared, "human", &[command], doc.into(), None, "Cut selected pixels",
            ).map_err(|reason| {
                format!("{reason}. Selection was copied; original pixels remain unchanged")
            })?;
            Ok(edit_status(&reply, "Selected pixels cut; paste to place them again".into()))
        } else {
            Ok("Selection copied".into())
        }
    }
    fn copy_native_pixels(
        &mut self,
        clipboard: &mut dyn Provider,
        shared: Shared,
        doc: &crate::engine::native::NativeDocument,
        target: &str,
        mask: bool,
        merged: bool,
        cut: bool,
        step: Option<String>,
    ) -> Result<String, String> {
        if cut && (merged || doc.metadata().selection().is_none()) {
            return Err("Select pixels from one layer to cut".into());
        }
        let area = native::copy_area(doc)?;
        // Refuse oversized transport buffers before capturing or rendering sources.
        native::transport_layout((area[2] - area[0]) as u32, (area[3] - area[1]) as u32, doc.metadata().depth())?;
        let command = json!({"op":"paint.clear_selection","layer":target,"mask":mask,"step":step,"document_id":doc.metadata().id(),"source_revision":doc.metadata().revision()});
        let descriptor = cut
            .then(|| {
                crate::engine::native::CommandDescriptor::from_normalized(
                    doc,
                    doc.metadata().revision(),
                    &[command.clone()],
                    &|| Ok(()),
                )
            })
            .transpose()?;
        let captured = {
            let engine = shared.lock().unwrap();
            let seed = engine
                .native_seed()?
                .ok_or("Observed clipboard source changed; current work preserved")?;
            if seed.document().key() != doc.key() {
                return Err("Observed clipboard source changed; current work preserved".into());
            }
            if let Some(descriptor) = descriptor {
                engine.capture_native(
                    &seed,
                    descriptor,
                    "human",
                    Some(doc.metadata().revision()),
                    Some(doc.metadata().id()),
                )?;
            }
            engine.capture_scene(None)?
        };
        let scene = captured.into_native()?;
        let session = crate::workspace::native_dispatch::begin_edit_scene(&shared, &scene)?;
        let checked = || session.check();
        let mut work = crate::transform::native::Work::new(128_000_000)?;
        let copied = if cut && mask {
            native::stage_mask_cut(doc, &scene, &command, &session, &mut work, &|| Ok(()))?
        } else {
            native::stage_copy(
                doc, &scene, native::Request { target, mask, merged }, &session, &mut work, &|| Ok(()),
            )?
        };
        let image = copied.retire_into_transport(&session, doc.budget(), &checked)?;
        self.image_identity = image.write(clipboard, &checked)?;
        self.image = Some(image);
        self.layers = None;
        if cut {
            self.check_cut_clipboard(clipboard)?;
            let reply = crate::server::edit_shared_observed(
                &shared,
                "human",
                &[command],
                doc.into(),
                None,
                "Cut selected pixels",
            )
            .map_err(|reason| {
                format!("{reason}. Selection was copied; original pixels remain unchanged")
            })?;
            Ok(edit_status(
                &reply,
                "Selected pixels cut; paste to place them again".into(),
            ))
        } else {
            Ok("Selection copied".into())
        }
    }
    fn copy_layers(
        &mut self,
        clipboard: &mut dyn Provider,
        doc: DocumentView<'_>,
        ids: Vec<String>,
        cut: Option<Shared>,
    ) -> Result<String, String> {
        let (layers, commands) = match doc {
            DocumentView::Resident(doc) => {
                let layers = crate::layer_clipboard::copy(doc, &ids)?;
                let commands = cut
                    .as_ref()
                    .map(|_| crate::layer_clipboard::cut_commands(doc, &ids))
                    .transpose()?;
                (LayerSnapshot::Resident(layers), commands)
            }
            DocumentView::Native(doc) => {
                let mut work = crate::transform::native::Work::new(4_000_000)?;
                let layers =
                    native::layers::LayerCopyPlan::capture(doc, &ids, &mut work, &|| Ok(()))?;
                let commands = cut
                    .as_ref()
                    .map(|_| layers.cut_commands(&mut work, &|| Ok(())))
                    .transpose()?;
                (LayerSnapshot::Native(layers), commands)
            }
        };
        let marker = format!("peerbrush://layers/{}", uuid::Uuid::new_v4());
        // Never delete source work unless ownership of the marker was successfully established.
        clipboard.write_text(&marker)?;
        if cut.is_some() && clipboard.read_text().ok().as_deref() != Some(marker.as_str()) {
            return Err("Clipboard changed before the cut could remove the original layers".into());
        }
        self.layers = Some(LayerBuffer { marker, layers });
        self.image = None;
        self.image_identity = None;
        if let Some(shared) = cut {
            let reply = crate::server::edit_shared_observed(
                &shared,
                "human",
                commands.as_deref().unwrap(),
                doc,
                None,
                "Cut layers",
            )
            .map_err(|reason| {
                format!("{reason}. Layers were copied; original layers remain unchanged")
            })?;
            Ok(edit_status(
                &reply,
                "Layers cut; paste to place them again".into(),
            ))
        } else {
            Ok("Layers copied".into())
        }
    }
    fn process(&mut self, clipboard: &mut dyn Provider, request: Request) -> Reply {
        self.process_observed(clipboard, request, None)
    }
    fn process_observed(
        &mut self,
        clipboard: &mut dyn Provider,
        request: Request,
        source: Option<DocumentView<'_>>,
    ) -> Reply {
        let document = request.document().to_owned();
        let mut selected = None;
        let mut selected_layers = vec![];
        let result = (|| match request {
            Request::Copy {
                doc,
                target,
                mask,
                merged,
            } => self.copy_resident_pixels(clipboard, &doc, &target, mask, merged, None),
            Request::CopyLayers { doc, ids } => {
                self.copy_layers(clipboard, (&doc).into(), ids, None)
            }
            Request::Cut {
                shared,
                doc,
                target,
                mask,
                step,
            } => self.copy_resident_pixels(
                clipboard, &doc, &target, mask, false, Some((&shared, step.as_deref())),
            ),
            Request::CutLayers { shared, doc, ids } => {
                self.copy_layers(clipboard, (&doc).into(), ids, Some(shared))
            }
            Request::Paste {
                shared,
                document,
                target,
                revision,
            } => {
                let native_destination = {
                    let engine = shared.lock().unwrap();
                    crate::workspace::guard(&engine, &document, revision)?;
                    if source.is_some_and(|source| {
                        !source.same_observation(crate::engine::view::current(&engine))
                    }) {
                        return Err(
                            "Observed paste destination changed; current work preserved".into()
                        );
                    }
                    matches!(
                        crate::engine::view::current(&engine),
                        DocumentView::Native(_)
                    )
                };
                let sequence = clipboard.sequence();
                let files = clipboard.read_files()?;
                let text = if files.is_empty() && self.layers.is_some() { clipboard.read_text().ok() } else { None };
                if let Some(buffer) = self
                    .layers
                    .as_ref()
                    .filter(|buffer| text.as_deref() == Some(buffer.marker.as_str()))
                {
                    if clipboard.sequence() != sequence {
                        return Err("Clipboard changed while preparing paste; try again".into());
                    }
                    let reply = crate::server::paste_layers_shared(
                        &shared,
                        "human",
                        &buffer.layers,
                        &target,
                        Some(revision),
                        None,
                        Some(&document),
                        source,
                    )?;
                    selected_layers = reply.data()["created_roots"]
                        .as_array()
                        .map(|ids| {
                            ids.iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    selected = selected_layers.first().cloned();
                    return Ok(edit_status(&reply, "Pasted editable layers".into()));
                }
                let mut images = vec![];
                let mut commands = vec![];
                let mut received = None;
                let mut png = None;
                let bitmap = if files.is_empty() {
                    let (image, identity) = clipboard.read_image()?;
                    let identity = identity.or_else(|| sequence.map(ImageIdentity::Generation));
                    let retained = self.image.as_ref().filter(|_| match (&image, self.image_identity) {
                        (ClipboardImage::Png(png), Some(ImageIdentity::Png { digest, generation })) => {
                            generation.is_none_or(|_| generation == sequence) && png_digest(&png.allocation) == digest
                        }
                        (ClipboardImage::Bitmap(_), Some(ImageIdentity::Png { generation, .. })) => {
                            generation.is_some() && generation.map(ImageIdentity::Generation) == identity
                        }
                        (_, expected) => identity.is_some() && expected == identity,
                    });
                    match image {
                        // Exact bytes came from our validated encoder. Keep the
                        // admitted native pixels and skip a second PNG decode.
                        ClipboardImage::Png(_) if retained.is_some()
                            && matches!(self.image_identity, Some(ImageIdentity::Png { .. })) => retained,
                        ClipboardImage::Png(incoming) if native_destination && retained.is_none() => {
                            png = Some(incoming.into_payload());
                            None
                        }
                        image => {
                            let mut image = image.decode()?;
                            if let Some(retained) = retained.filter(|old| {
                                old.width == image.width
                                    && old.height == image.height
                                    && old.bytes == image.bytes
                                    // Display equality cannot identify different native16 images.
                                    && old.samples16.as_ref().zip(image.samples16.as_ref())
                                        .is_none_or(|(old, incoming)| old == incoming)
                            }) {
                                image.origin = retained.origin;
                                if retained.samples16.is_some() || image.samples16.is_none() {
                                    Some(retained)
                                } else {
                                    Some(&*received.insert(image.into()))
                                }
                            } else {
                                Some(&*received.insert(image.into()))
                            }
                        }
                    }
                } else {
                    if files.len() > MAX_PASTE_FILES {
                        return Err("Paste up to 16 image files at a time".into());
                    }
                    if files.iter().any(|path| !path.is_absolute() || path.to_str().is_none()) {
                        return Err("Clipboard files require absolute paths representable as UTF-8".into());
                    }
                    let mut budget = 0usize;
                    for file in files {
                        if native_destination {
                            commands.push(json!({"op":"image.paste","layer":target,"path":file}));
                            continue;
                        }
                        let pixels = crate::image_import::decode_file(
                            &file, &json!({}), crate::image_import::BUDGET - budget,
                        )?;
                        let channels = pixels.width as usize * pixels.height as usize * 4;
                        let decoded_bytes = channels * if pixels.depth == 16 { 3 } else { 1 };
                        budget = budget.checked_add(decoded_bytes.max(pixels.stored_bytes()))
                            .filter(|bytes| *bytes <= crate::image_import::BUDGET)
                            .ok_or("Clipboard images exceed the 256 MiB paste budget; paste fewer files")?;
                        commands.push(json!({"op":"image.paste","layer":target}));
                        images.push(pixels);
                    }
                    None
                };
                if clipboard.sequence() != sequence {
                    return Err("Clipboard changed while preparing paste; try again".into());
                }
                let image_count = usize::from(bitmap.is_some() || png.is_some()) + commands.len();
                let clipboard_image = png.as_ref().map(native::PasteImage::Png)
                    .or_else(|| bitmap.map(native::PasteImage::Bitmap));
                let reply = if let Some(image) = clipboard_image {
                    crate::server::paste_image_shared(
                        &shared, image, &target, revision, &document, source,
                    )
                } else if !images.is_empty() {
                    crate::server::paste_resident_images_shared(
                        &shared, &commands, images, revision, &document, source,
                    )
                } else {
                    if let Some(source) = source {
                        crate::server::edit_shared_observed(
                            &shared,
                            "human",
                            &commands,
                            source,
                            None,
                            "Paste images",
                        )
                    } else {
                        crate::server::edit_shared(
                            &shared,
                            "human",
                            &commands,
                            Some(revision),
                            None,
                            "Paste images",
                            None,
                            Some(&document),
                        )
                    }
                }?;
                let engine = shared.lock().unwrap();
                let pasted = reply.data()["created"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(Value::as_str)
                            .filter(|id| {
                                crate::engine::view::current(&engine)
                                    .layers()
                                    .any(|layer| layer.id() == *id && layer.kind() == "paint")
                            })
                            .map(String::from)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                drop(engine);
                selected = pasted.first().cloned();
                if image_count > 1 {
                    selected_layers = pasted;
                }
                Ok(edit_status(
                    &reply,
                    if image_count == 1 {
                        "Pasted as a new layer".into()
                    } else {
                        format!("Pasted {image_count} images as new layers")
                    },
                ))
            }
        })();
        Reply {
            document,
            result,
            selected,
            selected_layers,
        }
    }
}

fn edit_status(reply: &crate::server::EditReply, success: String) -> String {
    match reply.data()["cleanup_warning"].as_str() {
        Some(warning) => format!("{success}. {warning}"),
        None => success,
    }
}

impl Request {
    fn document(&self) -> &str {
        match self {
            Self::Copy { doc, .. }
            | Self::Cut { doc, .. }
            | Self::CopyLayers { doc, .. }
            | Self::CutLayers { doc, .. } => &doc.id,
            Self::Paste { document, .. } => document,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    pub(super) struct Fake {
        text: Option<String>,
        image: Option<Image>,
        image_reads: std::collections::VecDeque<Result<ClipboardImage, String>>,
        pub(super) image_identity: Option<ImageIdentity>,
        fail_write: bool,
        fail_read: bool,
        files: Vec<PathBuf>,
        sequence: Option<u64>,
        change_on_read: bool,
        replace_after_write: bool,
        edit_on_write: Option<(Shared, Value)>,
        edit_on_read: Option<(Shared, Value)>,
    }
    impl Fake {
        pub(super) fn with_images(images: Vec<Result<Image, String>>) -> Self {
            Self {
                image_reads: images.into_iter().map(|result| result.map(ClipboardImage::Bitmap)).collect(),
                ..Default::default()
            }
        }
    }
    impl Provider for Fake {
        fn write_image(
            &mut self,
            image: &Image,
            check: &dyn Fn() -> Result<(), String>,
        ) -> Result<Option<ImageIdentity>, String> {
            check()?;
            if self.fail_write {
                return Err("clipboard write failed".into());
            }
            self.image = Some(image.clone());
            self.image_identity = None;
            self.text = None;
            self.files.clear();
            self.sequence = Some(self.sequence.unwrap_or(0) + 1);
            if let Some((shared, command)) = self.edit_on_write.take() {
                shared
                    .lock()
                    .unwrap()
                    .edit("human", &[command], None, None, "Concurrent work")?;
            }
            let published = self.sequence.map(ImageIdentity::Generation);
            if self.replace_after_write {
                self.sequence = self.sequence.map(|generation| generation + 1);
                self.image = None;
                self.text = Some("New clipboard owner".into());
            }
            Ok(published)
        }
        fn read_image(&mut self) -> Result<(ClipboardImage, Option<ImageIdentity>), String> {
            if let Some(image) = self.image_reads.pop_front() {
                return image.map(|image| (image, self.image_identity));
            }
            if self.fail_read {
                return Err("clipboard read denied".into());
            }
            if self.change_on_read {
                self.sequence = self.sequence.map(|n| n + 1);
            }
            self.image.clone().map(|image| (ClipboardImage::Bitmap(image), self.image_identity)).ok_or_else(|| "no OS image".into())
        }
        fn write_text(&mut self, text: &str) -> Result<(), String> {
            if self.fail_write {
                return Err("clipboard write failed".into());
            }
            self.text = Some(if self.replace_after_write { "New clipboard owner" } else { text }.into());
            self.image = None;
            self.files.clear();
            self.sequence = self.sequence.map(|n| n + 1);
            Ok(())
        }
        fn read_text(&mut self) -> Result<String, String> {
            self.text.clone().ok_or_else(|| "no OS text".into())
        }
        fn read_files(&mut self) -> Result<Vec<PathBuf>, String> {
            if let Some((shared, command)) = self.edit_on_read.take() {
                shared
                    .lock()
                    .unwrap()
                    .edit("human", &[command], None, None, "Concurrent work")?;
            }
            Ok(self.files.clone())
        }
        fn sequence(&mut self) -> Option<u64> {
            self.sequence
        }
    }
    fn fixture() -> (Shared, Document, String) {
        let mut e = Engine::new();
        e.resident_state_mut().expect("resident fixture").document = Document::new(8, 8).unwrap();
        let doc = e.resident_state().expect("resident fixture").document.clone();
        let id = doc.layers[0].id.clone();
        (Arc::new(Mutex::new(e)), doc, id)
    }
    #[test]
    fn clipboard_takeover_after_publication_preserves_original_selected_pixels() {
        for depth in [8, 16] {
            let (shared, _, _) = fixture();
            let mut doc = Document::new_depth(8, 8, depth).unwrap();
            doc.selection = Some([1, 1, 3, 3]);
            doc.layers[0].pixels.set16(1, 1, [12347, 23459, 34571, 65535]);
            let target = doc.layers[0].id.clone();
            shared.lock().unwrap().resident_state_mut().unwrap().document = doc.clone();
            let mut clipboard = Fake { replace_after_write: true, ..Default::default() };
            let reply = Session::default().process(&mut clipboard, Request::Cut {
                shared: shared.clone(), doc: doc.clone(), target, mask: false, step: None,
            });
            assert!(reply.result.unwrap_err().contains("Clipboard changed"));
            assert_eq!(clipboard.text.as_deref(), Some("New clipboard owner"));
            let engine = shared.lock().unwrap();
            let current = &engine.resident_state().unwrap().document;
            assert_eq!(current.layers[0].pixels, doc.layers[0].pixels);
            assert_eq!(current.selection, doc.selection);
            assert_eq!(engine.history_lengths(), [0, 0]);
        }
    }
    #[test]
    fn clipboard_cut_requires_the_exact_published_png_without_a_generation() {
        let image = Image {
            width: 1,
            height: 1,
            bytes: vec![13, 29, 83, 255],
            samples16: None,
            origin: None,
        };
        let published = image.identified_png(&|| Ok(())).unwrap();
        let replacement = image.identified_png(&|| Ok(())).unwrap();
        let session = Session {
            image_identity: Some(ImageIdentity::Png {
                digest: png_digest(&published), generation: None,
            }),
            ..Default::default()
        };
        let mut clipboard = Fake {
            image_reads: [
                Ok(ClipboardImage::Png(published.into())),
                Ok(ClipboardImage::Png(replacement.into())),
                Ok(ClipboardImage::Bitmap(image)),
                Err("Clipboard owner disappeared".into()),
            ].into(),
            ..Default::default()
        };
        assert!(session.check_cut_clipboard(&mut clipboard).is_ok());
        for _ in 0..3 {
            assert!(session.check_cut_clipboard(&mut clipboard).is_err());
        }
    }
    #[test]
    fn clipboard_takeover_after_marker_publication_preserves_original_layers() {
        let (shared, doc, target) = fixture();
        let mut clipboard = Fake { replace_after_write: true, ..Default::default() };
        let reply = Session::default().process(&mut clipboard, Request::CutLayers {
            shared: shared.clone(), doc, ids: vec![target.clone()],
        });
        assert!(reply.result.unwrap_err().contains("Clipboard changed"));
        assert_eq!(clipboard.text.as_deref(), Some("New clipboard owner"));
        let engine = shared.lock().unwrap();
        assert_eq!(engine.resident_state().unwrap().document.layers[0].id, target);
        assert_eq!(engine.history_lengths(), [0, 0]);
    }
    #[test]
    fn resident_oversized_selection_refuses_before_render_or_clipboard_write() {
        let (shared, _, _) = fixture();
        let snapshot = {
            let mut engine = shared.lock().unwrap();
            let mut doc = Document::new_depth(8192, 2049, 16).unwrap();
            doc.selection = Some([0, 0, 8192, 2049]);
            engine.resident_state_mut().unwrap().document = doc;
            engine.doc_snapshot()
        };
        let mut clipboard = Fake { text: Some("Keep this".into()), sequence: Some(17), ..Default::default() };
        let mut session = Session::default();
        for cut in [false, true] {
            let reply = session.process_edit(&mut clipboard, EditRequest::Pixels {
                shared: shared.clone(),
                doc: snapshot.clone(),
                target: "uncaptured layer".into(),
                mask: false,
                merged: false,
                cut,
                step: None,
            });
            assert!(reply.result.unwrap_err().contains("import budget"));
            assert_eq!(clipboard.text.as_deref(), Some("Keep this"));
            assert_eq!(clipboard.sequence, Some(17));
            assert!(session.image.is_none());
        }
        let engine = shared.lock().unwrap();
        assert!(snapshot.view().same_observation(crate::engine::view::current(&engine)));
        assert_eq!(engine.history_lengths(), [0, 0]);
    }
    #[test]
    fn dimension_reads_preserve_clipboard_images_native_words_text_files_and_sequence() {
        let mut pixels = Raster::new_depth(7, 5, 16);
        pixels.set16(2, 3, [12347, 33559, 51237, 65535]);
        let image = Image::from_raster(pixels);
        let mut fake = Fake {
            image: Some(image.clone()),
            text: Some("clipboard marker".into()),
            files: vec![PathBuf::from("synthetic.png")],
            sequence: Some(17),
            ..Default::default()
        };
        let mut active = || Ok(());
        assert_eq!(image_dimensions(&mut fake, &mut active).unwrap(), [7, 5]);
        assert_eq!(fake.image.as_ref().unwrap().samples16, image.samples16);
        assert_eq!(fake.image.as_ref().unwrap().bytes, image.bytes);
        assert_eq!(fake.text.as_deref(), Some("clipboard marker"));
        assert_eq!(fake.files, vec![PathBuf::from("synthetic.png")]);
        assert_eq!(fake.sequence, Some(17));
        fake.change_on_read = true;
        assert!(image_dimensions(&mut fake, &mut active).unwrap_err().contains("Clipboard changed"));
        fake.change_on_read = false;
        assert_eq!(fake.sequence, Some(18));
        fake.fail_read = true;
        assert!(image_dimensions(&mut fake, &mut active).is_err());
        fake.fail_read = false;
        fake.image.as_mut().unwrap().width = 16001;
        assert!(image_dimensions(&mut fake, &mut active).is_err());
        fake.image = None;
        assert!(image_dimensions(&mut fake, &mut active).is_err());
        assert_eq!(fake.text.as_deref(), Some("clipboard marker"));
        assert_eq!(fake.sequence, Some(18));
    }

    #[test]
    fn dimension_reads_stream_a_rotated_native16_png_larger_than_the_bitmap_budget() {
        use std::io::Write;
        let [width, height] = [16000, 1050];
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Sixteen);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().unwrap();
        let exif = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        writer.write_chunk(png::chunk::eXIf, exif).unwrap();
        let mut stream = writer.stream_writer_with_size(64 * 1024).unwrap();
        let row = vec![0; width as usize * 8];
        for _ in 0..height {
            stream.write_all(&row).unwrap();
        }
        stream.finish().unwrap();
        writer.finish().unwrap();
        assert!(Image::png_bytes(bytes.clone()).err().unwrap().contains("import budget"));
        let mut checks = 0;
        let stopped = crate::image_import::prepared_png::clipboard_dimensions(&bytes, &mut || {
            checks += 1;
            if checks < 64 { Ok(()) } else { Err("Size read cancelled".into()) }
        });
        assert!(stopped.unwrap_err().contains("Size read cancelled"));
        let mut fake = Fake {
            image_reads: [Ok(ClipboardImage::Png(bytes.into()))].into(),
            sequence: Some(17),
            ..Default::default()
        };
        assert_eq!(image_dimensions(&mut fake, &mut || Ok(())).unwrap(), [height, width]);
        assert_eq!(fake.sequence, Some(17));
        let animation = crate::image_import::prepared_png::apng::tests::single();
        fake.image_reads.push_back(Ok(ClipboardImage::Png(animation.into())));
        assert!(image_dimensions(&mut fake, &mut || Ok(())).unwrap_err().contains("explicit frame"));
    }
    #[test]
    fn clipboard_tiff_fallback_keeps_display_alpha_alongside_native_png_words() {
        let words = vec![12347, 33559, 51237, 27001, 7, 11, 13, 0];
        let bytes = words.iter().copied().map(crate::raster::project16).collect::<Vec<_>>();
        let image = Image { width: 2, height: 1, bytes: bytes.clone(), samples16: Some(words.clone()), origin: None };
        let tiff = image.display_tiff().unwrap();
        assert_eq!(image::load_from_memory_with_format(&tiff, image::ImageFormat::Tiff).unwrap().into_rgba8().into_raw(), bytes);
        assert_eq!(Image::png_bytes(image.png().unwrap()).unwrap().samples16, Some(words));
    }

    #[test]
    fn clipboard_png_refuses_animation_corruption_and_invalid_native_buffers() {
        let animated = crate::image_import::prepared_png::apng::tests::single();
        assert!(Image::png_bytes(animated).err().unwrap().contains("explicit frame"));
        let mut image = Image {
            width: 16000, height: 1,
            bytes: [13, 57, 123, 0].repeat(16000),
            samples16: None, origin: None,
        };
        let mut png = image.png().unwrap();
        *png.last_mut().unwrap() ^= 1;
        assert!(Image::png_bytes(png).err().unwrap().contains("checksum"));
        image.samples16 = Some(vec![12347; 3]);
        assert!(image.command("target").err().unwrap().contains("Wrong clipboard image size"));
        image.samples16 = None;
        image.width = 16001;
        assert!(image.command("target").is_err());
        image.width = 16000;
        image.height = 16000;
        assert!(image.command("target").err().unwrap().contains("256 MiB"));
    }

    #[test]
    fn dimension_worker_returns_separate_read_only_replies_without_editing() {
        let worker = Worker::memory(eframe::egui::Context::default());
        let result = worker
            .image_dimensions()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(result.is_err());
        assert!(matches!(
            worker.replies.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn cancelled_dimension_work_leaves_the_next_clipboard_read_available() {
        let mut pixels = Raster::new_depth(7, 5, 16);
        pixels.set16(0, 0, [12347, 33559, 51237, 65535]);
        let worker = Worker::memory_images(
            eframe::egui::Context::default(), vec![Ok(Image::from_raster(pixels))],
        );
        let cancel = Cancellation::default();
        cancel.cancel();
        let abandoned = worker.image_dimensions_until(cancel).unwrap();
        let timeout = std::time::Duration::from_secs(2);
        assert!(abandoned.recv_timeout(timeout).unwrap().unwrap_err().contains("cancelled"));
        let next = worker.image_dimensions().unwrap();
        assert_eq!(next.recv_timeout(timeout).unwrap().unwrap(), [7, 5]);
        assert!(matches!(worker.replies.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
    #[test]
    fn snapshot_paste_rechecks_destination_after_clipboard_reads_for_layers_and_pixels() {
        for layers in [false, true] {
            let (shared, doc, target) = fixture();
            let mut session = Session::default();
            let mut fake = Fake::default();
            let copy = if layers {
                Request::CopyLayers {
                    doc: doc.clone(),
                    ids: vec![target.clone()],
                }
            } else {
                Request::Copy {
                    doc: doc.clone(),
                    target: target.clone(),
                    mask: false,
                    merged: false,
                }
            };
            assert!(session.process(&mut fake, copy).result.is_ok());
            fake.edit_on_read = Some((
                shared.clone(),
                json!({"op":"layer.update","layer":target,"name":"Human work"}),
            ));
            let reply = session.process_edit(
                &mut fake,
                EditRequest::Paste {
                    shared: shared.clone(),
                    doc: doc.clone().into(),
                    target,
                },
            );
            assert!(reply
                .result
                .unwrap_err()
                .contains("Observed edit source changed"));
            assert!(reply.selected.is_none());
            assert!(reply.selected_layers.is_empty());
            let engine = shared.lock().unwrap();
            assert_eq!(engine.resident_state().expect("resident fixture").document.revision, doc.revision + 1);
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), doc.layers.len());
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].name, "Human work");
            assert_eq!(
                engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                doc.layers[0].pixels.rgba16()
            );
            assert_eq!(engine.history_lengths(), [1, 0]);
            assert_eq!(
                engine
                    .file_operations
                    .load(std::sync::atomic::Ordering::Acquire),
                0
            );
        }
    }

    #[test]
    fn selected_cut_paste_retains_coordinates_native_depth_and_separate_undo_steps() {
        for depth in [8, 16] {
            let (shared, _, id) = fixture();
            let doc = {
                let mut e = shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                e.resident_state_mut().expect("resident fixture").document.layers[0].id = id.clone();
                for y in 0..8 {
                    for x in 0..8 {
                        e.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(x, y, [12347, 33559, 51237, 65535]);
                    }
                }
                e.resident_state_mut().expect("resident fixture").document.selection = Some([2, 3, 5, 7]);
                e.resident_state().expect("resident fixture").document.clone()
            };
            let mut session = Session::default();
            let mut fake = Fake::default();
            session
                .process(
                    &mut fake,
                    Request::Cut {
                        shared: shared.clone(),
                        doc: doc.clone(),
                        target: id.clone(),
                        mask: false,
                        step: None,
                    },
                )
                .result
                .unwrap();
            {
                let e = shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
                assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].pixels.get16(2, 3), [0; 4]);
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.get16(1, 3),
                    doc.layers[0].pixels.get16(1, 3)
                );
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
            }
            assert_eq!(fake.image.as_ref().unwrap().origin, Some([2, 3]));
            let revision = shared.lock().unwrap().resident_state().expect("resident fixture").document.revision;
            session
                .process(
                    &mut fake,
                    Request::Paste {
                        shared: shared.clone(),
                        document: doc.id.clone(),
                        target: id,
                        revision,
                    },
                )
                .result
                .unwrap();
            let mut e = shared.lock().unwrap();
            let pasted = e
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .find(|l| l.name == "Pasted image")
                .unwrap();
            assert_eq!(
                (
                    pasted.x,
                    pasted.y,
                    pasted.pixels.width,
                    pasted.pixels.height
                ),
                (2, 3, 3, 4)
            );
            assert_eq!(pasted.pixels.get16(0, 0), doc.layers[0].pixels.get16(2, 3));
            assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, depth);
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
            e.undo("human").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
            e.undo("human").unwrap();
            assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), doc.export_png().unwrap());
        }
    }
    #[test]
    fn selected_cut_failed_clipboard_or_concurrent_edit_never_erases_source_work() {
        for (depth, concurrent) in [8, 16]
            .into_iter()
            .flat_map(|depth| [false, true].map(move |concurrent| (depth, concurrent)))
        {
            let (shared, _, id) = fixture();
            let mut doc = Document::new_depth(8, 8, depth).unwrap();
            doc.layers[0].id = id.clone();
            doc.selection = Some([2, 3, 5, 7]);
            doc.layers[0]
                .pixels
                .set16(2, 3, [12347, 33559, 51237, 60001]);
            shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
            let mut session = Session::default();
            let mut fake = Fake::default();
            if concurrent {
                fake.edit_on_write = Some((
                    shared.clone(),
                    json!({"op":"layer.update","layer":id,"name":"New human work"}),
                ));
            } else {
                fake.fail_write = true;
            }
            let result = session
                .process(
                    &mut fake,
                    Request::Cut {
                        shared: shared.clone(),
                        doc: doc.clone(),
                        target: id,
                        mask: false,
                        step: None,
                    },
                )
                .result;
            assert!(result.is_err());
            let e = shared.lock().unwrap();
            assert_eq!(
                e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                doc.layers[0].pixels.rgba16()
            );
            assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), usize::from(concurrent));
            if concurrent {
                assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].name, "New human work");
                assert!(session.image.is_some());
            }
        }
    }
    #[test]
    fn selected_cut_locks_and_reservations_keep_native_source_and_recoverable_copy() {
        for depth in [8, 16] {
            for locked in [false, true] {
                let (shared, _, _) = fixture();
                let doc = {
                    let mut e = shared.lock().unwrap();
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                    e.resident_state_mut().expect("resident fixture").document.selection = Some([2, 3, 5, 7]);
                    e.resident_state_mut().expect("resident fixture").document.layers[0]
                        .pixels
                        .set16(2, 3, [12347, 33559, 51237, 60001]);
                    if locked {
                        e.resident_state_mut().expect("resident fixture").document.layers[0].locked = true;
                    } else {
                        let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                        e.reserve(
                            "agent",
                            "Native source",
                            vec![crate::engine::Scope::layer(&id)],
                        )
                        .unwrap();
                    }
                    e.resident_state().expect("resident fixture").document.clone()
                };
                let mut session = Session::default();
                let mut fake = Fake::default();
                let reply = session.process(
                    &mut fake,
                    Request::Cut {
                        shared: shared.clone(),
                        target: doc.layers[0].id.clone(),
                        mask: false,
                        step: None,
                        doc: doc.clone(),
                    },
                );
                assert!(reply.result.is_err());
                let e = shared.lock().unwrap();
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    doc.layers[0].pixels.rgba16()
                );
                assert_eq!(e.resident_state().expect("resident fixture").document.selection, doc.selection);
                assert!(e.resident_state().expect("resident fixture").undo.is_empty());
                if !locked {
                    let copy = Image::copy(&doc, &doc.layers[0].id, false, false).unwrap();
                    assert_eq!(session.image.as_ref().unwrap().bytes, copy.bytes);
                    assert_eq!(session.image.as_ref().unwrap().samples16, copy.samples16);
                    assert_eq!(fake.image.as_ref().unwrap().origin, copy.origin);
                }
            }
        }
    }
    #[test]
    fn failed_os_marker_write_never_cuts_or_replaces_previous_internal_copy() {
        let (shared, doc, id) = fixture();
        let mut session = Session::default();
        let mut fake = Fake::default();
        assert!(session
            .process(
                &mut fake,
                Request::CopyLayers {
                    doc: doc.clone(),
                    ids: vec![id.clone()]
                }
            )
            .result
            .is_ok());
        let marker = fake.text.clone();
        fake.fail_write = true;
        assert!(session
            .process(
                &mut fake,
                Request::CutLayers {
                    shared: shared.clone(),
                    doc,
                    ids: vec![id]
                }
            )
            .result
            .is_err());
        assert_eq!(fake.text, marker);
        assert_eq!(shared.lock().unwrap().resident_state().expect("resident fixture").document.layers.len(), 1);
        assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        assert_eq!(session.layers.as_ref().unwrap().marker, marker.unwrap());
    }
    #[test]
    fn stale_revision_and_replaced_document_leave_cut_originals_untouched() {
        for replace in [false, true] {
            let (shared, doc, id) = fixture();
            let mut session = Session::default();
            let mut fake = Fake::default();
            {
                let mut e = shared.lock().unwrap();
                if replace {
                    e.resident_state_mut().expect("resident fixture").document = Document::new(8, 8).unwrap();
                } else {
                    e.edit(
                        "human",
                        &[json!({"op":"layer.update","layer":id,"name":"Human work"})],
                        None,
                        None,
                        "Rename",
                    )
                    .unwrap();
                }
            }
            let before = serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
            let reply = session.process(
                &mut fake,
                Request::CutLayers {
                    shared: shared.clone(),
                    doc,
                    ids: vec![id],
                },
            );
            assert!(reply.result.unwrap_err().contains("original layers remain"));
            assert!(fake
                .text
                .as_deref()
                .unwrap()
                .starts_with("peerbrush://layers/"));
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                before
            );
        }
    }
    #[test]
    fn matching_marker_pastes_typed_layers_but_external_image_wins_after_clipboard_changes() {
        let (shared, doc, id) = fixture();
        let mut session = Session::default();
        let mut fake = Fake::default();
        session
            .process(
                &mut fake,
                Request::CopyLayers {
                    doc,
                    ids: vec![id.clone()],
                },
            )
            .result
            .unwrap();
        let document = shared.lock().unwrap().resident_state().expect("resident fixture").document.id.clone();
        let pasted = session.process(
            &mut fake,
            Request::Paste {
                document: document.clone(),
                shared: shared.clone(),
                target: id.clone(),
                revision: 0,
            },
        );
        pasted.result.unwrap();
        assert_eq!(pasted.selected_layers.len(), 1);
        // Emulate an OS image that replaced the PeerBrush plain-text marker.
        fake.text = Some("outside application".into());
        fake.image = Some(Image {
            width: 1,
            height: 1,
            bytes: vec![12, 34, 56, 255],
            samples16: None,
            origin: None,
        });
        let revision = shared.lock().unwrap().resident_state().expect("resident fixture").document.revision;
        let document = shared.lock().unwrap().resident_state().expect("resident fixture").document.id.clone();
        let pasted = session.process(
            &mut fake,
            Request::Paste {
                document: document.clone(),
                shared: shared.clone(),
                target: pasted.selected.unwrap(),
                revision,
            },
        );
        pasted.result.unwrap();
        assert!(pasted.selected_layers.is_empty());
        let e = shared.lock().unwrap();
        assert_eq!(
            e.resident_state().expect("resident fixture").document
                .layers
                .iter()
                .find(|l| Some(&l.id) == pasted.selected.as_ref())
                .unwrap()
                .pixels
                .get(0, 0),
            [12, 34, 56, 255]
        );
    }
    #[test]
    fn cut_all_layers_then_paste_with_deleted_target_remains_undoable() {
        let (shared, doc, id) = fixture();
        let mut session = Session::default();
        let mut fake = Fake::default();
        session
            .process(
                &mut fake,
                Request::CutLayers {
                    shared: shared.clone(),
                    doc,
                    ids: vec![id.clone()],
                },
            )
            .result
            .unwrap();
        assert!(shared.lock().unwrap().resident_state().expect("resident fixture").document.layers.is_empty());
        let document = shared.lock().unwrap().resident_state().expect("resident fixture").document.id.clone();
        let pasted = session.process(
            &mut fake,
            Request::Paste {
                document: document.clone(),
                shared: shared.clone(),
                target: id,
                revision: 1,
            },
        );
        pasted.result.unwrap();
        assert_eq!(pasted.selected_layers.len(), 1);
        let mut e = shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 2);
        e.undo("human").unwrap();
        assert!(e.resident_state().expect("resident fixture").document.layers.is_empty());
        e.undo("human").unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
    }

    #[test]
    fn matching_display_pixels_preserve_incoming_native_words_and_distinguish_changed_sources() {
        let words = vec![12 * 257 + 1, 34 * 257 + 2, 56 * 257 + 3, 65535];
        let display = vec![12, 34, 56, 255];
        for sequence in [None, Some(17)] {
            for (old_words, origin) in [
                (None, [2, 3]),
                (Some(words.clone()), [2, 3]),
                (Some(vec![12 * 257 + 7, 34 * 257 + 9, 56 * 257 + 11, 65535]), [3, 3]),
            ] {
                let (shared, doc, target) = fixture();
                let original = serde_json::to_value(&doc).unwrap();
                let mut session = Session::default();
                session.image_identity = sequence.map(ImageIdentity::Generation);
                session.image = Some(Image {
                    width: 1, height: 1, bytes: display.clone(),
                    samples16: old_words.clone(), origin: Some([2, 3]),
                }.into());
                let mut fake = Fake::with_images(vec![Ok(Image {
                    width: 1, height: 1, bytes: display.clone(),
                    samples16: Some(words.clone()), origin: None,
                })]);
                fake.sequence = sequence;
                let reply = session.process_edit(&mut fake, EditRequest::Paste {
                    shared: shared.clone(), doc: doc.into(), target,
                });
                reply.result.unwrap();
                let mut engine = shared.lock().unwrap();
                let verify = |engine: &crate::engine::Engine| {
                    let document = &engine.resident_state().expect("resident fixture").document;
                    let layer = document.layers.iter().find(|l| Some(&l.id) == reply.selected.as_ref()).unwrap();
                    assert_eq!(document.bit_depth, 16);
                    assert_eq!([layer.x, layer.y], if sequence.is_some() { origin } else { [3, 3] });
                    assert_eq!(layer.pixels.rgba16(), words);
                };
                verify(&engine);
                assert_eq!(engine.history_lengths(), [1, 0]);
                assert_eq!(session.image.as_ref().unwrap().samples16, old_words);
                assert_eq!(session.image.as_ref().unwrap().origin, Some([2, 3]));
                engine.undo("human").unwrap();
                let mut restored = engine.resident_state().expect("resident fixture").document.clone();
                restored.revision = original["revision"].as_u64().unwrap();
                assert_eq!(serde_json::to_value(restored).unwrap(), original);
                engine.redo("human").unwrap();
                verify(&engine);
            }
        }
    }

    #[test]
    fn native_png_clipboard_identity_preserves_only_the_matching_copy_origin() {
        let words = vec![12347, 33559, 51237, 27001];
        let display = words.iter().copied().map(crate::raster::project16).collect::<Vec<_>>();
        let own = Image { width: 1, height: 1, bytes: display, samples16: Some(words.clone()), origin: Some([2, 1]) };
        let copied = own.identified_png(&|| Ok(())).unwrap();
        let other = own.identified_png(&|| Ok(())).unwrap();
        let digest = png_digest(&copied);
        assert_ne!(digest, png_digest(&other));
        let mut padded = copied.clone();
        padded.extend_from_slice(&[0; 13]);
        let padded_digest = png_digest(&padded);
        for (bytes, payload_len, digest, generation, sequence, expected_origin) in [
            (&copied, copied.len(), digest, None, None, [2, 1]),
            (&other, other.len(), digest, None, None, [3, 3]),
            (&copied, copied.len(), digest, Some(17), Some(17), [2, 1]),
            (&other, other.len(), digest, Some(17), Some(17), [3, 3]),
            (&copied, copied.len(), digest, Some(17), Some(18), [3, 3]),
            (&copied, copied.len(), digest, Some(17), None, [3, 3]),
            (&padded, copied.len(), padded_digest, Some(17), Some(17), [2, 1]),
            (&copied, copied.len(), padded_digest, Some(17), Some(17), [3, 3]),
            (&padded, copied.len(), padded_digest, Some(17), Some(18), [3, 3]),
        ] {
            let (shared, doc, target) = fixture();
            let mut session = Session { image: Some(own.clone().into()),
                image_identity: Some(ImageIdentity::Png { digest, generation }), ..Default::default() };
            let png = ClipboardPng { allocation: bytes.clone(), payload_len };
            let mut fake = Fake { sequence, image_reads: [Ok(ClipboardImage::Png(png))].into(), ..Default::default() };
            let reply = session.process_edit(&mut fake, EditRequest::Paste { shared: shared.clone(), doc: doc.into(), target });
            reply.result.unwrap();
            let engine = shared.lock().unwrap();
            let doc = &engine.resident_state().unwrap().document;
            let pasted = doc.layers.iter().find(|layer| Some(&layer.id) == reply.selected.as_ref()).unwrap();
            assert_eq!([pasted.x, pasted.y], expected_origin);
            assert_eq!(pasted.pixels.rgba16(), words);
            assert_eq!(engine.history_lengths(), [1, 0]);
        }
    }

    #[test]
    fn unknown_clipboard_generation_cannot_restore_retained_origin_or_hidden_native_words() {
        let (shared, doc, target) = fixture();
        let display = vec![12, 34, 56, 255];
        let mut session = Session {
            image: Some(Image {
                width: 1, height: 1, bytes: display.clone(),
                samples16: Some(vec![12 * 257 + 1, 34 * 257 + 2, 56 * 257 + 3, 65535]),
                origin: Some([2, 1]),
            }.into()),
            ..Default::default()
        };
        let mut external = Fake::with_images(vec![Ok(Image {
            width: 1, height: 1, bytes: display.clone(), samples16: None, origin: None,
        })]);
        let reply = session.process_edit(&mut external, EditRequest::Paste {
            shared: shared.clone(), doc: doc.into(), target,
        });
        reply.result.unwrap();
        let engine = shared.lock().unwrap();
        let document = &engine.resident_state().unwrap().document;
        let layer = document.layers.iter().find(|layer| Some(&layer.id) == reply.selected.as_ref()).unwrap();
        assert_eq!(document.bit_depth, 8);
        assert_eq!([layer.x, layer.y], [3, 3]);
        assert_eq!(layer.pixels.rgba(), display);
        assert_eq!(engine.history_lengths(), [1, 0]);
    }

    #[test]
    fn native_pixel_copy_retains_words_across_the_os_display_transport() {
        let (shared, mut doc, id) = fixture();
        doc.bit_depth = 16;
        doc.layers[0].pixels.promote16();
        doc.layers[0]
            .pixels
            .set16(2, 3, [10001, 30003, 50007, 65535]);
        doc.selection = Some([2, 3, 3, 4]);
        shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
        let mut session = Session::default();
        let mut fake = Fake::default();
        session
            .process(
                &mut fake,
                Request::Copy {
                    doc,
                    target: id.clone(),
                    mask: false,
                    merged: false,
                },
            )
            .result
            .unwrap();
        // Native OS image formats expose display bytes to arboard, so only internal retention
        // can preserve these original words during a PeerBrush-to-PeerBrush paste.
        fake.image.as_mut().unwrap().samples16 = None;
        let document = shared.lock().unwrap().resident_state().expect("resident fixture").document.id.clone();
        let reply = session.process(
            &mut fake,
            Request::Paste {
                document: document.clone(),
                shared: shared.clone(),
                target: id,
                revision: 0,
            },
        );
        reply.result.unwrap();
        let engine = shared.lock().unwrap();
        let pasted = engine
            .resident_state().expect("resident fixture").document
            .layers
            .iter()
            .find(|l| Some(&l.id) == reply.selected.as_ref())
            .unwrap();
        assert_eq!(pasted.pixels.get16(0, 0), [10001, 30003, 50007, 65535]);
    }
    #[test]
    fn changing_clipboard_during_prepare_never_edits_or_reuses_native_data() {
        let (shared, doc, id) = fixture();
        let before = serde_json::to_value(&doc).unwrap();
        let mut fake = Fake {
            sequence: Some(1),
            change_on_read: true,
            image: Some(Image {
                width: 1,
                height: 1,
                bytes: vec![23, 45, 67, 255],
                samples16: None,
                origin: None,
            }),
            ..Default::default()
        };
        let reply = Session::default().process(
            &mut fake,
            Request::Paste {
                shared: shared.clone(),
                document: doc.id,
                target: id,
                revision: 0,
            },
        );
        assert!(reply.result.unwrap_err().contains("Clipboard changed"));
        let e = shared.lock().unwrap();
        assert!(e.resident_state().expect("resident fixture").undo.is_empty());
        assert_eq!(serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(), before);
    }
    #[test]
    fn copied_image_files_take_priority_over_thumbnail_pixels_and_batch_once() {
        let (shared, doc, id) = fixture();
        let mut files = vec![];
        for (extension, format) in [
            ("bmp", image::ImageFormat::Bmp),
            ("jpg", image::ImageFormat::Jpeg),
        ] {
            let file = std::env::temp_dir().join(format!(
                "peerbrush-file-paste-{}.{}",
                uuid::Uuid::new_v4(),
                extension
            ));
            image::RgbImage::from_pixel(2, 2, image::Rgb([23, 45, 67]))
                .save_with_format(&file, format)
                .unwrap();
            files.push(file);
        }
        let native = std::env::temp_dir().join(format!("peerbrush-file-paste-{}.png", uuid::Uuid::new_v4()));
        let words: [u16; 4] = [12347, 23459, 34571, 65535];
        image::ImageBuffer::from_pixel(1, 1, image::Rgba(words)).save(&native).unwrap();
        files.push(native);
        let mut fake = Fake {
            files: files.clone(),
            image: Some(Image {
                width: 1,
                height: 1,
                bytes: vec![255, 0, 0, 255],
                samples16: None,
                origin: None,
            }),
            ..Default::default()
        };
        let reply = Session::default().process(
            &mut fake,
            Request::Paste {
                shared: shared.clone(),
                document: doc.id,
                target: id,
                revision: 0,
            },
        );
        reply.result.unwrap();
        assert_eq!(reply.selected_layers.len(), 3);
        let mut e = shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
        assert_eq!(e.resident_state().unwrap().document.bit_depth, 16);
        let mut found_native = false;
        for id in reply.selected_layers {
            let l = e.resident_state().expect("resident fixture").document.layers.iter().find(|l| l.id == id).unwrap();
            if (l.pixels.width, l.pixels.height) == (1, 1) {
                assert_eq!(l.pixels.get16(0, 0), words);
                found_native = true;
                continue;
            }
            assert_eq!((l.pixels.width, l.pixels.height), (2, 2));
            let color = l.pixels.get(0, 0);
            for (actual, expected) in color.into_iter().zip([23u8, 45, 67, 255]) {
                assert!(actual.abs_diff(expected) <= 2);
            }
        }
        assert!(found_native);
        e.undo("human").unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.layers.len(), 1);
        assert_eq!(e.resident_state().unwrap().document.bit_depth, 8);
        for file in files {
            std::fs::remove_file(file).unwrap();
        }
    }
    #[test]
    fn invalid_clipboard_file_counts_and_project_sources_preserve_work() {
        let (shared, doc, id) = fixture();
        for stale in [false, true] {
            let mut fake = Fake {
                files: vec![PathBuf::from("not-an-absolute-image.png"); if stale { 1 } else { 17 }],
                ..Default::default()
            };
            let reply = Session::default().process(
                &mut fake,
                Request::Paste {
                    shared: shared.clone(),
                    document: if stale {
                        "stale".into()
                    } else {
                        doc.id.clone()
                    },
                    target: id.clone(),
                    revision: 0,
                },
            );
            let error = reply.result.unwrap_err();
            assert!(
                if stale {
                    error.contains("changed")
                } else {
                    error.contains("16 image files")
                },
                "{error}"
            );
        }
        let e = shared.lock().unwrap();
        assert!(e.resident_state().expect("resident fixture").undo.is_empty());
        assert_eq!(
            serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(),
            serde_json::to_value(&doc).unwrap()
        );
    }
}
