//! Native-depth production exports. Plans and renders never mutate editable sources.
pub mod jobs;
pub mod library;
use crate::engine::Document;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub const MAX_OUTPUTS: usize = 64;
#[derive(Clone, Default)]
pub struct Control(Arc<AtomicBool>);
impl Control {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<(), String> {
        if self.0.load(Ordering::Acquire) {
            Err("Export canceled · source remains unchanged".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Crop {
    #[default]
    Canvas,
    Alpha,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resampling {
    Nearest,
    #[default]
    Bilinear,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateNames {
    #[default]
    Error,
    Number,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub name: String,
    pub scale: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub template: String,
    pub crop: Crop,
    pub padding: u32,
    pub resampling: Resampling,
    pub duplicate_names: DuplicateNames,
    pub variants: Vec<Variant>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            template: "{document}_{layer}_{variant}".into(),
            crop: Crop::Canvas,
            padding: 0,
            resampling: Resampling::Bilinear,
            duplicate_names: DuplicateNames::Error,
            variants: vec![Variant {
                name: "native".into(),
                scale: 1.,
            }],
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.template.trim().is_empty()
            || self.template.len() > 160
            || self
                .template
                .chars()
                .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        {
            return Err(
                "Use a filename template without path separators or reserved characters".into(),
            );
        }
        let template = self
            .template
            .replace("{document}", "")
            .replace("{layer}", "")
            .replace("{variant}", "");
        if template.contains(['{', '}']) {
            return Err("Filename placeholders are {document}, {layer} and {variant}".into());
        }
        if self.padding > 4096 || self.variants.is_empty() || self.variants.len() > 12 {
            return Err("Use 1–12 variants and at most 4096 transparent padding pixels".into());
        }
        let mut names = HashSet::new();
        for v in &self.variants {
            if v.name.trim().is_empty()
                || v.name.chars().count() > 48
                || v.name.chars().any(char::is_control)
                || !names.insert(v.name.to_lowercase())
            {
                return Err(
                    "Size variants need unique, readable names of up to 48 characters".into(),
                );
            }
            if !v.scale.is_finite() || !(0.01..=8.).contains(&v.scale) {
                return Err("Variant scale must be between 0.01 and 8".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Output {
    pub target: Option<String>,
    pub target_name: String,
    pub variant: String,
    pub filename: String,
    /// Unscaled document rectangle, including transparent padding.
    pub document_rect: [i32; 4],
    pub width: u32,
    pub height: u32,
    pub bit_depth: u16,
    pub scale_xy: [f64; 2],
}
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub document_id: String,
    pub revision: u64,
    pub outputs: Vec<Output>,
    pub isolated: bool,
    pub profile_retained: bool,
}
pub fn safe_name(name: &str) -> String {
    let mut name: String = name
        .chars()
        .scan(0usize, |bytes, c| {
            *bytes += c.len_utf8();
            (*bytes <= 180).then_some(c)
        })
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(100)
        .collect();
    name = name
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .into();
    if name.is_empty() {
        name = "Untitled".into();
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        name.insert(0, '_');
    }
    name
}
fn size(width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 32_000_000
    {
        return Err("Export dimensions must fit the 8192-edge and 32 MP limits".into());
    }
    Ok(())
}
fn target_document(doc: &Document, target: Option<&str>) -> Result<Document, String> {
    let Some(target) = target else {
        return Ok(doc.clone());
    };
    if doc.read_only {
        return Err("Protected documents can export only their preserved composite".into());
    }
    let root = doc
        .layers
        .iter()
        .find(|l| l.id == target)
        .ok_or("Export layer is missing")?;
    if root.kind == "adjustment" {
        return Err(
            "An adjustment requires its backdrop; export its folder or the whole document".into(),
        );
    }
    if root.clip_to.is_some() {
        return Err(
            "A clipped layer requires its base; export a folder containing the clipping stack"
                .into(),
        );
    }
    let mut kept = HashSet::from([target.to_owned()]);
    for _ in 0..=16 {
        let previous = kept.len();
        for layer in &doc.layers {
            if layer.parent.as_ref().is_some_and(|p| kept.contains(p)) {
                kept.insert(layer.id.clone());
            }
        }
        if previous == kept.len() {
            break;
        }
    }
    if doc
        .layers
        .iter()
        .filter(|l| kept.contains(&l.id))
        .any(|l| l.clip_to.as_ref().is_some_and(|base| !kept.contains(base)))
    {
        return Err("Export folder contains a clipping reference outside its tree".into());
    }
    let mut isolated = doc.clone();
    isolated.layers.retain(|l| kept.contains(&l.id));
    let root = isolated.layers.iter_mut().find(|l| l.id == target).unwrap();
    root.parent = None;
    root.visible = true;
    // Explicit isolated output excludes ancestor folders and document filters.
    isolated.filters.clear();
    Ok(isolated)
}
enum Pixels {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}
struct Image {
    width: u32,
    height: u32,
    pixels: Pixels,
}
impl Image {
    fn render(doc: &Document, target: Option<&str>, control: &Control) -> Result<Self, String> {
        control.check()?;
        let doc = target_document(doc, target)?;
        size(doc.width, doc.height)?;
        let pixels = if doc.bit_depth == 16 {
            Pixels::Sixteen(crate::depth16::render(&doc)?.words)
        } else if doc.bit_depth == 8 {
            let (w, h, bytes, _) =
                doc.preview_native8(None, doc.width.max(doc.height), None, false)?;
            if (w, h) != (doc.width, doc.height) {
                return Err("Export render did not retain native dimensions".into());
            }
            Pixels::Eight(bytes)
        } else {
            return Err("PNG exports require native 8/16-bit RGB channels".into());
        };
        control.check()?;
        Ok(Self {
            width: doc.width,
            height: doc.height,
            pixels,
        })
    }
    fn sample(&self, x: i32, y: i32) -> [u16; 4] {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0; 4];
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        match &self.pixels {
            Pixels::Eight(p) => std::array::from_fn(|c| u16::from(p[i + c])),
            Pixels::Sixteen(p) => p[i..i + 4].try_into().unwrap(),
        }
    }
    fn bounds(&self, control: &Control) -> Result<[i32; 4], String> {
        let mut bounds = [self.width as i32, self.height as i32, 0, 0];
        for y in 0..self.height as i32 {
            control.check()?;
            for x in 0..self.width as i32 {
                if self.sample(x, y)[3] > 0 {
                    bounds[0] = bounds[0].min(x);
                    bounds[1] = bounds[1].min(y);
                    bounds[2] = bounds[2].max(x + 1);
                    bounds[3] = bounds[3].max(y + 1);
                }
            }
        }
        if bounds[2] <= bounds[0] || bounds[3] <= bounds[1] {
            return Err(
                "Target is transparent; choose canvas bounds to export an empty image".into(),
            );
        }
        Ok(bounds)
    }
    fn resampled(&self, x: f64, y: f64, method: Resampling) -> [u16; 4] {
        if method == Resampling::Nearest {
            return self.sample((x + 0.5).floor() as i32, (y + 0.5).floor() as i32);
        }
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        let (fx, fy) = (x - f64::from(ix), y - f64::from(iy));
        if fx == 0. && fy == 0. {
            return self.sample(ix, iy);
        }
        let weights = [
            (1. - fx) * (1. - fy),
            fx * (1. - fy),
            (1. - fx) * fy,
            fx * fy,
        ];
        let pixels = [
            self.sample(ix, iy),
            self.sample(ix + 1, iy),
            self.sample(ix, iy + 1),
            self.sample(ix + 1, iy + 1),
        ];
        let alpha: f64 = (0..4).map(|i| weights[i] * f64::from(pixels[i][3])).sum();
        if alpha == 0. {
            return [0; 4];
        }
        let mut out = [0; 4];
        out[3] = alpha.round() as u16;
        for (c, value) in out.iter_mut().take(3).enumerate() {
            *value = ((0..4)
                .map(|i| weights[i] * f64::from(pixels[i][c]) * f64::from(pixels[i][3]))
                .sum::<f64>()
                / alpha)
                .round() as u16;
        }
        out
    }
}
pub fn plan(
    doc: &Document,
    targets: &[String],
    settings: &Settings,
    control: &Control,
) -> Result<Plan, String> {
    settings.validate()?;
    size(doc.width, doc.height)?;
    if !matches!(doc.bit_depth, 8 | 16) {
        return Err("PNG exports require native 8/16-bit RGB channels".into());
    }
    control.check()?;
    if targets.len() > 1024 {
        return Err("Export selection exceeds 1024 layer references".into());
    }
    let selected: HashSet<_> = targets.iter().cloned().collect();
    if selected.len() != targets.len()
        || selected
            .iter()
            .any(|id| !doc.layers.iter().any(|l| &l.id == id))
    {
        return Err("Choose unique existing export layers/folders".into());
    }
    let roots = crate::tree::roots(doc, &selected);
    if roots.len().max(1) * settings.variants.len() > MAX_OUTPUTS {
        return Err("Export at most 64 outputs per batch".into());
    }
    let targets: Vec<_> = if roots.is_empty() {
        vec![None]
    } else {
        roots.into_iter().map(Some).collect()
    };
    let mut outputs = vec![];
    let mut names = HashSet::new();
    let document_name = safe_name(
        std::path::Path::new(&doc.name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&doc.name),
    );
    for target in &targets {
        control.check()?;
        let target_name = target
            .as_ref()
            .and_then(|id| doc.layers.iter().find(|l| &l.id == id))
            .map(|l| l.name.as_str())
            .unwrap_or("composite");
        // Validate target semantics even when no alpha scan is needed.
        target_document(doc, target.as_deref())?;
        let mut rect = if settings.crop == Crop::Alpha {
            Image::render(doc, target.as_deref(), control)?.bounds(control)?
        } else {
            [0, 0, doc.width as i32, doc.height as i32]
        };
        let pad = settings.padding as i32;
        rect = [rect[0] - pad, rect[1] - pad, rect[2] + pad, rect[3] + pad];
        let (rw, rh) = ((rect[2] - rect[0]) as u32, (rect[3] - rect[1]) as u32);
        for variant in &settings.variants {
            let width = (f64::from(rw) * variant.scale).round().max(1.) as u32;
            let height = (f64::from(rh) * variant.scale).round().max(1.) as u32;
            size(width, height)?;
            let base = safe_name(
                &settings
                    .template
                    .replace("{document}", &document_name)
                    .replace("{layer}", &safe_name(target_name))
                    .replace("{variant}", &safe_name(&variant.name)),
            );
            let mut filename = format!("{base}.png");
            let mut number = 2;
            while !names.insert(filename.to_lowercase()) {
                if settings.duplicate_names == DuplicateNames::Error {
                    return Err(
                        "Export names collide; change the template or choose numbered duplicates"
                            .into(),
                    );
                }
                filename = format!("{base}_{number}.png");
                number += 1;
            }
            outputs.push(Output {
                target: target.clone(),
                target_name: target_name.into(),
                variant: variant.name.clone(),
                filename,
                document_rect: rect,
                width,
                height,
                bit_depth: doc.bit_depth,
                scale_xy: [
                    f64::from(width) / f64::from(rw),
                    f64::from(height) / f64::from(rh),
                ],
            });
        }
    }
    Ok(Plan {
        document_id: doc.id.clone(),
        revision: doc.revision,
        outputs,
        isolated: !targets.iter().any(Option::is_none),
        profile_retained: doc.icc_profile.is_some() || doc.srgb_tagged,
    })
}
pub fn render(
    doc: &Document,
    output: &Output,
    settings: &Settings,
    control: &Control,
) -> Result<Vec<u8>, String> {
    settings.validate()?;
    size(output.width, output.height)?;
    if output.bit_depth != doc.bit_depth {
        return Err("Export cannot silently change native channel depth".into());
    }
    let rect = output.document_rect;
    if rect[2] <= rect[0] || rect[3] <= rect[1] || rect.iter().any(|v| i64::from(*v).abs() > 16384)
    {
        return Err("Invalid export document rectangle".into());
    }
    let image = Image::render(doc, output.target.as_deref(), control)?;
    let profile = doc
        .icc_profile
        .as_deref()
        .map(Vec::as_slice)
        .or_else(|| doc.srgb_tagged.then(crate::color_profile::srgb_profile));
    if rect == [0, 0, doc.width as i32, doc.height as i32]
        && (output.width, output.height) == (doc.width, doc.height)
    {
        let png = match &image.pixels {
            Pixels::Eight(bytes) => {
                crate::raster::png_with_profile(output.width, output.height, bytes, profile)
            }
            Pixels::Sixteen(words) => {
                crate::raster::png16_with_profile(output.width, output.height, words, profile)
            }
        }?;
        control.check()?;
        return Ok(png);
    }
    let (rw, rh) = (f64::from(rect[2] - rect[0]), f64::from(rect[3] - rect[1]));
    let count = output.width as usize * output.height as usize * 4;
    let mut words = if doc.bit_depth == 16 {
        Vec::with_capacity(count)
    } else {
        Vec::new()
    };
    let mut bytes = if doc.bit_depth == 8 {
        Vec::with_capacity(count)
    } else {
        Vec::new()
    };
    for y in 0..output.height {
        control.check()?;
        let sy = (f64::from(rect[1]) + (f64::from(y) + 0.5) * rh / f64::from(output.height) - 0.5)
            .clamp(f64::from(rect[1]), f64::from(rect[3] - 1));
        for x in 0..output.width {
            let sx = (f64::from(rect[0]) + (f64::from(x) + 0.5) * rw / f64::from(output.width)
                - 0.5)
                .clamp(f64::from(rect[0]), f64::from(rect[2] - 1));
            let pixel = image.resampled(sx, sy, settings.resampling);
            if doc.bit_depth == 16 {
                words.extend(pixel);
            } else {
                bytes.extend(pixel.map(|v| v as u8));
            }
        }
    }
    control.check()?;
    let png = if doc.bit_depth == 16 {
        crate::raster::png16_with_profile(output.width, output.height, &words, profile)
    } else {
        crate::raster::png_with_profile(output.width, output.height, &bytes, profile)
    }?;
    control.check()?;
    Ok(png)
}

/// Native UI, MCP and CLI use this same preference/plan/publication surface.
pub fn request(
    root: &crate::server::Shared,
    p: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    request_with_control(root, p, &Control::default())
}
pub(crate) fn request_with_control(
    root: &crate::server::Shared,
    p: &serde_json::Value,
    control: &Control,
) -> Result<serde_json::Value, String> {
    let _operation = matches!(p["action"].as_str(), Some("save_preset" | "delete_preset"))
        .then(|| crate::server::FileOperation::begin_workspace(&crate::workspace::attach(root)))
        .transpose()?;
    use serde_json::json;
    for key in ["actor", "preset", "action"] {
        if p.get(key).is_some_and(|v| !v.is_string()) {
            return Err(format!("Export {key} must be a string"));
        }
    }
    for key in ["output_index", "max_edge", "expected_library_revision"] {
        if p.get(key).is_some_and(|v| v.as_u64().is_none()) {
            return Err(format!("Export {key} must be a nonnegative integer"));
        }
    }
    let library = root.lock().unwrap().export_library.clone();
    let text = |key: &str| p[key].as_str().ok_or_else(|| format!("Missing {key}"));
    let actor = p["actor"].as_str().unwrap_or("agent");
    match p["action"].as_str().unwrap_or("list") {
        "list" => {
            let mut snapshot = serde_json::to_value(library.lock().unwrap().snapshot())
                .map_err(|e| e.to_string())?;
            snapshot["runs"] = json!(jobs::states(&library));
            Ok(snapshot)
        }
        "save_preset" => {
            let settings: Settings = serde_json::from_value(p["settings"].clone())
                .map_err(|e| format!("Invalid export settings: {e}"))?;
            let revision = p["expected_library_revision"]
                .as_u64()
                .ok_or("Observe expected_library_revision before changing presets")?;
            let preset = library.lock().unwrap().save(
                p["preset"].as_str(),
                text("name")?,
                settings,
                revision,
                actor,
            )?;
            Ok(json!({"preset":preset,"library":library.lock().unwrap().snapshot()}))
        }
        "delete_preset" => {
            let revision = p["expected_library_revision"]
                .as_u64()
                .ok_or("Observe expected_library_revision before changing presets")?;
            library
                .lock()
                .unwrap()
                .delete(text("preset")?, revision, actor)?;
            Ok(json!({"library":library.lock().unwrap().snapshot()}))
        }
        "status" => Ok(json!(jobs::state(&library, text("run")?)?)),
        "cancel" => Ok(json!(jobs::cancel(&library, text("run")?, actor)?)),
        action @ ("plan" | "preview" | "start") => {
            let settings = if let Some(preset) = p["preset"].as_str() {
                if p.get("settings").is_some() {
                    return Err("Choose a stored preset or explicit settings, not both".into());
                }
                let library = library.lock().unwrap();
                if p["expected_library_revision"]
                    .as_u64()
                    .is_some_and(|r| r != library.snapshot().revision)
                {
                    return Err("Export preset changed; refresh before running it".into());
                }
                library.get(preset)?.settings
            } else if let Some(settings) = p.get("settings") {
                serde_json::from_value(settings.clone())
                    .map_err(|e| format!("Invalid export settings: {e}"))?
            } else {
                Settings::default()
            };
            settings.validate()?;
            let targets: Vec<String> = p
                .get("targets")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()
                .map_err(|e| format!("Invalid export targets: {e}"))?
                .unwrap_or_default();
            let shared = crate::workspace::route(root, "observe", p)?;
            if action == "start" {
                let existing = p
                    .get("existing")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| format!("Invalid existing-file policy: {e}"))?
                    .unwrap_or_default();
                return Ok(json!(jobs::start(
                    &shared,
                    p,
                    settings,
                    targets,
                    std::path::PathBuf::from(text("directory")?),
                    existing,
                    actor
                )?));
            }
            let doc = {
                let engine = shared.lock().unwrap();
                crate::workspace::guard(
                    &engine,
                    text("document_id")?,
                    p["expected_revision"]
                        .as_u64()
                        .ok_or("Observe expected_revision before export planning")?,
                )?;
                engine.doc.clone()
            };
            let plan = plan(&doc, &targets, &settings, control)?;
            let mut result = json!({"plan":plan});
            if action == "preview" {
                use base64::Engine as _;
                let index = p["output_index"].as_u64().unwrap_or(0) as usize;
                let output = plan
                    .outputs
                    .get(index)
                    .ok_or("Export preview output is missing")?;
                let bytes = render(&doc, output, &settings, control)?;
                let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
                let mut words = image.into_rgba16().into_raw();
                let mut managed = false;
                if let Some(profile) = doc.icc_profile.as_deref() {
                    if crate::color_profile::supported(profile).is_ok() {
                        crate::color_profile::convert16(profile, &mut words)?;
                        managed = true;
                    }
                }
                let edge = p["max_edge"].as_u64().unwrap_or(512).clamp(32, 1024) as f64;
                let scale = (edge / f64::from(output.width.max(output.height))).min(1.);
                let (w, h) = (
                    (f64::from(output.width) * scale).round().max(1.) as u32,
                    (f64::from(output.height) * scale).round().max(1.) as u32,
                );
                let rgba = crate::render::rgba8(w, h, |x, y| {
                    let sx = (f64::from(x) / scale)
                        .floor()
                        .min(f64::from(output.width - 1)) as usize;
                    let sy = (f64::from(y) / scale)
                        .floor()
                        .min(f64::from(output.height - 1)) as usize;
                    let at = (sy * output.width as usize + sx) * 4;
                    crate::depth16::display_pixel(words[at..at + 4].try_into().unwrap())
                });
                result["preview_only"] = json!(true);
                result["images"] = json!([{"mime_type":"image/png","data":base64::engine::general_purpose::STANDARD.encode(crate::raster::png(w,h,&rgba)?),"width":w,"height":h,"document_rect":output.document_rect,"output_size":[output.width,output.height],"native_source_depth":doc.bit_depth,"display_managed":managed,"revision":doc.revision}]);
            }
            {
                let engine = shared.lock().unwrap();
                crate::workspace::guard(&engine, &doc.id, doc.revision)?;
            }
            Ok(result)
        }
        _ => Err("Unknown export action".into()),
    }
}
