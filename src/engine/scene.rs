//! Explicit Source36 metadata and captured native-source roster. This module
//! grants neither file admission nor edit publication authority.
use super::{Document, Layer};
use crate::large_document::{DiskBudget, ResidentBudget};
use crate::psd::memory36::{Admitted, MetadataCharge};
use crate::raster::{Raster, Surface};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
#[path = "scene/bounds.rs"]
mod bounds;
#[path = "scene/capture.rs"]
mod capture;
#[path = "scene/icc.rs"]
pub(crate) mod icc;
#[path = "scene/payload.rs"]
pub(crate) mod payload;
pub use payload::MetadataPayload;
#[path = "scene/assets.rs"]
pub(crate) mod assets;
#[path = "scene/metadata.rs"]
mod metadata;
#[path = "scene/effect_wire.rs"]
mod effect_wire;
pub(crate) mod presentation;
#[path = "scene/runtime.rs"]
mod runtime;
pub(crate) mod source_copy;
pub(crate) use capture::legacy_coverage_bytes;
pub(crate) use capture::seal_native_sources;
pub(crate) use capture::{seal_decoded, stage_decoded, stage_resident_clipboard};
pub use capture::{CapturedScene, LegacyTileCache};
pub(crate) use capture::{StageRuntime, StageSession, StagedScene};

pub type Result<T> = std::result::Result<T, String>;
#[cfg(test)]
pub(crate) fn wire_copy<T: Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
    serde_json::from_slice(&serde_json::to_vec(value).unwrap()).unwrap()
}
pub const MAX_RASTER_LOCATIONS: usize = 65536;

/// Portable IDs index this strong-owner roster; SurfaceKey is deliberately not
/// the portable ID and is never deserialized from a Source36 document.
pub struct RasterRoster {
    entries: Vec<Surface>,
}
impl RasterRoster {
    pub fn new(entries: Vec<Surface>) -> Result<Self> {
        if entries.len() > MAX_RASTER_LOCATIONS {
            return Err("Too many native source locations".into());
        }
        Ok(Self { entries })
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub(crate) fn as_slice(&self) -> &[Surface] {
        &self.entries
    }
    pub(crate) fn descriptor_bytes(&self) -> Result<usize> {
        self.entries
            .capacity()
            .checked_mul(std::mem::size_of::<Surface>())
            .and_then(|bytes| bytes.checked_add(128))
            .ok_or_else(|| "Native roster footprint overflow".into())
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, reference: &RasterRef36) -> Result<&Surface> {
        let surface = reference
            .id
            .checked_sub(1)
            .and_then(|i| self.entries.get(i as usize))
            .ok_or("Missing native source location")?;
        if surface.width() != reference.width
            || surface.height() != reference.height
            || surface.depth() != reference.depth
        {
            return Err("Native source shape does not match its metadata reference".into());
        }
        Ok(surface)
    }
    pub(crate) fn ordinal(&self, ordinal: u32) -> Result<&Surface> {
        ordinal
            .checked_sub(1)
            .and_then(|index| self.entries.get(index as usize))
            .ok_or_else(|| "Missing captured native source slot".into())
    }
    pub fn iter(&self) -> impl Iterator<Item = (u32, &Surface)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, s)| (i as u32 + 1, s))
    }
}

