//! Capture, source admission, candidate rendering and upload conversion all run
//! on the bounded preview worker. Replies retain display owners only.
use super::{native_texture, preview_worker::Worker, INVENTORY_RETRY_DELAY};
use crate::engine::scene::presentation::Presentation;
use native_preview::crossfade::{Baseline, Blend};
mod cache;
mod proposal;
use crate::{
    engine::{
        native::{DocumentKey, NativeDocument},
        view,
    },
    psd::memory36::{Admitted, MetadataCharge},
    render::native_preview::{self, Frame, Origin, Request},
    server::Shared,
    workspace::native_dispatch::{retryable_source_read, try_source_engine},
};
use eframe::egui;
use serde_json::Value;
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Instant;
type Result<T> = std::result::Result<T, String>;

/// Immutable editor command: hash once when its admitted value changes, then
/// share both through repeated canvas captures without copying the payload.
#[derive(Clone)]
pub(super) struct SharedCommand {
    value: Admitted<Value>,
    signature: [u8; 32],
}
impl SharedCommand {
    pub(super) fn new(value: Admitted<Value>) -> Result<Self> {
        let signature = fingerprint(&*value, 1, None, false, None)?;
        Ok(Self { value, signature })
    }
    pub(super) fn value(&self) -> &Admitted<Value> {
        &self.value
    }
    pub(super) fn signature(&self) -> [u8; 32] {
        self.signature
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LiveGesture {
    Stroke([u8; 32]),
    Parameter([u8; 32]),
    Transform([u8; 32]),
    Crop([u8; 32]),
    Primitive([u8; 32]),
    Selection([u8; 32]),
    Refinement([u8; 32]),
    Filter([u8; 32]),
    CanvasText([u8; 32]),
    Pen([u8; 32]),
    LayerReorder([u8; 32]),
    LayerVisibility([u8; 32]),
    BlendHover([u8; 32]),
}

struct Stamp {
    source: DocumentKey,
    project: String,
    document: String,
    revision: u64,
    signature: [u8; 32],
    has_commands: bool,
    edge: u32,
    rect: Option<[i32; 4]>,
    presentation: Option<[u8; 32]>,
    blend: bool,
    streamed_before: Option<native_preview::Tag>,
    target: Option<String>,
    mask: bool,
    selection_view: Option<String>,
    gesture: Option<LiveGesture>,
    proposal: Option<proposal::Review>,
    _charge: Arc<MetadataCharge>,
}
impl Stamp {
    fn matches(&self, project: &str, document: &NativeDocument) -> bool {
        self.project == project
            && self.source == document.key()
            && self.document == document.metadata().id()
            && self.revision == document.metadata().revision()
    }
    fn same_request(&self, other: &Self) -> bool {
        self.source == other.source
            && self.project == other.project
            && self.document == other.document
            && self.revision == other.revision
            && self.signature == other.signature
            && self.gesture == other.gesture
            && self.presentation == other.presentation
            && self.blend == other.blend
            && match (&self.proposal, &other.proposal) {
                (None, None) => true,
                (Some(left), Some(right)) => left.approval.same(&right.approval),
                _ => false,
            }
    }
    fn same_display_source(&self, other: &Self) -> bool {
        self.source == other.source
            && self.project == other.project
            && self.document == other.document
            && self.revision == other.revision
            && self.target == other.target
            && self.mask == other.mask
            && self.selection_view == other.selection_view
    }
    fn same_view(&self, other: &Self) -> bool {
        self.same_display_source(other) && self.edge == other.edge && self.rect == other.rect
    }
}
pub(super) struct Input {
    stamp: Arc<Stamp>,
    commands: Admitted<Vec<Value>>,
    source: Option<(usize, SharedCommand)>,
    presentation: Option<Presentation>,
    blend: Option<(Blend, Arc<Baseline>, u64)>,
    streamed_blend: Option<(Blend, NativeDocument, native_preview::Tag)>,
    cache: bool,
}

/// Keep the exact borrowed command list and its hashed JSON prefix together
/// until either the request matches the current canvas or we admit its copy.
pub(super) struct CommandCapture<'a> {
    commands: &'a [&'a Value],
    source: Option<(usize, &'a SharedCommand)>,
    target: Option<&'a str>,
    mask: bool,
    selection_view: Option<&'a str>,
    hash: CommandHash,
}
impl<'a> CommandCapture<'a> {
    pub(super) fn new(
        commands: &'a [&'a Value],
        source: Option<(usize, &'a SharedCommand)>,
        target: Option<&'a str>,
        mask: bool,
        selection_view: Option<&'a str>,
    ) -> Result<Self> {
        Ok(Self {
            commands,
            source,
            target,
            mask,
            selection_view,
            hash: command_hash(commands, source)?,
        })
    }
    pub(super) fn signature(&self, edge: u32) -> Result<[u8; 32]> {
        self.hash.signature(edge, self.target, self.mask, self.selection_view)
    }
    pub(super) fn admit(self, document: &NativeDocument, project: &str, edge: u32) -> Result<Input> {
        let signature = self.signature(edge)?;
        let commands = document.copy_commands_metadata(self.commands, &|| Ok(()))?;
        Input::from_admitted(
            document,
            project,
            commands,
            self.source.map(|(at, source)| (at, source.clone())),
            edge,
            self.target,
            self.mask,
            self.selection_view,
            Some(signature),
        )
    }
}

impl Input {
    pub(super) fn new(
        document: &NativeDocument,
        project: &str,
        commands: Admitted<Vec<Value>>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
    ) -> Result<Self> {
        Self::with_source(
            document,
            project,
            commands,
            None,
            edge,
            target,
            mask,
            selection_view,
        )
    }
    pub(super) fn with_source(
        document: &NativeDocument,
        project: &str,
        commands: Admitted<Vec<Value>>,
        source: Option<(usize, SharedCommand)>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
    ) -> Result<Self> {
        Self::from_admitted(document, project, commands, source, edge, target, mask, selection_view, None)
    }
    fn from_admitted(
        document: &NativeDocument,
        project: &str,
        commands: Admitted<Vec<Value>>,
        source: Option<(usize, SharedCommand)>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
        captured_signature: Option<[u8; 32]>,
    ) -> Result<Self> {
        if commands.len() + usize::from(source.is_some()) > 100
            || !commands.owner().uses_budget(document.budget())
            || source.as_ref().is_some_and(|(at, source)| {
                *at > commands.len() || !source.value.owner().uses_budget(document.budget())
            })
            || !(1..=16000).contains(&edge)
            || selection_view.is_some_and(|mode| !["mask", "overlay", "cutout"].contains(&mode))
        {
            return Err("Native canvas request differs from its admitted command source".into());
        }
        let signature = match captured_signature {
            Some(signature) => signature,
            None => command_fingerprint(
                &*commands,
                source.as_ref().map(|(at, source)| (*at, source)),
                edge,
                target,
                mask,
                selection_view,
            )?,
        };
        let bytes = [
            project.len(),
            document.metadata().id().len(),
            target.map_or(0, str::len),
            selection_view.map_or(0, str::len),
        ]
        .into_iter()
        .try_fold(1024usize, |sum, length| {
            sum.checked_add(length)
                .ok_or("Native canvas tag size overflow")
        })?;
        let charge = MetadataCharge::reserve(document.budget(), bytes, &|| Ok(()))?;
        Ok(Self {
            stamp: Arc::new(Stamp {
                source: document.key(),
                project: project.into(),
                document: document.metadata().id().into(),
                revision: document.metadata().revision(),
                signature,
                has_commands: !commands.is_empty() || source.is_some(),
                edge,
                rect: None,
                presentation: None,
                blend: false,
                streamed_before: None,
                target: target.map(str::to_owned),
                mask,
                selection_view: selection_view.map(str::to_owned),
                gesture: None,
                proposal: None,
                _charge: charge,
            }),
            commands,
            source,
            presentation: None,
            blend: None,
            streamed_blend: None,
            cache: true,
        })
    }
    pub(super) fn with_gesture(mut self, gesture: Option<LiveGesture>) -> Result<Self> {
        Arc::get_mut(&mut self.stamp)
            .ok_or("Canvas gesture tag is already shared")?
            .gesture = gesture;
        Ok(self)
    }
    pub(super) fn with_region(
        mut self,
        document: &NativeDocument,
        rect: Option<[i32; 4]>,
    ) -> Result<Self> {
        if let Some(rect) = rect {
            let [width, height] = if let Some(review) = &self.stamp.proposal {
                review.canvas_size
            } else {
                super::geometry::canvas_size(
                    [document.metadata().width(), document.metadata().height()],
                    self.commands.iter(),
                    crate::raster::CanvasLimits::Native,
                )?
            };
            let stamp = Arc::get_mut(&mut self.stamp).ok_or("Canvas region tag is already shared")?;
            if stamp.source != document.key()
                || rect[0] < 0 || rect[1] < 0
                || rect[2] > width as i32
                || rect[3] > height as i32
                || rect[0] >= rect[2] || rect[1] >= rect[3]
            {
                return Err("Canvas region is outside its document".into());
            }
            stamp.signature = region_signature(stamp.signature, Some(rect));
            stamp.rect = Some(rect);
        }
        Ok(self)
    }
    pub(super) fn with_presentation(
        mut self,
        document: &NativeDocument,
        presentation: Option<Presentation>,
    ) -> Result<Self> {
        if self.presentation.is_some()
            || self.blend.is_some()
            || self.streamed_blend.is_some()
            || self.stamp.proposal.is_some()
            || self.stamp.source != document.key()
        {
            return Err("Animation request is already bound to another presentation".into());
        }
        if let Some(poses) = &presentation {
            if !poses.matches(document)
                || !self.commands.is_empty()
                || self.source.is_some()
                || self.stamp.selection_view.is_some()
            {
                return Err("Animation cannot replace an active edit preview".into());
            }
            let stamp =
                Arc::get_mut(&mut self.stamp).ok_or("Animation request tag is already shared")?;
            stamp.signature = presentation_signature(stamp.signature, Some(poses));
            stamp.presentation = Some(poses.signature());
        }
        self.presentation = presentation;
        Ok(self)
    }
    pub(super) fn with_blend(
        mut self,
        document: &NativeDocument,
        blend: Option<(Blend, Arc<Baseline>, u64)>,
    ) -> Result<Self> {
        if let Some((blend, before, _)) = &blend {
            if self.presentation.is_some()
                || self.blend.is_some()
                || self.streamed_blend.is_some()
                || self.stamp.proposal.is_some()
                || !blend.matches(document)
                || self.stamp.source != document.key()
                || !self.commands.is_empty()
                || self.source.is_some()
                || self.stamp.selection_view.is_some()
                || before.tag().key().identity.project_id != self.stamp.project
                || before.tag().key().target != self.stamp.target
                || before.tag().key().mask != self.stamp.mask
                || !before.tag().matches_region(self.stamp.rect)
                || !before.tag().matches_edge(self.stamp.edge)
            {
                return Err("Canvas fade differs from its admitted preview request".into());
            }
            let stamp =
                Arc::get_mut(&mut self.stamp).ok_or("Canvas fade request is already shared")?;
            stamp.signature = blend_signature(stamp.signature, Some(blend));
            stamp.presentation = Some(blend.signature());
            stamp.blend = true;
        }
        self.blend = blend;
        Ok(self)
    }
    pub(super) fn with_streamed_blend(
        mut self, document: &NativeDocument,
        blend: Option<(Blend, NativeDocument, native_preview::Tag)>,
    ) -> Result<Self> {
        if let Some((blend, before, shown)) = &blend {
            let key = shown.key();
            // The observed texture authenticates the view. The worker renders
            // both source snapshots at full detail on the requested grid.
            let [left, top, right, bottom] = key.rect;
            let full_edge = (right - left).max(bottom - top) as u32;
            if self.presentation.is_some() || self.blend.is_some() || self.streamed_blend.is_some()
                || self.stamp.proposal.is_some() || !blend.matches(document) || !blend.matches_before(before)
                || self.stamp.source != document.key() || !self.commands.is_empty() || self.source.is_some()
                || self.stamp.selection_view.is_some() || key.source != before.key()
                || key.identity.project_id != self.stamp.project || key.identity.document_id != before.metadata().id()
                || key.identity.revision != before.metadata().revision() || key.target != self.stamp.target
                || key.mask != self.stamp.mask || key.selection_view.is_some()
                || !shown.matches_region(self.stamp.rect) || self.stamp.edge < full_edge
            {
                return Err("Streamed fade differs from the observed before-frame".into());
            }
            let stamp = Arc::get_mut(&mut self.stamp).ok_or("Canvas fade request is already shared")?;
            stamp.signature = blend_signature(stamp.signature, Some(blend));
            stamp.presentation = Some(blend.signature());
            stamp.blend = true;
            stamp.streamed_before = Some(shown.clone());
        }
        self.streamed_blend = blend;
        Ok(self)
    }
    /// Only the worker assembles a contiguous command array for the shared
    /// engine. The UI retains the original admitted source value instead.
    fn assemble(
        &self,
        document: &NativeDocument,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Admitted<Vec<Value>>> {
        check()?;
        if self.stamp.source != document.key()
            || !self.commands.owner().uses_budget(document.budget())
        {
            return Err("Native command capture uses another admission pool".into());
        }
        let Some((at, source)) = &self.source else {
            return Ok(self.commands.clone());
        };
        if !source.value.owner().uses_budget(document.budget())
            || *at > self.commands.len()
            || self.commands.len() >= 100
        {
            return Err("Native source command placement differs from its capture".into());
        }
        let mut ordered = [&Value::Null; 100];
        for (index, command) in self.commands.iter().enumerate() {
            ordered[index + usize::from(index >= *at)] = command;
        }
        ordered[*at] = &source.value;
        document.copy_commands_metadata(&ordered[..self.commands.len() + 1], check)
    }
}
pub(super) fn region_signature(signature: [u8; 32], rect: Option<[i32; 4]>) -> [u8; 32] {
    let Some(rect) = rect else { return signature };
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"PeerBrush canvas region v1\0");
    hash.update(&signature);
    for coordinate in rect {
        hash.update(&coordinate.to_le_bytes());
    }
    hash.finish().as_ref().try_into().unwrap()
}
pub(super) fn presentation_signature(
    signature: [u8; 32],
    presentation: Option<&Presentation>,
) -> [u8; 32] {
    let Some(presentation) = presentation else {
        return signature;
    };
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"PeerBrush animated canvas v1\0");
    hash.update(&signature);
    hash.update(&presentation.signature());
    hash.finish().as_ref().try_into().unwrap()
}
pub(super) fn blend_signature(signature: [u8; 32], blend: Option<&Blend>) -> [u8; 32] {
    let Some(blend) = blend else {
        return signature;
    };
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"PeerBrush native canvas blend request v1\0");
    hash.update(&signature);
    hash.update(&blend.signature());
    hash.finish().as_ref().try_into().unwrap()
}
pub(super) fn command_fingerprint<T: serde::Serialize + ?Sized>(
    commands: &T,
    source: Option<(usize, &SharedCommand)>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
) -> Result<[u8; 32]> {
    command_hash(commands, source)?.signature(edge, target, mask, selection_view)
}
fn command_hash<T: serde::Serialize + ?Sized>(
    commands: &T,
    source: Option<(usize, &SharedCommand)>,
) -> Result<CommandHash> {
    match source {
        Some((at, source)) => CommandHash::new(&(commands, at, source.signature)),
        None => CommandHash::new(commands),
    }
}
pub(super) fn fingerprint<T: serde::Serialize + ?Sized>(
    commands: &T,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    selection_view: Option<&str>,
) -> Result<[u8; 32]> {
    CommandHash::new(commands)?.signature(edge, target, mask, selection_view)
}

