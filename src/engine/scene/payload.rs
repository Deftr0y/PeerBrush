//! Immutable physical metadata assets. Wire values stay under the parser's
//! admission until runtime binding; shared clones retain value AND charge.
use crate::{large_document::ResidentBudget, psd::memory36::MetadataCharge};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, ops::Deref, sync::Arc};

type Result<T> = std::result::Result<T, String>;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const OWNER_BYTES: usize = 512;
const MAX_NODES: usize = 8 * 1024 * 1024;

pub struct MetadataPayload<T> {
    state: State<T>,
}
enum State<T> {
    Wire(T),
    Shared(Arc<Owned<T>>),
}
struct Owned<T> {
    // Physical payload must drop before its last accounting owner.
    value: T,
    charge: Arc<MetadataCharge>,
}
impl<T> From<T> for MetadataPayload<T> {
    /// A wire/producer value, covered by its caller's preexisting admission.
    /// This does not grant a shared runtime payload or a document mutation.
    fn from(value: T) -> Self {
        Self {
            state: State::Wire(value),
        }
    }
}
impl<T> Deref for MetadataPayload<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match &self.state {
            State::Wire(value) => value,
            State::Shared(value) => &value.value,
        }
    }
}
impl<T: fmt::Debug> fmt::Debug for MetadataPayload<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}
impl<T: PartialEq> PartialEq for MetadataPayload<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value()
    }
}
impl<T: Serialize> Serialize for MetadataPayload<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for MetadataPayload<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::from)
    }
}
impl<T> MetadataPayload<T> {
    pub fn value(&self) -> &T {
        self
    }
    pub(crate) fn shared(&self) -> Result<Self> {
        match &self.state {
            State::Shared(value) => Ok(Self {
                state: State::Shared(value.clone()),
            }),
            State::Wire(_) => {
                Err("Unbound wire metadata cannot be shared as runtime authority".into())
            }
        }
    }
    pub(crate) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        matches!(&self.state,State::Shared(value) if value.charge.uses_budget(pool))
    }
    pub(crate) fn retained_bytes(&self) -> Option<usize> {
        match &self.state {
            State::Wire(_) => None,
            State::Shared(value) => Some(value.charge.bytes()),
        }
    }
    #[cfg(test)]
    pub(crate) fn same_allocation(&self, other: &Self) -> bool {
        matches!((&self.state,&other.state),(State::Shared(a),State::Shared(b)) if Arc::ptr_eq(a,b))
    }
}
impl<T: Footprint> MetadataPayload<T> {
    pub(crate) fn copy_from(
        value: &T,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self>
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let temporary: crate::psd::memory36::Admitted<T> =
            super::super::native::copy_admitted(value, pool, true, check)?;
        let mut result = None;
        let moved = temporary.map_unique(|value, _| {
            result = Some(Self::from(value).bind(pool, check)?);
            Ok(())
        })?;
        drop(moved);
        result.ok_or_else(|| "Metadata asset handoff failed".into())
    }
    pub(crate) fn bind(
        self,
        pool: &ResidentBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        match self.state {
            State::Shared(value) => {
                if !value.charge.uses_budget(pool) {
                    return Err("Metadata asset uses another resident pool".into());
                }
                Ok(Self {
                    state: State::Shared(value),
                })
            }
            State::Wire(value) => {
                let mut measure = Measure::new(check);
                measure.add(std::mem::size_of::<T>() + OWNER_BYTES)?;
                value.heap(&mut measure)?;
                let charge = MetadataCharge::reserve(pool, measure.bytes, check)?;
                check()?;
                Ok(Self {
                    state: State::Shared(Arc::new(Owned { value, charge })),
                })
            }
        }
    }
}