/// Shared output for the codec/recovery paths after their complete source proofs.
/// This validates metadata/roster shape only; engine adoption remains separate.
pub struct PendingScene {
    roster: RasterRoster,
    // The roster/index payload drops before its final shared metadata charge.
    metadata: Admitted<Document36>,
    resident: ResidentBudget,
    disk: DiskBudget,
}
impl PendingScene {
    pub(crate) fn new(
        metadata: Admitted<Document36>,
        roster: RasterRoster,
        resident: ResidentBudget,
        disk: DiskBudget,
    ) -> Result<Self> {
        if !metadata.owner().uses_budget(&resident) {
            return Err("Native metadata uses a different resident admission pool".into());
        }
        let count = metadata.visit_rasters(|reference| {
            let source = roster.get(reference)?;
            if !source.uses_budget(&resident) || !source.uses_disk_budget(&disk) {
                return Err(
                    "Native source owner is outside the shared resident or disk admission".into(),
                );
            }
            Ok(())
        })?;
        if count != roster.len() {
            return Err("Native source roster contains unused locations".into());
        }
        Ok(Self {
            metadata,
            roster,
            resident,
            disk,
        })
    }
    pub fn metadata(&self) -> &Document36 {
        &self.metadata
    }
    pub fn rasters(&self) -> &RasterRoster {
        &self.roster
    }
    pub fn budget(&self) -> &ResidentBudget {
        &self.resident
    }
    pub fn disk_budget(&self) -> &DiskBudget {
        &self.disk
    }
    /// The consuming adopter moves fields while the charge remains inseparable
    /// from the resulting document/candidate. No bare metadata extraction.
    pub(crate) fn map_unique<U>(
        self,
        map: impl FnOnce(Document36, RasterRoster, &Arc<MetadataCharge>) -> Result<U>,
    ) -> Result<Admitted<U>> {
        let Self {
            metadata,
            roster,
            resident: _,
            disk: _,
        } = self;
        metadata.map_unique(|document, owner| map(document, roster, owner))
    }
    pub(crate) fn rehome_unique<U>(
        self,
        map: impl FnOnce(
            Document36,
            RasterRoster,
            &Arc<MetadataCharge>,
        ) -> Result<(U, Arc<MetadataCharge>)>,
    ) -> Result<Admitted<U>> {
        let Self {
            metadata,
            roster,
            resident: _,
            disk: _,
        } = self;
        metadata.rehome_unique(|document, owner| map(document, roster, owner))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneIdentity {
    pub project_id: String,
    pub document_id: String,
    pub revision: u64,
}

/// Read-only scene used by regional workers. File/lease owners are runtime-only;
/// no native Surface or filesystem path participates in metadata serialization.
pub struct SceneSnapshot {
    identity: SceneIdentity,
    leases: Arc<Vec<super::Lease>>,
    file_version: Option<crate::file_safety::Version>,
    _file_ownership: Option<Arc<crate::file_ownership::Ownership>>,
    // Protected runtime inspection is retained, never promoted to editable sources.
    runtime_views: RuntimeViews,
    // Metadata admission outlives the other captured metadata allocations.
    document: SnapshotDocument,
    binding: super::native::DocumentKey,
    // Shared lease payload and each snapshot's small header have independent
    // tickets, so preview chains never retain predecessor header admissions.
    _lease_charge: Option<Arc<MetadataCharge>>,
    _header_charge: Option<Arc<MetadataCharge>>,
}
enum SnapshotDocument {
    Pending(PendingScene),
    Native(super::native::NativeDocument),
    Presentation {
        base: super::native::NativeDocument,
        metadata: Admitted<Document36>,
    },
}
#[derive(Default)]
struct RuntimeViews {
    structure: Vec<crate::psd::SourceLayerInfo>,
    patterns: Vec<crate::psd::patterns::Source>,
}
fn native_scene_header(
    identity: &SceneIdentity,
    pool: &ResidentBudget,
) -> Result<Arc<MetadataCharge>> {
    let bytes = identity
        .project_id
        .capacity()
        .checked_add(identity.document_id.capacity())
        .and_then(|n| n.checked_add(1024))
        .ok_or("Native scene header admission overflow")?;
    MetadataCharge::reserve(pool, bytes, &|| Ok(()))
}
fn native_scene_leases(
    leases: &Vec<super::Lease>,
    pool: &ResidentBudget,
) -> Result<Arc<MetadataCharge>> {
    let mut bytes = leases
        .capacity()
        .checked_mul(std::mem::size_of::<super::Lease>())
        .and_then(|n| n.checked_add(512))
        .ok_or("Native scene lease admission overflow")?;
    for lease in leases {
        for length in [
            lease.id.capacity(),
            lease.owner.capacity(),
            lease.description.capacity(),
            lease
                .scopes
                .capacity()
                .checked_mul(std::mem::size_of::<super::Scope>())
                .ok_or("Native scope footprint overflow")?,
        ] {
            bytes = bytes
                .checked_add(length)
                .ok_or("Native lease footprint overflow")?;
        }
        for scope in &lease.scopes {
            bytes = bytes
                .checked_add(scope.target.as_ref().map_or(0, String::capacity))
                .ok_or("Native scope text footprint overflow")?;
        }
    }
    MetadataCharge::reserve(pool, bytes, &|| Ok(()))
}
impl SceneSnapshot {
    pub(crate) fn native_document(&self) -> Result<&super::native::NativeDocument> {
        match &self.document {
            SnapshotDocument::Native(document) => Ok(document),
            SnapshotDocument::Pending(_) | SnapshotDocument::Presentation { .. } => {
                Err("This scene has no editable native document authority".into())
            }
        }
    }
    /// Exact immutable source generation; document ID/revision alone cannot
    /// distinguish a captured in-progress gesture or a different native owner.
    pub(crate) fn key(&self) -> super::native::DocumentKey {
        self.binding
    }
    pub(crate) fn new(
        identity: SceneIdentity,
        pending: PendingScene,
        leases: Vec<super::Lease>,
        file_version: Option<crate::file_safety::Version>,
        file_ownership: Option<Arc<crate::file_ownership::Ownership>>,
    ) -> Result<Self> {
        if identity.document_id != pending.metadata.id
            || identity.revision != pending.metadata.revision
        {
            return Err("Captured scene identity does not match its metadata".into());
        }
        Ok(Self {
            identity,
            document: SnapshotDocument::Pending(pending),
            binding: super::native::key(),
            leases: Arc::new(leases),
            file_version,
            _file_ownership: file_ownership,
            runtime_views: RuntimeViews::default(),
            _lease_charge: None,
            _header_charge: None,
        })
    }
    pub fn identity(&self) -> &SceneIdentity {
        &self.identity
    }
    #[cfg(test)]
    pub(super) fn capture_bytes(&self) -> usize {
        self._lease_charge
            .iter()
            .chain(self._header_charge.iter())
            .map(|c| c.bytes())
            .sum()
    }
    /// Admit the live borrowed headers before copying any identity/lease payload.
    pub(crate) fn capture_native(
        project_id: &str,
        document: &super::native::NativeDocument,
        leases: &[super::Lease],
        file_version: Option<crate::file_safety::Version>,
        file_ownership: Option<Arc<crate::file_ownership::Ownership>>,
    ) -> Result<Self> {
        #[derive(Serialize)]
        struct Identity<'a> {
            project_id: &'a str,
            document_id: &'a str,
            revision: u64,
        }
        let pool = document.budget();
        let (identity, header_charge) = super::native::project_admitted(
            &Identity {
                project_id,
                document_id: document.metadata().id(),
                revision: document.metadata().revision(),
            },
            pool,
            false,
            &|| Ok(()),
            |identity: SceneIdentity, charge| (identity, charge),
        )?;
        let (leases, lease_charge) = super::native::project_admitted(
            &leases,
            pool,
            true,
            &|| Ok(()),
            |leases: Vec<super::Lease>, charge| (Arc::new(leases), charge),
        )?;
        Ok(Self {
            identity,
            leases,
            file_version,
            _file_ownership: file_ownership,
            runtime_views: RuntimeViews::default(),
            document: SnapshotDocument::Native(document.clone()),
            binding: document.key(),
            _lease_charge: Some(lease_charge),
            _header_charge: Some(header_charge),
        })
    }
    pub(crate) fn from_native(
        identity: SceneIdentity,
        document: super::native::NativeDocument,
        leases: Vec<super::Lease>,
        file_version: Option<crate::file_safety::Version>,
        file_ownership: Option<Arc<crate::file_ownership::Ownership>>,
    ) -> Result<Self> {
        if identity.document_id != document.metadata().id()
            || identity.revision != document.metadata().revision()
        {
            return Err("Native snapshot identity differs".into());
        }
        let binding = document.key();
        let header_charge = native_scene_header(&identity, document.budget())?;
        let lease_charge = native_scene_leases(&leases, document.budget())?;
        Ok(Self {
            identity,
            leases: Arc::new(leases),
            file_version,
            _file_ownership: file_ownership,
            runtime_views: RuntimeViews::default(),
            document: SnapshotDocument::Native(document),
            binding,
            _lease_charge: Some(lease_charge),
            _header_charge: Some(header_charge),
        })
    }
    /// Derive unpublished preview provenance from the exact captured base. The
    /// UI cannot replace project identity, leases or file ownership through this
    /// route. Drop this scene before aborting the prepared output sources.
    pub(crate) fn with_prepared_preview(
        &self,
        prepared: &super::native::worker::PreparedNative,
    ) -> Result<Self> {
        let base = prepared.base();
        let candidate = prepared.preview();
        if !matches!(self.document, SnapshotDocument::Native(_))
            || self.binding != prepared.base_key()
            || self.identity.document_id != base.metadata().id()
            || self.identity.revision != base.metadata().revision()
            || candidate.metadata().id() != base.metadata().id()
            || base.metadata().revision().checked_add(1) != Some(candidate.metadata().revision())
            || !self.budget().same_pool(candidate.budget())
            || !self.disk_budget().same_pool(candidate.disk_budget())
            || !self
                ._lease_charge
                .as_ref()
                .is_some_and(|owner| owner.uses_budget(self.budget()))
        {
            return Err("Prepared preview differs from its captured native scene".into());
        }
        if !self.runtime_views.structure.is_empty() || !self.runtime_views.patterns.is_empty() {
            return Err("Native preview requires guarded protected inspection owners".into());
        }
        let bytes = self
            .identity
            .project_id
            .len()
            .checked_add(self.identity.document_id.len())
            .and_then(|n| n.checked_add(1024))
            .ok_or("Preview header admission overflow")?;
        let charge = MetadataCharge::reserve(candidate.budget(), bytes, &|| Ok(()))?;
        let copy = |value: &str| -> Result<String> {
            let mut out = String::new();
            out.try_reserve_exact(value.len())
                .map_err(|_| "Preview identity allocation refused")?;
            if out.capacity() > value.len() {
                return Err("Preview identity exceeds admission".into());
            }
            out.push_str(value);
            Ok(out)
        };
        Ok(Self {
            identity: SceneIdentity {
                project_id: copy(&self.identity.project_id)?,
                document_id: copy(&self.identity.document_id)?,
                revision: candidate.metadata().revision(),
            },
            leases: self.leases.clone(),
            file_version: self.file_version,
            _file_ownership: self._file_ownership.clone(),
            runtime_views: RuntimeViews::default(),
            document: SnapshotDocument::Native(candidate.clone()),
            binding: candidate.key(),
            _lease_charge: self._lease_charge.clone(),
            _header_charge: Some(charge),
        })
    }
    pub fn metadata(&self) -> super::native::Metadata<'_> {
        match &self.document {
            SnapshotDocument::Pending(pending) => {
                super::native::Metadata::from_wire(pending.metadata(), self.binding)
            }
            SnapshotDocument::Native(document) => document.metadata(),
            SnapshotDocument::Presentation { metadata, .. } => {
                super::native::Metadata::from_wire(metadata, self.binding)
            }
        }
    }
    pub fn rasters(&self) -> &RasterRoster {
        match &self.document {
            SnapshotDocument::Pending(p) => p.rasters(),
            SnapshotDocument::Native(n) => n.roster(),
            SnapshotDocument::Presentation { base, .. } => base.roster(),
        }
    }
    pub fn raster(&self, slot: super::native::RasterSlot) -> Result<&Surface> {
        if slot.document != self.binding {
            return Err("Native slot belongs to another captured generation".into());
        }
        match &self.document {
            SnapshotDocument::Native(document) => document.source(slot),
            SnapshotDocument::Pending(pending) => pending.roster.ordinal(slot.ordinal),
            SnapshotDocument::Presentation { base, .. } => base.roster().ordinal(slot.ordinal),
        }
    }
    pub fn budget(&self) -> &ResidentBudget {
        match &self.document {
            SnapshotDocument::Pending(p) => p.budget(),
            SnapshotDocument::Native(n) => n.budget(),
            SnapshotDocument::Presentation { base, .. } => base.budget(),
        }
    }
    pub fn disk_budget(&self) -> &DiskBudget {
        match &self.document {
            SnapshotDocument::Pending(p) => p.disk_budget(),
            SnapshotDocument::Native(n) => n.disk_budget(),
            SnapshotDocument::Presentation { base, .. } => base.disk_budget(),
        }
    }
    pub fn leases(&self) -> &[super::Lease] {
        &self.leases
    }
    pub fn file_version(&self) -> Option<crate::file_safety::Version> {
        self.file_version
    }
    pub fn source_structure(&self) -> &[crate::psd::SourceLayerInfo] {
        match &self.document {
            SnapshotDocument::Native(document) => document.protected_structure().unwrap_or_default(),
            _ => &self.runtime_views.structure,
        }
    }
    pub fn source_patterns(&self) -> &[crate::psd::patterns::Source] {
        &self.runtime_views.patterns
    }
    pub(crate) fn validate_provisional_editable(
        &self,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let SnapshotDocument::Pending(pending) = &self.document else {
            return Err("Editable import validation requires a provisional scene".into());
        };
        super::native::validation::validate_editable(
            pending.metadata(),
            pending.rasters(),
            pending.budget(),
            self.binding,
            check,
        )
    }
    /// Codec-only provisional handoff. Live guards/inspection owners must never
    /// be discarded through this path, and Native authority cannot be demoted.
    pub(crate) fn into_provisional_pending(self) -> Result<PendingScene> {
        if !self.leases.is_empty()
            || self.file_version.is_some()
            || self._file_ownership.is_some()
            || !self.runtime_views.structure.is_empty()
            || !self.runtime_views.patterns.is_empty()
        {
            return Err(
                "Captured runtime guards cannot be discarded for provisional import".into(),
            );
        }
        match self.document {
            SnapshotDocument::Pending(pending) => Ok(pending),
            SnapshotDocument::Native(_) | SnapshotDocument::Presentation { .. } => {
                Err("Native document cannot become provisional import data".into())
            }
        }
    }

