//! Procedural evaluation ends before command preparation/publication begins.
//! Callback/source owners drain before the completed command allocation moves.
use super::*;
mod input;
pub(super) use input::{Config, Input, Pixels, ResidentPixels};
pub(super) use input::write_metadata;

pub(super) fn commands(
    shared: &Shared,
    job: &Job,
    source: &Document,
    script: &str,
    start: Instant,
) -> Result<Vec<Value>, String> {
    let (live, control, frozen) = (shared.clone(), job.clone(), source.clone());
    let progress = Box::new(move |count| {
        if control.cancel.load(Ordering::Relaxed) {
            return Some(Dynamic::from(
                "Code canceled or evaluation time limit exceeded",
            ));
        }
        if count % 1024 == 0 {
            if let Ok(mut e) = live.try_lock() {
                if let Err(error) = guard(&mut e, &control, DocumentView::from(&frozen)) {
                    return Some(Dynamic::from(error));
                }
            }
        }
        None
    });
    let input = Input {
        config: Config::from_view(source.into()),
        metadata: input::metadata(source.into())?,
        pixels: Box::new(ResidentPixels::new(source.clone(), job.scopes.clone())),
        progress,
    };
    evaluate(input, script, start)
}

pub(super) fn evaluate(input: Input, script: &str, start: Instant) -> Result<Vec<Value>, String> {
    let Input {
        config,
        metadata,
        pixels,
        progress,
    } = input;
    config.validate()?;
    if script.trim().is_empty() || script.len() > 65536 {
        return Err("Script must contain 1–65536 UTF-8 bytes".into());
    }
    let context = Arc::new(Mutex::new(Context {
        config,
        source: pixels,
        patches: BTreeMap::new(),
        next: 0,
        pixels: 0,
        bytes: 0,
        commands: vec![],
    }));
    let mut runtime = rhai::Engine::new();
    runtime
        .set_max_operations(2_000_000)
        .set_max_variables(64)
        .set_max_functions(32)
        .set_max_call_levels(16)
        .set_max_expr_depths(32, 16)
        .set_max_array_size(4096)
        .set_max_map_size(1024)
        .set_max_string_size(16384)
        .set_optimization_level(rhai::OptimizationLevel::None);
    runtime.disable_symbol("eval");
    runtime.on_print(|_| {});
    runtime.on_debug(|_, _, _| {});
    runtime.on_progress(move |count| {
        if start.elapsed() > Duration::from_secs(5) {
            Some(Dynamic::from(
                "Code canceled or evaluation time limit exceeded",
            ))
        } else {
            progress(count)
        }
    });
    let c = context.clone();
    runtime.register_fn(
        "read_pixel",
        move |layer: ImmutableString, x: INT, y: INT| -> Result<Array, Box<EvalAltResult>> {
            Ok(c.lock()
                .unwrap()
                .pixel(&layer, x, y)
                .map_err(fail)?
                .map(|v| Dynamic::from(v as INT))
                .to_vec())
        },
    );
    let c = context.clone();
    runtime.register_fn(
        "begin_pixels",
        move |layer: ImmutableString, values: Array| -> Result<INT, Box<EvalAltResult>> {
            let rect = rect_array(values)?;
            let mut c = c.lock().unwrap();
            if rect[0] < 0
                || rect[1] < 0
                || rect[2] > c.config.width as i32
                || rect[3] > c.config.height as i32
            {
                return Err(fail("Pixel rectangle must be inside the document"));
            }
            let pixels = (rect[2] - rect[0]) as usize * (rect[3] - rect[1]) as usize;
            if c.pixels + pixels > MAX_PIXELS || c.patches.len() >= 8 {
                return Err(fail("Code exceeds 262144 patch pixels or 8 open buffers"));
            }
            let words = c.source.patch(&layer, rect).map_err(fail)?;
            if words.len() != pixels * 4 {
                return Err(fail("Pixel reader returned an incomplete buffer"));
            }
            if c.config.bit_depth == 8 && words.iter().any(|word| *word > 255) {
                return Err(fail("Pixel reader returned another channel depth"));
            }
            c.pixels += pixels;
            let handle = c.next;
            c.next += 1;
            c.patches.insert(
                handle,
                Patch {
                    layer: layer.into(),
                    rect,
                    words,
                },
            );
            Ok(handle)
        },
    );
    let c = context.clone();
    runtime.register_fn(
        "write_pixel",
        move |handle: INT, x: INT, y: INT, rgba: Array| -> Result<(), Box<EvalAltResult>> {
            let mut c = c.lock().unwrap();
            let max = if c.config.bit_depth == 16 { 65535 } else { 255 };
            if rgba.len() != 4 {
                return Err(fail("RGBA needs four native integers"));
            }
            let mut pixel = [0u16; 4];
            for (i, v) in rgba.into_iter().enumerate() {
                pixel[i] = v
                    .as_int()
                    .ok()
                    .filter(|v| (0..=max).contains(v))
                    .ok_or_else(|| fail("Native channel value is out of range"))?
                    as u16;
            }
            let patch = c
                .patches
                .get_mut(&handle)
                .ok_or_else(|| fail("Unknown pixel buffer"))?;
            let r = patch.rect;
            if x < r[0] as INT || y < r[1] as INT || x >= r[2] as INT || y >= r[3] as INT {
                return Err(fail("Write exceeds the pixel buffer rectangle"));
            }
            let at = ((y - r[1] as INT) * (r[2] - r[0]) as INT + x - r[0] as INT) as usize * 4;
            patch.words[at..at + 4].copy_from_slice(&pixel);
            Ok(())
        },
    );
    let c = context.clone();
    runtime.register_fn("commit_pixels",move |handle:INT|->Result<(),Box<EvalAltResult>> {
        let mut c=c.lock().unwrap();let patch=c.patches.remove(&handle).ok_or_else(||fail("Unknown pixel buffer"))?;
        let depth=c.config.bit_depth;
        let bytes=if depth==16 {patch.words.into_iter().flat_map(u16::to_le_bytes).collect()} else {patch.words.into_iter().map(|v|v as u8).collect::<Vec<_>>()};
        c.push(json!({"op":"pixels.replace","layer":patch.layer,"rect":patch.rect,"bit_depth":depth,"rgba":STANDARD.encode(bytes)})).map_err(fail)
    });
    let c = context.clone();
    runtime.register_fn("edit",move |map:Map|->Result<(),Box<EvalAltResult>> {
        let input = Dynamic::from_map(map);
        c.lock().unwrap().command_length(&input).map_err(fail)?;
        let command=rhai::serde::from_dynamic::<Value>(&input).map_err(|e|fail(e.to_string()))?;
        command::validate_edit(&command).map_err(fail)?;
        c.lock().unwrap().push(command).map_err(fail)
    });
    let mut scope = rhai::Scope::new();
    scope.push_constant_dynamic("document", metadata);
    runtime
        .run_with_scope(&mut scope, script)
        .map_err(|e| format!("Code discarded: {}", script_error(&e)))?;
    drop(scope);
    drop(runtime);
    take_commands(context)
}

