//! Selection pixels and OS clipboard transport. Editing stays in the shared engine.
use crate::{
    engine::{Document, Layer},
    raster::{check_size, png, Raster},
    server::Shared,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
#[cfg(not(windows))]
use std::borrow::Cow;
use std::{path::PathBuf, sync::mpsc};
#[cfg(windows)]
mod windows;

#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    /// Native words retained inside PeerBrush; OS clipboard transport uses display bytes.
    pub samples16: Option<Vec<u16>>,
    pub origin: Option<[i32; 2]>,
}
impl Image {
    fn decoder(decoder: impl image::ImageDecoder, budget: usize) -> Result<Self, String> {
        Ok(Self::from_raster(crate::image_import::decoder(
            decoder, budget,
        )?))
    }
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
        Ok(Self::from_raster(
            crate::image_import::Encoded::png(bytes)?
                .decode(&json!({}), crate::image_import::BUDGET)?,
        ))
    }
    fn file(path: &std::path::Path, budget: usize) -> Result<Self, String> {
        let pixels = crate::image_import::decode_file(path, &json!({}), budget)?;
        Ok(Self::from_raster(pixels))
    }

    pub fn copy(doc: &Document, target: &str, mask: bool, merged: bool) -> Result<Self, String> {
        if doc.bit_depth == 16 {
            return Self::copy16(doc, target, mask, merged);
        }
        let area = doc
            .selection
            .unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
        if area[2] <= area[0]
            || area[3] <= area[1]
            || area[2] <= 0
            || area[3] <= 0
            || area[0] >= doc.width as i32
            || area[1] >= doc.height as i32
        {
            return Err("Select an area inside the canvas to copy".into());
        }
        let (width, height, mut bytes, area) =
            doc.preview(Some(area), 8192, (!merged).then_some(target), mask)?;
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
    fn copy16(doc: &Document, target: &str, mask: bool, merged: bool) -> Result<Self, String> {
        let area = doc
            .selection
            .unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
        if area[2] <= area[0]
            || area[3] <= area[1]
            || area[2] <= 0
            || area[3] <= 0
            || area[0] >= doc.width as i32
            || area[1] >= doc.height as i32
        {
            return Err("Select an area inside the canvas to copy".into());
        }
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
                crate::depth16::mask_image(doc, index)?
            } else {
                None
            };
            for y in 0..height {
                for x in 0..width {
                    let factor = prepared.as_ref().map_or(1., |image| {
                        image.get(area[0] + x as i32 - layer.x, area[1] + y as i32 - layer.y)[0]
                            as f64
                            / 65535.
                    });
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
        check_size(self.width, self.height)?;
        let encoded = if let Some(words) = &self.samples16 {
            crate::raster::png16(self.width, self.height, words)?
        } else {
            png(self.width, self.height, &self.bytes)?
        };
        Ok(
            json!({"op":"image.paste","layer":context,"png":STANDARD.encode(encoded),"origin":self.origin}),
        )
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
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    let context = command["layer"]
        .as_str()
        .ok_or("Choose a layer or folder before pasting")?;
    let index = doc.layers.iter().position(|l| l.id == context);
    if index.is_none() && !doc.layers.is_empty() {
        return Err("Paste destination no longer exists".into());
    }
    let selected = index.map(|index| doc.layers[index].clone());
    let mut parent = selected.as_ref().map(|l| l.id.as_str());
    for _ in 0..=16 {
        let Some(pid) = parent else { break };
        let l = doc
            .layers
            .iter()
            .find(|l| l.id == pid)
            .ok_or("Invalid paste destination")?;
        if l.locked {
            return Err("Unlock the destination before pasting".into());
        }
        parent = l.parent.as_deref();
    }
    let wrap = selected
        .as_ref()
        .is_some_and(|l| l.kind != "group" && l.parent.is_none());
    if doc.layers.len() + if wrap { 2 } else { 1 } > 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let encoded = command["png"]
        .as_str()
        .ok_or("Paste needs base64 PNG pixels")?;
    if encoded.len() > 180 * 1024 * 1024 {
        return Err("Clipboard image is too large".into());
    }
    let encoded = STANDARD.decode(encoded).map_err(|e| e.to_string())?;
    let image = Image::png_bytes(encoded)?;
    let depth = if image.samples16.is_some() {
        16
    } else {
        doc.bit_depth
    };
    let mut pasted = Layer::new("Pasted image", "paint", image.width, image.height);
    pasted.pixels = if let Some(words) = &image.samples16 {
        Raster::from_rgba16(image.width, image.height, words)?
    } else {
        let mut pixels = Raster::from_rgba(image.width, image.height, &image.bytes)?;
        if depth == 16 {
            pixels.promote16();
        }
        pixels
    };
    let origin = if command["origin"].is_null() {
        [
            (doc.width as i32 - image.width as i32) / 2,
            (doc.height as i32 - image.height as i32) / 2,
        ]
    } else {
        let a = command["origin"]
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or("Invalid paste origin")?;
        let coordinate = |v: &Value| {
            v.as_i64()
                .filter(|n| (-100000..=100000).contains(n))
                .map(|n| n as i32)
                .ok_or("Invalid paste origin")
        };
        [coordinate(&a[0])?, coordinate(&a[1])?]
    };
    pasted.x = origin[0];
    pasted.y = origin[1];
    let mut draft = doc.clone();
    if depth == 16 && draft.bit_depth == 8 {
        draft.bit_depth = 16;
        draft.ensure_depth();
    }
    if wrap {
        let selected = selected.as_ref().unwrap();
        let index = index.unwrap();
        let mut folder = Layer::new(
            &format!("{} group", selected.name),
            "group",
            doc.width,
            doc.height,
        );
        folder.pixels = Raster::new_depth(doc.width, doc.height, depth);
        // Retain the source interaction with the backdrop when introducing a group.
        folder.blend = selected.blend.clone();
        folder.parent = selected.parent.clone();
        draft.layers[index].blend = "normal".into();
        pasted.parent = Some(folder.id.clone());
        draft.layers[index].parent = Some(folder.id.clone());
        draft.layers.insert(index, folder);
        draft.layers.insert(index + 1, pasted);
    } else {
        pasted.parent = selected.as_ref().and_then(|l| {
            if l.kind == "group" {
                Some(l.id.clone())
            } else {
                l.parent.clone()
            }
        });
        let at =
            index.unwrap_or(0) + usize::from(selected.as_ref().is_some_and(|l| l.kind == "group"));
        draft.layers.insert(at, pasted);
    }
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
pub struct Worker {
    tx: mpsc::SyncSender<Request>,
    pub replies: mpsc::Receiver<Reply>,
}
impl Default for Worker {
    fn default() -> Self {
        Self::new(eframe::egui::Context::default())
    }
}
impl Worker {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(1);
        let (reply_tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            // Retain ownership for Linux clipboard providers while the application is open.
            let mut clipboard = arboard::Clipboard::new().ok();
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
                let document = request.document().to_owned();
                if clipboard.is_none() {
                    clipboard = arboard::Clipboard::new().ok();
                }
                let reply = if let Some(clipboard) = clipboard.as_mut() {
                    session.process(clipboard, request)
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
        self.tx
            .try_send(request)
            .map_err(|_| "Finish the current clipboard operation first".into())
    }
}

// Native transport is replaceable in tests; safety tests never alter the real clipboard.
trait Provider {
    fn write_image(&mut self, image: &Image) -> Result<(), String>;
    fn read_image(&mut self) -> Result<Image, String>;
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
    fn write_image(&mut self, image: &Image) -> Result<(), String> {
        #[cfg(windows)]
        return windows::write_image(image);
        #[cfg(not(windows))]
        self.set_image(arboard::ImageData {
            width: image.width as usize,
            height: image.height as usize,
            bytes: Cow::Borrowed(&image.bytes),
        })
        .map_err(|e| e.to_string())
    }
    fn read_image(&mut self) -> Result<Image, String> {
        #[cfg(windows)]
        if let Some(image) = windows::png()? {
            return Ok(image);
        }
        let pixels = self
            .get_image()
            .map_err(|_| "Copy an image, image file or layers before pasting".to_owned());
        #[cfg(windows)]
        let pixels = match pixels {
            Ok(pixels) => pixels,
            Err(error) => return windows::dib()?.ok_or(error),
        };
        #[cfg(not(windows))]
        let pixels = pixels?;
        let width = u32::try_from(pixels.width).map_err(|_| "Clipboard image is too large")?;
        let height = u32::try_from(pixels.height).map_err(|_| "Clipboard image is too large")?;
        check_size(width, height)?;
        Ok(Image {
            width,
            height,
            bytes: pixels.bytes.into_owned(),
            samples16: None,
            origin: None,
        })
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
        #[cfg(windows)]
        {
            match self.get().file_list() {
            Ok(files)=>Ok(files),
            Err(arboard::Error::ContentNotAvailable)=>Ok(vec![]),
            Err(e)=>Err(format!("Cannot read clipboard files: {e}. Retry after the other clipboard operation finishes")),
        }
        }
        #[cfg(not(windows))]
        {
            Ok(vec![])
        }
    }
    fn sequence(&mut self) -> Option<u64> {
        #[cfg(windows)]
        {
            clipboard_win::seq_num().map(|s| s.get() as u64)
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}
struct LayerBuffer {
    marker: String,
    layers: crate::layer_clipboard::Layers,
}
#[derive(Default)]
struct Session {
    image: Option<Image>,
    image_sequence: Option<u64>,
    layers: Option<LayerBuffer>,
}
impl Session {
    fn copy_layers(
        &mut self,
        clipboard: &mut dyn Provider,
        doc: Document,
        ids: Vec<String>,
        cut: Option<Shared>,
    ) -> Result<String, String> {
        let layers = crate::layer_clipboard::copy(&doc, &ids)?;
        if cut.is_some() {
            crate::layer_clipboard::cut_commands(&doc, &ids)?;
        }
        let marker = format!("peerbrush://layers/{}", uuid::Uuid::new_v4());
        // Never delete source work unless ownership of the marker was successfully established.
        clipboard.write_text(&marker)?;
        self.layers = Some(LayerBuffer { marker, layers });
        self.image = None;
        if let Some(shared) = cut {
            let mut engine = shared.lock().unwrap();
            if engine.doc.id != doc.id || engine.doc.revision != doc.revision {
                return Err("Canvas changed before cut. Layers were copied; original layers remain unchanged".into());
            }
            let commands = crate::layer_clipboard::cut_commands(&engine.doc, &ids)?;
            engine.edit("human", &commands, Some(doc.revision), None, "Cut layers")?;
            Ok("Layers cut; paste to place them again".into())
        } else {
            Ok("Layers copied".into())
        }
    }
    fn process(&mut self, clipboard: &mut dyn Provider, request: Request) -> Reply {
        let document = request.document().to_owned();
        let mut selected = None;
        let mut selected_layers = vec![];
        let result = (|| match request {
            Request::Copy {
                doc,
                target,
                mask,
                merged,
            } => {
                let image = Image::copy(&doc, &target, mask, merged)?;
                clipboard.write_image(&image)?;
                self.image_sequence = clipboard.sequence();
                self.image = Some(image);
                self.layers = None;
                Ok("Selection copied".into())
            }
            Request::CopyLayers { doc, ids } => self.copy_layers(clipboard, doc, ids, None),
            Request::Cut {
                shared,
                doc,
                target,
                mask,
                step,
            } => {
                let command = json!({"op":"paint.clear_selection","layer":target,"mask":mask,"step":step,"document_id":doc.id,"source_revision":doc.revision});
                crate::engine::Engine::preview_edits(doc.clone(), &[command.clone()])?;
                let image = Image::copy(&doc, &target, mask, false)?;
                // Establish the clipboard copy before clearing any source pixels.
                clipboard.write_image(&image)?;
                self.image_sequence = clipboard.sequence();
                self.image = Some(image);
                self.layers = None;
                let mut engine = shared.lock().unwrap();
                crate::workspace::guard(&engine, &doc.id, doc.revision).map_err(|_| "Canvas changed before cut. Selection was copied; original pixels remain unchanged")?;
                engine.edit(
                    "human",
                    &[command],
                    Some(doc.revision),
                    None,
                    "Cut selected pixels",
                )?;
                Ok("Selected pixels cut; paste to place them again".into())
            }
            Request::CutLayers { shared, doc, ids } => {
                self.copy_layers(clipboard, doc, ids, Some(shared))
            }
            Request::Paste {
                shared,
                document,
                target,
                revision,
            } => {
                {
                    let engine = shared.lock().unwrap();
                    crate::workspace::guard(&engine, &document, revision)?;
                }
                let sequence = clipboard.sequence();
                let text = clipboard.read_text().ok();
                if let Some(buffer) = self
                    .layers
                    .as_ref()
                    .filter(|buffer| text.as_deref() == Some(buffer.marker.as_str()))
                {
                    if clipboard.sequence() != sequence {
                        return Err("Clipboard changed while preparing paste; try again".into());
                    }
                    let mut engine = shared.lock().unwrap();
                    crate::workspace::guard(&engine, &document, revision)?;
                    let result = engine.paste_layers(
                        "human",
                        &buffer.layers,
                        &target,
                        Some(revision),
                        None,
                    )?;
                    selected_layers = result["created_roots"]
                        .as_array()
                        .map(|ids| {
                            ids.iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    selected = selected_layers.first().cloned();
                    return Ok("Pasted editable layers".into());
                }
                let files = clipboard.read_files()?;
                let mut images = vec![];
                if files.is_empty() {
                    let mut image = clipboard.read_image()?;
                    if let Some(retained) = self.image.as_ref().filter(|old| {
                        self.image_sequence == sequence
                            && old.width == image.width
                            && old.height == image.height
                            && old.bytes == image.bytes
                    }) {
                        image.origin = retained.origin;
                        if retained.samples16.is_some() {
                            image.samples16 = retained.samples16.clone();
                        }
                    }
                    images.push(image);
                } else {
                    if files.len() > 16 {
                        return Err("Paste up to 16 image files at a time".into());
                    }
                    let mut budget = 0usize;
                    for file in files {
                        let image = Image::file(&file, 256 * 1024 * 1024 - budget)?;
                        budget = budget.saturating_add(image.bytes.len()).saturating_add(
                            image.samples16.as_ref().map_or(0, |words| words.len() * 2),
                        );
                        if budget > 256 * 1024 * 1024 {
                            return Err("Clipboard images exceed the 256 MiB paste budget; paste fewer files".into());
                        }
                        images.push(image);
                    }
                }
                if clipboard.sequence() != sequence {
                    return Err("Clipboard changed while preparing paste; try again".into());
                }
                let commands = images
                    .iter()
                    .map(|image| image.command(&target))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut engine = shared.lock().unwrap();
                crate::workspace::guard(&engine, &document, revision)?;
                let result =
                    engine.edit("human", &commands, Some(revision), None, "Paste images")?;
                let pasted = result["created"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(Value::as_str)
                            .filter(|id| {
                                engine
                                    .doc
                                    .layers
                                    .iter()
                                    .any(|l| l.id == **id && l.kind == "paint")
                            })
                            .map(String::from)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                selected = pasted.first().cloned();
                if images.len() > 1 {
                    selected_layers = pasted;
                }
                Ok(if images.len() == 1 {
                    "Pasted as a new layer".into()
                } else {
                    format!("Pasted {} images as new layers", images.len())
                })
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
    struct Fake {
        text: Option<String>,
        image: Option<Image>,
        fail_write: bool,
        files: Vec<PathBuf>,
        sequence: Option<u64>,
        change_on_read: bool,
        edit_on_write: Option<(Shared, Value)>,
    }
    impl Provider for Fake {
        fn write_image(&mut self, image: &Image) -> Result<(), String> {
            if self.fail_write {
                return Err("clipboard write failed".into());
            }
            self.image = Some(image.clone());
            self.text = None;
            self.files.clear();
            self.sequence = self.sequence.map(|n| n + 1);
            if let Some((shared, command)) = self.edit_on_write.take() {
                shared
                    .lock()
                    .unwrap()
                    .edit("human", &[command], None, None, "Concurrent work")?;
            }
            Ok(())
        }
        fn read_image(&mut self) -> Result<Image, String> {
            if self.change_on_read {
                self.sequence = self.sequence.map(|n| n + 1);
            }
            self.image.clone().ok_or_else(|| "no OS image".into())
        }
        fn write_text(&mut self, text: &str) -> Result<(), String> {
            if self.fail_write {
                return Err("clipboard write failed".into());
            }
            self.text = Some(text.into());
            self.image = None;
            self.files.clear();
            self.sequence = self.sequence.map(|n| n + 1);
            Ok(())
        }
        fn read_text(&mut self) -> Result<String, String> {
            self.text.clone().ok_or_else(|| "no OS text".into())
        }
        fn read_files(&mut self) -> Result<Vec<PathBuf>, String> {
            Ok(self.files.clone())
        }
        fn sequence(&mut self) -> Option<u64> {
            self.sequence
        }
    }
    fn fixture() -> (Shared, Document, String) {
        let mut e = Engine::new();
        e.doc = Document::new(8, 8).unwrap();
        let doc = e.doc.clone();
        let id = doc.layers[0].id.clone();
        (Arc::new(Mutex::new(e)), doc, id)
    }
    #[test]
    fn selected_cut_paste_retains_coordinates_native_depth_and_separate_undo_steps() {
        for depth in [8, 16] {
            let (shared, _, id) = fixture();
            let doc = {
                let mut e = shared.lock().unwrap();
                e.doc = Document::new_depth(8, 8, depth).unwrap();
                e.doc.layers[0].id = id.clone();
                for y in 0..8 {
                    for x in 0..8 {
                        e.doc.layers[0]
                            .pixels
                            .set16(x, y, [12347, 33559, 51237, 65535]);
                    }
                }
                e.doc.selection = Some([2, 3, 5, 7]);
                e.doc.clone()
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
                assert_eq!(e.doc.layers.len(), 1);
                assert_eq!(e.doc.layers[0].pixels.get16(2, 3), [0; 4]);
                assert_eq!(
                    e.doc.layers[0].pixels.get16(1, 3),
                    doc.layers[0].pixels.get16(1, 3)
                );
                assert_eq!(e.undo.len(), 1);
            }
            assert_eq!(fake.image.as_ref().unwrap().origin, Some([2, 3]));
            let revision = shared.lock().unwrap().doc.revision;
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
                .doc
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
            assert_eq!(e.doc.bit_depth, depth);
            assert_eq!(e.undo.len(), 2);
            e.undo("human").unwrap();
            assert_eq!(e.doc.layers.len(), 1);
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), doc.export_png().unwrap());
        }
    }
    #[test]
    fn selected_cut_failed_clipboard_or_concurrent_edit_never_erases_source_work() {
        for concurrent in [false, true] {
            let (shared, mut doc, id) = fixture();
            doc.selection = Some([2, 3, 5, 7]);
            doc.layers[0].pixels.set(2, 3, [21, 43, 65, 255]);
            shared.lock().unwrap().doc = doc.clone();
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
                e.doc.layers[0].pixels.rgba16(),
                doc.layers[0].pixels.rgba16()
            );
            assert_eq!(e.doc.layers.len(), 1);
            assert_eq!(e.undo.len(), usize::from(concurrent));
            if concurrent {
                assert_eq!(e.doc.layers[0].name, "New human work");
                assert!(session.image.is_some());
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
        assert_eq!(shared.lock().unwrap().doc.layers.len(), 1);
        assert!(shared.lock().unwrap().undo.is_empty());
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
                    e.doc = Document::new(8, 8).unwrap();
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
            let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
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
                serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
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
        let document = shared.lock().unwrap().doc.id.clone();
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
        let revision = shared.lock().unwrap().doc.revision;
        let document = shared.lock().unwrap().doc.id.clone();
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
            e.doc
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
        assert!(shared.lock().unwrap().doc.layers.is_empty());
        let document = shared.lock().unwrap().doc.id.clone();
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
        assert_eq!(e.doc.layers.len(), 1);
        assert_eq!(e.undo.len(), 2);
        e.undo("human").unwrap();
        assert!(e.doc.layers.is_empty());
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers.len(), 1);
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
        shared.lock().unwrap().doc = doc.clone();
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
        let document = shared.lock().unwrap().doc.id.clone();
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
            .doc
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
        assert!(e.undo.is_empty());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
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
        assert_eq!(reply.selected_layers.len(), 2);
        let mut e = shared.lock().unwrap();
        assert_eq!(e.undo.len(), 1);
        for id in reply.selected_layers {
            let l = e.doc.layers.iter().find(|l| l.id == id).unwrap();
            assert_eq!((l.pixels.width, l.pixels.height), (2, 2));
            let color = l.pixels.get(0, 0);
            for (actual, expected) in color.into_iter().zip([23u8, 45, 67, 255]) {
                assert!(actual.abs_diff(expected) <= 2);
            }
        }
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers.len(), 1);
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
        assert!(e.undo.is_empty());
        assert_eq!(
            serde_json::to_value(&e.doc).unwrap(),
            serde_json::to_value(&doc).unwrap()
        );
        assert!(Image::file(std::path::Path::new("relative.png"), 1024).is_err());
    }
}
