//! Borrowed presentation metadata. Pixel preparation and mutation have separate
//! fallible commands; this view never reads backing files or copies a document.
use super::{native, Document, Engine, Layer, Mask, MaskStep, Scope};
use crate::raster::Pixel;
use std::collections::{HashMap, HashSet};

pub(crate) fn descendant(doc: DocumentView<'_>, child: &str, ancestor: &str) -> bool {
    descendant_with_parent(child, ancestor, |id| {
        doc.find_layer(id).and_then(LayerView::parent)
    })
}

fn descendant_with_parent<'a>(
    child: &str,
    ancestor: &str,
    parent_of: impl Fn(&str) -> Option<&'a str>,
) -> bool {
    let mut parent = parent_of(child);
    for _ in 0..16 {
        let Some(id) = parent else { return false };
        if id == ancestor {
            return true;
        }
        parent = parent_of(id);
    }
    false
}

fn scope_targets_overlap<'a>(
    a: Option<&str>,
    b: Option<&str>,
    parent_of: impl Fn(&str) -> Option<&'a str>,
) -> bool {
    let visibility = |target: Option<&str>| target.is_some_and(|id| id.starts_with("@visibility:"));
    if visibility(a) || visibility(b) {
        return a == b;
    }
    match (a, b) {
        (Some(x), Some(y)) => {
            x == y || descendant_with_parent(x, y, &parent_of)
                || descendant_with_parent(y, x, parent_of)
        }
        _ => true,
    }
}

pub(crate) fn scope_overlap(doc: DocumentView<'_>, a: &Scope, b: &Scope) -> bool {
    let a_target = a.target.as_deref();
    let b_target = b.target.as_deref();
    scope_targets_overlap(a_target, b_target, |id| {
        doc.find_layer(id).and_then(LayerView::parent)
    }) && (a_target.is_some_and(|id| id.starts_with("@visibility:"))
        || Scope { target: None, rect: a.rect }.overlap(b))
}

pub(crate) fn reserved_layers<'a>(doc: DocumentView<'a>, scopes: &[Scope]) -> HashSet<&'a str> {
    if scopes.is_empty() {
        return HashSet::new();
    }
    let parents: HashMap<_, _> = doc.layers()
        .map(|layer| (layer.id(), layer.parent()))
        .collect();
    // Layer panel targets cover the whole layer, so any rectangle on a matching scope overlaps.
    parents.keys().copied()
        .filter(|id| {
            scopes.iter().any(|scope| {
                scope_targets_overlap(scope.target.as_deref(), Some(*id), |id| {
                    parents.get(id).copied().flatten()
                })
            })
        })
        .collect()
}
/// Shared declared-scope containment, evaluated against the current draft.
pub(crate) fn scopes_contained(
    doc: DocumentView<'_>,
    declared: &[Scope],
    actual: &[Scope],
) -> bool {
    actual.iter().all(|inner| {
        declared.iter().any(|outer| {
            let target = match outer.target.as_deref() {
                None => true,
                Some(a) => inner.target.as_deref().is_some_and(|b| {
                    let b = b.strip_prefix("@visibility:").unwrap_or(b);
                    a == b || descendant(doc, b, a)
                }),
            };
            target
                && match (outer.rect, inner.rect) {
                    (None, _) => true,
                    (Some(a), Some(b)) => {
                        a[0] <= b[0] && a[1] <= b[1] && a[2] >= b[2] && a[3] >= b[3]
                    }
                    _ => false,
                }
        })
    })
}