struct Patch {
    layer: String,
    rect: [i32; 4],
    words: Vec<u16>,
}
struct Context {
    config: Config,
    source: Box<dyn Pixels>,
    patches: BTreeMap<INT, Patch>,
    next: INT,
    pixels: usize,
    bytes: usize,
    commands: Vec<Value>,
}
impl Context {
    fn command_length<T: serde::Serialize>(&self, command: &T) -> Result<usize, String> {
        if self.commands.len() >= 32 {
            return Err("Code exceeds 32 commands".into());
        }
        if self.bytes >= MAX_BYTES {
            return Err("Code exceeds 8 MiB command output".into());
        }
        crate::psd::canonical36::Canonical::raw_length(
            command,
            crate::psd::canonical36::Limits {
                bytes: MAX_BYTES.saturating_sub(self.bytes),
                nodes: 8_388_608,
                depth: 128,
            },
            &|| Ok(()),
        )
        .map_err(|error| {
            if error.contains("exceeds byte admission") {
                "Code exceeds 8 MiB command output".into()
            } else {
                format!("Invalid code command: {error}")
            }
        })
    }
    fn push(&mut self, command: Value) -> Result<(), String> {
        let length = self.command_length(&command)?;
        self.bytes += length;
        self.commands.push(command);
        Ok(())
    }
    fn pixel(&self, layer: &str, x: INT, y: INT) -> Result<[u16; 4], String> {
        if x < 0 || y < 0 || x >= INT::from(self.config.width) || y >= INT::from(self.config.height)
        {
            return Err("Pixel coordinates must be inside the document".into());
        }
        let pixel = self.source.pixel(layer, x, y)?;
        if self.config.bit_depth == 8 && pixel.iter().any(|word| *word > 255) {
            return Err("Pixel reader returned another channel depth".into());
        }
        Ok(pixel)
    }
}

fn rect_array(values: Array) -> Result<[i32; 4], Box<EvalAltResult>> {
    let value = rhai::serde::from_dynamic::<Value>(&Dynamic::from_array(values))
        .map_err(|e| fail(e.to_string()))?;
    engine::rect(&value)
        .filter(|r| r[0] < r[2] && r[1] < r[3])
        .ok_or_else(|| fail("Use a nonempty integer rectangle"))
}
fn take_commands(context: Arc<Mutex<Context>>) -> Result<Vec<Value>, String> {
    let context = Arc::try_unwrap(context)
        .map_err(|_| "Code interpreter still retains its context; no edits committed")?
        .into_inner()
        .map_err(|_| "Code context failed; no edits committed")?;
    if !context.patches.is_empty() {
        return Err("Commit every open pixel buffer before finishing the script".into());
    }
    if context.commands.is_empty() {
        return Err("Code produced no edits".into());
    }
    Ok(context.commands)
}

#[cfg(test)]
mod tests;
