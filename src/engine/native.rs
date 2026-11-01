//! Immutable native document owners. No live Engine field is switched here.
#[path = "native/admission.rs"]
mod admission;
#[path = "native/batch.rs"]
mod batch;
#[path = "native/create.rs"]
mod create;
#[path = "native/descriptor.rs"]
mod descriptor;
#[path = "native/effect_patch.rs"]
mod effect_patch;
#[path = "native/history.rs"]
mod history;
#[path = "native/insertion.rs"]
mod insertion;
#[path = "native/intent.rs"]
mod intent;
#[path = "native/metadata.rs"]
mod metadata;
#[path = "native/output.rs"]
mod output;
#[path = "native/patch.rs"]
mod patch;
#[path = "native/pixel_inverse.rs"]
mod pixel_inverse;
#[path = "native/publication.rs"]
mod publication;
#[path = "native/scopes.rs"]
mod scopes;
#[path = "native/transaction.rs"]
mod transaction;
use super::scene::{Document36, Layer36, PendingScene, RasterRef36, RasterRoster, Result};
use crate::{
    large_document::{DiskBudget, ResidentBudget, ScratchBytes},
    psd::{
        canonical36::{Limits, Stats, MAX_BYTES},
        memory36::{Admitted, MetadataCharge},
    },
    raster::Surface,
};
pub(crate) use batch::{
    CoveragePatch, CoveragePixels, LayerPatch, RasterBatch, RefusedBatch, SelectionPatch,
    SourceUpdate,
};
pub(crate) use descriptor::{Command, CommandDescriptor, CommandToken, Method};
pub(crate) use effect_patch::{EffectClipPatch, EffectPatch};
pub(crate) use history::{
    Direction, HistoryRequest, HistorySnapshot, NativeHistory, PreparedCommit, PreparedMove,
    Publication, RefusedCommit, RefusedMove, RefusedTask, RetiredHistory, TaskCandidate,
    TaskCapture, TaskEntry, TaskPreparationError, TaskRequest,
};
pub(crate) use insertion::RefusedInsertion;
pub use intent::{Intent, IntentKind};
pub use output::{OutputDraft, RefusedOutput, StagedOutput};
pub use patch::{RasterPatch, RefusedPatch, RetainedUpdate};
use serde::{Serialize, Serializer};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};
pub(crate) use transaction::{NativeTransaction, RefusedTransaction};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocumentKey([u8; 16]);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RasterSlot {
    pub(super) document: DocumentKey,
    pub(super) ordinal: u32,
}

