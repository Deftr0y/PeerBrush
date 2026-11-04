use crate::{
    engine::native::{
        CoverageMetadata, CoveragePatch, CoveragePixels, EffectClipPatch, EffectPatch,
        LayerMetadata, NativeDocument,
    },
    psd::memory36::MetadataCharge,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub(super) fn coverage(
    base: &NativeDocument,
    source: CoverageMetadata<'_>,
    delta: [i32; 2],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<CoveragePatch, String> {
    let add = |value: i32, by: i32| {
        value
            .checked_add(by)
            .filter(|v| v.unsigned_abs() <= 100000)
            .ok_or("Translated coverage exceeds coordinate limits")
    };
    let b = source.bounds();
    let o = source.origin();
    let bounds = [
        add(b[0], delta[0])?,
        add(b[1], delta[1])?,
        add(b[2], delta[0])?,
        add(b[3], delta[1])?,
    ];
    let origin = [add(o[0], delta[0])?, add(o[1], delta[1])?];
    let contours = base
        .copy_contour_metadata(source.contours(), check)?
        .map_unique(|mut contours, _| {
            for line in &mut contours {
                check()?;
                for p in line {
                    for axis in 0..2 {
                        p[axis] -= (-delta[axis]) as f32;
                        if !p[axis].is_finite() || p[axis].abs() > 100000. {
                            return Err(
                                "Translated coverage contour exceeds coordinate limits".into()
                            );
                        }
                    }
                }
            }
            Ok(contours)
        })?;
    Ok(CoveragePatch {
        pixels: CoveragePixels::Existing(source.mask().slot()),
        bounds,
        origin,
        contours,
    })
}

/// Literal translation of local Liquify controls. Typed clip masks stay bound
/// to their exact existing source slots; only placement/contours are translated.
pub(super) fn patches(
    base: &NativeDocument,
    layer: LayerMetadata<'_>,
    delta: [i32; 2],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<EffectPatch>, String> {
    let count = layer.effects().filter(|e| e.kind() == "liquify").count();
    let mut patches = Vec::new();
    patches
        .try_reserve_exact(count)
        .map_err(|_| "Liquify patch allocation refused")?;
    for effect in layer.effects().filter(|e| e.kind() == "liquify") {
        check()?;
        let strokes = effect
            .settings()
            .get("strokes")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let extra = MetadataCharge::reserve(
            base.budget(),
            strokes
                .checked_mul(512)
                .and_then(|n| n.checked_add(4096))
                .ok_or("Liquify translation envelope overflow")?,
            check,
        )?;
        let settings = base
            .copy_value_metadata(effect.settings(), check)?
            .retain_owner(&extra, check)?
            .map_unique(|mut settings, _| {
                if let Some(strokes) = settings.get_mut("strokes").and_then(Value::as_array_mut) {
                    for stroke in strokes {
                        check()?;
                        if stroke.get("coverage").is_some_and(|v| !v.is_null()) {
                            return Err("Inline native Liquify coverage is forbidden".into());
                        }
                        for key in ["points", "polygon"] {
                            // The resident spatial mapper normalizes an absent
                            // control array to null via mutable JSON indexing.
                            if stroke.get(key).is_none() {
                                stroke[key] = Value::Null;
                            }
                            if let Some(points) = stroke.get_mut(key).and_then(Value::as_array_mut)
                            {
                                for p in points {
                                    let p = p
                                        .as_array_mut()
                                        .filter(|p| p.len() >= 2)
                                        .ok_or("Invalid spatial control point")?;
                                    for axis in 0..2 {
                                        p[axis] = json!(
                                            p[axis]
                                                .as_f64()
                                                .ok_or("Invalid spatial control point")?
                                                + f64::from(delta[axis])
                                        );
                                    }
                                }
                            }
                        }
                        if let Some(points) = stroke.get("polygon").filter(|v| !v.is_null()) {
                            let points = points
                                .as_array()
                                .filter(|p| p.len() <= 8192)
                                .ok_or("Invalid Liquify polygon")?;
                            let bytes = points.len() * std::mem::size_of::<[f32; 2]>() + 4096;
                            let scratch = base
                                .budget()
                                .reserve_bytes(bytes, bytes)
                                .map_err(|e| e.to_string())?;
                            let mut polygon = Vec::new();
                            polygon
                                .try_reserve_exact(points.len())
                                .map_err(|_| "Liquify polygon scratch allocation refused")?;
                            if polygon.capacity() * std::mem::size_of::<[f32; 2]>()
                                > scratch.bytes()
                            {
                                return Err("Liquify polygon scratch exceeds admission".into());
                            }
                            for point in points {
                                polygon.push(
                                    <[f32; 2]>::deserialize(point)
                                        .map_err(|_| "Invalid Liquify polygon")?,
                                );
                            }
                            let bounds = crate::selection::bounds(&polygon)?;
                            stroke["selection"] = json!(bounds);
                        } else if let Some(rect) = stroke.get("selection").and_then(Value::as_array)
                        {
                            if rect.len() != 4 {
                                return Err("Invalid Liquify selection rectangle".into());
                            }
                            let mapped = std::array::from_fn::<_, 4, _>(|i| {
                                (rect[i].as_f64().unwrap_or(0.) + f64::from(delta[i % 2])).round()
                                    as i32
                            });
                            if mapped[0] >= mapped[2] || mapped[1] >= mapped[3] {
                                return Err(
                                    "Resize would collapse a liquify selection; enlarge the image"
                                        .into(),
                                );
                            }
                            stroke["selection"] = json!(mapped);
                        }
                    }
                }
                crate::effects::validate("liquify", &settings)?;
                Ok(settings)
            })?;
        let mut clips = Vec::new();
        clips
            .try_reserve_exact(effect.clips().len())
            .map_err(|_| "Liquify clip patch allocation refused")?;
        for (stroke, source) in effect.clips() {
            clips.push(EffectClipPatch {
                stroke,
                coverage: coverage(base, source, delta, check)?,
            });
        }
        patches.push(EffectPatch {
            layer: Some(layer.pixels().slot()),
            id: effect.id().to_owned(),
            settings: Some(settings),
            enabled: None,
            weight: None,
            clips: Some(clips),
        });
    }
    Ok(patches)
}
