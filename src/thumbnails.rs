//! A bounded background queue keeps effect evaluation off the UI thread.
use crate::engine::Document;
use std::{
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
};

pub struct Request {
    pub key: String,
    pub document: Document,
    pub layer: String,
    pub mask: bool,
    pub step: Option<usize>,
}
pub struct Reply {
    pub key: String,
    pub document: String,
    pub revision: u64,
    pub pixels: Option<(u32, u32, Vec<u8>)>,
}
pub struct Worker {
    sender: mpsc::SyncSender<Request>,
    pub replies: mpsc::Receiver<Reply>,
    stamp: Arc<AtomicU64>,
}
fn stamp(document: &Document) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    document.id.hash(&mut hasher);
    document.revision.hash(&mut hasher);
    hasher.finish()
}
impl Worker {
    pub fn new(context: eframe::egui::Context) -> Self {
        let (sender, requests) = mpsc::sync_channel::<Request>(32);
        let (completed, replies) = mpsc::channel();
        let current = Arc::new(AtomicU64::new(0));
        let worker_stamp = current.clone();
        std::thread::spawn(move || {
            while let Ok(request) = requests.recv() {
                let requested_stamp = stamp(&request.document);
                let result = if requested_stamp == worker_stamp.load(Ordering::Relaxed) {
                    if let Some(index) = request.step {
                        request
                            .document
                            .layers
                            .iter()
                            .find(|l| l.id == request.layer)
                            .ok_or("Layer was removed".to_string())
                            .and_then(|l| crate::mask::step_preview(l, index, 48))
                    } else {
                        request
                            .document
                            .preview(None, 48, Some(&request.layer), request.mask)
                            .map(|(w, h, p, _)| (w, h, p))
                    }
                    .ok()
                } else {
                    None
                };
                let pixels = if requested_stamp == worker_stamp.load(Ordering::Relaxed) {
                    result
                } else {
                    None
                };
                if completed
                    .send(Reply {
                        key: request.key,
                        document: request.document.id,
                        revision: request.document.revision,
                        pixels,
                    })
                    .is_err()
                {
                    break;
                }
                context.request_repaint();
            }
        });
        Self {
            sender,
            replies,
            stamp: current,
        }
    }
    pub fn document(&self, document: &Document) {
        self.stamp.store(stamp(document), Ordering::Relaxed);
    }
    pub fn request(&self, request: Request) -> bool {
        self.sender.try_send(request).is_ok()
    }
}