/// Conservative retained heap measurement, including actual vector/string
/// capacities. This measures owned allocations, not serialized text length.
pub(crate) trait Footprint {
    fn heap(&self, measure: &mut Measure<'_>) -> Result<()>;
}
pub(crate) struct Measure<'a> {
    bytes: usize,
    nodes: usize,
    depth: usize,
    check: &'a dyn Fn() -> Result<()>,
}
impl<'a> Measure<'a> {
    pub(super) fn new(check: &'a dyn Fn() -> Result<()>) -> Self {
        Self {
            bytes: 0,
            nodes: 0,
            depth: 0,
            check,
        }
    }
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(crate) fn add(&mut self, bytes: usize) -> Result<()> {
        (self.check)()?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or("Metadata asset retained footprint exceeds admission")?;
        Ok(())
    }
    pub(crate) fn node(&mut self) -> Result<()> {
        (self.check)()?;
        self.nodes = self
            .nodes
            .checked_add(1)
            .filter(|n| *n <= MAX_NODES)
            .ok_or("Metadata asset node count exceeds admission")?;
        Ok(())
    }
    pub(crate) fn vector<T>(&mut self, value: &Vec<T>) -> Result<()> {
        self.add(
            value
                .capacity()
                .checked_mul(std::mem::size_of::<T>())
                .ok_or("Metadata vector footprint overflow")?,
        )
    }
}
impl Footprint for u8 {
    fn heap(&self, _: &mut Measure<'_>) -> Result<()> {
        Ok(())
    }
}
impl Footprint for [f32; 2] {
    fn heap(&self, measure: &mut Measure<'_>) -> Result<()> {
        measure.node()?;
        if self.iter().any(|v| !v.is_finite()) {
            return Err("Nonfinite metadata coordinate".into());
        }
        Ok(())
    }
}
impl Footprint for String {
    fn heap(&self, measure: &mut Measure<'_>) -> Result<()> {
        measure.node()?;
        measure.add(self.capacity())
    }
}
impl<T: Footprint> Footprint for Vec<T> {
    fn heap(&self, measure: &mut Measure<'_>) -> Result<()> {
        measure.node()?;
        measure.vector(self)?;
        for value in self {
            value.heap(measure)?;
        }
        Ok(())
    }
}
impl Footprint for serde_json::Value {
    fn heap(&self, measure: &mut Measure<'_>) -> Result<()> {
        measure.node()?;
        measure.depth += 1;
        if measure.depth > 128 {
            return Err("Metadata asset nesting exceeds admission".into());
        }
        match self {
            Self::Null | Self::Bool(_) | Self::Number(_) => {}
            Self::String(value) => value.heap(measure)?,
            Self::Array(values) => values.heap(measure)?,
            Self::Object(values) => {
                // Covers B-tree or ordered-map nodes, buckets and their keys;
                // Value bodies are already in this per-entry envelope.
                measure.add(
                    values
                        .len()
                        .checked_mul(512)
                        .and_then(|n| n.checked_add(128))
                        .ok_or("Metadata object footprint overflow")?,
                )?;
                for (key, value) in values {
                    key.heap(measure)?;
                    value.heap(measure)?;
                }
            }
        }
        measure.depth -= 1;
        Ok(())
    }
}
impl Footprint for crate::psd::LayerMetadata {
    fn heap(&self, measure: &mut Measure<'_>) -> Result<()> {
        self.key.heap(measure)?;
        self.data.heap(measure)
    }
}
impl Footprint for crate::psd::GlobalMaskSettings {
    fn heap(&self, measure: &mut Measure<'_>) -> Result<()> {
        self.data.heap(measure)
    }
}
impl Footprint for super::Content36 {
    fn heap(&self, measure: &mut Measure<'_>) -> Result<()> {
        measure.node()?;
        match self {
            Self::Bezier {
                nodes,
                stroke_width,
                ..
            } => {
                if !stroke_width.is_finite() {
                    return Err("Nonfinite native source metadata".into());
                }
                measure.vector(nodes)?;
                for anchor in nodes {
                    anchor.point.heap(measure)?;
                    if let Some(p) = anchor.in_handle {
                        p.heap(measure)?;
                    }
                    if let Some(p) = anchor.out_handle {
                        p.heap(measure)?;
                    }
                }
            }
            Self::Shape {
                shape,
                points,
                bounds,
                stroke_width,
                ..
            } => {
                if !stroke_width.is_finite() || bounds.iter().any(|v| !v.is_finite()) {
                    return Err("Nonfinite native source metadata".into());
                }
                shape.heap(measure)?;
                points.heap(measure)?;
            }
            Self::Svg { svg } => svg.heap(measure)?,
            Self::Text {
                text,
                font,
                align,
                size,
                line_height,
                letter_spacing,
                ..
            } => {
                if [size, line_height, letter_spacing]
                    .into_iter()
                    .any(|v| !v.is_finite())
                {
                    return Err("Nonfinite native source metadata".into());
                }
                text.heap(measure)?;
                align.heap(measure)?;
                match font {
                    crate::fonts::TextFont::Bundled(name) => name.heap(measure)?,
                    crate::fonts::TextFont::Installed(font) => {
                        font.family.heap(measure)?;
                        font.postscript.heap(measure)?;
                        font.style.heap(measure)?;
                        font.sha256.heap(measure)?;
                    }
                }
            }
        }
        Ok(())
    }
}
pub(super) fn contours<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<MetadataPayload<Vec<Vec<[f32; 2]>>>, D::Error> {
    super::bounds::contours(deserializer).map(MetadataPayload::from)
}
pub(super) fn polygon<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<MetadataPayload<Vec<[f32; 2]>>>, D::Error> {
    super::bounds::polygon(deserializer).map(|v| v.map(MetadataPayload::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_asset_has_one_physical_charge_and_wire_bytes_across_many_headers() {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let text = "x".repeat(32 * 1024 * 1024);
        let wire = MetadataPayload::from(serde_json::json!({"png":text,"unknown":{"keep":true}}));
        assert!(wire.shared().is_err());
        let before = serde_json::to_string(&wire).unwrap();
        let asset = wire.bind(&pool, &|| Ok(())).unwrap();
        let retained = pool.used();
        assert!(retained >= 32 * 1024 * 1024 && retained < 33 * 1024 * 1024);
        let mut copies = Vec::new();
        for _ in 0..200 {
            copies.push(asset.shared().unwrap());
        }
        assert_eq!(pool.used(), retained);
        assert!(copies.iter().all(|value| value.same_allocation(&asset)));
        assert_eq!(serde_json::to_string(&asset).unwrap(), before);
        drop(asset);
        assert_eq!(pool.used(), retained);
        copies.pop();
        assert_eq!(pool.used(), retained);
        drop(copies);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn wire_parse_stays_unbound_and_refusal_cancellation_and_pool_checks_release_charges() {
        let pool = ResidentBudget::new(4096).unwrap();
        let parsed: MetadataPayload<serde_json::Value> =
            serde_json::from_str("{\"x\":[1,2,3]}").unwrap();
        assert!(parsed.shared().is_err());
        let asset = parsed.bind(&pool, &|| Ok(())).unwrap();
        let retained = pool.used();
        assert!(retained > 0);
        let other = ResidentBudget::new(4096).unwrap();
        assert!(asset.shared().unwrap().bind(&other, &|| Ok(())).is_err());
        assert_eq!(other.used(), 0);
        assert!(MetadataPayload::from("z".repeat(8192))
            .bind(&pool, &|| Ok(()))
            .is_err());
        assert!(MetadataPayload::from(String::from("cancel"))
            .bind(&pool, &|| Err("cancelled".into()))
            .is_err());
        assert_eq!(pool.used(), retained);
        drop(asset);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn retained_capacity_is_charged_and_nonfinite_geometry_is_refused() {
        let pool = ResidentBudget::new(16384).unwrap();
        let mut text = String::with_capacity(8192);
        text.push('x');
        let asset = MetadataPayload::from(text).bind(&pool, &|| Ok(())).unwrap();
        assert!(asset.retained_bytes().unwrap() >= 8192 + OWNER_BYTES);
        assert!(MetadataPayload::from(vec![[f32::NAN, 0.]])
            .bind(&pool, &|| Ok(()))
            .is_err());
        drop(asset);
        assert_eq!(pool.used(), 0);
    }
}
