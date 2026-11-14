//! Parent-only exchange over already supervised pipes. Transport completion
//! is not successful process exit or permission to publish document edits.
use super::{
    build_label,
    protocol::{Message, State, METADATA_LIMIT, SCRIPT_LIMIT},
};
use crate::{
    code::{
        command,
        evaluation::{self, Config},
        native_source::NativeSource,
        Job, MAX_BYTES, MAX_PIXELS,
    },
    large_document::{ResidentBudget, ScratchBytes},
    psd::{
        canonical36::{Canonical, Limits, Stats},
        memory36::{Admitted, MetadataCharge},
    },
    server::Shared,
};
use serde_json::Value;
use std::{
    io::{Cursor, Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;

pub(super) struct Received {
    // Deliberately private: a supervisor must independently observe exit before
    // handing these commands to ordinary native preparation/publication.
    commands: Admitted<Vec<Value>>,
}
impl Received {
    pub(super) fn into_commands(self) -> Admitted<Vec<Value>> {
        self.commands
    }
}

struct Commands {
    values: Vec<Value>,
    owners: Vec<Arc<MetadataCharge>>,
    pixels: usize,
}
impl Commands {
    fn new(pool: &ResidentBudget, check: &dyn Fn() -> Result<()>) -> Result<Self> {
        let header = MetadataCharge::reserve(pool, 65536, check)?;
        let mut values = Vec::new();
        let mut owners = Vec::new();
        values
            .try_reserve_exact(32)
            .map_err(|_| "Code command headers refused")?;
        owners
            .try_reserve_exact(33)
            .map_err(|_| "Code command owners refused")?;
        owners.push(header);
        Ok(Self {
            values,
            owners,
            pixels: 0,
        })
    }
    fn push(&mut self, value: Value, owner: Arc<MetadataCharge>, pixels: usize) -> Result<()> {
        let total = self
            .pixels
            .checked_add(pixels)
            .filter(|n| *n <= MAX_PIXELS)
            .ok_or("Code exceeds 262144 patch pixels")?;
        if self.values.len() >= 32 {
            return Err("Code exceeds 32 commands".into());
        }
        self.values.push(value);
        self.owners.push(owner);
        self.pixels = total;
        Ok(())
    }
    fn finish(self, check: &dyn Fn() -> Result<()>) -> Result<Received> {
        let owner = MetadataCharge::combine(&self.owners, check)?;
        Ok(Received {
            commands: Admitted::from_parts(Arc::new(self.values), owner),
        })
    }
}

fn parse_command(
    bytes: &[u8],
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<()>,
) -> Result<Admitted<Value>> {
    let limits = Limits {
        bytes: MAX_BYTES,
        nodes: 8_388_608,
        depth: 128,
    };
    // Scan scratch is separately admitted by exchange before any JSON parsing.
    let stats = Stats::scan(bytes, limits, check)?;
    let encoded = bytes
        .len()
        .checked_next_power_of_two()
        .and_then(|n| n.checked_mul(2))
        .ok_or("Code command capacity overflow")?;
    let total = stats
        .tree_upper_bytes()?
        .checked_add(encoded)
        .and_then(|n| n.checked_add(32768))
        .ok_or("Code command admission overflow")?;
    let owner = MetadataCharge::reserve(pool, total, check)?;
    let canonical = Canonical::parse(bytes, limits, check)?;
    let (encoded_bytes, value) = canonical.into_parts();
    if encoded_bytes.capacity() > encoded {
        return Err("Code command capacity exceeds its admission".into());
    }
    drop(encoded_bytes);
    check()?;
    Ok(Admitted::from_parts(Arc::new(value), owner))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn exchange(
    shared: &Shared,
    job: &Job,
    source: &mut NativeSource,
    script: &str,
    nonce: [u8; 16],
    mut reader: impl Read,
    mut writer: impl Write,
    started: Instant,
    check: &dyn Fn() -> Result<()>,
) -> Result<Received> {
    let active = || {
        check()?;
        if started.elapsed() >= Duration::from_secs(30) {
            return Err("Code preparation time limit exceeded; no edits committed".into());
        }
        Ok(())
    };
    source.check(shared, job, &active)?;
    let config = Config::from_view(source.view());
    config.validate()?;
    if script.trim().is_empty() || script.len() > SCRIPT_LIMIT {
        return Err("Script must contain 1–65536 UTF-8 bytes".into());
    }
    let mut state = State::parent(nonce)?;
    let pool = source.budget().clone();
    // Covers escaped JSON scan strings, fixed/control headers and callback
    // bookkeeping. Each returned DOM gets its own measured admission below.
    let _scan = MetadataCharge::reserve(&pool, 2 * MAX_BYTES + 65536, &active)?;
    let mut body = ScratchBytes::new(&pool, MAX_BYTES, MAX_BYTES).map_err(|e| e.to_string())?;
    let mut commands = Commands::new(&pool, &active)?;
    let mut metadata = Cursor::new(&mut body[..METADATA_LIMIT]);
    evaluation::write_metadata(source.view(), &mut metadata)?;
    let length = metadata.position() as usize;
    body[length..length + script.len()].copy_from_slice(script.as_bytes());
    let start = Message::Start {
        config,
        metadata_bytes: length as u32,
        script_bytes: script.len() as u32,
        build: build_label(),
    };
    state.send(&start, &body[..length + script.len()], &mut writer, &|| {
        source.check(shared, job, &active)
    })?;
    loop {
        let frame = state.receive(&mut reader, &mut body, &|| {
            source.check(shared, job, &active)
        })?;
        match frame.message {
            Message::Point { layer, point } => {
                let result = source.pixel(shared, job, &layer, point[0], point[1], &active);
                source.check(shared, job, &active)?;
                match result {
                    Ok(words) => {
                        for (word, output) in words.iter().zip(body.chunks_exact_mut(2)) {
                            output.copy_from_slice(&word.to_le_bytes());
                        }
                        state.send(
                            &Message::Words { words: 4 },
                            &body[..8],
                            &mut writer,
                            &|| source.check(shared, job, &active),
                        )?;
                    }
                    Err(error) => state.send(
                        &Message::Error {
                            message: super::child::error_text(&error),
                        },
                        &[],
                        &mut writer,
                        &|| source.check(shared, job, &active),
                    )?,
                }
            }
            Message::Patch { layer, rect } => {
                let result = source.patch(shared, job, &layer, rect, &active);
                source.check(shared, job, &active)?;
                match result {
                    Ok(words) => {
                        for (word, output) in words.iter().zip(body.chunks_exact_mut(2)) {
                            output.copy_from_slice(&word.to_le_bytes());
                        }
                        state.send(
                            &Message::Words {
                                words: words.len() as u32,
                            },
                            &body[..words.len() * 2],
                            &mut writer,
                            &|| source.check(shared, job, &active),
                        )?;
                    }
                    Err(error) => state.send(
                        &Message::Error {
                            message: super::child::error_text(&error),
                        },
                        &[],
                        &mut writer,
                        &|| source.check(shared, job, &active),
                    )?,
                }
            }
            Message::Progress { .. } => {
                state.send(&Message::Continue, &[], &mut writer, &|| {
                    source.check(shared, job, &active)
                })?
            }
            Message::Command { .. } => {
                let command = parse_command(frame.body, &pool, &active)?;
                let pixels = command::validate_returned(&command, config, &mut body)?;
                source.check(shared, job, &active)?;
                command.map_unique(|value, owner| commands.push(value, owner.clone(), pixels))?;
                state.send(&Message::Continue, &[], &mut writer, &|| {
                    source.check(shared, job, &active)
                })?;
            }
            Message::Done { .. } => {
                drop(writer);
                state.exact_eof(&mut reader, &|| source.check(shared, job, &active))?;
                if !state.successful() {
                    return Err("Code worker transport is incomplete".into());
                }
                source.check(shared, job, &active)?;
                return commands.finish(&active);
            }
            Message::Failed { message } => return Err(format!("Code discarded: {message}")),
            _ => return Err("Unexpected code worker reply".into()),
        }
    }
}

#[cfg(test)]
mod tests;
