//! Worker-local typed asset slots. These tokens never enter a saved Source36
//! document or establish pixel/source authority.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssetKind {
    Settings,
    Profile,
    GlobalMask,
    Contours,
    Polygon,
    Content,
    Psd,
    Warnings,
}
pub(crate) enum MetadataAsset {
    Settings(MetadataPayload<Value>),
    Profile(MetadataPayload<Vec<u8>>),
    GlobalMask(MetadataPayload<crate::psd::GlobalMaskSettings>),
    Contours(MetadataPayload<Vec<Vec<[f32; 2]>>>),
    Polygon(MetadataPayload<Vec<[f32; 2]>>),
    Content(MetadataPayload<Content36>),
    Psd(MetadataPayload<Vec<crate::psd::LayerMetadata>>),
    Warnings(MetadataPayload<Vec<String>>),
}
macro_rules! asset_match {
    ($self:expr,$value:ident=>$body:expr) => {
        match $self {
            MetadataAsset::Settings($value) => $body,
            MetadataAsset::Profile($value) => $body,
            MetadataAsset::GlobalMask($value) => $body,
            MetadataAsset::Contours($value) => $body,
            MetadataAsset::Polygon($value) => $body,
            MetadataAsset::Content($value) => $body,
            MetadataAsset::Psd($value) => $body,
            MetadataAsset::Warnings($value) => $body,
        }
    };
}
impl MetadataAsset {
    pub(crate) fn kind(&self) -> AssetKind {
        match self {
            Self::Settings(_) => AssetKind::Settings,
            Self::Profile(_) => AssetKind::Profile,
            Self::GlobalMask(_) => AssetKind::GlobalMask,
            Self::Contours(_) => AssetKind::Contours,
            Self::Polygon(_) => AssetKind::Polygon,
            Self::Content(_) => AssetKind::Content,
            Self::Psd(_) => AssetKind::Psd,
            Self::Warnings(_) => AssetKind::Warnings,
        }
    }
    pub(crate) fn shared(&self) -> Result<Self> {
        Ok(match self {
            Self::Settings(v) => Self::Settings(v.shared()?),
            Self::Profile(v) => Self::Profile(v.shared()?),
            Self::GlobalMask(v) => Self::GlobalMask(v.shared()?),
            Self::Contours(v) => Self::Contours(v.shared()?),
            Self::Polygon(v) => Self::Polygon(v.shared()?),
            Self::Content(v) => Self::Content(v.shared()?),
            Self::Psd(v) => Self::Psd(v.shared()?),
            Self::Warnings(v) => Self::Warnings(v.shared()?),
        })
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Settings(a), Self::Settings(b)) => a == b,
            (Self::Profile(a), Self::Profile(b)) => a == b,
            (Self::GlobalMask(a), Self::GlobalMask(b)) => a == b,
            (Self::Contours(a), Self::Contours(b)) => a == b,
            (Self::Polygon(a), Self::Polygon(b)) => a == b,
            (Self::Content(a), Self::Content(b)) => a == b,
            (Self::Psd(a), Self::Psd(b)) => a == b,
            (Self::Warnings(a), Self::Warnings(b)) => a == b,
            _ => false,
        }
    }
    pub(crate) fn retained_bytes(&self) -> Result<usize> {
        asset_match!(self,value=>value.retained_bytes().ok_or_else(||"Task asset is not runtime-bound".into()))
    }
    pub(crate) fn settings(&self) -> Option<&Value> {
        if let Self::Settings(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub(crate) fn content(&self) -> Option<&Content36> {
        if let Self::Content(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub(crate) fn value(&self) -> Result<Value> {
        asset_match!(self,value=>serde_json::to_value(value).map_err(|e|e.to_string()))
    }
    pub(crate) fn from_value(
        kind: AssetKind,
        value: Value,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        macro_rules! make {
            ($ty:ty,$variant:ident) => {{
                let value = serde_json::from_value::<$ty>(value).map_err(|e| e.to_string())?;
                Self::$variant(MetadataPayload::from(value).bind(pool, check)?)
            }};
        }
        Ok(match kind {
            AssetKind::Settings => make!(Value, Settings),
            AssetKind::Profile => make!(Vec<u8>, Profile),
            AssetKind::GlobalMask => make!(crate::psd::GlobalMaskSettings, GlobalMask),
            AssetKind::Contours => make!(Vec<Vec<[f32; 2]>>, Contours),
            AssetKind::Polygon => make!(Vec<[f32; 2]>, Polygon),
            AssetKind::Content => make!(Content36, Content),
            AssetKind::Psd => make!(Vec<crate::psd::LayerMetadata>, Psd),
            AssetKind::Warnings => make!(Vec<String>, Warnings),
        })
    }
    pub(crate) fn marker(&self, id: u32) -> Result<Value> {
        let bytes = marker_bytes(id);
        let point = [(id >> 16) as f32, (id & 65535) as f32];
        Ok(match self.kind() {
            AssetKind::Settings => serde_json::json!(["PBA1", id]),
            AssetKind::Profile => Value::String({
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(bytes)
            }),
            AssetKind::GlobalMask => serde_json::json!({"data":bytes}),
            AssetKind::Contours => serde_json::json!([[point]]),
            AssetKind::Polygon => serde_json::json!([point, point, point]),
            AssetKind::Content => serde_json::json!({"kind":"svg","svg":format!("PBA1:{id}")}),
            AssetKind::Psd => serde_json::json!([{"key":"PBA1","data":bytes}]),
            AssetKind::Warnings => serde_json::json!([format!("PBA1:{id}")]),
        })
    }
    fn stub(&self, id: u32) -> Result<Self> {
        if self.kind() == AssetKind::Profile {
            return Ok(Self::Profile(marker_bytes(id).to_vec().into()));
        }
        Self::wire(self.kind(), self.marker(id)?)
    }
    fn wire(kind: AssetKind, value: Value) -> Result<Self> {
        macro_rules! make {
            ($ty:ty,$variant:ident) => {{
                Self::$variant(MetadataPayload::from(
                    serde_json::from_value::<$ty>(value).map_err(|e| e.to_string())?,
                ))
            }};
        }
        Ok(match kind {
            AssetKind::Settings => make!(Value, Settings),
            AssetKind::Profile => make!(Vec<u8>, Profile),
            AssetKind::GlobalMask => make!(crate::psd::GlobalMaskSettings, GlobalMask),
            AssetKind::Contours => make!(Vec<Vec<[f32; 2]>>, Contours),
            AssetKind::Polygon => make!(Vec<[f32; 2]>, Polygon),
            AssetKind::Content => make!(Content36, Content),
            AssetKind::Psd => make!(Vec<crate::psd::LayerMetadata>, Psd),
            AssetKind::Warnings => make!(Vec<String>, Warnings),
        })
    }
    fn token(&self) -> Result<u32> {
        let id = match self {
            Self::Settings(value) => {
                let v = value
                    .as_array()
                    .filter(|v| v.len() == 2 && v[0] == "PBA1")
                    .ok_or("Invalid settings asset token")?;
                u32::try_from(v[1].as_u64().ok_or("Invalid settings asset ordinal")?)
                    .map_err(|_| "Asset ordinal overflow")?
            }
            Self::Profile(value) => read_marker(value)?,
            Self::GlobalMask(value) => read_marker(&value.data)?,
            Self::Contours(value) => {
                if value.len() != 1 || value[0].len() != 1 {
                    return Err("Invalid contour asset token".into());
                }
                point_marker(&value[0][0])?
            }
            Self::Polygon(value) => {
                if value.len() != 3 || value.iter().any(|v| v != &value[0]) {
                    return Err("Invalid polygon asset token".into());
                }
                point_marker(&value[0])?
            }
            Self::Content(value) => {
                let Content36::Svg { svg } = value.value() else {
                    return Err("Invalid source-content asset token".into());
                };
                svg.strip_prefix("PBA1:")
                    .ok_or("Invalid source asset marker")?
                    .parse::<u32>()
                    .map_err(|_| "Invalid source asset ordinal")?
            }
            Self::Psd(value) => {
                if value.len() != 1 || value[0].key != "PBA1" {
                    return Err("Invalid PSD asset token".into());
                }
                read_marker(&value[0].data)?
            }
            Self::Warnings(value) => {
                if value.len() != 1 {
                    return Err("Invalid warning asset token".into());
                }
                value[0]
                    .strip_prefix("PBA1:")
                    .ok_or("Invalid warning asset marker")?
                    .parse::<u32>()
                    .map_err(|_| "Invalid warning asset ordinal")?
            }
        };
        if id == 0 {
            return Err("Zero metadata asset ordinal".into());
        }
        Ok(id)
    }
}
fn marker_bytes(id: u32) -> [u8; 8] {
    let mut bytes = *b"PBA1\0\0\0\0";
    bytes[4..].copy_from_slice(&id.to_be_bytes());
    bytes
}
fn read_marker(bytes: &[u8]) -> Result<u32> {
    if bytes.len() != 8 || &bytes[..4] != b"PBA1" {
        return Err("Invalid provisional metadata asset token".into());
    }
    Ok(u32::from_be_bytes(bytes[4..].try_into().unwrap()))
}
fn point_marker(point: &[f32; 2]) -> Result<u32> {
    if point
        .iter()
        .any(|v| !v.is_finite() || *v < 0. || *v > 65535. || v.fract() != 0.)
    {
        return Err("Invalid provisional contour asset token".into());
    }
    Ok(((point[0] as u32) << 16) | (point[1] as u32))
}

#[derive(Clone, Copy)]
pub(crate) enum Part {
    Key(&'static str),
    Index(usize),
}
#[derive(Clone, Copy)]
pub(crate) struct AssetPath([Option<Part>; 9]);
impl AssetPath {
    fn new(parts: &[Part]) -> Self {
        let mut path = [None; 9];
        for (at, part) in parts.iter().enumerate() {
            path[at] = Some(*part);
        }
        Self(path)
    }
    pub(crate) fn parts(&self) -> impl Iterator<Item = Part> + '_ {
        self.0.iter().flatten().copied()
    }
}
pub(crate) struct AssetEntry {
    pub path: AssetPath,
    pub asset: MetadataAsset,
}
pub(crate) struct AssetProjection {
    pub header: Admitted<Value>,
    pub entries: Vec<AssetEntry>,
    _charge: Arc<MetadataCharge>,
}
impl Document36 {
    fn asset_count(&self) -> Result<usize> {
        fn add(total: &mut usize, count: usize) -> Result<()> {
            *total = total
                .checked_add(count)
                .filter(|n| *n <= 1_048_576)
                .ok_or("Native asset count exceeds admission")?;
            Ok(())
        }
        let mut count = 1;
        add(
            &mut count,
            usize::from(self.icc_profile.is_some())
                + usize::from(self.psd_global_mask.is_some())
                + usize::from(self.selection_polygon.is_some())
                + usize::from(self.selection_coverage.is_some())
                + usize::from(self.selection_previous.is_some()),
        )?;
        for layer in &self.layers {
            add(&mut count, 1 + usize::from(layer.source.is_some()))?;
            if let Some(mask) = &layer.mask {
                add(&mut count, mask.steps.len())?;
            }
            for effect in &layer.effects {
                add(&mut count, 1 + effect.clips.len())?;
            }
        }
        for effect in &self.filters {
            add(&mut count, 1 + effect.clips.len())?;
        }
        Ok(count)
    }
    fn map_assets(
        &mut self,
        mut map: impl FnMut(AssetPath, MetadataAsset) -> Result<MetadataAsset>,
    ) -> Result<()> {
        use Part::{Index, Key};
        macro_rules! field {
            ($field:expr,$variant:ident,$default:expr,$parts:expr) => {{
                let current = MetadataAsset::$variant(std::mem::replace(
                    &mut $field,
                    MetadataPayload::from($default),
                ));
                let next = map(AssetPath::new($parts), current)?;
                let MetadataAsset::$variant(value) = next else {
                    return Err("Typed metadata asset slot changed kind".into());
                };
                $field = value;
            }};
        }
        if let Some(value) = &mut self.icc_profile {
            field!(*value, Profile, Vec::<u8>::new(), &[Key("icc_profile")]);
        }
        if let Some(value) = &mut self.psd_global_mask {
            field!(
                *value,
                GlobalMask,
                crate::psd::GlobalMaskSettings { data: vec![] },
                &[Key("psd_global_mask")]
            );
        }
        for (li, layer) in self.layers.iter_mut().enumerate() {
            if let Some(mask) = &mut layer.mask {
                for (si, step) in mask.steps.iter_mut().enumerate() {
                    field!(
                        step.settings,
                        Settings,
                        Value::Null,
                        &[
                            Key("layers"),
                            Index(li),
                            Key("mask"),
                            Key("steps"),
                            Index(si),
                            Key("settings")
                        ]
                    );
                }
            }
            for (ei, effect) in layer.effects.iter_mut().enumerate() {
                field!(
                    effect.settings,
                    Settings,
                    Value::Null,
                    &[
                        Key("layers"),
                        Index(li),
                        Key("effects"),
                        Index(ei),
                        Key("settings")
                    ]
                );
                for (ci, clip) in effect.clips.iter_mut().enumerate() {
                    field!(
                        clip.coverage.contours,
                        Contours,
                        Vec::<Vec<[f32; 2]>>::new(),
                        &[
                            Key("layers"),
                            Index(li),
                            Key("effects"),
                            Index(ei),
                            Key("clips"),
                            Index(ci),
                            Key("coverage"),
                            Key("contours")
                        ]
                    );
                }
            }
            field!(
                layer.psd_metadata,
                Psd,
                Vec::<crate::psd::LayerMetadata>::new(),
                &[Key("layers"), Index(li), Key("psd_metadata")]
            );
            if let Some(source) = &mut layer.source {
                field!(
                    source.content,
                    Content,
                    Content36::Svg { svg: String::new() },
                    &[Key("layers"), Index(li), Key("source"), Key("content")]
                );
            }
        }
        for (ei, effect) in self.filters.iter_mut().enumerate() {
            field!(
                effect.settings,
                Settings,
                Value::Null,
                &[Key("filters"), Index(ei), Key("settings")]
            );
            for (ci, clip) in effect.clips.iter_mut().enumerate() {
                field!(
                    clip.coverage.contours,
                    Contours,
                    Vec::<Vec<[f32; 2]>>::new(),
                    &[
                        Key("filters"),
                        Index(ei),
                        Key("clips"),
                        Index(ci),
                        Key("coverage"),
                        Key("contours")
                    ]
                );
            }
        }
        if let Some(value) = &mut self.selection_polygon {
            field!(
                *value,
                Polygon,
                Vec::<[f32; 2]>::new(),
                &[Key("selection_polygon")]
            );
        }
        if let Some(value) = &mut self.selection_coverage {
            field!(
                value.contours,
                Contours,
                Vec::<Vec<[f32; 2]>>::new(),
                &[Key("selection_coverage"), Key("contours")]
            );
        }
        if let Some(value) = &mut self.selection_previous {
            field!(
                value.contours,
                Contours,
                Vec::<Vec<[f32; 2]>>::new(),
                &[Key("selection_previous"), Key("contours")]
            );
        }
        field!(
            self.warnings,
            Warnings,
            Vec::<String>::new(),
            &[Key("warnings")]
        );
        Ok(())
    }
    pub(crate) fn project_assets(
        &self,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<AssetProjection> {
        let count = self.asset_count()?;
        let bytes = count
            .checked_mul(std::mem::size_of::<AssetEntry>() + 512)
            .and_then(|n| n.checked_add(128))
            .ok_or("Native asset projection footprint overflow")?;
        let charge = MetadataCharge::reserve(pool, bytes, check)?;
        let copied = self.copy_runtime(pool, check)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count)
            .map_err(|_| "Native asset projection allocation refused")?;
        if entries.capacity() > count {
            return Err("Native asset projection exceeds admission".into());
        }
        let mut header = None;
        let moved = copied.map_unique(|mut metadata, _| {
            metadata.map_assets(|path, asset| {
                check()?;
                let id =
                    u32::try_from(entries.len() + 1).map_err(|_| "Native asset token overflow")?;
                let marker = asset.stub(id)?;
                entries.push(AssetEntry { path, asset });
                Ok(marker)
            })?;
            header = Some(super::super::native::copy_admitted(
                &metadata, pool, false, check,
            )?);
            Ok(())
        })?;
        drop(moved);
        Ok(AssetProjection {
            header: header.ok_or("Native asset projection handoff failed")?,
            entries,
            _charge: charge,
        })
    }
    pub(crate) fn restore_assets(
        &mut self,
        mut restore: impl FnMut(u32, AssetKind) -> Result<MetadataAsset>,
    ) -> Result<()> {
        self.map_assets(|_, asset| {
            let id = asset.token()?;
            let restored = restore(id, asset.kind())?;
            if restored.kind() != asset.kind() {
                return Err("Native asset token kind differs".into());
            }
            Ok(restored)
        })
    }
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;
