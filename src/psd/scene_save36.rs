//! Pure standard metadata plan over an immutable admitted scene. Pixel providers
//! and final save publication remain fallible, separately guarded operations.
pub(crate) use super::save36::CompletedSave36;
use super::{
    bulk36::Digest,
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
        for layer in doc.layers() {
            super::validate_metadata(layer.psd_metadata())?;
        }
        let max_records = doc
            .layers()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(3))
            .ok_or("Source36 standard record index overflow")?;
        let bytes = max_records
            .checked_mul(std::mem::size_of::<Record<'_>>())
            .and_then(|n| {
                n.checked_add(doc.layers().len() * 5 * std::mem::size_of::<Metadata<'_>>())
            })
            .ok_or("Source36 standard metadata index overflow")?;
        let index = scene
            .budget()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(max_records)
            .map_err(|_| "Source36 standard record allocation refused")?;
        let baked = needs_bake(doc);
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
        collect(doc, None, baked, &mut records, check)?;
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
fn needs_bake(doc: DocumentMetadata<'_>) -> bool {
    doc.filters().len() != 0
        || doc.layers().any(|layer| {
            layer.kind() == "adjustment"
                || layer.kind() == "group"
                    && layer.effects().any(|e| e.enabled() && e.weight() > 0.)
                || layer.clip_to().is_some_and(|id| {
                    !["paint", "fill"].contains(&layer.kind())
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
            push(out, record(doc, layer, i, 1, originals)?)?;
            collect(doc, Some(layer.id()), originals, out, check)?;
            push(out, record(doc, layer, i, 3, originals)?)?;
        } else {
            push(out, record(doc, layer, i, 0, originals)?)?;
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
) -> Result<Record<'a>, String> {
    let frame = layer_frame(doc, layer, originals)?;
    let mut metadata = Vec::new();
    if section != 3 {
        metadata
            .try_reserve_exact(layer.psd_metadata().len())
            .map_err(|_| "Source36 layer metadata index allocation refused")?;
        for m in layer.psd_metadata() {
            metadata.push(Metadata {
                key: m
                    .key
                    .as_bytes()
                    .try_into()
                    .map_err(|_| "Source36 metadata key differs")?,
                bytes: &m.data,
            });
        }
    }
    Ok(Record {
        source: if originals {
            RowSource::OriginalLayer(i)
        } else {
            RowSource::Layer(i)
        },
        bounds: if section != 0 { [0; 4] } else { frame },
        mask: if section != 3 {
            layer.mask().map(|m| Mask {
                bounds: frame,
                enabled: m.enabled(),
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
        has_clipped_layers: doc.layers().any(|l| l.clip_to() == Some(layer.id())),
        section,
        metadata,
    })
}
fn blend(value: &str) -> Result<[u8; 4], String> {
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
        "soft_light" => *b"sLit",
        "difference" => *b"diff",
        "exclusion" => *b"smud",
        "subtract" => *b"fsub",
        "divide" => *b"fdiv",
        _ => return Err("Unsupported Source36 standard blend mode".into()),
    })
}
