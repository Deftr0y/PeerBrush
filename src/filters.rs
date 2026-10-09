//! Whole-composite editable filters. Derived images and COW cache identities are never sources.
use crate::{
    depth16::{self, Image16},
    effects::{self, Effect, Image},
    engine::{id, Document},
    raster::Raster,
};
use serde_json::{json, Value};
use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

pub const LIMIT: usize = 32;
pub const CACHE_BUDGET: usize = 128 * 1024 * 1024;
pub fn supports(kind: &str) -> bool {
    effects::KINDS.contains(&kind) && kind != "liquify"
}
pub fn active(doc: &Document) -> bool {
    doc.filters.iter().any(|f| f.enabled && f.weight > 0.0)
}
pub fn settings(kind: &str, value: &Value) -> Result<Value, String> {
    if !supports(kind) {
        return Err("Unsupported whole-image filter".into());
    }
    let defaults = effects::defaults(kind);
    if value
        .as_object()
        .ok_or("Filter settings must be an object")?
        .keys()
        .any(|k| defaults.get(k).is_none())
    {
        return Err("Unknown whole-image filter setting".into());
    }
    if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > 64 * 1024 {
        return Err("Filter settings exceed 64 KiB".into());
    }
    effects::normalized(kind, value)
}
pub fn validate(doc: &Document) -> Result<(), String> {
    if doc.filters.len() > LIMIT {
        return Err("Whole-image filter stack limit: 32".into());
    }
    let mut ids = HashSet::new();
    let bytes =
        u64::from(doc.width) * u64::from(doc.height) * if doc.bit_depth == 16 { 8 } else { 4 };
    let budget = if doc.bit_depth == 16 {
        128 * 1024 * 1024
    } else {
        effects::BUDGET as u64
    };
    for filter in &doc.filters {
        if filter.id.is_empty() || !ids.insert(&filter.id) {
            return Err("Invalid or duplicate whole-image filter ID".into());
        }
        if !supports(&filter.kind) {
            return Err("Unsupported whole-image filter".into());
        }
        effects::validate_weight(filter.weight)?;
        settings(&filter.kind, &filter.settings)?;
        if filter.enabled && filter.weight > 0.0 {
            if bytes > budget {
                return Err(
                    "Whole-image filter output exceeds the native derived-image budget".into(),
                );
            }
            if doc.bit_depth == 16 {
                if depth16::color::weighted_working_bytes(filter, doc.width, doc.height)?
                    > 128 * 1024 * 1024
                {
                    return Err("Whole-image native16 filter scratch budget exceeded".into());
                }
            } else if effects::weighted_working_bytes(filter, doc.width, doc.height)?
                > effects::WORKING_BUDGET as u64
            {
                return Err("Whole-image filter scratch budget exceeded".into());
            }
        }
    }
    Ok(())
}
pub(crate) fn apply(doc: &mut Document, command: &Value) -> Result<(), String> {
    if doc.read_only {
        return Err("This document is read-only".into());
    }
    if ["layer", "rect", "mask"]
        .iter()
        .any(|key| command.get(key).is_some_and(|v| !v.is_null()))
    {
        return Err(
            "Whole-image filters target the document, without a layer, region or mask".into(),
        );
    }
    if doc.layers.iter().any(|l| l.locked) {
        return Err("Unlock layers and folders before changing whole-image filters".into());
    }
    match command["op"].as_str().ok_or("Missing filter operation")? {
        "filter.add" => {
            if doc.filters.len() >= LIMIT {
                return Err("Whole-image filter stack limit: 32".into());
            }
            let kind = command["kind"]
                .as_str()
                .ok_or("Choose a whole-image filter kind")?;
            if !supports(kind) {
                return Err("Unsupported whole-image filter".into());
            }
            let settings = command
                .get("settings")
                .cloned()
                .unwrap_or_else(|| effects::defaults(kind));
            doc.filters.push(Effect {
                id: id(),
                kind: kind.into(),
                enabled: true,
                weight: effects::command_weight(command)?,
                settings: self::settings(kind, &settings)?,
            });
        }
        "filter.update" => {
            let filter = doc
                .filters
                .iter_mut()
                .find(|f| Some(f.id.as_str()) == command["filter"].as_str())
                .ok_or("Unknown whole-image filter")?;
            if let Some(kind) = command.get("kind") {
                let kind = kind.as_str().ok_or("Filter kind must be a name")?;
                if !supports(kind) {
                    return Err("Unsupported whole-image filter".into());
                }
                if kind != filter.kind {
                    filter.kind = kind.into();
                    filter.settings = effects::defaults(kind);
                }
            }
            if let Some(enabled) = command.get("enabled") {
                filter.enabled = enabled
                    .as_bool()
                    .ok_or("Filter enabled must be true or false")?;
            }
            if command.get("weight").is_some() {
                filter.weight = effects::command_weight(command)?;
            }
            if let Some(settings) = command.get("settings") {
                filter.settings = self::settings(&filter.kind, settings)?;
            }
        }
        "filter.delete" | "filter.reorder" => {
            let index = doc
                .filters
                .iter()
                .position(|f| Some(f.id.as_str()) == command["filter"].as_str())
                .ok_or("Unknown whole-image filter")?;
            if command["op"] == "filter.delete" {
                doc.filters.remove(index);
            } else {
                let dest = command["index"]
                    .as_u64()
                    .ok_or("Choose a filter stack index")?;
                if dest >= doc.filters.len() as u64 {
                    return Err("Invalid filter stack index".into());
                }
                let filter = doc.filters.remove(index);
                doc.filters.insert(dest as usize, filter);
            }
        }
        _ => return Err("Unknown whole-image filter operation".into()),
    }
    validate(doc)
}

