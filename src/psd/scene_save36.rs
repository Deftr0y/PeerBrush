//! Pure standard metadata plan over an immutable admitted scene. Pixel providers
//! and final save publication remain fallible, separately guarded operations.
pub(crate) use super::save36::CompletedSave36;
use super::{
    adjustments::{curves_settings, is_tone_tag, CurveRecords, ToneRecords, MAX_ADJUSTMENT_DESCRIPTOR_BYTES, MAX_CURVES_RECORD},
    bulk36::Digest,
    patterns::Geometry,
    standard36::{self, Mask, Metadata, Record, RowSource},
    structural::{Format, Header},
};
use crate::{
    engine::{
        native::{LayerMetadata, Metadata as DocumentMetadata},
        scene::SceneSnapshot,
    },
    large_document::Reservation,
};
use ring::digest::{Context, SHA256};
use std::io::{Read, Seek};

/// A provider retains the exact immutable snapshot used by standard planning.
/// A matching digest alone does not supply this captured source provenance.
pub(crate) trait CapturedRows: standard36::NativeRows {
    fn scene(&self) -> &SceneSnapshot;
}

/// The plan/index allocation charge outlives every borrowed record vector.
pub(crate) struct ScenePlan<'a> {
    standard: standard36::Plan<'a>,
    scene: &'a SceneSnapshot,
    baked: bool,
    _index: Reservation,
}

#[derive(Clone, Copy)]
struct RecordFeatures {
    adjustments: bool,
    tones: ToneRecords,
    clipped_adjustments: bool,
    blended_adjustments: bool,
    mask_links: bool,
    solid_fills: bool,
    layer_styles: bool,
    gradient_fills: bool,
    pattern_geometry: Option<Geometry>,
}