struct Core {
    metadata: Document36,
    roster: Arc<OwnedRoster>,
    resident: ResidentBudget,
    disk: DiskBudget,
    key: DocumentKey,
    render_key: OnceLock<[u8; 32]>,
}
struct OwnedRoster {
    roster: RasterRoster,
    _charge: crate::large_document::Reservation,
}
/// Clones share the exact metadata allocation, source owners and admission.
/// This type has neither serde nor Deref/DerefMut pixel/metadata access.
pub struct NativeDocument {
    core: Admitted<Core>,
}
impl std::fmt::Debug for NativeDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeDocument")
            .field("generation", &self.key())
            .finish_non_exhaustive()
    }
}
impl Clone for NativeDocument {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
        }
    }
}
impl NativeDocument {
    /// PendingScene's complete codec/recovery and semantic admission remains a
    /// registration requirement; this constructor grants no registry authority.
    pub(crate) fn from_pending(pending: PendingScene) -> Result<Self> {
        let resident = pending.budget().clone();
        let disk = pending.disk_budget().clone();
        let bytes = pending.rasters().descriptor_bytes()?;
        let roster_charge = resident
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let core = pending.map_unique(|metadata, roster, _| {
            Ok(Core {
                metadata,
                roster: Arc::new(OwnedRoster {
                    roster,
                    _charge: roster_charge,
                }),
                resident,
                disk,
                key: key(),
                render_key: OnceLock::new(),
            })
        })?;
        Ok(Self { core })
    }
    pub fn key(&self) -> DocumentKey {
        self.core.key
    }
    pub fn metadata(&self) -> Metadata<'_> {
        Metadata(&self.core.metadata, self.key())
    }
    pub fn budget(&self) -> &ResidentBudget {
        &self.core.resident
    }
    pub fn disk_budget(&self) -> &DiskBudget {
        &self.core.disk
    }
    /// Metadata-only producer copies retain a fresh shared-pool charge. These
    /// helpers grant no source staging or editor publication authority.
    pub(crate) fn copy_value_metadata(
        &self,
        value: &serde_json::Value,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Admitted<serde_json::Value>> {
        copy_admitted(value, self.budget(), true, check)
    }
    pub(crate) fn copy_contour_metadata(
        &self,
        contours: &[Vec<[f32; 2]>],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Admitted<Vec<Vec<[f32; 2]>>>> {
        if contours.len() > 500_000 {
            return Err("Native contour array count exceeds its bound".into());
        }
        let mut vertices = 0usize;
        for contour in contours {
            check()?;
            vertices = vertices
                .checked_add(contour.len())
                .filter(|count| *count <= 500_000)
                .ok_or("Native contour vertex count exceeds its bound")?;
            if contour.iter().flatten().any(|v| !v.is_finite()) {
                return Err("Native contour coordinates must be finite".into());
            }
        }
        copy_admitted(&contours, self.budget(), true, check)
    }
    pub(super) fn roster(&self) -> &RasterRoster {
        &self.core.roster.roster
    }
    pub fn render_key(&self) -> Result<[u8; 32]> {
        if let Some(key) = self.core.render_key.get() {
            return Ok(*key);
        }
        let key = super::scene::hash_metadata(&self.metadata(), self.roster())?;
        let _ = self.core.render_key.set(key);
        Ok(key)
    }
    pub fn layer_slot(&self, layer: &str) -> Result<RasterSlot> {
        let layer = self
            .core
            .metadata
            .layers
            .iter()
            .find(|l| l.id == layer)
            .ok_or("Unknown native layer")?;
        Ok(RasterSlot {
            document: self.key(),
            ordinal: layer.pixels.id,
        })
    }
    pub fn source(&self, slot: RasterSlot) -> Result<&Surface> {
        if slot.document != self.key() {
            return Err("Native raster slot belongs to another immutable document".into());
        }
        self.core.roster.roster.ordinal(slot.ordinal)
    }
    pub fn raster_slot(&self, reference: &RasterRef36) -> Result<RasterSlot> {
        self.core.roster.roster.get(reference)?;
        Ok(RasterSlot {
            document: self.key(),
            ordinal: reference.id,
        })
    }
    /// Capture data owners only. Engine authorization, reservations and final
    /// revision guards remain mandatory when integrating the edit dispatcher.
    pub fn capture_edit(&self) -> Result<CapturedEdit> {
        if self.core.metadata.read_only {
            return Err("Native document is read-only".into());
        }
        Ok(CapturedEdit {
            base: self.clone(),
            cancellation: Cancellation::default(),
            admission: None,
        })
    }
    /// No callbacks, file access or history duplication occurs here.
    #[cfg(test)]
    pub fn adopt(
        &self,
        prepared: PreparedDelta,
    ) -> std::result::Result<NativeDocument, RefusedDelta> {
        if !prepared.outputs.is_empty() {
            return Err(RefusedDelta {
                reason: "Source edits require guarded Engine publication".into(),
                prepared,
            });
        }
        self.finish(prepared)
    }
    #[cfg(test)]
    pub(crate) fn publish(
        &self,
        actor: &str,
        leases: &[super::Lease],
        now: u64,
        closed: bool,
        prepared: PreparedDelta,
    ) -> std::result::Result<NativeDocument, RefusedDelta> {
        let (candidate, outputs) = self.publish_deferred(actor, leases, now, closed, prepared)?;
        for output in outputs {
            output.mark_published();
        }
        Ok(candidate)
    }
    fn publish_deferred(
        &self,
        actor: &str,
        leases: &[super::Lease],
        now: u64,
        closed: bool,
        prepared: PreparedDelta,
    ) -> std::result::Result<(NativeDocument, Vec<StagedOutput>), RefusedDelta> {
        if closed {
            return Err(RefusedDelta {
                reason: "Project is closed".into(),
                prepared,
            });
        }
        if let Err(reason) = prepared.intent.check(self, actor, leases, now) {
            return Err(RefusedDelta { reason, prepared });
        }
        if let Some(admission) = &prepared.admission {
            if let Err(reason) = admission.check_publication(self, actor, &prepared.intent) {
                return Err(RefusedDelta { reason, prepared });
            }
        }
        self.finish_deferred(prepared)
    }
    #[cfg(test)]
    fn finish(&self, prepared: PreparedDelta) -> std::result::Result<NativeDocument, RefusedDelta> {
        let (candidate, outputs) = self.finish_deferred(prepared)?;
        for output in outputs {
            output.mark_published();
        }
        Ok(candidate)
    }
    fn finish_deferred(
        &self,
        prepared: PreparedDelta,
    ) -> std::result::Result<(NativeDocument, Vec<StagedOutput>), RefusedDelta> {
        if prepared
            .admission
            .as_ref()
            .is_some_and(|a| a.is_transaction_step())
        {
            return Err(RefusedDelta {
                reason: "Native transaction steps cannot publish independently".into(),
                prepared,
            });
        }
        if let Err(reason) = prepared.cancellation.check() {
            return Err(RefusedDelta { reason, prepared });
        }
        if prepared.base != self.key()
            || self.core.metadata.id != prepared.candidate.core.metadata.id
            || self.core.metadata.revision.checked_add(1)
                != Some(prepared.candidate.core.metadata.revision)
            || self.core.metadata.read_only
        {
            return Err(RefusedDelta {
                reason: "Prepared native edit is stale or refused".into(),
                prepared,
            });
        }
        Ok((prepared.candidate, prepared.outputs))
    }
}
pub(super) fn key() -> DocumentKey {
    DocumentKey(*uuid::Uuid::new_v4().as_bytes())
}

/// Read-only metadata projection. Large Arc payloads cannot escape this view.
/// Serializing the projection emits metadata only and performs no source I/O.
#[derive(Clone, Copy)]
pub struct Metadata<'a>(&'a Document36, DocumentKey);
impl Serialize for Metadata<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'a> Metadata<'a> {
    pub(super) fn from_wire(wire: &'a Document36, key: DocumentKey) -> Metadata<'a> {
        Metadata(wire, key)
    }
    pub fn binding(&self) -> DocumentKey {
        self.1
    }
    pub fn visit_rasters(
        &self,
        mut visit: impl FnMut(RasterMetadata<'a>) -> Result<()>,
    ) -> Result<usize> {
        self.0
            .visit_rasters(|reference| visit(RasterMetadata(reference, self.1)))
    }
    pub fn id(&self) -> &'a str {
        &self.0.id
    }
    pub fn name(&self) -> &'a str {
        &self.0.name
    }
    pub fn width(&self) -> u32 {
        self.0.width
    }
    pub fn height(&self) -> u32 {
        self.0.height
    }
    pub fn depth(&self) -> u16 {
        self.0.bit_depth
    }
    pub fn revision(&self) -> u64 {
        self.0.revision
    }
    pub fn read_only(&self) -> bool {
        self.0.read_only
    }
    pub fn profile(&self) -> Option<&'a [u8]> {
        self.0.icc_profile.as_deref().map(Vec::as_slice)
    }
    pub fn srgb_tagged(&self) -> bool {
        self.0.srgb_tagged
    }
    pub fn global_mask(&self) -> Option<&'a crate::psd::GlobalMaskSettings> {
        self.0.psd_global_mask.as_ref()
    }
    pub fn warnings(&self) -> &'a [String] {
        &self.0.warnings
    }
    pub fn selection(&self) -> Option<[i32; 4]> {
        self.0.selection
    }
    pub fn selection_polygon(&self) -> Option<&'a [[f32; 2]]> {
        self.0.selection_polygon.as_deref()
    }
    pub fn selection_coverage(&self) -> Option<CoverageMetadata<'a>> {
        self.0
            .selection_coverage
            .as_ref()
            .map(|c| CoverageMetadata(c, self.1))
    }
    pub fn selection_previous(&self) -> Option<CoverageMetadata<'a>> {
        self.0
            .selection_previous
            .as_ref()
            .map(|c| CoverageMetadata(c, self.1))
    }
    pub fn layers(
        &self,
    ) -> impl ExactSizeIterator<Item = LayerMetadata<'a>> + DoubleEndedIterator + 'a {
        let binding = self.1;
        self.0.layers.iter().map(move |l| LayerMetadata(l, binding))
    }
    pub fn layer(&self, index: usize) -> Option<LayerMetadata<'a>> {
        self.0
            .layers
            .get(index)
            .map(|layer| LayerMetadata(layer, self.1))
    }
    pub fn filters(
        &self,
    ) -> impl ExactSizeIterator<Item = EffectMetadata<'a>> + DoubleEndedIterator + 'a {
        let binding = self.1;
        self.0
            .filters
            .iter()
            .map(move |e| EffectMetadata(e, binding))
    }
}
#[derive(Clone, Copy)]
pub struct LayerMetadata<'a>(&'a Layer36, DocumentKey);
impl<'a> LayerMetadata<'a> {
    pub fn id(&self) -> &'a str {
        &self.0.id
    }
    pub fn name(&self) -> &'a str {
        &self.0.name
    }
    pub fn kind(&self) -> &'a str {
        &self.0.kind
    }
    pub fn visible(&self) -> bool {
        self.0.visible
    }
    pub fn locked(&self) -> bool {
        self.0.locked
    }
    pub fn opacity(&self) -> f32 {
        self.0.opacity
    }
    pub fn blend(&self) -> &'a str {
        &self.0.blend
    }
    pub fn origin(&self) -> [i32; 2] {
        [self.0.x, self.0.y]
    }
    pub fn pixels(&self) -> RasterMetadata<'a> {
        RasterMetadata(&self.0.pixels, self.1)
    }
    pub fn parent(&self) -> Option<&'a str> {
        self.0.parent.as_deref()
    }
    pub fn clip_to(&self) -> Option<&'a str> {
        self.0.clip_to.as_deref()
    }
    pub fn color(&self) -> [u8; 4] {
        self.0.color
    }
    pub fn mask(&self) -> Option<MaskMetadata<'a>> {
        self.0.mask.as_ref().map(|m| MaskMetadata(m, self.1))
    }
    pub fn effects(
        &self,
    ) -> impl ExactSizeIterator<Item = EffectMetadata<'a>> + DoubleEndedIterator + 'a {
        let binding = self.1;
        self.0
            .effects
            .iter()
            .map(move |e| EffectMetadata(e, binding))
    }
    pub fn psd_metadata(&self) -> &'a [crate::psd::LayerMetadata] {
        &self.0.psd_metadata
    }
    pub fn source(&self) -> Option<&'a super::scene::Source36> {
        self.0.source.as_ref()
    }
}
#[derive(Clone, Copy)]
pub struct CoverageMetadata<'a>(&'a super::scene::Coverage36, DocumentKey);
impl<'a> CoverageMetadata<'a> {
    pub fn mask(&self) -> RasterMetadata<'a> {
        RasterMetadata(&self.0.mask, self.1)
    }
    pub fn bounds(&self) -> [i32; 4] {
        self.0.bounds
    }
    pub fn origin(&self) -> [i32; 2] {
        self.0.origin
    }
    pub fn contours(&self) -> &'a [Vec<[f32; 2]>] {
        &self.0.contours
    }
}
#[derive(Clone, Copy)]
pub struct EffectMetadata<'a>(&'a super::scene::Effect36, DocumentKey);
impl<'a> EffectMetadata<'a> {
    pub fn id(&self) -> &'a str {
        &self.0.id
    }
    pub fn kind(&self) -> &'a str {
        &self.0.kind
    }
    pub fn enabled(&self) -> bool {
        self.0.enabled
    }
    pub fn weight(&self) -> f32 {
        self.0.weight
    }
    pub fn settings(&self) -> &'a serde_json::Value {
        &self.0.settings
    }
    pub fn clips(
        &self,
    ) -> impl ExactSizeIterator<Item = (u32, CoverageMetadata<'a>)> + DoubleEndedIterator + 'a {
        let binding = self.1;
        self.0
            .clips
            .iter()
            .map(move |clip| (clip.stroke, CoverageMetadata(&clip.coverage, binding)))
    }
}
#[derive(Clone, Copy)]
pub struct MaskMetadata<'a>(&'a super::scene::Mask36, DocumentKey);
impl<'a> MaskMetadata<'a> {
    pub fn enabled(&self) -> bool {
        self.0.enabled
    }
    pub fn linked(&self) -> bool {
        self.0.linked
    }
    pub fn steps(
        &self,
    ) -> impl ExactSizeIterator<Item = MaskStepMetadata<'a>> + DoubleEndedIterator + 'a {
        let binding = self.1;
        self.0
            .steps
            .iter()
            .map(move |s| MaskStepMetadata(s, binding))
    }
}
#[derive(Clone, Copy)]
pub struct MaskStepMetadata<'a>(&'a super::scene::MaskStep36, DocumentKey);
impl<'a> MaskStepMetadata<'a> {
    pub fn id(&self) -> &'a str {
        &self.0.id
    }
    pub fn kind(&self) -> &'a str {
        &self.0.kind
    }
    pub fn enabled(&self) -> bool {
        self.0.enabled
    }
    pub fn weight(&self) -> f32 {
        self.0.weight
    }
    pub fn value(&self) -> f32 {
        self.0.value
    }
    pub fn pixels(&self) -> RasterMetadata<'a> {
        RasterMetadata(&self.0.pixels, self.1)
    }
    pub fn settings(&self) -> &'a serde_json::Value {
        &self.0.settings
    }
}
#[derive(Clone, Copy)]
pub struct RasterMetadata<'a>(&'a RasterRef36, DocumentKey);
impl<'a> RasterMetadata<'a> {
    pub fn ordinal(&self) -> u32 {
        self.0.id
    }
    pub fn width(&self) -> u32 {
        self.0.width
    }
    pub fn height(&self) -> u32 {
        self.0.height
    }
    pub fn depth(&self) -> u16 {
        self.0.depth
    }
    pub fn slot(&self) -> RasterSlot {
        RasterSlot {
            document: self.1,
            ordinal: self.0.id,
        }
    }
    pub fn retained(&self) -> Option<RetainedMetadata<'a>> {
        self.0
            .retained
            .as_deref()
            .map(|r| RetainedMetadata(r, self.1))
    }
}
#[derive(Clone, Copy)]
pub struct RetainedMetadata<'a>(&'a super::scene::Retained36, DocumentKey);
impl<'a> RetainedMetadata<'a> {
    pub fn pixels(&self) -> RasterMetadata<'a> {
        RasterMetadata(&self.0.pixels, self.1)
    }
    pub fn matrix(&self) -> [f64; 6] {
        self.0.matrix
    }
    pub fn projective(&self) -> Option<[f64; 9]> {
        self.0.projective
    }
    pub fn warps(&self) -> &'a [super::scene::Warp36] {
        &self.0.warps
    }
}

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub(crate) fn check(&self) -> Result<()> {
        if self.0.load(Ordering::Acquire) {
            Err("Native edit cancelled".into())
        } else {
            Ok(())
        }
    }
}
pub struct CapturedEdit {
    base: NativeDocument,
    cancellation: Cancellation,
    admission: Option<Arc<admission::EditAdmission>>,
}
pub struct PreparedDelta {
    base: DocumentKey,
    candidate: NativeDocument,
    cancellation: Cancellation,
    intent: Intent,
    outputs: Vec<StagedOutput>,
    admission: Option<Arc<admission::EditAdmission>>,
}
#[must_use]
pub struct RefusedDelta {
    reason: String,
    prepared: PreparedDelta,
}
impl std::fmt::Debug for RefusedDelta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RefusedDelta").field(&self.reason).finish()
    }
}
impl RefusedDelta {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn into_prepared(self) -> PreparedDelta {
        self.prepared
    }
    pub(crate) fn abort(
        self,
        session: &impl super::scene::StageSession,
        cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let PreparedDelta {
            candidate,
            outputs,
            admission,
            ..
        } = self.prepared;
        drop(candidate);
        let result = output::abort_outputs(outputs, session, cleanup);
        drop(admission);
        result
    }
}
impl PreparedDelta {
    pub fn base_key(&self) -> DocumentKey {
        self.base
    }
    pub fn intent(&self) -> &Intent {
        &self.intent
    }
    pub fn preview(&self) -> &NativeDocument {
        &self.candidate
    }
}
impl CapturedEdit {
    pub(crate) fn descriptor(&self) -> Option<&CommandDescriptor> {
        self.admission
            .as_ref()
            .and_then(|admission| admission.descriptor())
    }
    fn planned_intent(&self, intent: Intent) -> Result<Intent> {
        match self.admission.as_ref() {
            Some(admission) => admission.planned_intent(&self.base, intent),
            None => Ok(intent),
        }
    }
    pub fn base_key(&self) -> DocumentKey {
        self.base.key()
    }
    pub fn document(&self) -> &NativeDocument {
        &self.base
    }
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
    /// First complete metadata operation. Source/operator adapters are separate;
    /// this path does not mint authority for arbitrary unsealed source blocks.
    pub fn visibility(
        self,
        layer: &str,
        visible: bool,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<PreparedDelta> {
        let checked = || {
            self.cancellation.check()?;
            check()?;
            self.cancellation.check()
        };
        let check = &checked;
        check()?;
        let target = self
            .base
            .core
            .metadata
            .layers
            .iter()
            .position(|l| l.id == layer)
            .ok_or("Unknown native layer")?;
        if let Some(descriptor) = self.descriptor() {
            let command = descriptor
                .commands()
                .next()
                .ok_or("Missing normalized visibility command")?;
            if descriptor.commands().len() != 1
                || command.parameters()["op"] != "layer.update"
                || command.parameters()["layer"] != layer
                || command.parameters()["visible"] != visible
                || !command.parameters().as_object().is_some_and(|object| {
                    object
                        .keys()
                        .all(|key| ["op", "layer", "visible"].contains(&key.as_str()))
                })
            {
                return Err("Visibility preparation differs from its normalized command".into());
            }
        }
        let intent =
            self.planned_intent(Intent::visibility(layer, visible, self.base.budget())?)?;
        // The shared engine permits visibility-only changes on locked layers.
        let copied = copy_metadata(&self.base.core.metadata, self.base.budget(), check)?;
        let roster = self.base.core.roster.clone();
        let resident = self.base.core.resident.clone();
        let disk = self.base.core.disk.clone();
        let core = copied.map_unique(|mut metadata, _| {
            check()?;
            metadata.layers[target].visible = visible;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            check()?;
            Ok(Core {
                metadata,
                roster,
                resident,
                disk,
                key: key(),
                render_key: OnceLock::new(),
            })
        })?;
        check()?;
        Ok(PreparedDelta {
            base: self.base.key(),
            candidate: NativeDocument { core },
            cancellation: self.cancellation,
            intent,
            outputs: vec![],
            admission: self.admission.clone(),
        })
    }
    /// Bounded first structural operation. Hierarchical/clip policy stays with
    /// the full edit dispatcher; this method accepts independent roots only.
    pub fn reorder_independent_layers(
        self,
        order: &[String],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<PreparedDelta> {
        let checked = || {
            self.cancellation.check()?;
            check()?;
            self.cancellation.check()
        };
        let check = &checked;
        check()?;
        if self.descriptor().is_some() {
            return Err(
                "Use the normalized native metadata dispatcher for reorder commands".into(),
            );
        }
        if order.len() != self.base.core.metadata.layers.len()
            || self
                .base
                .core
                .metadata
                .layers
                .iter()
                .any(|l| l.parent.is_some() || l.clip_to.is_some())
        {
            return Err("Native reorder requires an exact independent-root permutation".into());
        }
        let copied = copy_metadata(&self.base.core.metadata, self.base.budget(), check)?;
        let intent = Intent::layers(IntentKind::Reorder, order, self.base.budget())?;
        let source = &self.base.core.roster.roster;
        let resident = self.base.core.resident.clone();
        let disk = self.base.core.disk.clone();
        let core = copied.map_unique(|mut metadata, _| {
            for (index, id) in order.iter().enumerate() {
                check()?;
                let offset = metadata.layers[index..]
                    .iter()
                    .position(|layer| layer.id == *id)
                    .ok_or("Native layer order is not a permutation")?;
                metadata.layers.swap(index, index + offset);
            }
            let roster = rebuild_roster(&mut metadata, source, &resident, &disk, check)?;
            metadata.revision = metadata
                .revision
                .checked_add(1)
                .ok_or("Native revision overflow")?;
            Ok(Core {
                metadata,
                roster,
                resident,
                disk,
                key: key(),
                render_key: OnceLock::new(),
            })
        })?;
        check()?;
        Ok(PreparedDelta {
            base: self.base.key(),
            candidate: NativeDocument { core },
            cancellation: self.cancellation,
            intent,
            outputs: vec![],
            admission: self.admission.clone(),
        })
    }
}

fn rebuild_roster(
    metadata: &mut Document36,
    source: &RasterRoster,
    resident: &ResidentBudget,
    disk: &DiskBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Arc<OwnedRoster>> {
    rebuild_from(metadata, source.len(), resident, disk, check, |reference| {
        source.get(reference).cloned()
    })
}
fn rebuild_from(
    metadata: &mut Document36,
    capacity: usize,
    resident: &ResidentBudget,
    disk: &DiskBudget,
    check: &dyn Fn() -> Result<()>,
    mut resolve: impl FnMut(&RasterRef36) -> Result<Surface>,
) -> Result<Arc<OwnedRoster>> {
    let bytes = capacity
        .checked_mul(std::mem::size_of::<Surface>())
        .and_then(|n| n.checked_add(128))
        .ok_or("Native roster footprint overflow")?;
    let charge = resident
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(capacity)
        .map_err(|_| "Native roster allocation refused")?;
    if entries.capacity() > capacity {
        return Err("Native roster allocation exceeds its admission".into());
    }
    metadata.remap_rasters(|reference| {
        check()?;
        if entries.len() >= capacity {
            return Err("Native source roster exceeds its planned locations".into());
        }
        let surface = resolve(reference)?;
        if !surface.uses_budget(resident) || !surface.uses_disk_budget(disk) {
            return Err("Native source is outside shared admission".into());
        }
        entries.push(surface);
        reference.id = entries.len() as u32;
        Ok(())
    })?;
    let roster = RasterRoster::new(entries)?;
    let count = metadata.visit_rasters(|reference| {
        roster.get(reference)?;
        Ok(())
    })?;
    if count != roster.len() {
        return Err("Native remapped source count differs".into());
    }
    check()?;
    Ok(Arc::new(OwnedRoster {
        roster,
        _charge: charge,
    }))
}

/// Fresh admitted deep copy, outside every engine/UI/registry lock. No prior
/// header ticket or naked shared Arc payload is inherited into the new header.
fn copy_metadata(
    source: &Document36,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Admitted<Document36>> {
    let metadata: Admitted<Document36> = copy_admitted(source, pool, true, check)?;
    metadata.visit_rasters(|_| Ok(()))?;
    Ok(metadata)
}
fn copy_admitted<T: Serialize, U: serde::de::DeserializeOwned + Serialize>(
    source: &T,
    pool: &ResidentBudget,
    compare_wire: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<Admitted<U>> {
    struct Count<'a> {
        bytes: usize,
        check: &'a dyn Fn() -> Result<()>,
    }
    impl Write for Count<'_> {
        fn write(&mut self, part: &[u8]) -> std::io::Result<usize> {
            (self.check)().map_err(std::io::Error::other)?;
            self.bytes = self
                .bytes
                .checked_add(part.len())
                .filter(|n| *n <= MAX_BYTES)
                .ok_or_else(|| std::io::Error::other("Native metadata exceeds its byte bound"))?;
            Ok(part.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count { bytes: 0, check };
    serde_json::to_writer(&mut count, source).map_err(|e| e.to_string())?;
    let mut encoded = ScratchBytes::new(pool, count.bytes, MAX_BYTES).map_err(|e| e.to_string())?;
    let mut cursor = std::io::Cursor::new(&mut encoded[..]);
    serde_json::to_writer(&mut cursor, source).map_err(|e| e.to_string())?;
    if cursor.position() != count.bytes as u64 {
        return Err("Native metadata measurement differs".into());
    }
    let limits = Limits {
        bytes: count.bytes,
        nodes: 8_388_608,
        depth: 128,
    };
    // Allocation-free DOM preflight can still need serde's escaped-string
    // scratch. Admit its raw-byte upper bound before starting that parser.
    let preflight = pool
        .reserve_bytes(encoded.len(), MAX_BYTES)
        .map_err(|e| e.to_string())?;
    let stats = Stats::scan(&encoded, limits, check)?;
    drop(preflight);
    let owner = MetadataCharge::reserve(pool, stats.tree_upper_bytes()?, check)?;
    check()?;
    let metadata: U = serde_json::from_slice(&encoded).map_err(|e| e.to_string())?;
    struct Same<'a> {
        expected: &'a [u8],
        at: usize,
    }
    impl Write for Same<'_> {
        fn write(&mut self, part: &[u8]) -> std::io::Result<usize> {
            let end = self
                .at
                .checked_add(part.len())
                .filter(|n| *n <= self.expected.len())
                .ok_or_else(|| std::io::Error::other("Native copied metadata differs"))?;
            if self.expected[self.at..end] != *part {
                return Err(std::io::Error::other("Native copied metadata differs"));
            }
            self.at = end;
            Ok(part.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut same = Same {
        expected: &encoded,
        at: 0,
    };
    if compare_wire {
        serde_json::to_writer(&mut same, &metadata).map_err(|e| e.to_string())?;
    }
    if compare_wire && same.at != encoded.len() {
        return Err("Native copied metadata length differs".into());
    }
    check()?;
    Ok(Admitted::from_parts(Arc::new(metadata), owner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{scene::RasterCollector, Document};
    #[test]
    fn producer_metadata_copies_preserve_unknown_fields_and_own_their_fresh_charge() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let document = document(&pool);
        let value =
            serde_json::json!({"future":{"enabled":true,"exact":[-0.0,1.23456789]},"strokes":[]});
        let copied = document.copy_value_metadata(&value, &|| Ok(())).unwrap();
        assert_eq!(*copied, value);
        let copied = copied
            .map_unique(|mut value, _| {
                value["future"]["enabled"] = serde_json::json!(false);
                Ok(value)
            })
            .unwrap();
        assert_eq!(value["future"]["enabled"], true);
        let contours = vec![vec![[1., 2.], [3., 4.]], vec![]];
        let copied_contours = document
            .copy_contour_metadata(&contours, &|| Ok(()))
            .unwrap();
        let copied_contours = copied_contours
            .map_unique(|mut values, _| {
                values[0][0][0] += 4.;
                Ok(values)
            })
            .unwrap();
        assert_eq!(copied_contours[0][0], [5., 2.]);
        assert_eq!(contours[0][0], [1., 2.]);
        assert!(document
            .copy_contour_metadata(&[vec![[f32::NAN, 0.]]], &|| Ok(()))
            .is_err());
        assert!(document
            .copy_value_metadata(&value, &|| Err("cancelled".into()))
            .is_err());
        drop(document);
        assert!(pool.used() > 0);
        assert_eq!(copied["future"]["exact"], value["future"]["exact"]);
        drop(copied);
        drop(copied_contours);
        assert_eq!(pool.used(), 0);
    }
    struct Collector {
        rasters: Vec<Surface>,
    }
    impl RasterCollector for Collector {
        fn resident(&mut self, r: &crate::raster::Raster) -> Result<RasterRef36> {
            self.rasters
                .push(Surface::zero(r.width, r.height, r.depth).unwrap());
            Ok(RasterRef36 {
                id: self.rasters.len() as u32,
                width: r.width,
                height: r.height,
                depth: r.depth,
                retained: None,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
            Err("Unexpected fixture surface".into())
        }
    }
    pub(super) fn document(pool: &ResidentBudget) -> NativeDocument {
        let mut source = Document::new_depth(8, 8, 16).unwrap();
        source.icc_profile = Some(Arc::new(vec![0, 127, 128, 255]));
        let mut collector = Collector { rasters: vec![] };
        let metadata = Document36::capture(&source, &mut collector).unwrap();
        let owner = MetadataCharge::reserve(pool, 64 * 1024, &|| Ok(())).unwrap();
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(collector.rasters).unwrap(),
            pool.clone(),
            DiskBudget::new(1024 * 1024, 8).unwrap(),
        )
        .unwrap();
        NativeDocument::from_pending(pending).unwrap()
    }
    #[test]
    fn immutable_clones_share_sources_and_header_charge_until_last_owner() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let first = document(&pool);
        let second = first.clone();
        let baseline = pool.used();
        assert!(baseline > 64 * 1024);
        assert_eq!(first.key(), second.key());
        assert_eq!(pool.used(), baseline);
        let layer = first.metadata().layers().next().unwrap().id().to_string();
        let slot = first.layer_slot(&layer).unwrap();
        assert!(std::ptr::eq(
            first.source(slot).unwrap(),
            second.source(slot).unwrap()
        ));
        drop(first);
        assert_eq!(
            second.metadata().profile(),
            Some([0, 127, 128, 255].as_slice())
        );
        assert_eq!(pool.used(), baseline);
        drop(second);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn prepared_edit_is_atomic_stale_checked_and_does_not_inherit_old_header_tickets() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let mut current = document(&pool);
        let layer = current.metadata().layers().next().unwrap().id().to_string();
        let old = current.clone();
        let stale = current
            .capture_edit()
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        let prepared = current
            .capture_edit()
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        assert!(current.metadata().layers().next().unwrap().visible());
        current = current.adopt(prepared).unwrap();
        assert!(!Arc::ptr_eq(
            current.core.metadata.icc_profile.as_ref().unwrap(),
            old.core.metadata.icc_profile.as_ref().unwrap()
        ));
        assert!(!current.metadata().layers().next().unwrap().visible());
        assert!(current.adopt(stale).is_err());
        drop(old);
        let retained = pool.used();
        for i in 0..20 {
            let next = current
                .capture_edit()
                .unwrap()
                .visibility(&layer, i % 2 == 0, &|| Ok(()))
                .unwrap();
            current = current.adopt(next).unwrap();
            assert_eq!(pool.used(), retained);
        }
        assert!(current
            .source(RasterSlot {
                document: key(),
                ordinal: 1
            })
            .is_err());
        drop(current);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn cancellation_and_admission_refusal_preserve_original_native_metadata() {
        let pool = ResidentBudget::new(64 * 1024 + 4096).unwrap();
        let current = document(&pool);
        let key = current.key();
        let baseline = pool.used();
        let layer = current.metadata().layers().next().unwrap().id().to_string();
        assert!(current
            .capture_edit()
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .is_err());
        assert!(current
            .capture_edit()
            .unwrap()
            .visibility(&layer, false, &|| Err("cancel".into()))
            .is_err());
        assert_eq!(current.key(), key);
        assert_eq!(current.metadata().revision(), 0);
        assert!(current.metadata().layers().next().unwrap().visible());
        assert_eq!(pool.used(), baseline);
    }
    #[test]
    fn cancellation_after_preparation_prevents_pure_adoption() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let current = document(&pool);
        let layer = current.metadata().layers().next().unwrap().id().to_string();
        let capture = current.capture_edit().unwrap();
        let cancellation = capture.cancellation();
        let prepared = capture.visibility(&layer, false, &|| Ok(())).unwrap();
        cancellation.cancel();
        assert!(current.adopt(prepared).is_err());
        assert!(current.metadata().layers().next().unwrap().visible());
        assert_eq!(current.metadata().revision(), 0);
    }
    #[test]
    fn visibility_preserves_locked_layer_exception_but_refuses_read_only_documents() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let source = document(&pool);
        let locked = NativeDocument {
            core: source
                .core
                .map_unique(|mut core, _| {
                    core.metadata.layers[0].locked = true;
                    Ok(core)
                })
                .unwrap(),
        };
        let layer = locked.metadata().layers().next().unwrap().id().to_string();
        let change = locked
            .capture_edit()
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        let changed = locked.adopt(change).unwrap();
        assert!(!changed.metadata().layers().next().unwrap().visible());
        assert!(changed.metadata().layers().next().unwrap().locked());
        let protected = NativeDocument {
            core: changed
                .core
                .map_unique(|mut core, _| {
                    core.metadata.read_only = true;
                    Ok(core)
                })
                .unwrap(),
        };
        assert!(protected.capture_edit().is_err());
    }
    #[test]
    fn native_snapshots_share_owned_headers_and_bind_the_common_projection_to_exact_slots() {
        use crate::engine::scene::{SceneIdentity, SceneSnapshot};
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let document = document(&pool);
        let before = pool.used();
        let make = |doc: NativeDocument| {
            let identity = SceneIdentity {
                project_id: "native-project".into(),
                document_id: doc.metadata().id().into(),
                revision: doc.metadata().revision(),
            };
            SceneSnapshot::from_native(identity, doc, vec![], None, None).unwrap()
        };
        let first = make(document.clone());
        let second = make(document.clone());
        assert_eq!(pool.used(), before);
        let first_metadata = first.metadata();
        let layer = first_metadata.layers().next().unwrap();
        let slot = layer.pixels().slot();
        assert!(std::ptr::eq(
            first.raster(slot).unwrap(),
            second.raster(slot).unwrap()
        ));
        assert_eq!(first.render_key().unwrap(), second.render_key().unwrap());
        assert_eq!(first.render_key().unwrap(), document.render_key().unwrap());
        let wire: Document36 =
            serde_json::from_slice(&serde_json::to_vec(&first.metadata()).unwrap()).unwrap();
        assert_eq!(
            wire.icc_profile.as_deref().map(Vec::as_slice),
            document.metadata().profile()
        );
        let prepared = document
            .capture_edit()
            .unwrap()
            .visibility(layer.id(), false, &|| Ok(()))
            .unwrap();
        let changed = document.adopt(prepared).unwrap();
        let changed = make(changed);
        assert!(changed.raster(slot).is_err());
        assert!(changed.into_provisional_pending().is_err());
        drop(first);
        drop(second);
        drop(document);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn borrowed_topology_outlives_function_local_projection_and_loop_copies() {
        use crate::engine::scene::{SceneIdentity, SceneSnapshot};
        fn layers<'a>(snapshot: &'a SceneSnapshot) -> Vec<LayerMetadata<'a>> {
            let projection = snapshot.metadata();
            projection.layers().collect()
        }
        fn labels<'a>(snapshot: &'a SceneSnapshot) -> Vec<(&'a str, &'a str)> {
            snapshot
                .metadata()
                .layers()
                .map(|layer| {
                    let copy = layer;
                    (copy.name(), copy.blend())
                })
                .collect()
        }
        fn profile(snapshot: &SceneSnapshot) -> Option<&[u8]> {
            snapshot.metadata().profile()
        }
        fn settings<'a>(layer: LayerMetadata<'a>) -> Vec<&'a serde_json::Value> {
            layer.effects().map(|effect| effect.settings()).collect()
        }
        fn contours<'a>(metadata: Metadata<'a>) -> Option<&'a [Vec<[f32; 2]>]> {
            metadata
                .selection_coverage()
                .map(|coverage| coverage.contours())
        }
        fn warps<'a>(raster: RasterMetadata<'a>) -> Option<&'a [super::super::scene::Warp36]> {
            raster.retained().map(|original| original.warps())
        }
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let document = document(&pool);
        let identity = SceneIdentity {
            project_id: "borrowed-topology".into(),
            document_id: document.metadata().id().into(),
            revision: 0,
        };
        let snapshot = SceneSnapshot::from_native(identity, document, vec![], None, None).unwrap();
        let table = layers(&snapshot);
        let names = labels(&snapshot);
        assert_eq!(table[0].name(), names[0].0);
        assert_eq!(table[0].blend(), names[0].1);
        assert_eq!(profile(&snapshot), Some([0, 127, 128, 255].as_slice()));
        assert!(settings(table[0]).is_empty());
        assert!(contours(snapshot.metadata()).is_none());
        assert!(warps(table[0].pixels()).is_none());
    }
    #[test]
    fn final_lease_and_closed_guards_return_the_prepared_edit_for_recovery() {
        use crate::engine::{Lease, Scope};
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let current = document(&pool);
        let layer = current.metadata().layers().next().unwrap().id().to_string();
        let prepared = current
            .capture_edit()
            .unwrap()
            .visibility(&layer, false, &|| Ok(()))
            .unwrap();
        let lease = Lease {
            id: "new-lease".into(),
            owner: "human".into(),
            description: "visible review".into(),
            scopes: vec![Scope::layer(&format!("@visibility:{layer}"))],
            expires: 100,
        };
        let refused = current
            .publish("worker", &[lease], 10, false, prepared)
            .unwrap_err();
        assert!(refused.reason().contains("Reserved by human"));
        assert!(current.metadata().layers().next().unwrap().visible());
        let recovered = refused.into_prepared();
        let refused = current
            .publish("worker", &[], 10, true, recovered)
            .unwrap_err();
        assert!(refused.reason().contains("closed"));
        let committed = current
            .publish("worker", &[], 10, false, refused.into_prepared())
            .unwrap();
        assert!(!committed.metadata().layers().next().unwrap().visible());
        assert_eq!(committed.metadata().revision(), 1);
    }
    #[test]
    fn reorder_renumbers_all_native_locations_and_retains_real_eight_and_sixteen_bit_sources() {
        use crate::large_document::{Limits as StoreLimits, Store};
        use crate::raster::{backed::Region, Raster};
        struct Backed {
            store: Store,
            planes: Vec<Surface>,
        }
        impl RasterCollector for Backed {
            fn resident(&mut self, raster: &Raster) -> Result<RasterRef36> {
                let id = self.planes.len() as u32 + 1;
                self.planes.push(
                    Surface::stage_current_pixels(raster, self.store.clone(), &mut || Ok(()))
                        .map_err(|e| e.to_string())?,
                );
                let retained = raster
                    .retained
                    .as_ref()
                    .map(|original| -> Result<Box<super::super::scene::Retained36>> {
                        Ok(Box::new(super::super::scene::Retained36 {
                            pixels: self.resident(&original.pixels)?,
                            matrix: original.matrix,
                            projective: original.projective,
                            warps: original
                                .warps
                                .iter()
                                .map(super::super::scene::Warp36::capture)
                                .collect(),
                        }))
                    })
                    .transpose()?;
                Ok(RasterRef36 {
                    id,
                    width: raster.width,
                    height: raster.height,
                    depth: raster.depth,
                    retained,
                })
            }
            fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
                Err("Unexpected fixture surface".into())
            }
            fn embedded_coverage(
                &mut self,
                value: &serde_json::Value,
            ) -> Result<super::super::scene::Coverage36> {
                use serde::Deserialize;
                let coverage =
                    crate::selection::Coverage::deserialize(value).map_err(|e| e.to_string())?;
                super::super::scene::Coverage36::from_capture(&coverage, self)
            }
        }
        for depth in [8, 16] {
            let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
            let disk = DiskBudget::new(16 * 1024 * 1024, 16).unwrap();
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("peerbrush-native-roster-{}", crate::engine::id()));
            let store = Store::create_with_disk_budget(
                &path,
                pool.clone(),
                disk.clone(),
                StoreLimits {
                    disk_bytes: 8 * 1024 * 1024,
                    max_blocks: 64,
                    max_resident_blocks: 16,
                    manifest_bytes: 1024 * 1024,
                },
            )
            .unwrap();
            let mut legacy = Document::new_depth(8, 8, depth).unwrap();
            let mut second = legacy.layers[0].clone();
            second.id = crate::engine::id();
            second.name = "Second".into();
            legacy.layers[0]
                .pixels
                .set16(0, 0, [12345, 23456, 34567, 65535]);
            legacy.layers[0].pixels.retained = Some(Box::new(crate::retained::Original::from(
                &legacy.layers[0].pixels,
            )));
            second.pixels.set16(0, 0, [42223, 45678, 17, 65535]);
            legacy.layers.push(second);
            let mut mask = Raster::new_depth(8, 8, depth);
            mask.set16(0, 0, [10001; 4]);
            legacy.layers[0].mask = Some(crate::engine::Mask {
                enabled: true,
                linked: true,
                cache_key: crate::engine::id(),
                steps: vec![crate::engine::MaskStep {
                    id: crate::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 255.,
                    pixels: mask,
                    settings: serde_json::json!({}),
                }],
            });
            let mut scalar = Raster::new(8, 8);
            scalar.set(0, 0, [127, 127, 127, 255]);
            let coverage = crate::selection::Coverage {
                mask: scalar,
                bounds: [0, 0, 8, 8],
                origin: [0, 0],
                contours: vec![vec![[0., 0.], [8., 0.], [8., 8.]]],
            };
            let effect = crate::effects::Effect {
                id: crate::engine::id(),
                kind: "liquify".into(),
                enabled: true,
                weight: 1.,
                settings: Arc::new(
                    serde_json::json!({"radius":3.,"strength":0.75,"strokes":[{"mode":"push","points":[[0,0],[1,1]],"coverage":coverage}]}),
                ),
            };
            legacy.layers[0].effects.push(effect.clone());
            legacy.filters.push(effect);
            legacy.selection = Some(coverage.bounds);
            legacy.selection_coverage = Some(coverage.clone());
            legacy.selection_previous = Some(coverage);
            let mut collector = Backed {
                store: store.clone(),
                planes: vec![],
            };
            let metadata = Document36::capture(&legacy, &mut collector).unwrap();
            assert_eq!(collector.planes.len(), 8);
            let refs: Vec<_> = collector
                .planes
                .iter()
                .flat_map(Surface::physical_tiles)
                .map(|p| p.reference())
                .collect();
            let sealed = store
                .commit(
                    store.identity(),
                    1,
                    None,
                    b"native-roster-fixture",
                    &refs,
                    &mut || Ok(()),
                )
                .unwrap();
            let owner = MetadataCharge::reserve(&pool, 64 * 1024, &|| Ok(())).unwrap();
            let planes = collector.planes;
            drop(collector.store);
            let pending = PendingScene::new(
                Admitted::from_parts(Arc::new(metadata), owner),
                RasterRoster::new(planes).unwrap(),
                pool.clone(),
                disk.clone(),
            )
            .unwrap();
            let original = NativeDocument::from_pending(pending).unwrap();
            let order: Vec<_> = original
                .metadata()
                .layers()
                .map(|l| l.id().to_string())
                .rev()
                .collect();
            let before: Vec<_> = original
                .metadata()
                .layers()
                .map(|l| {
                    (
                        l.id().to_string(),
                        original.source(l.pixels().slot()).unwrap().key(),
                    )
                })
                .collect();
            let bytes = disk.used().unwrap();
            let delta = original
                .capture_edit()
                .unwrap()
                .reorder_independent_layers(&order, &|| Ok(()))
                .unwrap();
            let current = original.adopt(delta).unwrap();
            assert_eq!(disk.used().unwrap(), bytes);
            for layer in current.metadata().layers() {
                assert_eq!(
                    current.source(layer.pixels().slot()).unwrap().key(),
                    before.iter().find(|(id, _)| id == layer.id()).unwrap().1
                );
            }
            let layer = current.metadata().layers().next().unwrap();
            assert_eq!(layer.name(), "Second");
            let view = current
                .source(layer.pixels().slot())
                .unwrap()
                .prepare_region(Region::new([0, 0, 1, 1]).unwrap(), &mut || Ok(()))
                .unwrap();
            let expected = if depth == 16 {
                [42223, 45678, 17, 65535]
            } else {
                [42223, 45678, 17, 65535]
                    .map(crate::raster::project16)
                    .map(|v| v as u16 * 257)
            };
            assert_eq!(view.pixel16(0, 0).unwrap(), expected);
            drop(view);
            let wire: Document36 =
                serde_json::from_slice(&serde_json::to_vec(&current.metadata()).unwrap()).unwrap();
            let mut ordinals = vec![];
            wire.visit_rasters(|r| {
                ordinals.push(r.id);
                Ok(())
            })
            .unwrap();
            assert_eq!(ordinals, (1..=8).collect::<Vec<_>>());
            assert_eq!(
                current.metadata().selection_coverage().unwrap().contours()[0][2],
                [8., 8.]
            );
            drop(current);
            drop(original);
            drop(sealed);
            drop(store);
            std::fs::remove_dir_all(path).unwrap();
            assert_eq!(pool.used(), 0);
        }
    }
}