#[derive(Clone)]
enum Output {
    Eight(Arc<Image>),
    Sixteen(Arc<Image16>),
}
struct Entry {
    key: String,
    sources: Vec<Raster>,
    output: Output,
    bytes: usize,
}
#[derive(Default)]
struct Cache {
    entries: VecDeque<Entry>,
    bytes: usize,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
fn sources(doc: &Document) -> Vec<&Raster> {
    doc.layers
        .iter()
        .flat_map(|l| {
            std::iter::once(&l.pixels).chain(
                l.mask
                    .iter()
                    .flat_map(|m| m.steps.iter().map(|s| &s.pixels)),
            )
        })
        .collect()
}
fn same_raster(a: &Raster, b: &Raster) -> bool {
    (a.width, a.height, a.depth) == (b.width, b.height, b.depth)
        && a.tiles.len() == b.tiles.len()
        && a.samples16.len() == b.samples16.len()
        && a.tiles
            .iter()
            .zip(&b.tiles)
            .all(|((ak, a), (bk, b))| ak == bk && Arc::ptr_eq(a, b))
        && a.samples16
            .iter()
            .zip(&b.samples16)
            .all(|((ak, a), (bk, b))| ak == bk && Arc::ptr_eq(a, b))
}
fn key(doc: &Document, filtered: bool) -> String {
    // Every rendered property is explicit. COW tile handles below protect source
    // identity even in an uncommitted draft or direct library-owned document.
    json!([
        doc.id,
        doc.width,
        doc.height,
        doc.bit_depth,
        filtered.then_some(&doc.filters),
        doc.layers
            .iter()
            .map(|l| json!([
                l.id,
                l.kind,
                l.parent,
                l.clip_to,
                l.visible,
                l.opacity,
                l.blend,
                l.x,
                l.y,
                l.color,
                l.effect_key,
                l.effects,
                l.mask.as_ref().map(|m| json!([
                    m.enabled,
                    m.cache_key,
                    m.steps
                        .iter()
                        .map(|s| json!([s.id, s.kind, s.enabled, s.weight, s.value, s.settings]))
                        .collect::<Vec<_>>()
                ]))
            ]))
            .collect::<Vec<_>>()
    ])
    .to_string()
}
fn cached(doc: &Document, key: &str) -> Option<Output> {
    let current = sources(doc);
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .ok()?;
    let index = cache.entries.iter().position(|e| {
        e.key == key
            && e.sources.len() == current.len()
            && e.sources
                .iter()
                .zip(&current)
                .all(|(a, b)| same_raster(a, b))
    })?;
    let entry = cache.entries.remove(index)?;
    let output = entry.output.clone();
    cache.entries.push_back(entry);
    Some(output)
}
fn retain(doc: &Document, key: String, output: Output) {
    let current = sources(doc);
    let output_bytes = match &output {
        Output::Eight(image) => image.bytes.len(),
        Output::Sixteen(image) => image.words.len() * 2,
    };
    let bytes = output_bytes
        + key.capacity()
        + current.len() * std::mem::size_of::<Raster>()
        + current
            .iter()
            .map(|s| s.bytes() + (s.tiles.len() + s.samples16.len()) * 64)
            .sum::<usize>();
    if bytes > CACHE_BUDGET {
        return;
    }
    let snapshots = current
        .into_iter()
        .map(|r| {
            let mut r = r.clone();
            r.retained = None;
            r
        })
        .collect();
    if let Ok(mut cache) = CACHE.get_or_init(|| Mutex::new(Cache::default())).lock() {
        if let Some(index) = cache.entries.iter().position(|e| e.key == key) {
            let old = cache.entries.remove(index).unwrap();
            cache.bytes -= old.bytes;
        }
        while cache.bytes + bytes > CACHE_BUDGET || cache.entries.len() >= 16 {
            let Some(old) = cache.entries.pop_front() else {
                break;
            };
            cache.bytes -= old.bytes;
        }
        cache.bytes += bytes;
        cache.entries.push_back(Entry {
            key,
            sources: snapshots,
            output,
            bytes,
        });
    }
}
pub(crate) fn prepare8(doc: &Document) -> Result<Arc<Image>, String> {
    validate(doc)?;
    let output_key = key(doc, true);
    if let Some(Output::Eight(image)) = cached(doc, &output_key) {
        return Ok(image);
    }
    let raw_key = key(doc, false);
    let raw = if let Some(Output::Eight(image)) = cached(doc, &raw_key) {
        image
    } else {
        let mut original = doc.clone();
        original.filters.clear();
        let (width, height, bytes, _) =
            original.preview_native8(None, doc.width.max(doc.height), None, false)?;
        let image = Arc::new(Image {
            width,
            height,
            bytes,
        });
        retain(doc, raw_key, Output::Eight(image.clone()));
        image
    };
    let mut image = (*raw).clone();
    for filter in &doc.filters {
        effects::apply_effect(&mut image, filter, None)?;
    }
    let image = Arc::new(image);
    retain(doc, output_key, Output::Eight(image.clone()));
    Ok(image)
}
pub(crate) fn prepare16(doc: &Document) -> Result<Arc<Image16>, String> {
    validate(doc)?;
    let output_key = key(doc, true);
    if let Some(Output::Sixteen(image)) = cached(doc, &output_key) {
        return Ok(image);
    }
    let raw_key = key(doc, false);
    let raw = if let Some(Output::Sixteen(image)) = cached(doc, &raw_key) {
        image
    } else {
        let mut original = doc.clone();
        original.filters.clear();
        let image = Arc::new(depth16::render(&original)?);
        retain(doc, raw_key, Output::Sixteen(image.clone()));
        image
    };
    let mut image = (*raw).clone();
    for filter in &doc.filters {
        depth16::color::apply_effect(&mut image, filter, None)?;
    }
    let image = Arc::new(image);
    retain(doc, output_key, Output::Sixteen(image.clone()));
    Ok(image)
}