#[derive(Clone, Copy)]
pub(crate) enum DocumentView<'a> {
    Resident(&'a Document),
    Native(&'a native::NativeDocument),
}
/// An immutable frame owner. Native snapshots share the admitted source and
/// metadata owners; creating or cloning one never materializes raster pixels.
#[derive(Clone)]
pub(crate) enum DocumentSnapshot {
    Resident(std::sync::Arc<Document>),
    Native(native::NativeDocument),
}
impl DocumentSnapshot {
    pub(crate) fn view(&self) -> DocumentView<'_> {
        match self {
            Self::Resident(doc) => DocumentView::Resident(doc),
            Self::Native(doc) => DocumentView::Native(doc),
        }
    }
}
impl From<Document> for DocumentSnapshot {
    fn from(doc: Document) -> Self {
        Self::Resident(std::sync::Arc::new(doc))
    }
}
impl From<native::NativeDocument> for DocumentSnapshot {
    fn from(doc: native::NativeDocument) -> Self {
        Self::Native(doc)
    }
}
impl<'a> From<&'a Document> for DocumentView<'a> {
    fn from(doc: &'a Document) -> Self {
        Self::Resident(doc)
    }
}
impl<'a> From<&'a native::NativeDocument> for DocumentView<'a> {
    fn from(doc: &'a native::NativeDocument) -> Self {
        Self::Native(doc)
    }
}
impl<'a> From<&'a DocumentSnapshot> for DocumentView<'a> {
    fn from(doc: &'a DocumentSnapshot) -> Self {
        doc.view()
    }
}
impl<'a> From<&DocumentView<'a>> for DocumentView<'a> {
    fn from(view: &DocumentView<'a>) -> Self {
        *view
    }
}
impl<'a> From<&'a Layer> for LayerView<'a> {
    fn from(layer: &'a Layer) -> Self {
        Self::Resident(layer)
    }
}
impl<'a> From<&'a crate::effects::Effect> for EffectView<'a> {
    fn from(effect: &'a crate::effects::Effect) -> Self {
        Self::Resident(effect)
    }
}
pub(crate) fn current(engine: &Engine) -> DocumentView<'_> {
    engine.authority.view()
}
#[derive(Clone, Copy)]
pub(crate) enum LayerView<'a> {
    Resident(&'a Layer),
    Native(native::LayerMetadata<'a>),
}
#[derive(Clone, Copy)]
pub(crate) enum SourceView<'a> {
    Resident(&'a crate::source::Source),
    Native(&'a super::scene::Source36),
}
#[derive(Clone, Copy)]
pub(crate) enum EffectView<'a> {
    Resident(&'a crate::effects::Effect),
    Native(native::EffectMetadata<'a>),
}
#[derive(Clone, Copy)]
pub(crate) enum MaskView<'a> {
    Resident(&'a Mask),
    Native(native::MaskMetadata<'a>),
}
#[derive(Clone, Copy)]
pub(crate) enum MaskStepView<'a> {
    Resident(&'a MaskStep),
    Native(native::MaskStepMetadata<'a>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RasterShape {
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub retained: bool,
}
impl RasterShape {
    fn resident(pixels: &crate::raster::Raster) -> Self {
        Self {
            width: pixels.width,
            height: pixels.height,
            depth: pixels.depth,
            retained: pixels.retained.is_some(),
        }
    }
    fn native(pixels: native::RasterMetadata<'_>) -> Self {
        Self {
            width: pixels.width(),
            height: pixels.height(),
            depth: pixels.depth(),
            retained: pixels.retained().is_some(),
        }
    }
}
macro_rules! doc_field {
    ($method:ident,$field:ident,$output:ty) => {
        pub(crate) fn $method(self) -> $output {
            match self {
                Self::Resident(doc) => doc.$field,
                Self::Native(doc) => doc.metadata().$method(),
            }
        }
    };
}
macro_rules! doc_ref {
    ($method:ident,$field:ident,$output:ty) => {
        pub(crate) fn $method(self) -> $output {
            match self {
                Self::Resident(doc) => &doc.$field,
                Self::Native(doc) => doc.metadata().$method(),
            }
        }
    };
}
impl<'a> DocumentView<'a> {
    pub(crate) fn same_observation(self, other: Self) -> bool {
        self.id() == other.id()
            && self.revision() == other.revision()
            && match (self.native(), other.native()) {
                (Some(a), Some(b)) => a.key() == b.key(),
                (None, None) => true,
                _ => false,
            }
    }
    pub(crate) fn native(self) -> Option<&'a native::NativeDocument> {
        match self {
            Self::Native(doc) => Some(doc),
            Self::Resident(_) => None,
        }
    }
    pub(crate) fn resident(self) -> Option<&'a Document> {
        match self {
            Self::Resident(doc) => Some(doc),
            Self::Native(_) => None,
        }
    }
    doc_ref!(id, id, &'a str);
    doc_ref!(name, name, &'a str);
    doc_field!(width, width, u32);
    doc_field!(height, height, u32);
    doc_field!(depth, bit_depth, u16);
    doc_field!(revision, revision, u64);
    doc_field!(read_only, read_only, bool);
    doc_field!(srgb_tagged, srgb_tagged, bool);
    doc_field!(selection, selection, Option<[i32; 4]>);
    doc_ref!(warnings, warnings, &'a [String]);
    pub(crate) fn global_light_angle(self) -> Option<u32> {
        match self {
            Self::Resident(doc) => doc.psd_global_angle.map(u32::from_be_bytes),
            Self::Native(doc) => doc.metadata().global_light_angle(),
        }
    }
    pub(crate) fn profile(self) -> Option<&'a [u8]> {
        match self {
            Self::Resident(doc) => doc.icc_profile.as_deref().map(Vec::as_slice),
            Self::Native(doc) => doc.metadata().profile(),
        }
    }
    pub(crate) fn selection_polygon(self) -> Option<&'a [[f32; 2]]> {
        match self {
            Self::Resident(doc) => doc.selection_polygon.as_deref(),
            Self::Native(doc) => doc.metadata().selection_polygon(),
        }
    }
    pub(crate) fn selection_contours(self) -> Option<&'a [Vec<[f32; 2]>]> {
        match self {
            Self::Resident(doc) => doc
                .selection_coverage
                .as_ref()
                .filter(|coverage| doc.selection == Some(coverage.bounds))
                .map(|coverage| coverage.contours.as_slice()),
            Self::Native(doc) => doc
                .metadata()
                .selection_coverage()
                .filter(|coverage| doc.metadata().selection() == Some(coverage.bounds()))
                .map(|coverage| coverage.contours()),
        }
    }
    pub(crate) fn has_soft_selection(self) -> bool {
        match self {
            Self::Resident(doc) => doc.selection_coverage.is_some(),
            Self::Native(doc) => doc.metadata().selection_coverage().is_some(),
        }
    }
    pub(crate) fn layer_count(self) -> usize {
        match self {
            Self::Resident(doc) => doc.layers.len(),
            Self::Native(doc) => doc.metadata().layers().len(),
        }
    }
    pub(crate) fn layer(self, index: usize) -> Option<LayerView<'a>> {
        match self {
            Self::Resident(doc) => doc.layers.get(index).map(LayerView::Resident),
            Self::Native(doc) => doc.metadata().layer(index).map(LayerView::Native),
        }
    }
    pub(crate) fn find_layer(self, id: &str) -> Option<LayerView<'a>> {
        self.layers().find(|layer| layer.id() == id)
    }
    pub(crate) fn layers(self) -> Layers<'a> {
        Layers {
            document: self,
            first: 0,
            end: self.layer_count(),
        }
    }
    pub(crate) fn filter_count(self) -> usize {
        match self {
            Self::Resident(doc) => doc.filters.len(),
            Self::Native(doc) => doc.metadata().filters().len(),
        }
    }
    pub(crate) fn wand_source(self) -> Option<(&'a str, &'a serde_json::Value)> {
        match self {
            Self::Resident(doc) => crate::selection::wand_source(doc)
                .map(|source| (source.id.as_str(), &source.command)),
            Self::Native(doc) => doc.wand().map(|source| (source.id(), source.command())),
        }
    }
    pub(crate) fn filter(self, index: usize) -> Option<EffectView<'a>> {
        match self {
            Self::Resident(doc) => doc.filters.get(index).map(EffectView::Resident),
            Self::Native(doc) => doc.metadata().filters().nth(index).map(EffectView::Native),
        }
    }
    pub(crate) fn global_mask(self) -> Option<&'a crate::psd::GlobalMaskSettings> {
        match self {
            Self::Resident(doc) => doc.psd_global_mask.as_ref(),
            Self::Native(doc) => doc.metadata().global_mask(),
        }
    }
    pub(crate) fn source_structure(self) -> Option<&'a [crate::psd::SourceLayerInfo]> {
        match self {
            Self::Resident(doc) => Some(&doc.source_structure),
            Self::Native(doc) => doc.protected_structure(),
        }
    }
    /// Absence is explicit so callers cannot treat it as an empty legacy inspection.
    pub(crate) fn legacy_inspection(
        self,
    ) -> Option<(
        &'a [crate::psd::SourceLayerInfo],
        &'a [crate::psd::patterns::Source],
    )> {
        match self {
            Self::Resident(doc) => Some((&doc.source_structure, &doc.source_patterns)),
            Self::Native(_) => None,
        }
    }
}
pub(crate) struct Layers<'a> {
    document: DocumentView<'a>,
    first: usize,
    end: usize,
}
impl<'a> Iterator for Layers<'a> {
    type Item = LayerView<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.first == self.end {
            return None;
        }
        let index = self.first;
        self.first += 1;
        self.document.layer(index)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.end - self.first;
        (count, Some(count))
    }
}
impl DoubleEndedIterator for Layers<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.first == self.end {
            return None;
        }
        self.end -= 1;
        self.document.layer(self.end)
    }
}
impl ExactSizeIterator for Layers<'_> {}
macro_rules! layer_field {
    ($method:ident,$output:ty) => {
        pub(crate) fn $method(self) -> $output {
            match self {
                Self::Resident(layer) => layer.$method,
                Self::Native(layer) => layer.$method(),
            }
        }
    };
}
macro_rules! layer_ref {
    ($method:ident,$output:ty) => {
        pub(crate) fn $method(self) -> $output {
            match self {
                Self::Resident(layer) => &layer.$method,
                Self::Native(layer) => layer.$method(),
            }
        }
    };
}
impl<'a> LayerView<'a> {
    pub(crate) fn source(self) -> Option<SourceView<'a>> {
        match self {
            Self::Resident(layer) => layer.source.as_deref().map(SourceView::Resident),
            Self::Native(layer) => layer.source().map(SourceView::Native),
        }
    }
    layer_ref!(id, &'a str);
    layer_ref!(name, &'a str);
    layer_ref!(kind, &'a str);
    layer_ref!(blend, &'a str);
    layer_field!(visible, bool);
    layer_field!(locked, bool);
    layer_field!(opacity, f32);
    pub(crate) fn fill_opacity(self) -> u8 {
        match self {
            Self::Resident(layer) => crate::psd::fill_opacity::value(&layer.psd_metadata),
            Self::Native(layer) => layer.fill_opacity(),
        }
    }
    pub(crate) fn blend_clipped_as_group(self) -> bool {
        match self {
            Self::Resident(layer) => crate::psd::clipping::grouped(&layer.psd_metadata),
            Self::Native(layer) => layer.blend_clipped_as_group(),
        }
    }
    pub(crate) fn blend_if(self) -> Option<[[u8; 8]; 4]> {
        match self {
            Self::Resident(layer) => crate::psd::blend_ranges::value(&layer.psd_metadata),
            Self::Native(layer) => layer.blend_if(),
        }
    }
    pub(crate) fn blend_channels(self) -> [bool; 3] {
        match self {
            Self::Resident(layer) => crate::psd::blend_channels::enabled(&layer.psd_metadata),
            Self::Native(layer) => layer.blend_channels(),
        }
    }
    pub(crate) fn color_label(self) -> &'static str {
        crate::psd::layer_color_label(match self {
            Self::Resident(layer) => &layer.psd_metadata,
            Self::Native(layer) => layer.psd_metadata(),
        })
    }
    pub(crate) fn photoshop_folder_is_closed(self) -> bool {
        self.kind() == "group" && crate::psd::folder_is_closed(match self {
            Self::Resident(layer) => &layer.psd_metadata,
            Self::Native(layer) => layer.psd_metadata(),
        })
    }
    layer_field!(color, Pixel);
    pub(crate) fn origin(self) -> [i32; 2] {
        match self {
            Self::Resident(layer) => [layer.x, layer.y],
            Self::Native(layer) => layer.origin(),
        }
    }
    pub(crate) fn parent(self) -> Option<&'a str> {
        match self {
            Self::Resident(layer) => layer.parent.as_deref(),
            Self::Native(layer) => layer.parent(),
        }
    }
    pub(crate) fn clip_to(self) -> Option<&'a str> {
        match self {
            Self::Resident(layer) => layer.clip_to.as_deref(),
            Self::Native(layer) => layer.clip_to(),
        }
    }
    pub(crate) fn pixels(self) -> RasterShape {
        match self {
            Self::Resident(layer) => RasterShape::resident(&layer.pixels),
            Self::Native(layer) => RasterShape::native(layer.pixels()),
        }
    }
    pub(crate) fn mask(self) -> Option<MaskView<'a>> {
        match self {
            Self::Resident(layer) => layer.mask.as_ref().map(MaskView::Resident),
            Self::Native(layer) => layer.mask().map(MaskView::Native),
        }
    }
    pub(crate) fn effect_count(self) -> usize {
        match self {
            Self::Resident(layer) => layer.effects.len(),
            Self::Native(layer) => layer.effects().len(),
        }
    }
    pub(crate) fn effect(self, index: usize) -> Option<EffectView<'a>> {
        match self {
            Self::Resident(layer) => layer.effects.get(index).map(EffectView::Resident),
            Self::Native(layer) => layer.effects().nth(index).map(EffectView::Native),
        }
    }
    pub(crate) fn source_kind(self) -> Option<&'static str> {
        match self {
            Self::Resident(layer) => layer.source.as_ref().map(|source| match &source.content {
                crate::source::Content::Bezier { .. } => "bezier",
                crate::source::Content::Text { .. } => "text",
                crate::source::Content::Shape { .. } => "shape",
                crate::source::Content::Svg { .. } => "svg",
            }),
            Self::Native(layer) => layer.source().map(|source| match source.definition() {
                super::scene::Content36::Bezier { .. } => "bezier",
                super::scene::Content36::Text { .. } => "text",
                super::scene::Content36::Shape { .. } => "shape",
                super::scene::Content36::Svg { .. } => "svg",
            }),
        }
    }
}
impl<'a> SourceView<'a> {
    pub(crate) fn path_nodes(self) -> Option<(&'a [crate::path::Anchor], bool)> {
        match self {
            Self::Resident(source) => match &source.content {
                crate::source::Content::Bezier { nodes, closed, .. } => Some((nodes, *closed)),
                _ => None,
            },
            Self::Native(source) => match source.definition() {
                super::scene::Content36::Bezier { nodes, closed, .. } => Some((nodes, *closed)),
                _ => None,
            },
        }
    }
    pub(crate) fn dimensions(self) -> [u32; 2] {
        match self {
            Self::Resident(source) => [source.width, source.height],
            Self::Native(source) => [source.width, source.height],
        }
    }
    pub(crate) fn matrix(self) -> [f32; 6] {
        match self {
            Self::Resident(source) => source.matrix,
            Self::Native(source) => source.matrix,
        }
    }
}
impl<'a> EffectView<'a> {
    pub(crate) fn id(self) -> &'a str {
        match self {
            Self::Resident(value) => &value.id,
            Self::Native(value) => value.id(),
        }
    }
    pub(crate) fn kind(self) -> &'a str {
        match self {
            Self::Resident(value) => &value.kind,
            Self::Native(value) => value.kind(),
        }
    }
    pub(crate) fn enabled(self) -> bool {
        match self {
            Self::Resident(value) => value.enabled,
            Self::Native(value) => value.enabled(),
        }
    }
    pub(crate) fn weight(self) -> f32 {
        match self {
            Self::Resident(value) => value.weight,
            Self::Native(value) => value.weight(),
        }
    }
    pub(crate) fn settings(self) -> &'a serde_json::Value {
        match self {
            Self::Resident(value) => &value.settings,
            Self::Native(value) => value.settings(),
        }
    }
}
impl<'a> MaskView<'a> {
    pub(crate) fn enabled(self) -> bool {
        match self {
            Self::Resident(value) => value.enabled,
            Self::Native(value) => value.enabled(),
        }
    }
    pub(crate) fn linked(self) -> bool {
        match self {
            Self::Resident(value) => value.linked,
            Self::Native(value) => value.linked(),
        }
    }
    pub(crate) fn step_count(self) -> usize {
        match self {
            Self::Resident(value) => value.steps.len(),
            Self::Native(value) => value.steps().len(),
        }
    }
    pub(crate) fn step(self, index: usize) -> Option<MaskStepView<'a>> {
        match self {
            Self::Resident(value) => value.steps.get(index).map(MaskStepView::Resident),
            Self::Native(value) => value.steps().nth(index).map(MaskStepView::Native),
        }
    }
}
impl<'a> MaskStepView<'a> {
    pub(crate) fn id(self) -> &'a str {
        match self {
            Self::Resident(value) => &value.id,
            Self::Native(value) => value.id(),
        }
    }
    pub(crate) fn kind(self) -> &'a str {
        match self {
            Self::Resident(value) => &value.kind,
            Self::Native(value) => value.kind(),
        }
    }
    pub(crate) fn enabled(self) -> bool {
        match self {
            Self::Resident(value) => value.enabled,
            Self::Native(value) => value.enabled(),
        }
    }
    pub(crate) fn weight(self) -> f32 {
        match self {
            Self::Resident(value) => value.weight,
            Self::Native(value) => value.weight(),
        }
    }
    pub(crate) fn value(self) -> f32 {
        match self {
            Self::Resident(value) => value.value,
            Self::Native(value) => value.value(),
        }
    }
    pub(crate) fn pixels(self) -> RasterShape {
        match self {
            Self::Resident(value) => RasterShape::resident(&value.pixels),
            Self::Native(value) => RasterShape::native(value.pixels()),
        }
    }
    pub(crate) fn settings(self) -> &'a serde_json::Value {
        match self {
            Self::Resident(value) => &value.settings,
            Self::Native(value) => value.settings(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::scene::{Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster},
        large_document::{DiskBudget, ResidentBudget},
        raster::{Raster, Surface},
    };

    #[test]
    fn layer_reservation_indicators_use_the_shared_hierarchy_rules() {
        let mut document = Document::new(12, 11).unwrap();
        let outer = Layer::new("Outer", "group", 12, 11);
        let mut inner = Layer::new("Inner", "group", 12, 11);
        inner.parent = Some(outer.id.clone());
        document.layers[0].parent = Some(inner.id.clone());
        let leaf = document.layers[0].id.clone();
        let child = inner.id.clone();
        let root = outer.id.clone();
        document.layers.extend([inner, outer, Layer::new("Unrelated", "paint", 12, 11)]);
        let doc = DocumentView::from(&document);
        let rect = Some([1, 1, 4, 4]);
        for (target, expected_count) in [
            (None, 4),
            (Some(leaf.clone()), 3),
            (Some(root.clone()), 3),
            (Some(format!("@visibility:{leaf}")), 0),
            (Some("@selection".into()), 0),
        ] {
            let scope = Scope { target, rect };
            let reserved = reserved_layers(doc, std::slice::from_ref(&scope));
            assert_eq!(reserved.len(), expected_count);
            for layer in doc.layers() {
                assert_eq!(reserved.contains(layer.id()), scope_overlap(doc, &scope, &Scope::layer(layer.id())));
            }
        }
        let scope = Scope { target: Some(root), rect };
        let disjoint = Scope { target: Some(child), rect: Some([4, 4, 8, 8]) };
        assert!(!scope_overlap(doc, &scope, &disjoint));
    }

    struct Zeros(Vec<Surface>);
    impl RasterCollector for Zeros {
        fn resident(&mut self, pixels: &Raster) -> Result<RasterRef36, String> {
            assert!(pixels.tiles.is_empty() && pixels.samples16.is_empty());
            let surface = Surface::zero(pixels.width, pixels.height, pixels.depth)
                .map_err(|e| e.to_string())?;
            self.0.push(surface);
            Ok(RasterRef36 {
                id: self.0.len() as u32,
                width: pixels.width,
                height: pixels.height,
                depth: pixels.depth,
                retained: None,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
            unreachable!()
        }
    }
    #[test]
    fn borrowed_native_and_resident_controls_match_without_pixel_materialization_or_new_admission()
    {
        for depth in [8, 16] {
            let mut document = Document::new_depth(12, 11, depth).unwrap();
            document.layers[0].name = "Borrowed layer".into();
            document.layers[0].x = -3;
            document.layers[0].y = 5;
            document.layers[0].opacity = 0.4375;
            document.layers[0].mask = Some(Mask {
                enabled: true,
                linked: false,
                cache_key: crate::engine::id(),
                steps: vec![MaskStep {
                    id: crate::engine::id(),
                    kind: "fill".into(),
                    enabled: true,
                    weight: 0.25,
                    value: 143.0,
                    pixels: Raster::new(12, 11),
                    settings: serde_json::json!({"example":17}),
                }],
            });
            document.layers[0].effects.push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: "color_overlay".into(),
                enabled: false,
                weight: 0.75,
                settings: std::sync::Arc::new(serde_json::json!({"color":[19,37,71,255]})),
            });
            document.layers[0].source = Some(std::sync::Arc::new(crate::source::Source {
                width: 12,
                height: 11,
                matrix: [1.1, 0.25, -0.1, 0.9, 2., -1.],
                content: crate::source::Content::Bezier {
                    nodes: vec![
                        crate::path::Anchor {
                            point: [2., 3.],
                            in_handle: None,
                            out_handle: Some([5., 6.]),
                            smooth: false,
                        },
                        crate::path::Anchor {
                            point: [8., 7.],
                            in_handle: Some([6., 5.]),
                            out_handle: None,
                            smooth: false,
                        },
                    ],
                    closed: false,
                    fill: [0; 4],
                    stroke: [12345, 23457, 34569, 65535],
                    stroke_width: 2.,
                },
            }));
            document.selection = Some([1, 2, 7, 8]);
            document.selection_polygon = Some(vec![[1., 2.], [7., 2.], [7., 8.]]);
            document.selection_coverage = Some(crate::selection::Coverage {
                mask: Raster::new(6, 6),
                bounds: [1, 2, 7, 8],
                origin: [1, 2],
                contours: vec![vec![[1., 2.], [7., 2.], [7., 8.]]],
            });
            let budget = ResidentBudget::new(32 * 1024 * 1024).unwrap();
            let owner =
                crate::psd::memory36::MetadataCharge::reserve(&budget, 8 * 1024 * 1024, &|| Ok(()))
                    .unwrap();
            let mut collector = Zeros(vec![]);
            let metadata = Document36::capture(&document, &mut collector).unwrap();
            let metadata =
                crate::psd::memory36::Admitted::from_parts(std::sync::Arc::new(metadata), owner);
            let pending = PendingScene::new(
                metadata,
                RasterRoster::new(collector.0).unwrap(),
                budget.clone(),
                DiskBudget::new(1024 * 1024, 4).unwrap(),
            )
            .unwrap();
            let native = native::NativeDocument::from_pending(pending).unwrap();
            let left = DocumentView::Resident(&document);
            let right = DocumentView::Native(&native);
            let before = budget.used();
            assert_eq!(left.id(), right.id());
            assert_eq!(left.name(), right.name());
            assert_eq!(
                (left.width(), left.height(), left.depth(), left.revision()),
                (
                    right.width(),
                    right.height(),
                    right.depth(),
                    right.revision()
                )
            );
            assert_eq!(left.selection(), right.selection());
            assert_eq!(left.selection_polygon(), right.selection_polygon());
            assert_eq!(left.selection_contours(), right.selection_contours());
            assert!(std::ptr::eq(
                left.selection_contours().unwrap(),
                document
                    .selection_coverage
                    .as_ref()
                    .unwrap()
                    .contours
                    .as_slice()
            ));
            assert!(std::ptr::eq(
                right.selection_contours().unwrap(),
                native.metadata().selection_coverage().unwrap().contours()
            ));
            assert_eq!(left.layer_count(), right.layer_count());
            for (a, b) in left.layers().zip(right.layers()) {
                assert_eq!(
                    (a.id(), a.name(), a.kind(), a.blend()),
                    (b.id(), b.name(), b.kind(), b.blend())
                );
                assert_eq!(
                    (
                        a.visible(),
                        a.locked(),
                        a.opacity(),
                        a.origin(),
                        a.pixels(),
                        a.source_kind()
                    ),
                    (
                        b.visible(),
                        b.locked(),
                        b.opacity(),
                        b.origin(),
                        b.pixels(),
                        b.source_kind()
                    )
                );
                assert_eq!(
                    a.source().unwrap().path_nodes(),
                    b.source().unwrap().path_nodes()
                );
                for point in [[2., 3.], [5., 6.], [8., 7.]] {
                    assert_eq!(
                        crate::path::document_point_view(a, point),
                        crate::path::document_point_view(b, point)
                    );
                    assert_eq!(
                        crate::path::nearest_segment_view(a, point, 12.),
                        crate::path::nearest_segment_view(b, point, 12.)
                    );
                }
                let command = serde_json::json!({"op":"path.node.move","layer":a.id(),"index":1,"point":[9,6]});
                let controls_a = crate::path::preview_controls(left, Some(a), &[command.clone()])
                    .unwrap()
                    .unwrap();
                let controls_b = crate::path::preview_controls(right, Some(b), &[command])
                    .unwrap()
                    .unwrap();
                assert_eq!(controls_a.0, controls_b.0);
                assert_eq!(
                    serde_json::to_value(controls_a.1).unwrap(),
                    serde_json::to_value(controls_b.1).unwrap()
                );
                let (am, bm) = (a.mask().unwrap(), b.mask().unwrap());
                assert_eq!(
                    (am.enabled(), am.linked(), am.step_count()),
                    (bm.enabled(), bm.linked(), bm.step_count())
                );
                let (am, bm) = (am.step(0).unwrap(), bm.step(0).unwrap());
                assert_eq!(
                    (
                        am.id(),
                        am.kind(),
                        am.enabled(),
                        am.weight(),
                        am.value(),
                        am.pixels(),
                        am.settings()
                    ),
                    (
                        bm.id(),
                        bm.kind(),
                        bm.enabled(),
                        bm.weight(),
                        bm.value(),
                        bm.pixels(),
                        bm.settings()
                    )
                );
                let (ae, be) = (a.effect(0).unwrap(), b.effect(0).unwrap());
                assert_eq!(
                    (ae.id(), ae.kind(), ae.enabled(), ae.weight(), ae.settings()),
                    (be.id(), be.kind(), be.enabled(), be.weight(), be.settings())
                );
            }
            assert!(right.layer(right.layer_count()).is_none());
            assert_eq!(
                right.layers().rev().next().unwrap().id(),
                right.layer(0).unwrap().id()
            );
            assert!(left.legacy_inspection().is_some());
            assert!(right.legacy_inspection().is_none());
            assert_eq!(budget.used(), before);
            drop(native);
            assert_eq!(budget.used(), 0);
        }
    }
}