#[derive(Clone)]
struct CommandHash {
    digest: ring::digest::Context,
    bytes: usize,
}
impl Write for CommandHash {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= crate::psd::canonical36::MAX_BYTES)
            .ok_or_else(|| io::Error::other("Native canvas request exceeds its metadata bound"))?;
        self.digest.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl CommandHash {
    fn new<T: serde::Serialize + ?Sized>(commands: &T) -> Result<Self> {
        let mut hash = Self {
            digest: ring::digest::Context::new(&ring::digest::SHA256),
            bytes: 0,
        };
        // Preserve the compact JSON tuple's bytes while hashing its expensive first field once.
        hash.write_all(b"[").map_err(|error| error.to_string())?;
        hash.json(commands)?;
        Ok(hash)
    }
    fn json<T: serde::Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        serde_json::to_writer(self, value)
            .map_err(|error| format!("Native canvas request fingerprint failed: {error}"))
    }
    fn field(&mut self, value: &impl serde::Serialize) -> Result<()> {
        self.write_all(b",").map_err(|error| error.to_string())?;
        self.json(value)
    }
    fn signature(
        &self,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        selection_view: Option<&str>,
    ) -> Result<[u8; 32]> {
        let mut hash = self.clone();
        hash.field(&edge)?;
        hash.field(&target)?;
        hash.field(&mask)?;
        hash.field(&selection_view)?;
        hash.write_all(b"]").map_err(|error| error.to_string())?;
        hash.digest.finish().as_ref().try_into().map_err(|_| "Invalid canvas fingerprint".into())
    }
}
struct Output {
    uploads: native_texture::Prepared,
    origin: Option<Arc<Origin>>,
    baseline: Option<Arc<Baseline>>,
    text: Option<TextPreview>,
}
struct TextPreview {
    command: [u8; 32],
    carets: Option<crate::source::text_position::NativeCarets>,
}
impl Output {
    fn validate(&self, stamp: &Stamp) -> Result<()> {
        let key = self.uploads.tag().key();
        let source_matches = if let Some(origin) = &self.origin {
            origin.matches_source(&stamp.project, stamp.source, stamp.revision)
                && origin.candidate_tag().key() == key
                && origin.presentation_signature() == stamp.presentation
        } else {
            stamp.presentation.is_none()
                && key.source == stamp.source
                && key.identity.project_id == stamp.project
                && key.identity.document_id == stamp.document
                && key.identity.revision == stamp.revision
        };
        let mode = match key.selection_view {
            Some(native_preview::SelectionView::Mask) => Some("mask"),
            Some(native_preview::SelectionView::Overlay) => Some("overlay"),
            Some(native_preview::SelectionView::Cutout) => Some("cutout"),
            None => None,
        };
        if !source_matches
            || stamp.proposal.as_ref().is_some_and(|review| {
                let [w, h] = review.canvas_size;
                self.origin.is_none()
                    || key.rect != stamp.rect.unwrap_or([0, 0, w as i32, h as i32])
            })
            || self.baseline.as_ref().is_some_and(|baseline| {
                self.origin.is_some()
                    || stamp.presentation.is_some()
                    || !baseline.tag().same_allocation(self.uploads.tag())
            })
            || key.target != stamp.target
            || !self.uploads.tag().matches_region(stamp.rect)
            || key.mask != stamp.mask
            || mode != stamp.selection_view.as_deref()
            || key.width.max(key.height) > stamp.edge
            || !self.uploads.tag().matches_edge(stamp.edge)
        {
            return Err("Native canvas output differs from its captured request".into());
        }
        Ok(())
    }
}
struct Reply {
    job: u64,
    stamp: Arc<Stamp>,
    result: Result<Output>,
}
#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Option<Reply>>>);
impl Replies {
    fn complete(&self, reply: Reply) {
        let old = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(reply);
        drop(old);
    }
    fn take(&self) -> Option<Reply> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}
