//! Ordered half-duplex messages. These checks grant no source, task, executable
//! or filesystem authority; those owners remain with the parent broker.
use crate::code::{evaluation::Config, MAX_BYTES, MAX_PIXELS};
use serde::{Deserialize, Serialize};

pub(super) const HEADER_LIMIT: usize = 4096;
pub(super) const METADATA_LIMIT: usize = 256 * 1024;
pub(super) const SCRIPT_LIMIT: usize = 65536;
const FRAME_LIMIT: u64 = 4_000_256;
const FAILED: &str = "Invalid code worker exchange; current work preserved";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    Parent,
    Child,
}
impl Side {
    fn other(self) -> Self {
        match self {
            Self::Parent => Self::Child,
            Self::Child => Self::Parent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum Message {
    Start {
        config: Config,
        metadata_bytes: u32,
        script_bytes: u32,
        build: [u8; 32],
    },
    Point {
        layer: String,
        point: [i64; 2],
    },
    Patch {
        layer: String,
        rect: [i32; 4],
    },
    Progress {
        operations: u64,
    },
    Words {
        words: u32,
    },
    Continue,
    Error {
        message: String,
    },
    Command {
        index: u8,
    },
    Done {
        commands: u8,
    },
    Failed {
        message: String,
    },
    Abort {
        message: String,
    },
}
#[derive(Clone, Copy)]
enum Pending {
    Point,
    Patch(usize),
    Progress,
    Command,
}
#[derive(Clone, Copy)]
enum Phase {
    Start,
    Child,
    Parent(Pending),
    End(bool),
}

#[derive(Clone)]
pub(super) struct State {
    side: Side,
    nonce: Option<[u8; 16]>,
    sequence: u64,
    phase: Phase,
    config: Option<Config>,
    progress: Option<u64>,
    pixels: usize,
    commands: u8,
    command_bytes: usize,
    poisoned: bool,
    eof: bool,
}
impl State {
    pub(super) fn parent(nonce: [u8; 16]) -> Result<Self> {
        if nonce == [0; 16] {
            return Err(FAILED.into());
        }
        let mut state = Self::child();
        state.side = Side::Parent;
        state.nonce = Some(nonce);
        Ok(state)
    }
    pub(super) fn child() -> Self {
        Self {
            side: Side::Child,
            nonce: None,
            sequence: 0,
            phase: Phase::Start,
            config: None,
            progress: None,
            pixels: 0,
            commands: 0,
            command_bytes: 0,
            poisoned: false,
            eof: false,
        }
    }
    fn expected(&self) -> Option<Side> {
        match self.phase {
            Phase::Start | Phase::Parent(_) => Some(Side::Parent),
            Phase::Child => Some(Side::Child),
            Phase::End(_) => None,
        }
    }
    pub(super) fn successful(&self) -> bool {
        self.side == Side::Parent
            && self.eof
            && !self.poisoned
            && matches!(self.phase, Phase::End(true))
    }
    fn plan(
        &self,
        side: Side,
        nonce: [u8; 16],
        sequence: u64,
        message: &Message,
        bytes: usize,
    ) -> Result<Self> {
        if self.poisoned
            || self.expected() != Some(side)
            || sequence != self.sequence
            || sequence >= FRAME_LIMIT
            || nonce == [0; 16]
            || self.nonce.is_some_and(|expected| expected != nonce)
        {
            return Err(FAILED.into());
        }
        let mut next = self.clone();
        next.nonce = Some(nonce);
        next.sequence += 1;
        let config = self.config;
        let text = |value: &str| !value.is_empty() && value.len() <= 2048;
        let layer = |id: &str| !id.is_empty() && id.len() <= 1024;
        next.phase = match (&self.phase, message) {
            (
                Phase::Start,
                Message::Start {
                    config,
                    metadata_bytes,
                    script_bytes,
                    build,
                },
            ) => {
                config.validate()?;
                if *metadata_bytes == 0
                    || *metadata_bytes as usize > METADATA_LIMIT
                    || *script_bytes == 0
                    || *script_bytes as usize > SCRIPT_LIMIT
                    || *build == [0; 32]
                    || bytes != *metadata_bytes as usize + *script_bytes as usize
                {
                    return Err(FAILED.into());
                }
                next.config = Some(*config);
                Phase::Child
            }
            (
                Phase::Child,
                Message::Point {
                    layer: id,
                    point: [x, y],
                },
            ) => {
                let c = config.ok_or(FAILED)?;
                if bytes != 0
                    || !layer(id)
                    || *x < 0
                    || *y < 0
                    || *x >= i64::from(c.width)
                    || *y >= i64::from(c.height)
                {
                    return Err(FAILED.into());
                }
                Phase::Parent(Pending::Point)
            }
            (Phase::Child, Message::Patch { layer: id, rect: r }) => {
                let c = config.ok_or(FAILED)?;
                if bytes != 0
                    || !layer(id)
                    || r[0] < 0
                    || r[1] < 0
                    || r[0] >= r[2]
                    || r[1] >= r[3]
                    || r[2] > c.width as i32
                    || r[3] > c.height as i32
                {
                    return Err(FAILED.into());
                }
                let count = (r[2] - r[0]) as usize * (r[3] - r[1]) as usize;
                if count > MAX_PIXELS - self.pixels {
                    return Err(FAILED.into());
                }
                Phase::Parent(Pending::Patch(count))
            }
            (Phase::Child, Message::Progress { operations }) => {
                if bytes != 0
                    || *operations > 2_000_000
                    || self.progress.is_some_and(|old| *operations <= old)
                {
                    return Err(FAILED.into());
                }
                next.progress = Some(*operations);
                Phase::Parent(Pending::Progress)
            }
            (Phase::Parent(pending), Message::Words { words }) => {
                let expected = match pending {
                    Pending::Point => 4,
                    Pending::Patch(count) => count * 4,
                    _ => return Err(FAILED.into()),
                };
                if *words as usize != expected || bytes != expected * 2 {
                    return Err(FAILED.into());
                }
                if let Pending::Patch(count) = pending {
                    next.pixels += *count;
                }
                Phase::Child
            }
            (Phase::Parent(Pending::Progress | Pending::Command), Message::Continue)
                if bytes == 0 =>
            {
                Phase::Child
            }
            (Phase::Parent(Pending::Point | Pending::Patch(_)), Message::Error { message })
                if bytes == 0 && text(message) =>
            {
                Phase::Child
            }
            (Phase::Parent(_), Message::Abort { message }) if bytes == 0 && text(message) => {
                Phase::End(false)
            }
            (Phase::Child, Message::Command { index }) => {
                if *index != self.commands
                    || self.commands >= 32
                    || bytes == 0
                    || bytes > MAX_BYTES - self.command_bytes
                {
                    return Err(FAILED.into());
                }
                next.commands += 1;
                next.command_bytes += bytes;
                Phase::Parent(Pending::Command)
            }
            (Phase::Child, Message::Done { commands })
                if bytes == 0 && *commands == self.commands && *commands > 0 =>
            {
                Phase::End(true)
            }
            (Phase::Child, Message::Failed { message }) if bytes == 0 && text(message) => {
                Phase::End(false)
            }
            _ => return Err(FAILED.into()),
        };
        Ok(next)
    }
}

#[cfg(test)]
mod tests;
mod wire;