impl<'a> ScenePlan<'a> {
    /// Pure metadata inspection uses the PSB length envelope. A report does not
    /// authorize either output format or supply pixel/publication authority.
    pub(crate) fn for_report(
        scene: &'a SceneSnapshot,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::new(scene, Format::Psb, &[], check)
    }
    pub(crate) fn new(
        scene: &'a SceneSnapshot,
        format: Format,
        chunks: &'a [u32],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::with_features(
            scene,
            format,
            chunks,
            RecordFeatures {
                adjustments: true,
                tones: ToneRecords::current(scene.metadata().depth()),
                clipped_adjustments: true,
                blended_adjustments: true,
                mask_links: true,
                solid_fills: true,
                layer_styles: true,
                gradient_fills: true,
                pattern_geometry: Some(Geometry::Rotation),
            },
            None,
            check,
        )
    }
    pub(crate) fn for_import<R: Read + Seek>(
        scene: &'a SceneSnapshot,
        source: &super::row_stream::Source<R>,
        index: &super::structural::Index,
        chunks: &'a [u32],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        // Earlier Source36 writers baked every adjustment. Select their exact
        // plan when nvrt is absent; the caller still compares every record and
        // native channel before allowing editable restoration.
        // Clipped nvrt also identifies the newer unbaked clipping plan.
        // Earlier writers also kept the standard mask flag clear. lmgm
        // identifies the newer link layout. Verify all records in that layout.
        let has_tag = |key| {
            index
                .layers
                .iter()
                .any(|l| l.tags.iter().any(|t| t.key == key))
        };
        // A legacy hidden layer can retain shifted settings even though its
        // writer emitted only full-range Curves. Select the layout from the
        // actual bounded standard records, then compare every record and row.
        let mut curves = CurveRecords::Off;
        let mut payload = [0; MAX_CURVES_RECORD];
        for tag in index.layers.iter().flat_map(|layer| &layer.tags).filter(|tag| tag.key == *b"curv") {
            let bytes = usize::try_from(tag.data.bytes).map_err(|_| "Source36 Curves record is oversized")?;
            let bytes = payload.get_mut(..bytes).ok_or("Source36 Curves record is oversized")?;
            source.range(tag.data, check)?.read_exact(bytes).map_err(|e| e.to_string())?;
            curves = curves.max(CurveRecords::from_settings(&curves_settings(bytes)?));
        }
        if index.header.depth != 8 && curves >= CurveRecords::Photoshop {
            return Err("Source36 standard spline or combined Curves require 8-bit RGB".into());
        }
        let features = RecordFeatures {
            adjustments: has_tag(*b"nvrt"),
            tones: ToneRecords {
                levels: has_tag(*b"levl"),
                combined_levels: index.header.depth == 8,
                curves,
                posterize_depth: has_tag(*b"post").then_some(index.header.depth),
                threshold: has_tag(*b"thrs"),
                legacy_brightness: has_tag(*b"brit") && index.header.depth == 8,
                modern_brightness: has_tag(*b"CgEd") && index.header.depth == 8,
                exposure: has_tag(*b"expA") && index.header.depth == 8,
                channel_mixer: has_tag(*b"mixr") && index.header.depth == 8,
                black_white: has_tag(*b"blwh") && index.header.depth == 8,
                selective_color: has_tag(*b"selc") && index.header.depth == 8,
                vibrance: has_tag(*b"vibA") && index.header.depth == 8,
                photo_filter: has_tag(*b"phfl") && index.header.depth == 8,
                gradient_map: has_tag(*b"grdm") && index.header.depth == 8,
                color_balance: has_tag(*b"blnc") && index.header.depth == 8,
                hue_saturation: has_tag(*b"hue2") && index.header.depth == 8,
                tone_presets: has_tag(*b"CgEd"),
                color_lookup: has_tag(*b"clrL"),
                cube_lookup: has_tag(*b"clrL"),
            },
            clipped_adjustments: index.layers.iter().any(|layer| {
                layer.clipping == 1
                    && layer.tags.iter().any(|tag| tag.key == *b"nvrt" || is_tone_tag(&tag.key))
            }),
            blended_adjustments: index.layers.iter().any(|layer| {
                layer.blend_mode != *b"norm"
                    && layer.tags.iter().any(|tag| tag.key == *b"nvrt" || is_tone_tag(&tag.key))
            }),
            mask_links: has_tag(*b"lmgm"),
            solid_fills: has_tag(*b"SoCo"),
            layer_styles: has_tag(*b"lfx2") || has_tag(*b"lmfx"),
            gradient_fills: has_tag(*b"GdFl"),
            // Older plans rasterized some placements. Choose the layout from
            // the bounded record lengths before comparing every saved byte.
            pattern_geometry: index.layers.iter().flat_map(|layer| &layer.tags)
                .filter(|tag| tag.key == *b"PtFl")
                .filter_map(|tag| Geometry::for_record_size(tag.data.bytes)).max(),
        };
        Self::with_features(
            scene,
            index.header.format,
            chunks,
            features,
            Some(index),
            check,
        )
    }
    fn with_features(
        scene: &'a SceneSnapshot,
        format: Format,
        chunks: &'a [u32],
        features: RecordFeatures,
        indexed: Option<&super::structural::Index>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        let doc = scene.metadata();
        if doc.read_only()
            || doc.profile().is_some()
            || doc.layers().len() > 200
            || ![8, 16].contains(&doc.depth())
        {
            return Err("Source36 save requires a supported editable captured scene".into());
        }
        validate_hierarchy(doc, check)?;
        if let Some(mask) = doc.global_mask() {
            super::validate_global_mask_settings(&mask.data)?;
        }
        super::validate_photoshop_layer_ids(doc.layers().map(|layer| layer.psd_metadata()))?;
        for layer in doc.layers() {
            super::validate_layer_metadata(layer.psd_metadata(), layer.locked(), layer.kind() == "group")?;
        }
        let max_records = doc
            .layers()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(3))
            .ok_or("Source36 standard record index overflow")?;
        let tone_descriptor_bytes: usize = doc.layers().filter(|layer| layer.kind() == "adjustment")
            .filter_map(|layer| layer.effects().next()).map(|effect| match effect.kind() {
                crate::effects::gradient_map::KIND => super::gradient_map::MAX_RECORD,
                crate::effects::black_white::KIND => MAX_ADJUSTMENT_DESCRIPTOR_BYTES,
                crate::effects::color_lookup::KIND => {
                    use crate::effects::color_lookup::{MAX_NAME, source_size};
                    source_size(effect.settings(), "profile") + source_size(effect.settings(), "cube")
                        + 4 * MAX_NAME + 512
                        + if effect.settings()["embed_profile"] == true {
                            crate::color_profile::device_link::MAX_BYTES
                        } else { 0 }
                }
                kind if (crate::effects::supports_tone_preset(kind)
                    || matches!(kind, crate::effects::channel_mixer::KIND | crate::effects::photoshop_hue_saturation::KIND))
                    && effect.settings().get("preset").is_some() => MAX_ADJUSTMENT_DESCRIPTOR_BYTES,
                _ => 0,
            }).sum();
        let bytes = max_records
            .checked_mul(std::mem::size_of::<Record<'_>>())
            .and_then(|n| {
                n.checked_add(doc.layers().len() * super::MAX_LAYER_METADATA * std::mem::size_of::<Metadata<'_>>())
            })
            .and_then(|n| n.checked_add(tone_descriptor_bytes))
            .ok_or("Source36 standard metadata index overflow")?;
        let index = scene
            .budget()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(max_records)
            .map_err(|_| "Source36 standard record allocation refused")?;
        let baked = needs_bake(doc, features);
        if baked {
            push(
                &mut records,
                synthetic(
                    RowSource::BakedComposite,
                    "PeerBrush current composite",
                    [0, 0, doc.width() as i32, doc.height() as i32],
                    0,
                    true,
                ),
            )?;
            // An invisible enclosing folder preserves every original root and
            // relative child visibility without changing the private metadata.
            push(
                &mut records,
                synthetic(
                    RowSource::Zero,
                    "PeerBrush editable source layers",
                    [0; 4],
                    1,
                    false,
                ),
            )?;
        }
        collect(doc, None, baked, features, &mut records, check)?;
        if baked {
            push(
                &mut records,
                synthetic(
                    RowSource::Zero,
                    "PeerBrush editable source layers",
                    [0; 4],
                    3,
                    false,
                ),
            )?;
        }
        if records.is_empty() {
            push(
                &mut records,
                synthetic(
                    RowSource::Zero,
                    "Canvas",
                    [0, 0, doc.width() as i32, doc.height() as i32],
                    0,
                    true,
                ),
            )?;
        }
        records.reverse();
        let style_layout = |ordinal: usize, style: super::styles::StandardStyles<'_>| {
            // Older files can mix standard effects and baked layer appearances.
            style.source_format() <= super::COLOR_OVERLAY_FORMAT || !indexed.is_some_and(|index| {
                index.layers.get(ordinal).is_some_and(|layer| {
                    !layer.tags.iter().any(|tag| matches!(&tag.key, b"lfx2" | b"lmfx"))
                })
            })
        };
        let allow_patterns = !baked && !doc.global_metadata().iter().any(|entry| entry.key == "Patt");
        let pattern_fill = |source, section| {
            if !allow_patterns || section != 0 { return None; }
            let RowSource::Layer(index) = source else { return None; };
            super::patterns::standard(crate::engine::view::LayerView::Native(doc.layer(index)?),
                doc.width(), doc.height(), features.pattern_geometry?)
        };
        let fills = records.iter().filter_map(|record| {
            pattern_fill(record.source, record.section).map(|fill| fill.png)
        });
        let styles = records.iter().enumerate().filter_map(|(ordinal, record)| {
            record.standard_style.filter(|style| allow_patterns && style_layout(ordinal, *style))
                .and_then(super::styles::StandardStyles::pattern_source)
        });
        let patterns = super::patterns::StandardPatterns::prepare(
            fills.chain(styles), doc.depth(), scene.budget(), check,
        )?;
        for (ordinal, record) in records.iter_mut().enumerate() {
            check()?;
            if let Some(style) = record.standard_style {
                record.standard_style = style_layout(ordinal, style).then_some(style)
                    .and_then(|style| style.bind_patterns(&patterns));
                if record.standard_style.is_none() {
                    let RowSource::OriginalLayer(i) = record.source else {
                        return Err("Standard layer style has no original source layer".into());
                    };
                    record.solid = None;
                    record.source = RowSource::Layer(i);
                    let layer = doc.layer(i).ok_or("Rendered style layer is missing")?;
                    let frame = layer_frame(doc, layer, false)?;
                    record.bounds = frame;
                    if let Some(mask) = record.mask.as_mut() {
                        mask.bounds = frame;
                    }
                }
            }
            record.pattern = pattern_fill(record.source, record.section).and_then(|fill| patterns.record(fill));
            let Some(mask) = record.mask.as_mut() else {
                continue;
            };
            // Older standard saves emitted only -2, including on vector-mask layers.
            mask.dual = record.metadata.iter().any(|entry| entry.key == *b"vmsk")
                && indexed.is_none_or(|index| {
                    index.layers.get(ordinal)
                        .is_some_and(|layer| layer.channels.iter().any(|channel| channel.id == -3))
                });
            let layer = match record.source {
                RowSource::Layer(i) | RowSource::OriginalLayer(i) => {
                    doc.layer(i).ok_or("Standard mask layer is missing")?
                }
                _ => return Err("Standard mask has no layer source".into()),
            };
            if !features.mask_links && layer.mask().is_some_and(|m| !m.linked()) {
                continue;
            }
            let view = crate::engine::view::LayerView::Native(layer);
            mask.density = super::standard_mask_density(view);
            if mask.density.is_some() {
                mask.default = super::density_mask_default(view.mask().expect("Density requires a mask"));
            }
            let Some(extended) = super::extended_mask(
                view, mask.bounds, mask.density.is_some(),
            ) else {
                continue;
            };
            let [l, t, r, b] = extended.bounds;
            // Older saves can carry the retained original while writing only
            // the current mask frame. Readback still checks every standard byte.
            if indexed.is_none_or(|index| {
                index
                    .layers
                    .get(ordinal)
                    .and_then(|layer| layer.channels.iter().find(|channel| channel.id == -2))
                    .and_then(|channel| channel.dimensions)
                    == Some([(r - l) as u32, (b - t) as u32])
            }) {
                mask.bounds = extended.bounds;
                mask.default = extended.default;
            }
        }
        let header = Header {
            format,
            width: doc.width(),
            height: doc.height(),
            depth: doc.depth(),
            channels: 4,
        };
        let standard = standard36::Plan::new(
            header,
            provenance(scene),
            records,
            doc.global_mask().map_or(&[], |m| m.data.as_slice()),
            doc.global_metadata(),
            patterns,
            chunks,
            check,
        )?;
        check()?;
        Ok(Self {
            standard,
            scene,
            baked,
            _index: index,
        })
    }
    pub(crate) fn standard(&self) -> &standard36::Plan<'a> {
        &self.standard
    }
    pub(crate) fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    pub(crate) fn has_fallback(&self) -> bool {
        self.baked
    }
}

