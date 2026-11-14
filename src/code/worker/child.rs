//! The child holds metadata and pipe replies. It cannot open document sources
//! or publish edits, even after a successful interpreter result.
use super::{
    protocol::{Message, State},
    resources::Permit,
};
use crate::code::{
    evaluation::{self, Config, Input, Pixels},
    MAX_BYTES,
};
use rhai::{Dynamic, INT};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Instant,
};
type Result<T> = std::result::Result<T, String>;
const FAILED: &str = "Code worker exchange failed; current work preserved";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    width: u32,
    height: u32,
    bit_depth: u16,
    channel_max: u16,
    layers: Vec<Layer>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Layer {
    id: String,
    name: String,
    kind: String,
    parent: Option<String>,
    x: i32,
    y: i32,
    locked: bool,
}
impl Metadata {
    fn validate(&self, config: Config) -> Result<()> {
        if (self.width, self.height, self.bit_depth)
            != (config.width, config.height, config.bit_depth)
            || self.channel_max != if config.bit_depth == 16 { 65535 } else { 255 }
            || self.layers.len() > 100
        {
            return Err(FAILED.into());
        }
        for (index, layer) in self.layers.iter().enumerate() {
            if layer.id.is_empty()
                || layer.id.len() > 1024
                || layer.name.len() > 1024
                || layer.kind.len() > 32
                || self.layers[..index]
                    .iter()
                    .any(|previous| previous.id == layer.id)
                || layer.parent.as_ref().is_some_and(|parent| {
                    parent == &layer.id || !self.layers.iter().any(|other| other.id == *parent)
                })
            {
                return Err(FAILED.into());
            }
        }
        Ok(())
    }
}
struct Channel<R, W> {
    state: State,
    reader: R,
    writer: W,
    body: Vec<u8>,
}
impl<R: Read, W: Write> Channel<R, W> {
    fn words(&mut self, message: Message) -> Result<&[u8]> {
        self.state
            .send(&message, &[], &mut self.writer, &|| Ok(()))?;
        let reply = self
            .state
            .receive(&mut self.reader, &mut self.body, &|| Ok(()))?;
        match reply.message {
            Message::Words { .. } => Ok(reply.body),
            Message::Error { message } | Message::Abort { message } => Err(message),
            _ => Err(FAILED.into()),
        }
    }
    fn progress(&mut self, operations: u64) -> Result<()> {
        self.state.send(
            &Message::Progress { operations },
            &[],
            &mut self.writer,
            &|| Ok(()),
        )?;
        let reply = self
            .state
            .receive(&mut self.reader, &mut self.body, &|| Ok(()))?;
        match reply.message {
            Message::Continue => Ok(()),
            Message::Abort { message } => Err(message),
            _ => Err(FAILED.into()),
        }
    }
    fn command(&mut self, index: u8, value: &serde_json::Value) -> Result<()> {
        let mut output = std::io::Cursor::new(&mut self.body[..]);
        serde_json::to_writer(&mut output, value).map_err(|_| FAILED)?;
        let length = output.position() as usize;
        self.state.send(
            &Message::Command { index },
            &self.body[..length],
            &mut self.writer,
            &|| Ok(()),
        )?;
        let reply = self
            .state
            .receive(&mut self.reader, &mut self.body, &|| Ok(()))?;
        match reply.message {
            Message::Continue => Ok(()),
            Message::Abort { message } => Err(message),
            _ => Err(FAILED.into()),
        }
    }
}
struct Replies<R, W>(Arc<Mutex<Channel<R, W>>>);
impl<R: Read + Send, W: Write + Send> Pixels for Replies<R, W> {
    fn pixel(&self, layer: &str, x: INT, y: INT) -> Result<[u16; 4]> {
        let mut channel = self.0.lock().map_err(|_| FAILED)?;
        let bytes = channel.words(Message::Point {
            layer: layer.into(),
            point: [x, y],
        })?;
        if bytes.len() != 8 {
            return Err(FAILED.into());
        }
        Ok(std::array::from_fn(|i| {
            u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]])
        }))
    }
    fn patch(&self, layer: &str, rect: [i32; 4]) -> Result<Vec<u16>> {
        let mut channel = self.0.lock().map_err(|_| FAILED)?;
        let bytes = channel.words(Message::Patch {
            layer: layer.into(),
            rect,
        })?;
        let mut words = Vec::new();
        words
            .try_reserve_exact(bytes.len() / 2)
            .map_err(|_| FAILED)?;
        words.extend(
            bytes
                .chunks_exact(2)
                .map(|word| u16::from_le_bytes([word[0], word[1]])),
        );
        Ok(words)
    }
}
pub(in crate::code) fn error_text(error: &str) -> String {
    let mut end = error.len().min(512);
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    let message = error[..end]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    if message.is_empty() {
        FAILED.into()
    } else {
        message
    }
}
pub(super) fn serve<R: Read + Send + 'static, W: Write + Send + 'static>(
    reader: R,
    writer: W,
    expected_build: [u8; 32],
    _permit: &Permit,
) -> Result<()> {
    let mut body = Vec::new();
    body.try_reserve_exact(MAX_BYTES).map_err(|_| FAILED)?;
    body.resize(MAX_BYTES, 0);
    let mut channel = Channel {
        state: State::child(),
        reader,
        writer,
        body,
    };
    let start = channel
        .state
        .receive(&mut channel.reader, &mut channel.body, &|| Ok(()))?;
    let Message::Start {
        config,
        metadata_bytes,
        build,
        ..
    } = start.message
    else {
        return Err(FAILED.into());
    };
    if build != expected_build {
        return Err(FAILED.into());
    }
    let metadata: Metadata =
        serde_json::from_slice(&start.body[..metadata_bytes as usize]).map_err(|_| FAILED)?;
    metadata.validate(config)?;
    let metadata = rhai::serde::to_dynamic(metadata).map_err(|_| FAILED)?;
    let script = std::str::from_utf8(&start.body[metadata_bytes as usize..])
        .map_err(|_| FAILED)?
        .to_owned();
    let channel = Arc::new(Mutex::new(channel));
    let progress = channel.clone();
    let input = Input {
        config,
        metadata,
        pixels: Box::new(Replies(channel.clone())),
        progress: Box::new(move |count| {
            if count % 1024 != 0 {
                return None;
            }
            match progress
                .lock()
                .map_err(|_| FAILED.to_owned())
                .and_then(|mut channel| channel.progress(count))
            {
                Ok(()) => None,
                Err(error) => Some(Dynamic::from(error)),
            }
        }),
    };
    let result = evaluation::evaluate(input, &script, Instant::now());
    let mut channel = channel.lock().map_err(|_| FAILED)?;
    match result {
        Ok(commands) => {
            for (index, command) in commands.iter().enumerate() {
                channel.command(index as u8, command)?;
            }
            let Channel { state, writer, .. } = &mut *channel;
            state.send(
                &Message::Done {
                    commands: commands.len() as u8,
                },
                &[],
                writer,
                &|| Ok(()),
            )
        }
        Err(error) => {
            let Channel { state, writer, .. } = &mut *channel;
            state.send(
                &Message::Failed {
                    message: error_text(&error),
                },
                &[],
                writer,
                &|| Ok(()),
            )
        }
    }
}

#[cfg(test)]
mod tests;