#[derive(Default)]
pub(super) struct Dispatch {
    expected: Option<(u64, Arc<Stamp>, bool)>,
    retry_after: Option<Instant>,
    pub(super) gesture_deferred: bool,
    replies: Replies,
    retirement: Arc<Mutex<Option<native_texture::Retirement>>>,
    accepted: Option<Receipt>,
    skip_cache: Option<DocumentKey>,
    endpoint: cache::Endpoint,
}
/// Display-only proof: it owns admitted request tags, never a source session.
pub(super) struct Receipt {
    job: u64,
    stamp: Arc<Stamp>,
    tag: native_preview::Tag,
    baseline: Option<Arc<Baseline>>,
    text: Option<TextPreview>,
}
impl Receipt {
    pub(super) fn streamed_before(
        &self, project: &str, document: &NativeDocument, painted: &native_preview::Tag,
    ) -> Option<&native_preview::Tag> {
        (self.stamp.matches(project, document) && self.tag.same_allocation(painted))
            .then_some(self.stamp.streamed_before.as_ref()).flatten()
    }
}
impl Dispatch {
    pub(super) fn fade_baseline(
        &self,
        before: &NativeDocument,
        project: &str,
        target: Option<&str>,
        mask: bool,
    ) -> Option<(u32, Arc<Baseline>, u64)> {
        self.endpoint.fade_baseline(before, project, target, mask)
    }
    pub(super) fn clear_frames(&mut self) -> bool {
        self.endpoint.clear()
    }
    pub(super) fn observe_canvas(&mut self, painted: Option<&native_preview::Tag>) {
        self.endpoint.observe_canvas(painted);
    }
    pub(super) fn capture_ready(
        &self,
        receipt: &Receipt,
        project: &str,
        document: &NativeDocument,
        epoch: u64,
        painted: &native_preview::Tag,
    ) -> bool {
        receipt.job == epoch
            && self.retry_after.is_none()
            && receipt.stamp.matches(project, document)
            && receipt.stamp.edge == document.metadata().width().max(document.metadata().height()).max(8192)
            && receipt.stamp.presentation.is_none()
            && receipt.stamp.proposal.is_none()
            && receipt.stamp.gesture.is_none()
            && receipt.tag.same_allocation(painted)
            && self.expected.as_ref().is_some_and(|(job, stamp, pending)| {
                !pending && *job == epoch && stamp.same_request(&receipt.stamp)
            })
    }
    fn retry_due(&self) -> bool {
        self.retry_after
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
    pub(super) fn unchanged(&self, input: &Input, epoch: u64) -> bool {
        !self.retry_due()
            && self.expected.as_ref().is_some_and(|(job, stamp, _)| {
                *job == epoch && stamp.same_request(&input.stamp)
            })
    }
    pub(super) fn matches_capture(
        &self,
        document: &NativeDocument,
        project: &str,
        signature: [u8; 32],
        gesture: Option<LiveGesture>,
        epoch: u64,
    ) -> bool {
        !self.retry_due()
            && self.expected.as_ref().is_some_and(|(job, stamp, _)| {
                *job == epoch
                    && stamp.matches(project, document)
                    && stamp.signature == signature
                    && stamp.gesture == gesture
            })
    }
    pub(super) fn matches_base_capture(
        &self,
        document: &NativeDocument,
        project: &str,
        edge: u32,
        rect: Option<[i32; 4]>,
        target: Option<&str>,
        mask: bool,
        epoch: u64,
    ) -> bool {
        !self.retry_due()
            && self.expected.as_ref().is_some_and(|(job, stamp, _)| {
                *job == epoch && stamp.matches(project, document)
                    && !stamp.has_commands && stamp.edge == edge && stamp.rect == rect
                    && stamp.target.as_deref() == target && stamp.mask == mask
                    && stamp.selection_view.is_none() && stamp.gesture.is_none()
                    && stamp.presentation.is_none() && stamp.proposal.is_none()
            })
    }
    pub(super) fn pending_gesture(
        &self,
        document: &NativeDocument,
        project: &str,
        gesture: LiveGesture,
        edge: u32,
        rect: Option<[i32; 4]>,
        epoch: u64,
    ) -> bool {
        self.expected.as_ref().is_some_and(|(job, stamp, pending)| {
            *pending && *job == epoch && stamp.matches(project, document)
                && stamp.gesture == Some(gesture) && stamp.edge == edge && stamp.rect == rect
        })
    }
    pub(super) fn requested_region(
        &self, document: &NativeDocument, project: &str, epoch: u64,
    ) -> Option<[i32; 4]> {
        let (job, stamp, _) = self.expected.as_ref()?;
        if *job == epoch && stamp.matches(project, document) && stamp.proposal.is_none() {
            stamp.rect
        } else {
            None
        }
    }
    pub(super) fn allows_pan_margin(&self, document: &NativeDocument) -> bool {
        // The existing retry after a refused render also drops optional offscreen pixels.
        self.skip_cache != Some(document.key())
    }
    pub(super) fn pending(&self) -> bool {
        self.expected
            .as_ref()
            .is_some_and(|(_, _, pending)| *pending)
    }
    pub(super) fn request(
        &mut self,
        worker: &mut Worker,
        shared: Shared,
        input: Input,
        job: u64,
        epoch: Arc<AtomicU64>,
        side: usize,
        ctx: &egui::Context,
        reuse: Option<native_texture::Reuse>,
    ) -> Result<()> {
        let stamp = input.stamp.clone();
        let retirement = self.retirement.clone();
        let endpoint = self.endpoint.clone();
        endpoint.watch(stamp.source, &epoch);
        let display = ctx.clone();
        self.queue(worker, stamp, job, epoch, ctx, move |check| {
            render_with_context(&shared, input, &retirement, &endpoint, side, Some(&display), reuse.as_ref(), check)
        })
    }
    fn queue(
        &mut self,
        worker: &mut Worker,
        stamp: Arc<Stamp>,
        job: u64,
        epoch: Arc<AtomicU64>,
        ctx: &egui::Context,
        render: impl FnOnce(&dyn Fn() -> Result<()>) -> Result<Output> + Send + 'static,
    ) -> Result<()> {
        let replies = self.replies.clone();
        let failed = replies.clone();
        let captured = stamp.clone();
        let failed_stamp = stamp.clone();
        let ctx = ctx.clone();
        let failed_ctx = ctx.clone();
        worker
            .request(
                move || {
                    let check = || {
                        if epoch.load(Ordering::Relaxed) == job {
                            Ok(())
                        } else {
                            Err("Preview superseded".into())
                        }
                    };
                    let result = check().and_then(|_| render(&check)).and_then(|output| {
                        check()?;
                        output.validate(&captured)?;
                        Ok(output)
                    });
                    replies.complete(Reply {
                        job,
                        stamp: captured,
                        result,
                    });
                    ctx.request_repaint();
                },
                move || {
                    failed.complete(Reply {
                        job,
                        stamp: failed_stamp,
                        result: Err(
                            "Native canvas preview could not complete; current work preserved"
                                .into(),
                        ),
                    });
                    failed_ctx.request_repaint();
                },
            )
            .map_err(|_| "Native canvas preview worker could not start")?;
        self.expected = Some((job, stamp, true));
        self.retry_after = None;
        self.accepted = None;
        Ok(())
    }
    pub(super) fn take(
        &mut self,
        project: &str,
        document: &NativeDocument,
        epoch: u64,
    ) -> Result<Option<native_texture::Prepared>> {
        if self
            .expected
            .as_ref()
            .is_some_and(|(job, stamp, _)| *job != epoch || !stamp.matches(project, document))
        {
            self.clear();
            return Ok(None);
        }
        let Some(reply) = self.replies.take() else {
            return Ok(None);
        };
        let Some((job, stamp, pending)) = &mut self.expected else {
            return Ok(None);
        };
        if *job != epoch
            || reply.job != epoch
            || !stamp.same_request(&reply.stamp)
            || !stamp.matches(project, document)
        {
            return Ok(None);
        }
        *pending = false;
        self.accepted = None;
        let output = reply.result?;
        output.validate(stamp)?;
        self.accepted = Some(Receipt {
            job: *job,
            stamp: stamp.clone(),
            tag: output.uploads.tag().clone(),
            baseline: output.baseline,
            text: output.text,
        });
        Ok(Some(output.uploads))
    }
    pub(super) fn clear(&mut self) {
        self.clear_frames();
        self.expected = None;
        self.retry_after = None;
        self.gesture_deferred = false;
        self.accepted = None;
        // A cancelled worker may still complete against its old mailbox.
        self.replies = Replies::default();
        self.retirement
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}
#[cfg(test)]
fn render(
    shared: &Shared,
    input: Input,
    retirement: &Mutex<Option<native_texture::Retirement>>,
    endpoint: &cache::Endpoint,
    side: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<Output> {
    render_with_context(shared, input, retirement, endpoint, side, None, None, check)
}
fn render_with_context(
    shared: &Shared,
    mut input: Input,
    retirement: &Mutex<Option<native_texture::Retirement>>,
    endpoint: &cache::Endpoint,
    side: usize,
    context: Option<&egui::Context>,
    reuse: Option<&native_texture::Reuse>,
    check: &dyn Fn() -> Result<()>,
) -> Result<Output> {
    check()?;
    let stamp = &input.stamp;
    let (captured, document, damage) = {
        let engine = try_source_engine(shared)?;
        check()?;
        engine.ensure_open()?;
        let document = view::current(&engine)
            .native()
            .ok_or("Canvas source is no longer native")?;
        if !stamp.matches(&engine.project_id, document) {
            return Err("Canvas source changed before capture".into());
        }
        let captured =
            if input.commands.is_empty() && input.source.is_none() && stamp.proposal.is_none() {
                Some(engine.capture_scene(None)?)
            } else {
                None
            };
        (captured, document.clone(), engine.authority.native_raster_damage())
    };
    if input.cache {
        input.cache = endpoint.bind(document.budget(), stamp.source);
    }
    let generation = endpoint.generation();
    if input
        .blend
        .as_ref()
        .is_some_and(|(_, _, captured)| !input.cache || *captured != generation)
    {
        return Err("Canvas fade baseline was retired before rendering".into());
    }
    check()?;
    let commands = input.assemble(&document, check)?;
    let mut cached = None;
    let (frame, origin) = if let Some(review) = &stamp.proposal {
        let live = || {
            check()?;
            review.approval.check(shared)
        };
        live()?;
        let rendered = crate::collaboration::native::review_stored_with(
            shared, &review.id, stamp.rect, stamp.edge, None, false, None, &live,
            context.is_some(), |request, check| {
                let owner = upload_retirement(request.budget(), retirement)?;
                native_texture::Prepared::from_request(request, &owner, side, context, reuse, &mut || check())
            },
        )?;
        let (origin, uploads, supplement) = rendered.into_parts();
        drop(supplement);
        live()?;
        return Ok(Output { uploads, origin: Some(origin), baseline: None, text: None });
    } else if let Some(captured) = captured {
        let scene = Arc::new(captured.into_native()?);
        let session = crate::workspace::native_dispatch::begin_read_scene(shared, &scene)?;
        if let Some(presentation) = &input.presentation {
            let (uploads, origin) = native_preview::presentation::render_with(
                scene, session, presentation, stamp.rect, stamp.edge,
                stamp.target.as_deref(), stamp.mask, check, context.is_some(), |request, check| {
                    let owner = upload_retirement(request.budget(), retirement)?;
                    native_texture::Prepared::from_request(request, &owner, side, context, reuse, &mut || check())
                },
            )?;
            return Ok(Output { uploads, origin: Some(origin), baseline: None, text: None });
        } else {
            let request = Request::capture_selection(
                scene, session, stamp.rect, stamp.edge, stamp.target.as_deref(),
                stamp.mask, stamp.selection_view.as_deref(),
            )?;
            let request = if context.is_some() {
                request.for_canvas(stamp.edge)?.with_raster_damage(damage)?
            } else { request };
            if let Some((blend, before, _)) = &input.streamed_blend {
                let scene = Arc::new(crate::engine::scene::SceneSnapshot::from_native(
                    crate::engine::scene::SceneIdentity {
                        project_id: stamp.project.clone(), document_id: before.metadata().id().into(),
                        revision: before.metadata().revision(),
                    }, before.clone(), vec![], None, None,
                )?);
                // The current after-scene was validated above. This read is
                // intentionally against the owned, observed previous snapshot.
                let runtime = crate::workspace::native_dispatch::runtime_for(shared)?;
                let session = runtime.begin_read(&scene)?;
                let prior = Request::capture(scene, session, stamp.rect, stamp.edge, stamp.target.as_deref(), stamp.mask)?;
                let prior = if context.is_some() { prior.for_canvas(stamp.edge)? } else { prior };
                let streamed = blend.streamed(prior, request, check)?;
                let owner = upload_retirement(document.budget(), retirement)?;
                let uploads = native_texture::Prepared::from_request(&streamed, &owner, side, context, reuse, &mut || check())?;
                let origin = streamed.origin();
                drop(streamed);
                check()?;
                return Ok(Output { uploads, origin: Some(origin), baseline: None, text: None });
            }
            let oversized = u64::from(request.key().width) * u64::from(request.key().height)
                > native_preview::MAX_PREVIEW_PIXELS;
            if oversized {
                if input.blend.is_some() {
                    return Err("Full-detail canvas cannot retain a CPU fade baseline".into());
                }
                input.cache = false;
                endpoint.clear();
            }
            if let Some(pixels) = if input.cache { endpoint.get(stamp) } else { None } {
                let frame = pixels.frame(check)?;
                cached = Some(pixels);
                drop(request);
                (frame, None)
            } else {
                if !input.cache {
                    let owner = upload_retirement(request.budget(), retirement)?;
                    let uploads = native_texture::Prepared::from_canvas_request_reusing_tiles(&request, &owner, side, context, reuse, &mut || check())?;
                    check()?;
                    return Ok(Output { uploads, origin: None, baseline: None, text: None });
                }
                if input.blend.is_none()
                    && reuse.is_some_and(|prior| {
                        let old = prior.tag().key();
                        if old.source == stamp.source {
                            old.rect != request.key().rect
                        } else {
                            damage.is_some()
                        }
                    })
                {
                    // An overlapping full-detail crop can publish from the prior
                    // textures and newly rendered spans without a second CPU frame.
                    let owner = upload_retirement(request.budget(), retirement)?;
                    if let Some(uploads) = native_texture::Prepared::try_reusing_tiles(
                        &request, &owner, side, context, reuse, &mut || check(),
                    )? {
                        check()?;
                        return Ok(Output { uploads, origin: None, baseline: None, text: None });
                    }
                }
                let baseline = endpoint.pan_baseline(stamp);
                let frame = request.render_with_pan_baseline(baseline.as_deref(), &mut || check())?;
                drop(request);
                (frame, None)
            }
        }
    } else {
        let text_command = input.source.as_ref().and_then(|(_, source)| {
            let content = &source.value()["source"]["content"];
            (commands.len() == 1 && content["kind"] == "text"
                && (matches!(content["layout"].as_str(), Some("shaped" | "paragraph"))
                    || content["font"].is_object()))
                .then_some(source.signature())
        });
        let mut carets = None;
        let result = native_preview::preview_shared_with(
            shared, "human", &commands, Some(stamp.revision), Some(&stamp.document),
            stamp.rect, stamp.edge, stamp.target.as_deref(), stamp.mask,
            stamp.selection_view.as_deref(), check, context.is_some(),
            text_command.map(|_| &mut carets), |request, check| {
                let owner = upload_retirement(request.budget(), retirement)?;
                native_texture::Prepared::from_canvas_request_reusing_tiles(request, &owner, side, context, reuse, &mut || check())
            },
        )?;
        let (origin, uploads, source) = result.into_parts();
        drop(source);
        check()?;
        let text = text_command.map(|command| TextPreview { command, carets });
        return Ok(Output { uploads, origin: Some(origin), baseline: None, text });
    };
    let live = || {
        check()?;
        stamp.check_proposal(shared)
    };
    let output = finish_frame(
        &input, &document, frame, origin, cached, endpoint, generation, retirement, side,
        context.map(|context| (context, reuse)), &live,
    );
    drop((commands, input, document));
    output
}
fn finish_frame(
    input: &Input,
    document: &NativeDocument,
    frame: Frame,
    origin: Option<Arc<Origin>>,
    cached: Option<Arc<Baseline>>,
    endpoint: &cache::Endpoint,
    generation: u64,
    retirement: &Mutex<Option<native_texture::Retirement>>,
    side: usize,
    display: Option<(&egui::Context, Option<&native_texture::Reuse>)>,
    check: &dyn Fn() -> Result<()>,
) -> Result<Output> {
    check()?;
    let upload_owner = upload_retirement(frame.budget(), retirement)?;
    let pixels = if input.cache
        && origin.is_none()
        && input.commands.is_empty()
        && input.source.is_none()
        && input.stamp.selection_view.is_none()
    {
        match cached {
            Some(cached) => Some(cached),
            None => {
                let headroom = native_texture::Prepared::retained_upload_bytes(&frame, side, display.is_some())?;
                Baseline::try_capture_with_headroom(
                    &frame, document, &input.stamp.project, headroom, check,
                )?.map(Arc::new)
            }
        }
    } else {
        None
    };
    let (frame, origin) = if let Some((blend, before, captured)) = &input.blend {
        if *captured != endpoint.generation() {
            return Err("Canvas fade baseline was retired before blending".into());
        }
        if let Some(pixels) = &pixels {
            endpoint.store(
                generation,
                cache::Entry {
                    stamp: input.stamp.clone(),
                    pixels: pixels.clone(),
                },
                check,
            )?;
        }
        let (frame, origin) = blend.render(before, frame, check)?;
        (frame, Some(origin))
    } else {
        (frame, origin)
    };
    let baseline = origin.is_none().then_some(pixels).flatten();
    let mut output = upload_with_owner(frame, origin, &upload_owner, side, display, check)?;
    output.baseline = baseline;
    Ok(output)
}
#[cfg(test)]
fn upload(
    frame: Frame,
    origin: Option<Arc<Origin>>,
    retirement: &Mutex<Option<native_texture::Retirement>>,
    side: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<Output> {
    check()?;
    let owner = upload_retirement(frame.budget(), retirement)?;
    upload_with_owner(frame, origin, &owner, side, None, check)
}
fn upload_retirement(
    pool: &crate::large_document::ResidentBudget,
    retirement: &Mutex<Option<native_texture::Retirement>>,
) -> Result<native_texture::Retirement> {
    let mut cached = retirement.lock().unwrap_or_else(|e| e.into_inner());
    if cached
        .as_ref()
        .is_none_or(|owner| !owner.uses_budget(pool))
    {
        *cached = Some(native_texture::Retirement::new(pool)?);
    }
    Ok(cached.as_ref().unwrap().clone())
}
fn upload_with_owner(
    frame: Frame,
    origin: Option<Arc<Origin>>,
    retirement: &native_texture::Retirement,
    side: usize,
    display: Option<(&egui::Context, Option<&native_texture::Reuse>)>,
    check: &dyn Fn() -> Result<()>,
) -> Result<Output> {
    check()?;
    let uploads = native_texture::Prepared::from_frame_with_display(
        frame, retirement, side, display, &mut || check(),
    )?;
    Ok(Output {
        uploads,
        origin,
        baseline: None,
        text: None,
    })
}

impl super::PeerBrush {
    fn retry_native_canvas(&mut self, error: &str, ctx: &egui::Context) -> bool {
        if !retryable_source_read(error) {
            return false;
        }
        self.native_dispatch.retry_after = Some(Instant::now() + INVENTORY_RETRY_DELAY);
        ctx.request_repaint_after(INVENTORY_RETRY_DELAY);
        true
    }

    fn native_canvas_failed(&mut self, ctx: &egui::Context) {
        self.native_dispatch.retry_after = None;
        let proposal = self
            .native_dispatch
            .expected
            .as_ref()
            .is_some_and(|(_, stamp, _)| stamp.proposal.is_some());
        let animation = self
            .native_dispatch
            .expected
            .as_ref()
            .is_some_and(|(_, stamp, _)| stamp.presentation.is_some());
        let source = self
            .native_dispatch
            .expected
            .as_ref()
            .map(|(_, stamp, _)| stamp.source);
        let retry = source.is_some() && self.native_dispatch.skip_cache != source;
        self.native_dispatch.skip_cache = source;
        self.native_dispatch.clear_frames();
        if retry || proposal {
            self.native_dispatch.expected = None;
        }
        if animation {
            self.animation.stop_native_canvas();
        }
        ctx.request_repaint();
    }
    /// Command capture is metadata-only; scene/session acquisition and all
    /// pixel work remain on the existing bounded preview worker.
    pub(super) fn queue_native_canvas(
        &mut self,
        mut input: Input,
        ctx: &egui::Context,
    ) -> Result<()> {
        self.native_dispatch.gesture_deferred = false;
        input.cache = self.native_dispatch.skip_cache != Some(input.stamp.source)
            && self
                .native_dispatch
                .endpoint
                .cache_allowed(input.stamp.source)
            && input.commands.is_empty()
            && input.source.is_none()
            && input.presentation.is_none()
            && input.streamed_blend.is_none()
            && input.stamp.proposal.is_none()
            && input.stamp.selection_view.is_none();
        let epoch = self.preview_epoch.load(Ordering::Relaxed);
        if self.native_dispatch.unchanged(&input, epoch) {
            return Ok(());
        }
        if !input.cache {
            self.native_dispatch.clear_frames();
        }
        let job = self
            .preview_epoch
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.preview_job = job;
        if self.proposal_review.is_some() {
            self.proposal_rendered = None;
        }
        self.pending = false;
        let reuse = self.native_canvas.current().filter(|canvas| {
            let identity = &canvas.key().identity;
            identity.project_id == input.stamp.project && identity.document_id == input.stamp.document
        }).map(|canvas| canvas.reuse());
        self.native_dispatch.request(
            &mut self.preview_worker,
            self.shared.clone(),
            input,
            job,
            self.preview_epoch.clone(),
            ctx.input(|i| i.max_texture_side),
            ctx,
            reuse,
        )?;
        self.pending = true;
        Ok(())
    }
    pub(super) fn present_native_canvas(
        &mut self,
        project: &str,
        document: &NativeDocument,
        ctx: &egui::Context,
    ) -> Result<bool> {
        if let Some((_, stamp, _)) = &self.native_dispatch.expected {
            if let Err(error) = stamp.check_proposal(&self.shared) {
                if retryable_source_read(&error) {
                    // Leave the pending reply in the mailbox while authority is busy.
                    ctx.request_repaint_after(INVENTORY_RETRY_DELAY);
                    return Ok(false);
                }
                self.native_canvas_failed(ctx);
                self.pending = self.native_dispatch.pending();
                return Err(error);
            }
        }
        let reply = self.native_dispatch.take(
            project,
            document,
            self.preview_epoch.load(Ordering::Relaxed),
        );
        self.pending = self.native_dispatch.pending();
        if let Err(error) = &reply {
            if self.native_dispatch.gesture_deferred {
                return Ok(false);
            }
            if self.retry_native_canvas(error, ctx) {
                return Ok(false);
            }
            self.native_canvas_failed(ctx);
        }
        let Some(prepared) = reply? else {
            return Ok(false);
        };
        let mut receipt = self
            .native_dispatch
            .accepted
            .take()
            .ok_or("Native display receipt is missing")?;
        let presentation = (|| -> Result<bool> {
            receipt.stamp.check_proposal(&self.shared)?;
            let shown = self.present_native_prepared(prepared, ctx)?;
            if shown {
                receipt.stamp.check_proposal(&self.shared)?;
            }
            Ok(shown)
        })();
        let shown = match presentation {
            Ok(shown) => shown,
            Err(error) => {
                if self.native_dispatch.gesture_deferred || self.retry_native_canvas(&error, ctx) {
                    return Ok(false);
                }
                self.native_canvas_failed(ctx);
                return Err(error);
            }
        };
        if shown {
            if let (Some(editor), Some(text)) = (&mut self.source_editor, receipt.text.take()) {
                editor.accept_native_carets(text.command, text.carets);
            }
            self.native_dispatch
                .endpoint
                .published(&mut receipt, &self.preview_epoch);
            if receipt.stamp.presentation.is_some() {
                self.animation.native_presented(
                    document,
                    receipt.stamp.signature,
                    ctx.input(|input| input.time),
                );
            }
            if receipt.stamp.proposal.as_ref().is_some_and(|review| {
                self.proposal_review.as_deref() == Some(review.id.as_str())
                    && !self.proposal_original
            }) {
                self.proposal_rendered = Some(self.live_key());
                self.proposal_error = None;
            }
            self.native_receipt = Some(receipt);
        }
        Ok(shown)
    }
}

#[cfg(test)]
#[path = "native_dispatch/fade_tests.rs"]
mod fade_tests;
#[cfg(test)]
mod proposal_tests;
#[cfg(test)]
#[path = "native_dispatch/tests.rs"]
mod tests;
