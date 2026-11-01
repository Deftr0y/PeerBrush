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
pub use capture::{CapturedScene, LegacyTileCache};
pub(crate) use capture::{StageRuntime, StageSession, StagedScene};

pub type Result<T> = std::result::Result<T, String>;
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pending: PendingScene,
}
#[derive(Default)]
struct RuntimeViews {
    structure: Vec<crate::psd::SourceLayerInfo>,
    patterns: Vec<crate::psd::patterns::Source>,
}
impl SceneSnapshot {
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
            pending,
            leases: Arc::new(leases),
            file_version,
            _file_ownership: file_ownership,
            runtime_views: RuntimeViews::default(),
        })
    }
    pub fn identity(&self) -> &SceneIdentity {
        &self.identity
    }
    pub fn metadata(&self) -> &Document36 {
        self.pending.metadata()
    }
    pub fn rasters(&self) -> &RasterRoster {
        self.pending.rasters()
    }
    pub fn raster(&self, reference: &RasterRef36) -> Result<&Surface> {
        self.pending.roster.get(reference)
    }
    pub fn budget(&self) -> &ResidentBudget {
        self.pending.budget()
    }
    pub fn disk_budget(&self) -> &DiskBudget {
        self.pending.disk_budget()
    }
    pub fn leases(&self) -> &[super::Lease] {
        &self.leases
    }
    pub fn file_version(&self) -> Option<crate::file_safety::Version> {
        self.file_version
    }
    pub fn source_structure(&self) -> &[crate::psd::SourceLayerInfo] {
        &self.runtime_views.structure
    }
    pub fn source_patterns(&self) -> &[crate::psd::patterns::Source] {
        &self.runtime_views.patterns
    }

    /// Actual captured metadata and physical source identities, including gesture
    /// pixels whose owner changed without a committed document revision.
    pub fn render_key(&self) -> Result<[u8; 32]> {
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
        serde_json::to_writer(&mut writer, self.metadata()).map_err(|e| e.to_string())?;
        for (id, surface) in self.rasters().iter() {
            writer.hash.update(&id.to_be_bytes());
            writer.hash.update(&surface.key().bytes());
        }
        Ok(writer.hash.finish().as_ref().try_into().unwrap())
    }
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage36 {
    pub mask: RasterRef36,
    pub bounds: [i32; 4],
    pub origin: [i32; 2],
    #[serde(deserialize_with = "bounds::contours")]
    pub contours: Vec<Vec<[f32; 2]>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect36 {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    pub weight: f32,
    pub settings: Arc<Value>,
    pub clips: Vec<EffectClip36>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectClip36 {
    pub stroke: u32,
    pub coverage: Coverage36,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskStep36 {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    pub weight: f32,
    pub value: f32,
    pub pixels: RasterRef36,
    pub settings: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mask36 {
    pub enabled: bool,
    pub linked: bool,
    pub steps: Vec<MaskStep36>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub psd_metadata: Vec<crate::psd::LayerMetadata>,
    pub source: Option<Source36>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source36 {
    pub width: u32,
    pub height: u32,
    pub matrix: [f32; 6],
    pub content: Content36,
}
/// Exhaustive mirror: a future native-source variant requires an explicit wire
/// decision instead of inheriting serialization of new fields accidentally.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document36 {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub bit_depth: u16,
    #[serde(with = "icc")]
    pub icc_profile: Option<Arc<Vec<u8>>>,
    pub srgb_tagged: bool,
    pub psd_global_mask: Option<crate::psd::GlobalMaskSettings>,
    pub layers: Vec<Layer36>,
    pub filters: Vec<Effect36>,
    pub selection: Option<[i32; 4]>,
    #[serde(default, deserialize_with = "bounds::polygon")]
    pub selection_polygon: Option<Vec<[f32; 2]>>,
    pub selection_coverage: Option<Coverage36>,
    pub selection_previous: Option<Coverage36>,
    pub read_only: bool,
    pub warnings: Vec<String>,
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
            icc_profile: doc.icc_profile.clone(),
            srgb_tagged: doc.srgb_tagged,
            psd_global_mask: doc.psd_global_mask.clone(),
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
            selection_polygon: doc.selection_polygon.clone(),
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
            warnings: doc.warnings.clone(),
        })
    }

    /// Exact deterministic native-location order, including original sources.
    /// The ordinal rule rejects duplicate, unused, aliased or cyclic wire IDs.
    pub fn visit_rasters(
        &self,
        mut visit: impl FnMut(&RasterRef36) -> Result<()>,
    ) -> Result<usize> {
        let mut next = 1u32;
        let mut walk = |r: &RasterRef36| -> Result<()> {
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
fn visit_one(
    r: &RasterRef36,
    next: &mut u32,
    visit: &mut impl FnMut(&RasterRef36) -> Result<()>,
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
impl Layer36 {
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
                                    settings: s.settings.clone(),
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
            psd_metadata: layer.psd_metadata.clone(),
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
            contours: contours.to_vec(),
        })
    }
}
impl Effect36 {
    fn capture(effect: &crate::effects::Effect, c: &mut impl RasterCollector) -> Result<Self> {
        let mut clips = vec![];
        let settings = if effect.kind == "liquify" {
            let object = effect
                .settings
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
            Arc::new(Value::Object(output))
        } else {
            effect.settings.clone()
        };
        Ok(Self {
            id: effect.id.clone(),
            kind: effect.kind.clone(),
            enabled: effect.enabled,
            weight: effect.weight,
            settings,
            clips,
        })
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
    fn capture(source: &crate::source::Source) -> Self {
        use crate::source::Content;
        let content = match &source.content {
            Content::Bezier {
                nodes,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Content36::Bezier {
                nodes: nodes.clone(),
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                stroke_width: *stroke_width,
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
                text: text.clone(),
                layout: *layout,
                font: font.clone(),
                size: *size,
                line_height: *line_height,
                letter_spacing: *letter_spacing,
                align: align.clone(),
                color: *color,
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
                shape: shape.clone(),
                bounds: *bounds,
                points: points.clone(),
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                stroke_width: *stroke_width,
            },
            Content::Svg { svg } => Content36::Svg { svg: svg.clone() },
        };
        Self {
            width: source.width,
            height: source.height,
            matrix: source.matrix,
            content,
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
        let mut duplicate = metadata.clone();
        duplicate.layers[0]
            .pixels
            .retained
            .as_mut()
            .unwrap()
            .pixels
            .id = 1;
        assert!(duplicate.visit_rasters(|_| Ok(())).is_err());
        let mut mismatch = metadata.clone();
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
            admitted(metadata.clone(), &resident),
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
            admitted(simple.clone(), &resident),
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
        assert_eq!(snapshot.metadata().layers.len(), 1);
        assert_eq!(resident.used(), 64 * 1024);
        assert_eq!(disk.used().unwrap(), 0);
        drop(snapshot);
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
        let mut invalid = metadata.clone();
        invalid.layers[0].effects[0].settings = doc.layers[0].effects[0].settings.clone();
        assert!(invalid.visit_rasters(|_| Ok(())).is_err());
    }
}