    /// Actual captured metadata and physical source identities, including gesture
    /// pixels whose owner changed without a committed document revision.
    pub fn render_key(&self) -> Result<[u8; 32]> {
        if let SnapshotDocument::Native(document) = &self.document {
            return document.render_key();
        }
        hash_metadata(&self.metadata(), self.rasters())
    }
}
pub(super) fn hash_metadata(metadata: &impl Serialize, roster: &RasterRoster) -> Result<[u8; 32]> {
    use std::io::Write;
    struct KeyWriter {
        hash: ring::digest::Context,
        remaining: usize,
    }
    impl Write for KeyWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(std::io::Error::other(
                    "Captured scene metadata exceeds its bounded identity",
                ));
            }
            self.remaining -= bytes.len();
            self.hash.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = KeyWriter {
        hash: ring::digest::Context::new(&ring::digest::SHA256),
        remaining: 64 * 1024 * 1024,
    };
    writer.hash.update(b"PeerBrush captured scene v1\0");
    serde_json::to_writer(&mut writer, metadata).map_err(|e| e.to_string())?;
    for (id, surface) in roster.iter() {
        writer.hash.update(&id.to_be_bytes());
        writer.hash.update(&surface.key().bytes());
    }
    Ok(writer.hash.finish().as_ref().try_into().unwrap())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RasterRef36 {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub retained: Option<Box<Retained36>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retained36 {
    pub pixels: RasterRef36,
    pub matrix: [f64; 6],
    pub projective: Option<[f64; 9]>,
    pub warps: Vec<Warp36>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warp36 {
    pub before: [f64; 9],
    pub grid: Grid36,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grid36 {
    pub rect: [f64; 4],
    pub points: [[f64; 2]; 9],
}
impl Warp36 {
    pub fn capture(stage: &crate::warp::Stage) -> Self {
        let crate::warp::Stage { before, grid } = stage;
        let crate::warp::Grid { rect, points } = grid;
        Self {
            before: *before,
            grid: Grid36 {
                rect: *rect,
                points: *points,
            },
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage36 {
    pub mask: RasterRef36,
    pub bounds: [i32; 4],
    pub origin: [i32; 2],
    #[serde(deserialize_with = "payload::contours")]
    pub contours: MetadataPayload<Vec<Vec<[f32; 2]>>>,
}
#[derive(Debug)]
pub struct Effect36 {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    pub weight: f32,
    pub settings: MetadataPayload<Value>,
    pub clips: Vec<EffectClip36>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectClip36 {
    pub stroke: u32,
    pub coverage: Coverage36,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskStep36 {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    pub weight: f32,
    pub value: f32,
    pub pixels: RasterRef36,
    pub settings: MetadataPayload<Value>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mask36 {
    pub enabled: bool,
    pub linked: bool,
    pub steps: Vec<MaskStep36>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer36 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub clip_to: Option<String>,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub blend: String,
    pub x: i32,
    pub y: i32,
    pub pixels: RasterRef36,
    pub color: [u8; 4],
    pub mask: Option<Mask36>,
    pub effects: Vec<Effect36>,
    pub psd_metadata: MetadataPayload<Vec<crate::psd::LayerMetadata>>,
    pub source: Option<Source36>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source36 {
    pub width: u32,
    pub height: u32,
    pub matrix: [f32; 6],
    pub content: MetadataPayload<Content36>,
}
/// Exhaustive mirror: a future native-source variant requires an explicit wire
/// decision instead of inheriting serialization of new fields accidentally.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content36 {
    Bezier {
        nodes: Vec<crate::path::Anchor>,
        closed: bool,
        fill: [u16; 4],
        stroke: [u16; 4],
        stroke_width: f32,
    },
    Text {
        text: String,
        layout: crate::source::TextLayout,
        font: crate::fonts::TextFont,
        size: f32,
        line_height: f32,
        letter_spacing: f32,
        align: String,
        color: [u16; 4],
    },
    Shape {
        shape: String,
        bounds: [f32; 4],
        points: Vec<[f32; 2]>,
        closed: bool,
        fill: [u16; 4],
        stroke: [u16; 4],
        stroke_width: f32,
    },
    Svg {
        svg: String,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document36 {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub bit_depth: u16,
    #[serde(with = "icc")]
    pub icc_profile: Option<MetadataPayload<Vec<u8>>>,
    pub srgb_tagged: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub psd_intentionally_untagged: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub psd_rgb8_multiply: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_resolution: Option<[u8; 16]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_print_scale: Option<[u8; 14]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_pixel_aspect: Option<[u8; 12]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_grid: Option<[u8; 16]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_layer_id_seed: Option<[u8; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_print_flags: Option<[u8; 9]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_print_flags_info: Option<[u8; 10]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_copyright: Option<[u8; 1]>,
    #[serde(default, with = "icc::file_info", skip_serializing_if = "Option::is_none")]
    pub psd_file_info: Option<MetadataPayload<Vec<u8>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_global_angle: Option<[u8; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_global_altitude: Option<[u8; 4]>,
    pub psd_global_mask: Option<MetadataPayload<crate::psd::GlobalMaskSettings>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_global_metadata: Option<MetadataPayload<Vec<crate::psd::GlobalMetadata>>>,
    pub layers: Vec<Layer36>,
    pub filters: Vec<Effect36>,
    pub selection: Option<[i32; 4]>,
    #[serde(default, deserialize_with = "payload::polygon")]
    pub selection_polygon: Option<MetadataPayload<Vec<[f32; 2]>>>,
    pub selection_coverage: Option<Coverage36>,
    pub selection_previous: Option<Coverage36>,
    pub read_only: bool,
    pub warnings: MetadataPayload<Vec<String>>,
}

/// Implementations capture handles/provenance only. They may not serialize a
/// Raster/Surface or perform backing I/O while the engine/root lock is held.
pub trait RasterCollector {
    fn resident(&mut self, raster: &Raster) -> Result<RasterRef36>;
    fn surface(&mut self, surface: &Surface) -> Result<RasterRef36>;
    /// Known legacy wire clips need explicit bounded decoding/staging. A default
    /// refusal prevents a hidden pixel source escaping through opaque settings.
    fn embedded_coverage(&mut self, _: &Value) -> Result<Coverage36> {
        Err("Liquify coverage requires explicit admitted source collection".into())
    }
}
pub trait CapturePixels {
    fn capture<C: RasterCollector>(&self, collector: &mut C) -> Result<RasterRef36>;
}
/// Borrowed coverage metadata keeps contour ownership with its source object.
pub trait CaptureCoverage {
    type Pixels: CapturePixels;
    fn mask(&self) -> &Self::Pixels;
    fn bounds(&self) -> [i32; 4];
    fn origin(&self) -> [i32; 2];
    fn contours(&self) -> &[Vec<[f32; 2]>];
}
impl CaptureCoverage for crate::selection::Coverage {
    type Pixels = Raster;
    fn mask(&self) -> &Raster {
        &self.mask
    }
    fn bounds(&self) -> [i32; 4] {
        self.bounds
    }
    fn origin(&self) -> [i32; 2] {
        self.origin
    }
    fn contours(&self) -> &[Vec<[f32; 2]>] {
        &self.contours
    }
}
impl CapturePixels for Raster {
    fn capture<C: RasterCollector>(&self, c: &mut C) -> Result<RasterRef36> {
        c.resident(self)
    }
}
impl CapturePixels for Surface {
    fn capture<C: RasterCollector>(&self, c: &mut C) -> Result<RasterRef36> {
        c.surface(self)
    }
}

impl Document36 {
    /// Mutation traversal for a provisional native header. A rebuilt roster and
    /// the strict immutable traversal must validate the result before adoption.
    pub(crate) fn remap_rasters(
        &mut self,
        mut visit: impl FnMut(&mut RasterRef36) -> Result<()>,
    ) -> Result<()> {
        for layer in &mut self.layers {
            layer.remap_rasters(&mut visit)?;
        }
        for effect in &mut self.filters {
            effect.checked_clips()?;
            for clip in &mut effect.clips {
                remap_raster(&mut clip.coverage.mask, &mut visit)?;
            }
        }
        for coverage in [&mut self.selection_coverage, &mut self.selection_previous]
            .into_iter()
            .flatten()
        {
            remap_raster(&mut coverage.mask, &mut visit)?;
        }
        Ok(())
    }
    /// Source36 DTO construction is metadata-only. The caller first captures an
    /// immutable document, then runs this outside engine/root/registry locks.
    pub fn capture(doc: &Document, collector: &mut impl RasterCollector) -> Result<Self> {
        Ok(Self {
            id: doc.id.clone(),
            name: doc.name.clone(),
            width: doc.width,
            height: doc.height,
            revision: doc.revision,
            bit_depth: doc.bit_depth,
            icc_profile: doc
                .icc_profile
                .as_ref()
                .map(|value| MetadataPayload::from((**value).clone())),
            srgb_tagged: doc.srgb_tagged,
            psd_intentionally_untagged: doc.psd_intentionally_untagged,
            psd_rgb8_multiply: doc.psd_rgb8_multiply,
            psd_resolution: doc.psd_resolution,
            psd_print_scale: doc.psd_print_scale,
            psd_pixel_aspect: doc.psd_pixel_aspect,
            psd_grid: doc.psd_grid,
            psd_layer_id_seed: doc.psd_layer_id_seed,
            psd_print_flags: doc.psd_print_flags,
            psd_print_flags_info: doc.psd_print_flags_info,
            psd_copyright: doc.psd_copyright,
            psd_file_info: doc.psd_file_info.as_deref().cloned().map(MetadataPayload::from),
            psd_global_angle: doc.psd_global_angle,
            psd_global_altitude: doc.psd_global_altitude,
            psd_global_mask: doc.psd_global_mask.clone().map(MetadataPayload::from),
            psd_global_metadata: (!doc.psd_global_metadata.is_empty())
                .then(|| doc.psd_global_metadata.clone().into()),
            layers: doc
                .layers
                .iter()
                .map(|l| Layer36::capture(l, collector))
                .collect::<Result<_>>()?,
            filters: doc
                .filters
                .iter()
                .map(|e| Effect36::capture(e, collector))
                .collect::<Result<_>>()?,
            selection: doc.selection,
            selection_polygon: doc.selection_polygon.clone().map(MetadataPayload::from),
            selection_coverage: doc
                .selection_coverage
                .as_ref()
                .map(|m| Coverage36::from_capture(m, collector))
                .transpose()?,
            selection_previous: doc
                .selection_previous
                .as_ref()
                .map(|m| Coverage36::from_capture(m, collector))
                .transpose()?,
            read_only: doc.read_only,
            warnings: doc.warnings.clone().into(),
        })
    }

    /// Exact deterministic native-location order, including original sources.
    /// The ordinal rule rejects duplicate, unused, aliased or cyclic wire IDs.
    pub fn visit_rasters<'a>(
        &'a self,
        mut visit: impl FnMut(&'a RasterRef36) -> Result<()>,
    ) -> Result<usize> {
        let mut next = 1u32;
        let mut walk = |r: &'a RasterRef36| -> Result<()> {
            visit_one(r, &mut next, &mut visit)?;
            if let Some(original) = &r.retained {
                visit_one(&original.pixels, &mut next, &mut visit)?;
            }
            Ok(())
        };
        for layer in &self.layers {
            walk(&layer.pixels)?;
            if let Some(mask) = &layer.mask {
                for step in &mask.steps {
                    walk(&step.pixels)?;
                }
            }
            for effect in &layer.effects {
                for clip in effect.checked_clips()? {
                    walk(&clip.coverage.mask)?;
                }
            }
        }
        for effect in &self.filters {
            for clip in effect.checked_clips()? {
                walk(&clip.coverage.mask)?;
            }
        }
        for coverage in [
            self.selection_coverage.as_ref(),
            self.selection_previous.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            walk(&coverage.mask)?;
        }
        Ok(next as usize - 1)
    }
}
fn visit_one<'a>(
    r: &'a RasterRef36,
    next: &mut u32,
    visit: &mut impl FnMut(&'a RasterRef36) -> Result<()>,
) -> Result<()> {
    if r.id != *next
        || *next as usize > MAX_RASTER_LOCATIONS
        || r.width == 0
        || r.height == 0
        || r.width > 16000
        || r.height > 16000
        || ![8, 16].contains(&r.depth)
    {
        return Err("Invalid ordered native source reference".into());
    }
    if let Some(original) = &r.retained {
        if original.pixels.retained.is_some() || original.pixels.depth != r.depth {
            return Err("Invalid nested or mismatched native original".into());
        }
    }
    *next += 1;
    visit(r)
}
fn remap_raster(
    reference: &mut RasterRef36,
    visit: &mut impl FnMut(&mut RasterRef36) -> Result<()>,
) -> Result<()> {
    visit(reference)?;
    if let Some(original) = &mut reference.retained {
        visit(&mut original.pixels)?;
    }
    Ok(())
}
impl Layer36 {
    pub(crate) fn remap_rasters(
        &mut self,
        mut visit: impl FnMut(&mut RasterRef36) -> Result<()>,
    ) -> Result<()> {
        remap_raster(&mut self.pixels, &mut visit)?;
        if let Some(mask) = &mut self.mask {
            for step in &mut mask.steps {
                remap_raster(&mut step.pixels, &mut visit)?;
            }
        }
        for effect in &mut self.effects {
            effect.checked_clips()?;
            for clip in &mut effect.clips {
                remap_raster(&mut clip.coverage.mask, &mut visit)?;
            }
        }
        Ok(())
    }
    fn capture(layer: &Layer, c: &mut impl RasterCollector) -> Result<Self> {
        Ok(Self {
            id: layer.id.clone(),
            name: layer.name.clone(),
            kind: layer.kind.clone(),
            parent: layer.parent.clone(),
            clip_to: layer.clip_to.clone(),
            visible: layer.visible,
            locked: layer.locked,
            opacity: layer.opacity,
            blend: layer.blend.clone(),
            x: layer.x,
            y: layer.y,
            pixels: layer.pixels.capture(c)?,
            color: layer.color,
            mask: layer
                .mask
                .as_ref()
                .map(|m| -> Result<Mask36> {
                    Ok(Mask36 {
                        enabled: m.enabled,
                        linked: m.linked,
                        steps: m
                            .steps
                            .iter()
                            .map(|s| {
                                Ok(MaskStep36 {
                                    id: s.id.clone(),
                                    kind: s.kind.clone(),
                                    enabled: s.enabled,
                                    weight: s.weight,
                                    value: s.value,
                                    pixels: s.pixels.capture(c)?,
                                    settings: s.settings.clone().into(),
                                })
                            })
                            .collect::<Result<_>>()?,
                    })
                })
                .transpose()?,
            effects: layer
                .effects
                .iter()
                .map(|e| Effect36::capture(e, c))
                .collect::<Result<_>>()?,
            psd_metadata: layer.psd_metadata.clone().into(),
            source: layer.source.as_ref().map(|s| Source36::capture(s)),
        })
    }
}
impl Coverage36 {
    pub fn from_capture(
        value: &impl CaptureCoverage,
        c: &mut impl RasterCollector,
    ) -> Result<Self> {
        Self::capture(
            value.mask(),
            value.bounds(),
            value.origin(),
            value.contours(),
            c,
        )
    }
    fn capture(
        mask: &impl CapturePixels,
        bounds: [i32; 4],
        origin: [i32; 2],
        contours: &[Vec<[f32; 2]>],
        c: &mut impl RasterCollector,
    ) -> Result<Self> {
        Ok(Self {
            mask: mask.capture(c)?,
            bounds,
            origin,
            contours: contours.to_vec().into(),
        })
    }
}
impl Effect36 {
    fn capture(effect: &crate::effects::Effect, c: &mut impl RasterCollector) -> Result<Self> {
        let (settings, clips) = Self::capture_settings(&effect.kind, &effect.settings, c)?;
        Ok(Self {
            id: effect.id.clone(),
            kind: effect.kind.clone(),
            enabled: effect.enabled,
            weight: effect.weight,
            settings: settings.into(),
            clips,
        })
    }
    pub(crate) fn capture_settings(
        kind: &str,
        settings: &Value,
        c: &mut impl RasterCollector,
    ) -> Result<(Value, Vec<EffectClip36>)> {
        let mut clips = vec![];
        let settings = if kind == "liquify" {
            let object = settings
                .as_object()
                .ok_or("Liquify settings must be an object")?;
            let mut output = serde_json::Map::new();
            // Never clone a settings tree that still contains native coverage.
            for (key, value) in object {
                if key != "strokes" {
                    output.insert(key.clone(), value.clone());
                    continue;
                }
                let strokes = value
                    .as_array()
                    .filter(|s| s.len() <= 128)
                    .ok_or("Invalid bounded Liquify stroke metadata")?;
                let mut rows = Vec::new();
                rows.try_reserve_exact(strokes.len())
                    .map_err(|_| "Liquify metadata allocation refused")?;
                for (index, stroke) in strokes.iter().enumerate() {
                    let input = stroke
                        .as_object()
                        .ok_or("Invalid Liquify stroke metadata")?;
                    let mut row = serde_json::Map::new();
                    for (field, value) in input {
                        if field == "coverage" && !value.is_null() {
                            clips.push(EffectClip36 {
                                stroke: index as u32,
                                coverage: c.embedded_coverage(value)?,
                            });
                        } else {
                            row.insert(field.clone(), value.clone());
                        }
                    }
                    rows.push(Value::Object(row));
                }
                output.insert(key.clone(), Value::Array(rows));
            }
            Value::Object(output)
        } else {
            settings.clone()
        };
        Ok((settings, clips))
    }
    fn checked_clips(&self) -> Result<&[EffectClip36]> {
        if self.kind != "liquify" {
            return if self.clips.is_empty() {
                Ok(&self.clips)
            } else {
                Err("Native clips require a recognized Liquify source".into())
            };
        }
        let strokes = self.settings.get("strokes").and_then(Value::as_array);
        if strokes.is_some_and(|s| {
            s.iter()
                .any(|s| s.get("coverage").is_some_and(|v| !v.is_null()))
        }) {
            return Err("Inline Liquify coverage is forbidden in Source36 metadata".into());
        }
        let mut previous = None;
        for clip in &self.clips {
            if clip.stroke as usize >= strokes.map_or(0, Vec::len)
                || previous.is_some_and(|p| p >= clip.stroke)
            {
                return Err("Invalid ordered Liquify clip source".into());
            }
            previous = Some(clip.stroke);
        }
        Ok(&self.clips)
    }
}
impl Source36 {
    pub fn definition(&self) -> &Content36 {
        &self.content
    }
    pub(in crate::engine) fn to_resident(&self) -> crate::source::Source {
        use crate::source::{Content, Source};
        let content = match self.definition() {
            Content36::Bezier {
                nodes,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Content::Bezier {
                nodes: nodes.clone(),
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                stroke_width: *stroke_width,
            },
            Content36::Text {
                text,
                layout,
                font,
                size,
                line_height,
                letter_spacing,
                align,
                color,
            } => Content::Text {
                text: text.clone(),
                layout: *layout,
                font: font.clone(),
                size: *size,
                line_height: *line_height,
                letter_spacing: *letter_spacing,
                align: align.clone(),
                color: *color,
            },
            Content36::Shape {
                shape,
                bounds,
                points,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Content::Shape {
                shape: shape.clone(),
                bounds: *bounds,
                points: points.clone(),
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                stroke_width: *stroke_width,
            },
            Content36::Svg { svg } => Content::Svg { svg: svg.clone() },
        };
        Source {
            width: self.width,
            height: self.height,
            matrix: self.matrix,
            content,
        }
    }
    fn capture(source: &crate::source::Source) -> Self {
        Self::from_source(source.clone())
    }
    /// Move a parsed command definition without another unadmitted deep copy.
    pub(crate) fn from_source(source: crate::source::Source) -> Self {
        use crate::source::Content;
        let content = match source.content {
            Content::Bezier {
                nodes,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Content36::Bezier {
                nodes,
                closed,
                fill,
                stroke,
                stroke_width,
            },
            Content::Text {
                text,
                layout,
                font,
                size,
                line_height,
                letter_spacing,
                align,
                color,
            } => Content36::Text {
                text,
                layout,
                font,
                size,
                line_height,
                letter_spacing,
                align,
                color,
            },
            Content::Shape {
                shape,
                bounds,
                points,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Content36::Shape {
                shape,
                bounds,
                points,
                closed,
                fill,
                stroke,
                stroke_width,
            },
            Content::Svg { svg } => Content36::Svg { svg },
        };
        Self {
            width: source.width,
            height: source.height,
            matrix: source.matrix,
            content: content.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::Raster;

    #[derive(Default)]
    struct Capture {
        planes: Vec<Raster>,
    }
    fn admitted(metadata: Document36, budget: &ResidentBudget) -> Admitted<Document36> {
        let owner = MetadataCharge::reserve(budget, 64 * 1024, &|| Ok(())).unwrap();
        Admitted::from_parts(Arc::new(metadata), owner)
    }
    impl RasterCollector for Capture {
        fn resident(&mut self, r: &Raster) -> Result<RasterRef36> {
            r.validate_layout()?;
            if self.planes.len() >= MAX_RASTER_LOCATIONS {
                return Err("Too many sources".into());
            }
            let id = self.planes.len() as u32 + 1;
            let mut current = r.clone();
            current.retained = None;
            self.planes.push(current);
            let retained = r
                .retained
                .as_ref()
                .map(|o| -> Result<Box<Retained36>> {
                    Ok(Box::new(Retained36 {
                        pixels: self.resident(&o.pixels)?,
                        matrix: o.matrix,
                        projective: o.projective,
                        warps: o.warps.iter().map(Warp36::capture).collect(),
                    }))
                })
                .transpose()?;
            Ok(RasterRef36 {
                id,
                width: r.width,
                height: r.height,
                depth: r.depth,
                retained,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
            Err("Resident fixture only".into())
        }
        fn embedded_coverage(&mut self, value: &Value) -> Result<Coverage36> {
            let coverage = crate::selection::Coverage::deserialize(value)
                .map_err(|_| "Invalid fixture coverage")?;
            coverage.validate()?;
            Coverage36::from_capture(&coverage, self)
        }
    }
    fn doc() -> Document {
        let mut doc = Document::new_depth(256, 256, 16).unwrap();
        doc.layers[0]
            .pixels
            .set16(2, 3, [12345, 23456, 34567, 65535]);
        let original = doc.layers[0].pixels.clone();
        doc.layers[0].pixels.retained = Some(Box::new(crate::retained::Original {
            pixels: Box::new(original),
            matrix: crate::retained::IDENTITY,
            projective: None,
            warps: vec![],
        }));
        doc.layers[0].mask = Some(super::super::Mask {
            enabled: true,
            linked: true,
            cache_key: super::super::id(),
            steps: vec![super::super::MaskStep {
                id: super::super::id(),
                kind: "paint".into(),
                enabled: true,
                weight: 0.75,
                value: 255.,
                pixels: Raster::new_depth(256, 256, 16),
                settings: serde_json::json!({}),
            }],
        });
        let coverage = crate::selection::Coverage {
            mask: Raster::new(256, 256),
            bounds: [1, 2, 4, 5],
            origin: [1, 2],
            contours: vec![vec![[1., 2.], [4., 2.], [4., 5.]]],
        };
        doc.selection = Some(coverage.bounds);
        doc.selection_coverage = Some(coverage.clone());
        doc.selection_previous = Some(coverage);
        doc
    }
    #[test]
    fn metadata_capture_covers_all_native_locations_without_pixel_serialization() {
        let source = doc();
        let mut c = Capture::default();
        let metadata = Document36::capture(&source, &mut c).unwrap();
        let mut ids = vec![];
        let count = metadata
            .visit_rasters(|r| {
                ids.push(r.id);
                Ok(())
            })
            .unwrap();
        assert_eq!(count, 5);
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);
        assert_eq!(c.planes.len(), 5);
        assert_eq!(c.planes[0].get16(2, 3), [12345, 23456, 34567, 65535]);
        let bytes = serde_json::to_vec(&metadata).unwrap();
        assert!(bytes.len() < 16384);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("samples16"));
        assert!(!text.contains("\"tiles\""));
        assert!(!text.contains("\"rgba\""));
        let roundtrip: Document36 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&roundtrip).unwrap(), bytes);
        assert_eq!(
            roundtrip.layers[0].pixels.retained.as_ref().unwrap().matrix,
            crate::retained::IDENTITY
        );
    }
    #[test]
    fn duplicate_nested_mismatched_and_unused_references_fail_closed() {
        let mut c = Capture::default();
        let metadata = Document36::capture(&doc(), &mut c).unwrap();
        let mut duplicate = wire_copy(&metadata);
        duplicate.layers[0]
            .pixels
            .retained
            .as_mut()
            .unwrap()
            .pixels
            .id = 1;
        assert!(duplicate.visit_rasters(|_| Ok(())).is_err());
        let mut mismatch = wire_copy(&metadata);
        mismatch.layers[0]
            .pixels
            .retained
            .as_mut()
            .unwrap()
            .pixels
            .depth = 8;
        assert!(mismatch.visit_rasters(|_| Ok(())).is_err());
        let resident = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 16).unwrap();
        let roster = RasterRoster::new(vec![Surface::zero(256, 256, 16).unwrap()]).unwrap();
        assert!(PendingScene::new(
            admitted(wire_copy(&metadata), &resident),
            roster,
            resident.clone(),
            disk.clone()
        )
        .is_err());
        let mut simple =
            Document36::capture(&Document::new(4, 4).unwrap(), &mut Capture::default()).unwrap();
        let roster = RasterRoster::new(vec![
            Surface::zero(4, 4, 8).unwrap(),
            Surface::zero(4, 4, 8).unwrap(),
        ])
        .unwrap();
        assert!(PendingScene::new(
            admitted(wire_copy(&simple), &resident),
            roster,
            resident.clone(),
            disk.clone()
        )
        .is_err());
        simple.layers[0].pixels.width = 5;
        let roster = RasterRoster::new(vec![Surface::zero(4, 4, 8).unwrap()]).unwrap();
        assert!(PendingScene::new(admitted(simple, &resident), roster, resident, disk).is_err());
    }
    #[test]
    fn zero_scene_admission_and_snapshot_do_not_create_source_files() {
        let document = Document::new(4, 4).unwrap();
        let mut capture = Capture::default();
        let metadata = Document36::capture(&document, &mut capture).unwrap();
        let identity = SceneIdentity {
            project_id: super::super::id(),
            document_id: metadata.id.clone(),
            revision: metadata.revision,
        };
        let resident = ResidentBudget::new(1024 * 1024).unwrap();
        let disk = DiskBudget::new(1024 * 1024, 16).unwrap();
        let roster = RasterRoster::new(vec![Surface::zero(4, 4, 8).unwrap()]).unwrap();
        let pending = PendingScene::new(
            admitted(metadata, &resident),
            roster,
            resident.clone(),
            disk.clone(),
        )
        .unwrap();
        let snapshot = SceneSnapshot::new(identity.clone(), pending, vec![], None, None).unwrap();
        assert_eq!(snapshot.identity(), &identity);
        assert_eq!(snapshot.metadata().layers().len(), 1);
        assert_eq!(resident.used(), 64 * 1024);
        assert_eq!(disk.used().unwrap(), 0);
        let pending = snapshot.into_provisional_pending().unwrap();
        assert_eq!(pending.rasters().len(), 1);
        assert_eq!(resident.used(), 64 * 1024);
        drop(pending);
        assert_eq!(resident.used(), 0);
    }
    #[test]
    fn typed_metadata_rejects_unknown_fields() {
        let metadata =
            Document36::capture(&Document::new(4, 4).unwrap(), &mut Capture::default()).unwrap();
        let mut value = serde_json::to_value(&metadata).unwrap();
        value["layers"][0]["unexpected_native_bytes"] = serde_json::json!([1, 2, 3]);
        assert!(serde_json::from_value::<Document36>(value).is_err());
    }
    #[test]
    fn provisional_handoff_refuses_reservation_and_protected_inspection_owners() {
        let resident = ResidentBudget::new(1024 * 1024).unwrap();
        let make = || {
            let mut capture = Capture::default();
            let metadata =
                Document36::capture(&Document::new(4, 4).unwrap(), &mut capture).unwrap();
            let identity = SceneIdentity {
                project_id: super::super::id(),
                document_id: metadata.id.clone(),
                revision: metadata.revision,
            };
            let pending = PendingScene::new(
                admitted(metadata, &resident),
                RasterRoster::new(vec![Surface::zero(4, 4, 8).unwrap()]).unwrap(),
                resident.clone(),
                DiskBudget::new(1024 * 1024, 8).unwrap(),
            )
            .unwrap();
            SceneSnapshot::new(identity, pending, vec![], None, None).unwrap()
        };
        let mut leased = make();
        leased.leases = Arc::new(vec![super::super::Lease {
            id: "held".into(),
            owner: "human".into(),
            description: "held".into(),
            scopes: vec![],
            expires: u64::MAX,
        }]);
        assert!(leased.into_provisional_pending().is_err());
        assert_eq!(resident.used(), 0);
        let mut inspected = make();
        inspected
            .runtime_views
            .structure
            .push(crate::psd::SourceLayerInfo {
                source_index: 0,
                parent_index: None,
                name: "Protected".into(),
                kind: "paint".into(),
                bounds: [0, 0, 4, 4],
                visible: true,
                has_mask: false,
                tags: vec![],
                warnings: vec![],
                details_truncated: false,
            });
        assert!(inspected.into_provisional_pending().is_err());
        assert_eq!(resident.used(), 0);
    }
    #[test]
    fn liquify_clip_pixels_are_roster_sources_never_inline_metadata() {
        let mut doc = Document::new(4, 4).unwrap();
        let mut mask = Raster::new(4, 4);
        mask.set(1, 1, [127; 4]);
        let coverage = crate::selection::Coverage {
            mask,
            bounds: [0, 0, 4, 4],
            origin: [0, 0],
            contours: vec![],
        };
        let settings = serde_json::json!({"radius":3.0,"strength":0.75,"strokes":[{"mode":"push","points":[[1,1],[2,2]],"coverage":coverage}]});
        doc.layers[0].effects.push(crate::effects::Effect {
            id: super::super::id(),
            kind: "liquify".into(),
            enabled: true,
            weight: 1.0,
            settings: Arc::new(settings),
        });
        let mut c = Capture::default();
        let metadata = Document36::capture(&doc, &mut c).unwrap();
        assert_eq!(metadata.layers[0].effects[0].clips.len(), 1);
        assert!(metadata.layers[0].effects[0].settings["strokes"][0]
            .get("coverage")
            .is_none());
        assert_eq!(metadata.visit_rasters(|_| Ok(())).unwrap(), 2);
        assert_eq!(c.planes[1].get(1, 1), [127; 4]);
        let bytes = serde_json::to_vec(&metadata).unwrap();
        assert!(bytes.len() < 8192);
        let mut invalid = wire_copy(&metadata);
        invalid.layers[0].effects[0].settings = (*doc.layers[0].effects[0].settings).clone().into();
        assert!(invalid.visit_rasters(|_| Ok(())).is_err());
    }
}