/// Runtime scene binding, separate from portable document or Store lineage IDs.
/// Providers calculate it once while retaining the exact immutable snapshot.
pub(crate) fn provenance(scene: &SceneSnapshot) -> Digest {
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrushSource36SaveScene\0");
    let identity = scene.identity();
    for text in [&identity.project_id, &identity.document_id] {
        hash.update(&(text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    hash.update(&identity.revision.to_be_bytes());
    let doc = scene.metadata();
    hash.update(&doc.width().to_be_bytes());
    hash.update(&doc.height().to_be_bytes());
    hash.update(&doc.depth().to_be_bytes());
    for (id, source) in scene.rasters().iter() {
        hash.update(&id.to_be_bytes());
        hash.update(&source.key().bytes());
    }
    hash.finish().as_ref().try_into().unwrap()
}
fn needs_bake(doc: DocumentMetadata<'_>, features: RecordFeatures) -> bool {
    doc.filters().len() != 0
        || doc.layers().any(|layer| {
            layer.kind() == "adjustment"
                && !((features.adjustments
                    && (features.blended_adjustments || layer.blend() == "normal")
                    && super::adjustments::standard_invert(
                        crate::engine::view::LayerView::Native(layer),
                        doc.width(),
                        doc.height(),
                        features.clipped_adjustments,
                    )) || super::adjustments::standard_tone(
                        crate::engine::view::LayerView::Native(layer), doc.width(), doc.height(),
                        features.clipped_adjustments, features.tones,
                    ).is_some())
                || layer.kind() == "group"
                    && layer.effects().any(|e| e.enabled() && e.weight() > 0.)
                || layer.clip_to().is_some_and(|id| {
                    !(["paint", "fill"].contains(&layer.kind())
                        || features.clipped_adjustments && layer.kind() == "adjustment")
                        || doc
                            .layers()
                            .find(|base| base.id() == id)
                            .is_none_or(|base| {
                                !["paint", "fill"].contains(&base.kind())
                                    || base.parent() != layer.parent()
                            })
                })
        })
}
fn validate_hierarchy(
    doc: DocumentMetadata<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    for (i, layer) in doc.layers().enumerate() {
        check()?;
        if layer.id().is_empty()
            || doc.layers().take(i).any(|l| l.id() == layer.id())
            || !["paint", "fill", "group", "adjustment"].contains(&layer.kind())
            || !layer.opacity().is_finite()
            || !(0.0..=1.0).contains(&layer.opacity())
            || layer.pixels().depth() != doc.depth()
            || layer.origin()[0].unsigned_abs() > 100000
            || layer.origin()[1].unsigned_abs() > 100000
        {
            return Err("Source36 standard layer identity/kind/precision differs".into());
        }
        let mut parent = layer.parent();
        let mut depth = 0;
        while let Some(id) = parent {
            depth += 1;
            if depth > 16 {
                return Err("Source36 standard group topology is cyclic or too deep".into());
            }
            parent = doc
                .layers()
                .find(|l| l.id() == id && l.kind() == "group")
                .ok_or("Source36 standard group parent is missing")?
                .parent();
        }
        if let Some(id) = layer.clip_to() {
            if id == layer.id() || !doc.layers().any(|l| l.id() == id) {
                return Err("Source36 standard clipping source is missing or cyclic".into());
            }
        }
    }
    Ok(())
}
fn collect<'a>(
    doc: DocumentMetadata<'a>,
    parent: Option<&str>,
    originals: bool,
    features: RecordFeatures,
    out: &mut Vec<Record<'a>>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    for (i, layer) in doc
        .layers()
        .enumerate()
        .filter(|(_, l)| l.parent() == parent)
    {
        check()?;
        if layer.kind() == "group" {
            push(out, record(doc, layer, i, super::folder_section(layer.psd_metadata()), originals, features)?)?;
            collect(doc, Some(layer.id()), originals, features, out, check)?;
            push(out, record(doc, layer, i, 3, originals, features)?)?;
        } else {
            push(out, record(doc, layer, i, 0, originals, features)?)?;
        }
    }
    Ok(())
}
fn push<'a>(out: &mut Vec<Record<'a>>, record: Record<'a>) -> Result<(), String> {
    if out.len() == standard36::MAX_STANDARD_RECORDS {
        return Err(
            "Source36 standard fallback/group records exceed the derived 403-record limit".into(),
        );
    }
    out.try_reserve(1)
        .map_err(|_| "Source36 standard record allocation refused")?;
    out.push(record);
    Ok(())
}
fn synthetic(
    source: RowSource,
    name: &str,
    bounds: [i32; 4],
    section: u32,
    visible: bool,
) -> Record<'_> {
    Record {
        source,
        bounds,
        mask: None,
        name,
        blend: if section == 1 { *b"pass" } else { *b"norm" },
        opacity: 255,
        clipped: false,
        visible,
        locked: false,
        invert: false,
        tone: None,
        solid: None,
        standard_style: None,
        gradient: None,
        pattern: None,
        has_clipped_layers: false,
        section,
        metadata: vec![],
    }
}
pub(super) fn layer_frame(
    doc: DocumentMetadata<'_>,
    layer: LayerMetadata<'_>,
    originals: bool,
) -> Result<[i32; 4], String> {
    let canvas_frame = ["group", "adjustment"].contains(&layer.kind())
        || !originals
            && layer.effects().any(|e| {
                e.enabled() && e.weight() > 0. && crate::effects::styles::contains(e.kind())
            });
    Ok(if canvas_frame {
        [0, 0, doc.width() as i32, doc.height() as i32]
    } else {
        [
            layer.origin()[0],
            layer.origin()[1],
            i32::try_from(i64::from(layer.origin()[0]) + i64::from(layer.pixels().width()))
                .map_err(|_| "Source36 layer right overflow")?,
            i32::try_from(i64::from(layer.origin()[1]) + i64::from(layer.pixels().height()))
                .map_err(|_| "Source36 layer bottom overflow")?,
        ]
    })
}
fn record<'a>(
    doc: DocumentMetadata<'a>,
    layer: LayerMetadata<'a>,
    i: usize,
    section: u32,
    originals: bool,
    features: RecordFeatures,
) -> Result<Record<'a>, String> {
    let has_clipped_layers = doc.layers().any(|l| l.clip_to() == Some(layer.id()));
    let standard_style = if features.layer_styles && (features.solid_fills || layer.kind() == "paint")
        && section == 0 && !originals && !has_clipped_layers
    {
        super::styles::standard_styles(
            crate::engine::view::LayerView::Native(layer), doc.width(), doc.height(),
            doc.psd_resources().global_angle.map(u32::from_be_bytes),
        )
    } else { None };
    let frame = layer_frame(doc, layer, originals || standard_style.is_some())?;
    let mut metadata = Vec::new();
    for (key, bytes) in super::standard_layer_metadata(layer.psd_metadata(), section) {
        if metadata.is_empty() {
            metadata
                .try_reserve_exact(layer.psd_metadata().len())
                .map_err(|_| "Source36 layer metadata index allocation refused")?;
        }
        metadata.push(Metadata {
            key: key.as_bytes().try_into().map_err(|_| "Source36 metadata key differs")?,
            bytes,
        });
    }
    let invert = features.adjustments
        && (features.blended_adjustments || layer.blend() == "normal")
        && section == 0
        && super::adjustments::standard_invert(
            crate::engine::view::LayerView::Native(layer),
            doc.width(),
            doc.height(),
            features.clipped_adjustments && !originals,
        );
    let tone = if section == 0 {
        super::adjustments::standard_tone(crate::engine::view::LayerView::Native(layer),
            doc.width(), doc.height(), features.clipped_adjustments && !originals,
            features.tones)
    } else { None };
    Ok(Record {
        source: if originals || invert || tone.is_some() || standard_style.is_some() {
            RowSource::OriginalLayer(i)
        } else {
            RowSource::Layer(i)
        },
        bounds: if section != 0 { [0; 4] } else { frame },
        mask: if section != 3 {
            layer.mask().map(|m| Mask {
                bounds: frame,
                enabled: m.enabled(),
                linked: !features.mask_links || m.linked(),
                default: 255,
                dual: false,
                density: None,
            })
        } else {
            None
        },
        name: layer.name(),
        blend: if section == 3 && layer.blend() == "pass_through" {
            *b"norm"
        } else {
            blend(layer.blend())?
        },
        opacity: (layer.opacity() * 255.).round() as u8,
        clipped: layer.clip_to().is_some(),
        visible: layer.visible(),
        locked: layer.locked(),
        invert,
        tone,
        solid: if features.solid_fills && section == 0 && !originals {
            standard_style.filter(|overlay| overlay.fill).map(|_| layer.color()).or_else(|| {
                super::adjustments::standard_solid(crate::engine::view::LayerView::Native(layer), doc.width(), doc.height())
            })
        } else {
            None
        },
        standard_style,
        has_clipped_layers,
        gradient: if features.gradient_fills && section == 0 && !originals {
            super::gradient_fill::standard(crate::engine::view::LayerView::Native(layer), doc.width(), doc.height())
        } else { None },
        pattern: None,
        section,
        metadata,
    })
}
pub(super) fn blend(value: &str) -> Result<[u8; 4], String> {
    Ok(match value {
        "normal" => *b"norm",
        "pass_through" => *b"pass",
        "multiply" => *b"mul ",
        "screen" => *b"scrn",
        "overlay" => *b"over",
        "darken" => *b"dark",
        "lighten" => *b"lite",
        "linear_dodge" => *b"lddg",
        "linear_burn" => *b"lbrn",
        "color_dodge" => *b"div ",
        "color_burn" => *b"idiv",
        "hard_light" => *b"hLit",
        "linear_light" => *b"lLit",
        "vivid_light" => *b"vLit",
        "pin_light" => *b"pLit",
        "hard_mix" => *b"hMix",
        "darker_color" => *b"dkCl",
        "lighter_color" => *b"lgCl",
        "hue" => *b"hue ",
        "saturation" => *b"sat ",
        "color" => *b"colr",
        "luminosity" => *b"lum ",
        "soft_light" => *b"sLit",
        "difference" => *b"diff",
        "exclusion" => *b"smud",
        "subtract" => *b"fsub",
        "divide" => *b"fdiv",
        _ => return Err("Unsupported Source36 standard blend mode".into()),
    })
}
