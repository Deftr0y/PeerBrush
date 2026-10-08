//! Bounded soft-edge refinement. Source channels remain untouched.
use super::{current, system};
use crate::{
    engine::Document,
    raster::{Raster, TILE},
};
use serde_json::Value;

fn parameter(c: &Value, key: &str, min: f32, max: f32, default: f32) -> Result<f32, String> {
    let Some(v) = c.get(key) else {
        return Ok(default);
    };
    let v = v.as_f64().ok_or_else(|| format!("Invalid {key}"))?;
    if !v.is_finite() || v < min as f64 || v > max as f64 {
        return Err(format!("{key} must be between {min} and {max}"));
    }
    Ok(v as f32)
}
/// Separable clamped box means, linear work independent of radius.
fn mean(input: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 {
        return input.to_vec();
    }
    let mut tmp = vec![0.; input.len()];
    let mut output = vec![0.; input.len()];
    let count = (2 * r + 1) as f32;
    for y in 0..h {
        let row = &input[y * w..(y + 1) * w];
        let mut sum = 0.;
        for k in -(r as isize)..=r as isize {
            sum += row[k.clamp(0, w as isize - 1) as usize];
        }
        for x in 0..w {
            tmp[y * w + x] = sum / count;
            sum += row[(x + r + 1).min(w - 1)] - row[x.saturating_sub(r)];
        }
    }
    for x in 0..w {
        let mut sum = 0.;
        for k in -(r as isize)..=r as isize {
            sum += tmp[k.clamp(0, h as isize - 1) as usize * w + x];
        }
        for y in 0..h {
            output[y * w + x] = sum / count;
            sum += tmp[(y + r + 1).min(h - 1) * w + x] - tmp[y.saturating_sub(r) * w + x];
        }
    }
    output
}
/// Guided feathering uses the local linear model of He, Sun & Tang (ECCV 2010).
/// Two-radius halos make each tile equal to a whole-image filter in its interior.
fn guided(mask: &Raster, pixels: &[u8], radius: usize, amount: f32) -> Raster {
    let mut output = Raster::new_depth(mask.width, mask.height, mask.depth);
    for top in (0..mask.height as usize).step_by(TILE as usize) {
        for left in (0..mask.width as usize).step_by(TILE as usize) {
            let tw = (mask.width as usize - left).min(TILE as usize);
            let th = (mask.height as usize - top).min(TILE as usize);
            let halo = radius * 2;
            let w = tw + halo * 2;
            let h = th + halo * 2;
            let mut guide = vec![0.; w * h];
            let mut source = vec![0.; w * h];
            for y in 0..h {
                for x in 0..w {
                    let gx = (left as isize + x as isize - halo as isize)
                        .clamp(0, mask.width as isize - 1) as usize;
                    let gy = (top as isize + y as isize - halo as isize)
                        .clamp(0, mask.height as isize - 1) as usize;
                    let p = &pixels[(gy * mask.width as usize + gx) * 4..][..4];
                    guide[y * w + x] =
                        (p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722) / 255.
                            * p[3] as f32
                            / 255.;
                    source[y * w + x] = mask.get16(gx as i32, gy as i32)[0] as f32 / 65535.;
                }
            }
            let mi = mean(&guide, w, h, radius);
            let mp = mean(&source, w, h, radius);
            let ii = mean(
                &guide.iter().map(|i| i * i).collect::<Vec<_>>(),
                w,
                h,
                radius,
            );
            let ip = mean(
                &guide
                    .iter()
                    .zip(&source)
                    .map(|(i, p)| i * p)
                    .collect::<Vec<_>>(),
                w,
                h,
                radius,
            );
            let a: Vec<_> = (0..w * h)
                .map(|i| (ip[i] - mi[i] * mp[i]) / ((ii[i] - mi[i] * mi[i]).max(0.) + 0.0001))
                .collect();
            let b: Vec<_> = (0..w * h).map(|i| mp[i] - a[i] * mi[i]).collect();
            let ma = mean(&a, w, h, radius);
            let mb = mean(&b, w, h, radius);
            for y in 0..th {
                for x in 0..tw {
                    let i = (y + halo) * w + x + halo;
                    let q = (ma[i] * guide[i] + mb[i]).clamp(0., 1.);
                    let v = ((source[i] * (1. - amount) + q * amount) * 65535.)
                        .round()
                        .clamp(0., 65535.) as u16;
                    if v > 0 {
                        output.set16((left + x) as i32, (top + y) as i32, [v; 4]);
                    }
                }
            }
        }
    }
    output
}
pub fn apply(doc: &mut Document, c: &Value) -> Result<(), String> {
    let coverage = current(doc).ok_or("Select an area before refining its edge")?;
    if u64::from(doc.width) * u64::from(doc.height) > 16_000_000 {
        return Err("Selection refinement is limited to 16 megapixels".into());
    }
    let mut input = Raster::new(doc.width, doc.height);
    for y in 0..doc.height as i32 {
        for x in 0..doc.width as i32 {
            let value = (coverage.value(x, y) * 255.).round() as u8;
            if value > 0 {
                input.set(x, y, [value; 4]);
            }
        }
    }
    let pixels = if c["edge"].as_f64().unwrap_or(0.) > 0. {
        let mut guide = c.clone();
        if guide.get("sample_merged").is_none() {
            guide["sample_merged"] = Value::Bool(true);
        }
        Some(system::snapshot(doc, &guide)?.2)
    } else {
        None
    };
    let mask = run(&input, pixels.as_deref(), c)?;
    if mask == input {
        return Ok(());
    }
    system::put(doc, mask)
}
pub fn run(input: &Raster, pixels: Option<&[u8]>, c: &Value) -> Result<Raster, String> {
    let smooth = parameter(c, "smooth", 0., 32., 0.)?.round() as usize;
    let feather = parameter(c, "feather", 0., 64., 0.)?.round() as usize;
    let shift = parameter(c, "shift", -32., 32., 0.)?.round() as i32;
    let contrast = parameter(c, "contrast", 0., 100., 0.)? / 100.;
    let edge = parameter(c, "edge", 0., 100., 0.)? / 100.;
    let radius = parameter(c, "edge_radius", 1., 32., 8.)?.round() as usize;
    if u64::from(input.width) * u64::from(input.height) > 16_000_000 {
        return Err("Selection refinement is limited to 16 megapixels".into());
    }
    if smooth == 0 && feather == 0 && shift == 0 && contrast == 0. && edge == 0. {
        return Ok(input.clone());
    }
    let w = input.width as usize;
    let h = input.height as usize;
    let mut values: Vec<_> = (0..h)
        .flat_map(|y| (0..w).map(move |x| input.get16(x as i32, y as i32)[0]))
        .collect();
    if shift != 0 {
        let mut tmp = vec![0; w * h];
        let r = shift.unsigned_abs() as usize;
        for y in 0..h {
            tmp[y * w..(y + 1) * w].copy_from_slice(&system::morph(
                &values[y * w..(y + 1) * w],
                r,
                shift > 0,
            ));
        }
        for x in 0..w {
            let line: Vec<_> = (0..h).map(|y| tmp[y * w + x]).collect();
            for (y, v) in system::morph(&line, r, shift > 0).into_iter().enumerate() {
                values[y * w + x] = v;
            }
        }
    }
    let mut mask = Raster::new_depth(input.width, input.height, input.depth);
    // Local windows avoid full-canvas float buffers and stay within 16 MiB scratch.
    for top in (0..h).step_by(TILE as usize) {
        for left in (0..w).step_by(TILE as usize) {
            let tw = (w - left).min(TILE as usize);
            let th = (h - top).min(TILE as usize);
            let halo = smooth + feather;
            let bw = tw + 2 * halo;
            let bh = th + 2 * halo;
            let mut block = vec![0.; bw * bh];
            for y in 0..bh {
                for x in 0..bw {
                    let gx = left as isize + x as isize - halo as isize;
                    let gy = top as isize + y as isize - halo as isize;
                    if gx >= 0 && gy >= 0 && gx < w as isize && gy < h as isize {
                        block[y * bw + x] = values[gy as usize * w + gx as usize] as f32 / 65535.;
                    }
                }
            }
            if smooth > 0 {
                block = mean(&block, bw, bh, smooth);
                for v in &mut block {
                    *v = ((*v - 0.5) * 4. + 0.5).clamp(0., 1.);
                }
            }
            if feather > 0 {
                block = mean(&block, bw, bh, feather);
            }
            for y in 0..th {
                for x in 0..tw {
                    let v = ((block[(y + halo) * bw + x + halo] - 0.5) / (1. - contrast * 0.99)
                        + 0.5)
                        .clamp(0., 1.);
                    let v = (v * 65535.).round() as u16;
                    if v > 0 {
                        mask.set16((left + x) as i32, (top + y) as i32, [v; 4]);
                    }
                }
            }
        }
    }
    if edge > 0. {
        let pixels = pixels
            .filter(|p| p.len() == w * h * 4)
            .ok_or("Edge refinement needs an image matching the mask frame")?;
        mask = guided(&mask, pixels, radius, edge);
    }
    Ok(mask)
}
