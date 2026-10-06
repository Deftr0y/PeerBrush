//! Selection pixels and OS clipboard transport. Editing stays in the shared engine.
use crate::{
    engine::{Document, Layer},
    raster::{check_size, png, Raster},
    server::Shared,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{borrow::Cow, sync::mpsc};

#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub origin: Option<[i32; 2]>,
}
impl Image {
    pub fn copy(doc: &Document, target: &str, mask: bool, merged: bool) -> Result<Self, String> {
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
        if let Some(polygon) = crate::selection::polygon(doc) {
            for y in 0..height {
                for x in 0..width {
                    if !crate::selection::contains(
                        polygon,
                        (area[0] + x as i32) as f32 + 0.5,
                        (area[1] + y as i32) as f32 + 0.5,
                    ) {
                        let at = ((y * width + x) * 4) as usize;
                        bytes[at..at + 4].fill(0);
                    }
                }
            }
        }
        Ok(Self {
            width,
            height,
            bytes,
            origin: Some([area[0], area[1]]),
        })
    }
    pub fn command(&self, context: &str) -> Result<Value, String> {
        check_size(self.width, self.height)?;
        Ok(
            json!({"op":"image.paste","layer":context,"png":STANDARD.encode(png(self.width, self.height, &self.bytes)?),"origin":self.origin}),
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
    let context = command["layer"]
        .as_str()
        .ok_or("Choose a layer or folder before pasting")?;
    let index = doc
        .layers
        .iter()
        .position(|l| l.id == context)
        .ok_or("Paste destination no longer exists")?;
    let selected = &doc.layers[index];
    let mut parent = Some(selected.id.as_str());
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
    let wrap = selected.kind != "group" && selected.parent.is_none();
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
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(encoded), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
    check_size(image.width(), image.height())?;
    let mut pasted = Layer::new("Pasted image", "paint", image.width(), image.height());
    pasted.pixels = Raster::from_rgba(image.width(), image.height(), image.as_raw())?;
    let origin = if command["origin"].is_null() {
        [
            (doc.width as i32 - image.width() as i32) / 2,
            (doc.height as i32 - image.height() as i32) / 2,
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
    if wrap {
        let mut folder = Layer::new(
            &format!("{} group", selected.name),
            "group",
            doc.width,
            doc.height,
        );
        // Retain the source interaction with the backdrop when introducing a group.
        folder.blend = selected.blend.clone();
        folder.parent = selected.parent.clone();
        doc.layers[index].blend = "normal".into();
        pasted.parent = Some(folder.id.clone());
        doc.layers[index].parent = Some(folder.id.clone());
        doc.layers.insert(index, folder);
        doc.layers.insert(index + 1, pasted);
    } else {
        pasted.parent = if selected.kind == "group" {
            Some(selected.id.clone())
        } else {
            selected.parent.clone()
        };
        let at = index + usize::from(selected.kind == "group");
        doc.layers.insert(at, pasted);
    }
    doc.selection = None;
    doc.selection_polygon = None;
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
    CutLayers {
        shared: Shared,
        doc: Document,
        ids: Vec<String>,
    },
    Paste {
        shared: Shared,
        target: String,
        revision: u64,
    },
}
pub struct Reply {
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
        let (tx, rx) = mpsc::sync_channel(1);
        let (reply_tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            // Retain ownership for Linux clipboard providers while the application is open.
            let mut clipboard = arboard::Clipboard::new().ok();
            let mut session = Session::default();
            while let Ok(request) = rx.recv() {
                if clipboard.is_none() {
                    clipboard = arboard::Clipboard::new().ok();
                }
                let reply = if let Some(clipboard) = clipboard.as_mut() {
                    session.process(clipboard, request)
                } else {
                    Reply { result: Err("The system clipboard is unavailable. Retry after closing other clipboard operations".into()), selected: None, selected_layers: vec![] }
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
}
impl Provider for arboard::Clipboard {
    fn write_image(&mut self, image: &Image) -> Result<(), String> {
        self.set_image(arboard::ImageData {
            width: image.width as usize,
            height: image.height as usize,
            bytes: Cow::Borrowed(&image.bytes),
        })
        .map_err(|e| e.to_string())
    }
    fn read_image(&mut self) -> Result<Image, String> {
        let pixels = self
            .get_image()
            .map_err(|_| "Copy an image or layers before pasting".to_owned())?;
        let width = u32::try_from(pixels.width).map_err(|_| "Clipboard image is too large")?;
        let height = u32::try_from(pixels.height).map_err(|_| "Clipboard image is too large")?;
        check_size(width, height)?;
        Ok(Image {
            width,
            height,
            bytes: pixels.bytes.into_owned(),
            origin: None,
        })
    }
    fn write_text(&mut self, text: &str) -> Result<(), String> {
        self.set_text(text).map_err(|e| e.to_string())
    }
    fn read_text(&mut self) -> Result<String, String> {
        self.get_text().map_err(|e| e.to_string())
    }
}
struct LayerBuffer {
    marker: String,
    layers: crate::layer_clipboard::Layers,
}
#[derive(Default)]
struct Session {
    image: Option<Image>,
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
                self.image = Some(image);
                self.layers = None;
                Ok("Selection copied".into())
            }
            Request::CopyLayers { doc, ids } => self.copy_layers(clipboard, doc, ids, None),
            Request::CutLayers { shared, doc, ids } => {
                self.copy_layers(clipboard, doc, ids, Some(shared))
            }
            Request::Paste {
                shared,
                target,
                revision,
            } => {
                let text = clipboard.read_text().ok();
                if let Some(buffer) = self
                    .layers
                    .as_ref()
                    .filter(|buffer| text.as_deref() == Some(buffer.marker.as_str()))
                {
                    let mut engine = shared.lock().unwrap();
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
                let mut image = clipboard.read_image()?;
                image.origin = self
                    .image
                    .as_ref()
                    .filter(|old| {
                        old.width == image.width
                            && old.height == image.height
                            && old.bytes == image.bytes
                    })
                    .and_then(|old| old.origin);
                let command = image.command(&target)?;
                let mut engine = shared.lock().unwrap();
                let result =
                    engine.edit("human", &[command], Some(revision), None, "Paste image")?;
                selected = result["created"]
                    .as_array()
                    .and_then(|ids| {
                        ids.iter().filter_map(Value::as_str).find(|id| {
                            engine
                                .doc
                                .layers
                                .iter()
                                .any(|l| l.id == *id && l.kind == "paint")
                        })
                    })
                    .map(String::from);
                Ok("Pasted as a new layer".into())
            }
        })();
        Reply {
            result,
            selected,
            selected_layers,
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
    }
    impl Provider for Fake {
        fn write_image(&mut self, image: &Image) -> Result<(), String> {
            if self.fail_write {
                return Err("clipboard write failed".into());
            }
            self.image = Some(image.clone());
            self.text = None;
            Ok(())
        }
        fn read_image(&mut self) -> Result<Image, String> {
            self.image.clone().ok_or_else(|| "no OS image".into())
        }
        fn write_text(&mut self, text: &str) -> Result<(), String> {
            if self.fail_write {
                return Err("clipboard write failed".into());
            }
            self.text = Some(text.into());
            self.image = None;
            Ok(())
        }
        fn read_text(&mut self) -> Result<String, String> {
            self.text.clone().ok_or_else(|| "no OS text".into())
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
        let pasted = session.process(
            &mut fake,
            Request::Paste {
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
            origin: None,
        });
        let revision = shared.lock().unwrap().doc.revision;
        let pasted = session.process(
            &mut fake,
            Request::Paste {
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
        let pasted = session.process(
            &mut fake,
            Request::Paste {
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
}
