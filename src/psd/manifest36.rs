//! Canonical manifest/hash declarations, not typed document/source authority.
use super::{
    bulk36::{self, BulkLimits, BulkPlan, Descriptor, Digest, RasterSpec},
    canonical36::{Canonical, Limits},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;

/// The exact compressed range after the PBR1 marker. New format36 requires one
/// complete zlib stream with no encoded tail, unlike the retained legacy path.
pub(crate) fn inflate<R: Read>(
    input: &mut R,
    encoded_bytes: u64,
    metadata: Limits,
    bulk: BulkLimits,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Manifest, String> {
    let (bytes, origin) = inflate_bytes(input, encoded_bytes, metadata, check)?;
    let mut manifest = Manifest::parse(&bytes, metadata, bulk, check)?;
    manifest.origin = Some((encoded_bytes, origin));
    Ok(manifest)
}

fn inflate_bytes<R: Read>(
    input: &mut R,
    encoded_bytes: u64,
    metadata: Limits,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(Vec<u8>, Digest), String> {
    metadata.validate()?;
    check()?;
    if encoded_bytes < 6 || encoded_bytes > u64::from(u32::MAX) - 4 {
        return Err("Source36 compressed resource length differs".into());
    }
    let mut decoder = flate2::Decompress::new(true);
    let mut compressed = [0; 65536];
    let mut decoded = [0; 65536];
    let (mut begin, mut end, mut remaining) = (0usize, 0usize, encoded_bytes);
    let mut bytes = Vec::new();
    let mut encoded_hash = ring::digest::Context::new(&ring::digest::SHA256);
    loop {
        check()?;
        if begin == end && remaining != 0 {
            end = remaining.min(compressed.len() as u64) as usize;
            input
                .read_exact(&mut compressed[..end])
                .map_err(|e| check().err().unwrap_or_else(|| e.to_string()))?;
            encoded_hash.update(&compressed[..end]);
            remaining -= end as u64;
            begin = 0;
        }
        let left = metadata.bytes - bytes.len();
        let count = decoded.len().min(left + 1);
        let (before_in, before_out) = (decoder.total_in(), decoder.total_out());
        let status = decoder
            .decompress(
                &compressed[begin..end],
                &mut decoded[..count],
                if remaining == 0 && begin == end {
                    flate2::FlushDecompress::Finish
                } else {
                    flate2::FlushDecompress::None
                },
            )
            .map_err(|_| "Invalid Source36 metadata zlib stream")?;
        let consumed = (decoder.total_in() - before_in) as usize;
        let produced = (decoder.total_out() - before_out) as usize;
        begin += consumed;
        // The first excess decoded byte refuses the source. Do not inflate the
        // rest of a large over-budget object or materialize it through serde.
        if produced > left {
            return Err("Source36 decoded metadata exceeds byte admission".into());
        }
        let length = bytes.len() + produced;
        if length > bytes.capacity() {
            let target = length
                .checked_next_power_of_two()
                .unwrap_or(metadata.bytes)
                .min(metadata.bytes);
            bytes
                .try_reserve_exact(target - bytes.len())
                .map_err(|_| "Source36 metadata allocation refused")?;
        }
        bytes.extend_from_slice(&decoded[..produced]);
        if status == flate2::Status::StreamEnd {
            if decoder.total_in() != encoded_bytes || remaining != 0 || begin != end {
                return Err(
                    "Source36 metadata has trailing or concatenated compressed bytes".into(),
                );
            }
            let mut extra = [0; 1];
            check()?;
            if input.read(&mut extra).map_err(|e| e.to_string())? != 0 {
                return Err("Source36 compressed range has extra bytes".into());
            }
            check()?;
            return Ok((bytes, encoded_hash.finish().as_ref().try_into().unwrap()));
        }
        if consumed == 0 && produced == 0 && (remaining == 0 || begin != end) {
            return Err("Incomplete Source36 metadata zlib stream".into());
        }
    }
}

/// Complete the raw/preflight/DOM/manifest transition under retained same-pool
/// charges. Raw capacity is admitted before the first compressed byte is read.
/// Decoder scratch is a conservative component envelope, not a process RAM or
/// allocator bound. The caller must also retain source/file and tile I/O owners.
pub(crate) fn inflate_admitted<R: Read>(
    input: &mut R,
    encoded_bytes: u64,
    metadata: Limits,
    bulk: BulkLimits,
    working: super::memory36::WorkingSet,
    budget: &crate::large_document::ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<super::memory36::Admitted<Manifest>, String> {
    use super::{
        canonical36::Stats,
        memory36::{Admission, MetadataCharge},
    };
    metadata.validate()?;
    if working.raw_bytes == 0 || working.raw_bytes > metadata.bytes {
        return Err("Source36 raw inflation capacity exceeds its metadata byte admission".into());
    }
    let metadata = Limits {
        bytes: working.raw_bytes,
        ..metadata
    };
    let raw_owner = MetadataCharge::reserve(budget, working.raw_bytes, check)?;
    // Escaped JSON strings can require a growing parser scratch buffer before
    // Stats knows their decoded length. Charge its worst byte envelope first.
    let preflight_bytes = working
        .raw_bytes
        .checked_mul(2)
        .and_then(|n| n.checked_add(2 * 1024 * 1024))
        .ok_or("Source36 preflight workspace overflow")?;
    let preflight_owner = MetadataCharge::reserve(budget, preflight_bytes, check)?;
    let (bytes, origin) = inflate_bytes(input, encoded_bytes, metadata, check)?;
    let stats = Stats::scan(&bytes, metadata, check)?;
    drop(preflight_owner);
    let admission = Admission::reserve(stats, metadata, working, budget, &raw_owner, check)?;
    let canonical = admission.parse_owned(bytes, check)?;
    canonical.map_unique(|canonical, _| {
        let mut manifest = Manifest::from_canonical(canonical, metadata, bulk, check)?;
        manifest.origin = Some((encoded_bytes, origin));
        Ok(manifest)
    })
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Raster {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub references: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Core<T = Value> {
    pub format: u32,
    pub source_id: String,
    pub revision: u64,
    pub metadata: T,
    pub rasters: Vec<Raster>,
    pub tiles: u32,
    pub references: u32,
    pub tile_root: String,
    pub reference_root: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Standard {
    layer_bytes: u64,
    composite_bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    ordinal: u32,
    bytes: u32,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    format: u32,
    core: Core,
    core_sha256: String,
    standard: Standard,
    chunks: Vec<Chunk>,
}

pub(crate) fn hex(digest: Digest) -> String {
    let mut text = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(text, "{byte:02x}").unwrap();
    }
    text
}
fn digest(text: &str) -> Result<Digest, String> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Source36 digest is not canonical lowercase SHA256".into());
    }
    let mut bytes = [0; 32];
    for (i, value) in bytes.iter_mut().enumerate() {
        *value = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|_| "Invalid Source36 digest")?;
    }
    Ok(bytes)
}
fn source_id(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && text
            .bytes()
            .any(|b| (b'1'..=b'9').contains(&b) || (b'a'..=b'f').contains(&b))
}

/// A staged core binds metadata bytes/declared raster roster only. The engine
/// must independently validate its exhaustive typed DTO and source semantics.
pub(crate) struct StagedCore<T = Value> {
    wire: Core<T>,
    sha256: Digest,
    rasters: Vec<RasterSpec>,
    tile_root: Digest,
    reference_root: Digest,
}
impl StagedCore {
    pub(crate) fn new(
        wire: Core,
        metadata: Limits,
        bulk: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        if !wire.metadata.is_object() {
            return Err("Source36 typed metadata must be an object".into());
        }
        Self::stage(wire, metadata, bulk, check)
    }
}
impl<T: Serialize> StagedCore<T> {
    /// Writer-side borrowing avoids a second complete typed metadata graph.
    /// Combined raw/tree/canonical admission remains the caller's obligation.
    pub(crate) fn stage(
        wire: Core<T>,
        metadata: Limits,
        bulk: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if wire.format != 36 || !source_id(&wire.source_id) || wire.rasters.len() > bulk.rasters {
            return Err("Source36 core version/identity/roster differs".into());
        }
        let tile_root = digest(&wire.tile_root)?;
        let reference_root = digest(&wire.reference_root)?;
        let mut rasters = Vec::new();
        rasters
            .try_reserve_exact(wire.rasters.len())
            .map_err(|_| "Source36 roster allocation refused")?;
        for r in &wire.rasters {
            check()?;
            rasters.push(RasterSpec {
                id: r.id,
                width: r.width,
                height: r.height,
                depth: r.depth,
                references: r.references,
            });
        }
        BulkPlan {
            core: [0; 32],
            rasters: &rasters,
            tiles: wire.tiles,
            references: wire.references,
            tile_root,
            reference_root,
            chunks: &[],
        }
        .admit_core(bulk, check)?;
        let canonical = Canonical::from_serializable(&wire, metadata, check)?;
        if !canonical
            .value()
            .get("metadata")
            .is_some_and(Value::is_object)
        {
            return Err("Source36 typed metadata must be an object".into());
        }
        let sha256 = canonical.core_digest();
        Ok(Self {
            wire,
            sha256,
            rasters,
            tile_root,
            reference_root,
        })
    }
}
impl<T> StagedCore<T> {
    pub(crate) fn sha256(&self) -> Digest {
        self.sha256
    }
    pub(crate) fn metadata(&self) -> &T {
        &self.wire.metadata
    }
    pub(crate) fn source_id(&self) -> &str {
        &self.wire.source_id
    }
    pub(crate) fn revision(&self) -> u64 {
        self.wire.revision
    }
}

pub(crate) struct Manifest<T = Value> {
    core: StagedCore<T>,
    chunks: Vec<Descriptor>,
    standard_layer_bytes: u64,
    standard_composite_bytes: u64,
    standard_sha256: Digest,
    // Runtime byte observation from the exact bounded zlib range, never wire
    // authority or a replacement for retained whole-source/lease validation.
    origin: Option<(u64, Digest)>,
}
impl<T> Manifest<T> {
    pub(crate) fn bind_core(
        core: StagedCore<T>,
        layer_bytes: u64,
        composite_bytes: u64,
        standard_sha256: Digest,
        chunks: Vec<Descriptor>,
        limits: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        if layer_bytes == 0
            || composite_bytes < 2
            || layer_bytes.checked_add(composite_bytes).is_none()
        {
            return Err("Source36 standard byte declarations differ".into());
        }
        let result = Self {
            core,
            chunks,
            standard_layer_bytes: layer_bytes,
            standard_composite_bytes: composite_bytes,
            standard_sha256,
            origin: None,
        };
        result.plan().admit(limits, check)?;
        Ok(result)
    }
}
impl Manifest {
    pub(crate) fn bind(
        core: StagedCore,
        layer_bytes: u64,
        composite_bytes: u64,
        standard_sha256: Digest,
        chunks: Vec<Descriptor>,
        limits: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::bind_core(
            core,
            layer_bytes,
            composite_bytes,
            standard_sha256,
            chunks,
            limits,
            check,
        )
    }
    pub(crate) fn parse(
        bytes: &[u8],
        metadata: Limits,
        bulk: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::from_canonical(
            Canonical::parse(bytes, metadata, check)?,
            metadata,
            bulk,
            check,
        )
    }
    /// Consume an already admitted canonical tree rather than copying or parsing
    /// the raw manifest again. The caller must retain its metadata charge owner.
    pub(crate) fn from_canonical(
        canonical: Canonical,
        metadata: Limits,
        bulk: BulkLimits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let (_, value) = canonical.into_parts();
        let wire: Wire = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if wire.format != 36 {
            return Err("Unknown Source36 manifest version".into());
        }
        let expected_core = digest(&wire.core_sha256)?;
        let core = StagedCore::new(wire.core, metadata, bulk, check)?;
        if core.sha256 != expected_core {
            return Err("Source36 canonical core hash differs".into());
        }
        if wire.chunks.len() > bulk.chunks {
            return Err("Source36 chunk count exceeds admission".into());
        }
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(wire.chunks.len())
            .map_err(|_| "Source36 chunk index allocation refused")?;
        for chunk in wire.chunks {
            check()?;
            chunks.push(Descriptor {
                ordinal: chunk.ordinal,
                bytes: chunk.bytes,
                sha256: digest(&chunk.sha256)?,
            });
        }
        Self::bind(
            core,
            wire.standard.layer_bytes,
            wire.standard.composite_bytes,
            digest(&wire.standard.sha256)?,
            chunks,
            bulk,
            check,
        )
    }
    /// Moving the typed metadata also moves the caller's Admitted owner. Exact
    /// canonical roundtrip forbids ignored, defaulted or narrowed source fields.
    /// The closure must validate the full DTO/roster and document identity; this
    /// generic conversion grants no source, semantic or publication authority.
    pub(crate) fn into_typed<T: DeserializeOwned + Serialize>(
        self,
        limits: Limits,
        check: &dyn Fn() -> Result<(), String>,
        validate: impl FnOnce(&T, &[RasterSpec], &str, u64) -> Result<(), String>,
    ) -> Result<Manifest<T>, String> {
        check()?;
        let Core {
            format,
            source_id,
            revision,
            metadata,
            rasters,
            tiles,
            references,
            tile_root,
            reference_root,
        } = self.core.wire;
        let (original, original_tree) =
            Canonical::from_serializable(&metadata, limits, check)?.into_parts();
        drop(original_tree);
        let typed: T = serde_json::from_value(metadata).map_err(|e| e.to_string())?;
        let converted = Canonical::from_serializable(&typed, limits, check)?;
        if original != converted.bytes() {
            return Err(
                "Source36 typed metadata does not preserve its exact canonical fields".into(),
            );
        }
        drop(converted);
        drop(original);
        validate(&typed, &self.core.rasters, &source_id, revision)?;
        check()?;
        Ok(Manifest {
            core: StagedCore {
                wire: Core {
                    format,
                    source_id,
                    revision,
                    metadata: typed,
                    rasters,
                    tiles,
                    references,
                    tile_root,
                    reference_root,
                },
                sha256: self.core.sha256,
                rasters: self.core.rasters,
                tile_root: self.core.tile_root,
                reference_root: self.core.reference_root,
            },
            chunks: self.chunks,
            standard_layer_bytes: self.standard_layer_bytes,
            standard_composite_bytes: self.standard_composite_bytes,
            standard_sha256: self.standard_sha256,
            origin: self.origin,
        })
    }
}
impl<T: Serialize> Manifest<T> {
    pub(crate) fn canonical(
        &self,
        limits: Limits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Canonical, String> {
        #[derive(Serialize)]
        struct Emitted<'a, T> {
            format: u32,
            core: &'a Core<T>,
            core_sha256: String,
            standard: Standard,
            chunks: Vec<Chunk>,
        }
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(self.chunks.len())
            .map_err(|_| "Source36 chunk serialization allocation refused")?;
        for c in &self.chunks {
            check()?;
            chunks.push(Chunk {
                ordinal: c.ordinal,
                bytes: c.bytes,
                sha256: hex(c.sha256),
            });
        }
        Canonical::from_serializable(
            &Emitted {
                format: 36,
                core: &self.core.wire,
                core_sha256: hex(self.core.sha256),
                standard: Standard {
                    layer_bytes: self.standard_layer_bytes,
                    composite_bytes: self.standard_composite_bytes,
                    sha256: hex(self.standard_sha256),
                },
                chunks,
            },
            limits,
            check,
        )
    }
}
impl<T> Manifest<T> {
    pub(crate) fn core(&self) -> &StagedCore<T> {
        &self.core
    }
    pub(crate) fn plan(&self) -> BulkPlan<'_> {
        BulkPlan {
            core: self.core.sha256,
            rasters: &self.core.rasters,
            tiles: self.core.wire.tiles,
            references: self.core.wire.references,
            tile_root: self.core.tile_root,
            reference_root: self.core.reference_root,
            chunks: &self.chunks,
        }
    }
    pub(crate) fn standard_binding(&self) -> (u64, u64, Digest) {
        (
            self.standard_layer_bytes,
            self.standard_composite_bytes,
            self.standard_sha256,
        )
    }
    pub(crate) fn resource_origin(&self) -> Option<(u64, Digest)> {
        self.origin
    }
    /// Use inside the admitted-owner handoff. This moves the typed payload;
    /// semantic/file publication authority is deliberately not returned here.
    pub(super) fn into_metadata(self) -> T {
        self.core.wire.metadata
    }
    /// Split only while the caller carries the original Admitted owner with
    /// BOTH moved results. This header has no metadata/editability authority.
    pub(super) fn into_source_parts(self) -> (T, SourceBinding) {
        let metadata = self.core.wire.metadata;
        (
            metadata,
            SourceBinding {
                core: self.core.sha256,
                rasters: self.core.rasters,
                tiles: self.core.wire.tiles,
                references: self.core.wire.references,
                tile_root: self.core.tile_root,
                reference_root: self.core.reference_root,
                chunks: self.chunks,
                layer_bytes: self.standard_layer_bytes,
                composite_bytes: self.standard_composite_bytes,
                standard: self.standard_sha256,
                origin: self.origin,
            },
        )
    }
    #[cfg(test)]
    pub(crate) fn verify_standard<L: Read, C: Read>(
        &self,
        layer: &mut L,
        layer_bytes: u64,
        composite: &mut C,
        composite_bytes: u64,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<CompletedStandard, String> {
        let sha = bulk36::standard_digest(layer, layer_bytes, composite, composite_bytes, check)?;
        self.verify_standard_digest(layer_bytes, composite_bytes, sha)
    }

    pub(super) fn verify_standard_digest(
        &self,
        layer_bytes: u64,
        composite_bytes: u64,
        sha: Digest,
    ) -> Result<CompletedStandard, String> {
        if layer_bytes != self.standard_layer_bytes
            || composite_bytes != self.standard_composite_bytes
        {
            return Err("Source36 indexed standard lengths differ from manifest".into());
        }
        if sha != self.standard_sha256 {
            return Err("Source36 original standard SHA256 differs".into());
        }
        Ok(CompletedStandard::from_digest(self.core.sha256, layer_bytes, composite_bytes, sha))
    }
}

/// Immutable original byte bindings after typed metadata moves into a
/// provisional SceneSnapshot. No serializer or bare editable payload escape.
pub(crate) struct SourceBinding {
    core: Digest,
    rasters: Vec<RasterSpec>,
    tiles: u32,
    references: u32,
    tile_root: Digest,
    reference_root: Digest,
    chunks: Vec<Descriptor>,
    layer_bytes: u64,
    composite_bytes: u64,
    standard: Digest,
    origin: Option<(u64, Digest)>,
}
impl SourceBinding {
    pub(crate) fn plan(&self) -> BulkPlan<'_> {
        BulkPlan {
            core: self.core,
            rasters: &self.rasters,
            tiles: self.tiles,
            references: self.references,
            tile_root: self.tile_root,
            reference_root: self.reference_root,
            chunks: &self.chunks,
        }
    }
    pub(crate) fn origin(&self) -> Option<(u64, Digest)> {
        self.origin
    }
    pub(crate) fn core_sha256(&self) -> Digest {
        self.core
    }
}

/// A byte/hash observation only. Native row completions, typed DTO/metadata,
/// composite freshness and final source/lease publication checks remain required.
#[must_use = "Standard binding must be combined with all source and native stream proofs"]
pub(crate) struct CompletedStandard {
    core: Digest,
    layer_bytes: u64,
    composite_bytes: u64,
    sha256: Digest,
}
impl CompletedStandard {
    pub(super) fn from_digest(
        core: Digest,
        layer_bytes: u64,
        composite_bytes: u64,
        sha256: Digest,
    ) -> Self {
        Self {
            core,
            layer_bytes,
            composite_bytes,
            sha256,
        }
    }

    pub(super) fn sha256(&self) -> Digest {
        self.sha256
    }

    pub(crate) fn check<T>(&self, manifest: &Manifest<T>) -> Result<(), String> {
        if self.core != manifest.core.sha256
            || self.layer_bytes != manifest.standard_layer_bytes
            || self.composite_bytes != manifest.standard_composite_bytes
            || self.sha256 != manifest.standard_sha256
        {
            return Err("Source36 standard completion belongs to another manifest".into());
        }
        Ok(())
    }
    pub(crate) fn check_binding(&self, binding: &SourceBinding) -> Result<(), String> {
        if self.core != binding.core
            || self.layer_bytes != binding.layer_bytes
            || self.composite_bytes != binding.composite_bytes
            || self.sha256 != binding.standard
        {
            return Err("Source36 standard completion belongs to another retained binding".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    fn metadata_limits() -> Limits {
        Limits {
            bytes: 8192,
            nodes: 2048,
            depth: 32,
        }
    }
    fn bulk_limits() -> BulkLimits {
        BulkLimits {
            rasters: 4,
            tiles: 4,
            references: 8,
            chunks: 4,
            encoded_bytes: 1024 * 1024,
        }
    }
    fn empty_root(domain: &[u8]) -> String {
        let mut bytes = domain.to_vec();
        bytes.extend_from_slice(&0u64.to_be_bytes());
        hex(ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .try_into()
            .unwrap())
    }
    fn core() -> Core {
        Core {
            format: 36,
            source_id: "61f53110-3789-4693-81d7-56136f750be2".into(),
            revision: 7,
            metadata: json!({"id":"61f53110-3789-4693-81d7-56136f750be2","revision":7,"native":true}),
            rasters: vec![],
            tiles: 0,
            references: 0,
            tile_root: empty_root(b"PeerBrushSource36Tiles\0"),
            reference_root: empty_root(b"PeerBrushSource36Refs\0"),
        }
    }
    fn fixture() -> Manifest {
        let check = || Ok(());
        let core = StagedCore::new(core(), metadata_limits(), bulk_limits(), &check).unwrap();
        let standard = bulk36::standard_digest(
            &mut b"original-native-layer".as_slice(),
            21,
            &mut b"original-current-composite".as_slice(),
            26,
            &check,
        )
        .unwrap();
        Manifest::bind(core, 21, 26, standard, vec![], bulk_limits(), &check).unwrap()
    }
    #[test]
    fn staged_core_and_manifest_roundtrip_and_standard_token_remain_separate() {
        let original = fixture();
        let check = || Ok(());
        let bytes = original.canonical(metadata_limits(), &check).unwrap();
        let parsed =
            Manifest::parse(bytes.bytes(), metadata_limits(), bulk_limits(), &check).unwrap();
        assert_eq!(original.core().sha256(), parsed.core().sha256());
        assert_eq!(parsed.core().revision(), 7);
        assert_eq!(
            parsed.core().source_id(),
            "61f53110-3789-4693-81d7-56136f750be2"
        );
        assert_eq!(parsed.core().metadata(), original.core().metadata());
        let token = parsed
            .verify_standard(
                &mut b"original-native-layer".as_slice(),
                21,
                &mut b"original-current-composite".as_slice(),
                26,
                &check,
            )
            .unwrap();
        token.check(&original).unwrap();
        let mut changed_core = core();
        changed_core.revision += 1;
        let changed = Manifest::bind(
            StagedCore::new(changed_core, metadata_limits(), bulk_limits(), &check).unwrap(),
            21,
            26,
            original.standard_sha256,
            vec![],
            bulk_limits(),
            &check,
        )
        .unwrap();
        assert!(token.check(&changed).is_err());
        assert!(parsed
            .verify_standard(
                &mut b"original-native-layer!".as_slice(),
                21,
                &mut b"original-current-composite".as_slice(),
                26,
                &check
            )
            .is_err());
    }
    #[test]
    fn changed_core_unknown_fields_versions_and_noncanonical_hashes_fail_closed() {
        let original = fixture();
        let check = || Ok(());
        let bytes = original.canonical(metadata_limits(), &check).unwrap();
        let mut variants = vec![];
        let mut value = bytes.value().clone();
        value["core"]["revision"] = json!(8);
        variants.push(value);
        let mut value = bytes.value().clone();
        value["format"] = json!(35);
        variants.push(value);
        let mut value = bytes.value().clone();
        value["unexpected"] = json!(true);
        variants.push(value);
        let mut value = bytes.value().clone();
        value["core"]["unexpected"] = json!(true);
        variants.push(value);
        let mut value = bytes.value().clone();
        value["standard"]["sha256"] = json!(hex(original.standard_sha256).to_ascii_uppercase());
        variants.push(value);
        for value in variants {
            let changed = Canonical::from_value(value, metadata_limits(), &check).unwrap();
            assert!(
                Manifest::parse(changed.bytes(), metadata_limits(), bulk_limits(), &check).is_err()
            );
        }
    }
    #[test]
    fn roster_and_count_admission_precedes_bulk_index_allocation() {
        let check = || Ok(());
        let mut c = core();
        c.rasters.push(Raster {
            id: 1,
            width: 1,
            height: 1,
            depth: 16,
            references: 2,
        });
        c.references = 2;
        c.tiles = 1;
        assert!(StagedCore::new(c, metadata_limits(), bulk_limits(), &check).is_err());
        let mut c = core();
        c.source_id = "not-a-project-identity".into();
        assert!(StagedCore::new(c, metadata_limits(), bulk_limits(), &check).is_err());
        let mut c = core();
        c.metadata = json!({"oversized":"x".repeat(8193)});
        assert!(StagedCore::new(c, metadata_limits(), bulk_limits(), &check).is_err());
        for (width, height) in [(0, 1), (1, 0), (16001, 1), (1, 16001)] {
            let mut c = core();
            c.rasters.push(Raster {
                id: 1,
                width,
                height,
                depth: 16,
                references: 0,
            });
            assert!(StagedCore::new(c, metadata_limits(), bulk_limits(), &check).is_err());
        }
    }

    #[test]
    fn compressed_manifest_requires_real_zlib_end_checksum_exact_output_and_range_eof() {
        let check = || Ok(());
        let bytes = fixture().canonical(metadata_limits(), &check).unwrap();
        let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zip.write_all(bytes.bytes()).unwrap();
        let zipped = zip.finish().unwrap();
        let exact = Limits {
            bytes: bytes.bytes().len(),
            ..metadata_limits()
        };
        let parsed = inflate(
            &mut zipped.as_slice(),
            zipped.len() as u64,
            exact,
            bulk_limits(),
            &check,
        )
        .unwrap();
        assert_eq!(parsed.core().revision(), 7);
        assert!(inflate(
            &mut zipped.as_slice(),
            zipped.len() as u64,
            Limits {
                bytes: exact.bytes - 1,
                ..exact
            },
            bulk_limits(),
            &check
        )
        .is_err());
        let mut variants = vec![];
        for missing in 1..=4 {
            variants.push(zipped[..zipped.len() - missing].to_vec());
        }
        let mut corrupted = zipped.clone();
        *corrupted.last_mut().unwrap() ^= 1;
        variants.push(corrupted);
        variants.push([zipped.as_slice(), b"outside"].concat());
        variants.push([zipped.as_slice(), zipped.as_slice()].concat());
        for variant in variants {
            assert!(inflate(
                &mut variant.as_slice(),
                variant.len() as u64,
                exact,
                bulk_limits(),
                &check
            )
            .is_err());
        }
        let extra_range = [zipped.as_slice(), b"outside"].concat();
        assert!(inflate(
            &mut extra_range.as_slice(),
            zipped.len() as u64,
            exact,
            bulk_limits(),
            &check
        )
        .is_err());
    }

    #[test]
    fn consuming_typed_transition_preserves_exact_fields_identity_and_standard_proofs() {
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Metadata {
            id: String,
            revision: u64,
            native: bool,
        }
        let check = || Ok(());
        let original = fixture();
        let token = original
            .verify_standard(
                &mut b"original-native-layer".as_slice(),
                21,
                &mut b"original-current-composite".as_slice(),
                26,
                &check,
            )
            .unwrap();
        let canonical = original.canonical(metadata_limits(), &check).unwrap();
        let parsed =
            Manifest::from_canonical(canonical, metadata_limits(), bulk_limits(), &check).unwrap();
        let typed = parsed
            .into_typed::<Metadata>(
                metadata_limits(),
                &check,
                |metadata, roster, id, revision| {
                    if metadata.id != id || metadata.revision != revision || !roster.is_empty() {
                        return Err("Typed document identity or raster roster differs".into());
                    }
                    Ok(())
                },
            )
            .unwrap();
        assert!(typed.core().metadata().native);
        token.check(&typed).unwrap();
        assert_eq!(
            typed.canonical(metadata_limits(), &check).unwrap().bytes(),
            original
                .canonical(metadata_limits(), &check)
                .unwrap()
                .bytes()
        );
        assert!(fixture()
            .into_typed::<Metadata>(metadata_limits(), &check, |_, _, _, _| {
                Err("Final typed source refusal".into())
            })
            .is_err());
    }

    #[test]
    fn typed_transition_refuses_ignored_fields_defaults_and_narrowed_numbers() {
        #[derive(Serialize, Deserialize)]
        struct MissingField {
            id: String,
            revision: u64,
        }
        #[derive(Serialize, Deserialize)]
        struct AddedField {
            id: String,
            revision: u64,
            native: bool,
            #[serde(default)]
            extra: bool,
        }
        #[derive(Serialize, Deserialize)]
        struct NarrowedNumber {
            id: String,
            revision: f32,
            native: bool,
        }
        let check = || Ok(());
        assert!(fixture()
            .into_typed::<MissingField>(metadata_limits(), &check, |_, _, _, _| Ok(()))
            .is_err());
        assert!(fixture()
            .into_typed::<AddedField>(metadata_limits(), &check, |_, _, _, _| Ok(()))
            .is_err());
        let mut wire = core();
        wire.metadata["revision"] = json!(16777217);
        let core = StagedCore::new(wire, metadata_limits(), bulk_limits(), &check).unwrap();
        let original = fixture();
        let changed = Manifest::bind(
            core,
            21,
            26,
            original.standard_sha256,
            vec![],
            bulk_limits(),
            &check,
        )
        .unwrap();
        assert!(changed
            .into_typed::<NarrowedNumber>(metadata_limits(), &check, |_, _, _, _| Ok(()))
            .is_err());
    }

    #[test]
    fn compressed_admission_precedes_io_and_keeps_the_result_charged_through_handoff() {
        use super::super::memory36::WorkingSet;
        use crate::large_document::ResidentBudget;
        let check = || Ok(());
        let canonical = fixture().canonical(metadata_limits(), &check).unwrap();
        let mut zipped = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zipped.write_all(canonical.bytes()).unwrap();
        let zipped = zipped.finish().unwrap();
        let working = WorkingSet {
            raw_bytes: metadata_limits().bytes,
            canonical_core_bytes: metadata_limits().bytes,
            canonical_manifest_bytes: metadata_limits().bytes,
            typed_metadata_bytes: 65536,
            tile_scratch_bytes: bulk36::MAX_TILE_BYTES,
            index_scratch_bytes: 4096,
        };
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let admitted = inflate_admitted(
            &mut zipped.as_slice(),
            zipped.len() as u64,
            metadata_limits(),
            bulk_limits(),
            working,
            &pool,
            &check,
        )
        .unwrap();
        assert_eq!(admitted.core().revision(), 7);
        assert!(admitted.owner().uses_budget(&pool));
        assert!(pool.used() > working.raw_bytes);
        let retained = admitted.clone();
        drop(admitted);
        assert!(pool.used() > 0);
        drop(retained);
        assert_eq!(pool.used(), 0);
        let refused = ResidentBudget::new(working.raw_bytes + 1).unwrap();
        let mut cursor = std::io::Cursor::new(&zipped);
        assert!(inflate_admitted(
            &mut cursor,
            zipped.len() as u64,
            metadata_limits(),
            bulk_limits(),
            working,
            &refused,
            &check
        )
        .is_err());
        assert_eq!(cursor.position(), 0);
        assert_eq!(refused.used(), 0);
        let mut corrupt = zipped.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(inflate_admitted(
            &mut corrupt.as_slice(),
            corrupt.len() as u64,
            metadata_limits(),
            bulk_limits(),
            working,
            &pool,
            &check
        )
        .is_err());
        assert_eq!(pool.used(), 0);
    }

    #[test]
    fn typed_writer_borrows_metadata_without_cloning_and_retains_the_same_core_wire() {
        #[derive(Serialize)]
        struct Metadata {
            id: String,
            revision: u64,
            native: bool,
        }
        let check = || Ok(());
        let metadata = Metadata {
            id: "61f53110-3789-4693-81d7-56136f750be2".into(),
            revision: 7,
            native: true,
        };
        let old = core();
        let staged = StagedCore::stage(
            Core {
                format: old.format,
                source_id: old.source_id,
                revision: old.revision,
                metadata: &metadata,
                rasters: old.rasters,
                tiles: old.tiles,
                references: old.references,
                tile_root: old.tile_root,
                reference_root: old.reference_root,
            },
            metadata_limits(),
            bulk_limits(),
            &check,
        )
        .unwrap();
        let original = fixture();
        assert_eq!(staged.sha256(), original.core().sha256());
        let manifest = Manifest::bind_core(
            staged,
            21,
            26,
            original.standard_sha256,
            vec![],
            bulk_limits(),
            &check,
        )
        .unwrap();
        assert_eq!(
            manifest
                .canonical(metadata_limits(), &check)
                .unwrap()
                .bytes(),
            original
                .canonical(metadata_limits(), &check)
                .unwrap()
                .bytes()
        );
        assert!(std::ptr::eq(*manifest.core().metadata(), &metadata));
    }
}
