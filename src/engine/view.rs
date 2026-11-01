//! Borrowed presentation metadata. Pixel preparation and mutation have separate
//! fallible commands; this view never reads backing files or copies a document.
use super::{native, Document, Engine, Layer, Mask, MaskStep};
use crate::raster::Pixel;

#[derive(Clone, Copy)]
pub(crate) enum DocumentView<'a> {
    Resident(&'a Document),
    Native(&'a native::NativeDocument),
}
/// The active engine still has resident authority until the coordinated switch.
pub(crate) fn current(engine: &Engine) -> DocumentView<'_> {
    DocumentView::Resident(&engine.doc)
}
#[derive(Clone, Copy)]
pub(crate) enum LayerView<'a> {
    Resident(&'a Layer),
    Native(native::LayerMetadata<'a>),
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
    /// Native editable headers contain no legacy view-only unsupported records.
    /// Absence is explicit so callers cannot treat it as an empty source tree.
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
    layer_ref!(id, &'a str);
    layer_ref!(name, &'a str);
    layer_ref!(kind, &'a str);
    layer_ref!(blend, &'a str);
    layer_field!(visible, bool);
    layer_field!(locked, bool);
    layer_field!(opacity, f32);
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
            Self::Native(layer) => layer.source().map(|source| match &source.content {
                super::scene::Content36::Bezier { .. } => "bezier",
                super::scene::Content36::Text { .. } => "text",
                super::scene::Content36::Shape { .. } => "shape",
                super::scene::Content36::Svg { .. } => "svg",
            }),
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
            document.selection = Some([1, 2, 7, 8]);
            document.selection_polygon = Some(vec![[1., 2.], [7., 2.], [7., 8.]]);
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
