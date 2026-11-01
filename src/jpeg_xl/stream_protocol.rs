//! Fixed-frame protocol for the separate, compatible mode1 encoder operation.
//! Framing proves transport only. SourceSession ownership, byte-source admission,
//! segment coverage, canonical replay and publication remain parent-owned.
use super::{Info, Options};
use ring::digest::{Context, SHA256};
use std::io::{self, Read, Write};

pub(crate) type Digest = [u8; 32];
pub(crate) const BUFFER_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RECT_BYTES: u64 = 2048 * 2048 * 8;
pub(crate) const MAX_HELD_BYTES: u32 = 128 * 1024 * 1024;
pub(crate) const MAX_HELD_BUFFERS: u16 = 8;
pub(crate) const MAX_RECORDS: u32 = 131072;
pub(crate) const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub(crate) const MAX_ELAPSED_MS: u32 = 20 * 60 * 1000;
const MAGIC: &[u8; 8] = b"PBJXLS01";
const HEADER_BYTES: usize = 80;
const CONFIG_BYTES: usize = 224;
const ERROR: &str = "Invalid JPEG XL streaming transport";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Configuration {
    pub info: Info,
    pub options: Options,
    pub profile_bytes: u32,
    pub raw_bytes: u64,
    pub max_output_bytes: u64,
    pub max_log_bytes: u64,
    pub max_records: u32,
    pub max_held_bytes: u32,
    pub max_held_buffers: u16,
    pub max_elapsed_ms: u32,
    pub source_binding: Digest,
    pub output_binding: Digest,
    pub raw_sha256: Digest,
    pub profile_sha256: Digest,
    pub build_sha256: Digest,
}
impl Configuration {
    pub(crate) fn validate(&self) -> Result<()> {
        self.options.validate()?;
        let info = self.info;
        let raw = u64::from(info.width)
            .checked_mul(u64::from(info.height))
            .and_then(|n| n.checked_mul(4 * u64::from(info.bit_depth / 8)));
        if info.width == 0
            || info.height == 0
            || info.width > 16000
            || info.height > 16000
            || ![8, 16].contains(&info.bit_depth)
            || raw != Some(self.raw_bytes)
            || self.profile_bytes == 0
            || self.profile_bytes > 4 * 1024 * 1024
            || self.max_output_bytes == 0
            || self.max_output_bytes > MAX_OUTPUT_BYTES
            || self.max_log_bytes == 0
            || self.max_log_bytes > MAX_OUTPUT_BYTES
            || self.max_records == 0
            || self.max_records > MAX_RECORDS
            || self.max_held_bytes == 0
            || self.max_held_bytes > MAX_HELD_BYTES
            || self.max_held_buffers == 0
            || self.max_held_buffers > MAX_HELD_BUFFERS
            || self.max_elapsed_ms == 0
            || self.max_elapsed_ms > MAX_ELAPSED_MS
        {
            return Err(ERROR.into());
        }
        Ok(())
    }
    fn encode(&self) -> [u8; CONFIG_BYTES] {
        let mut bytes = [0; CONFIG_BYTES];
        bytes[0..4].copy_from_slice(&self.info.width.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.info.height.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.info.bit_depth.to_le_bytes());
        bytes[10] = u8::from(self.options.lossless);
        bytes[12..16].copy_from_slice(&self.options.distance.to_bits().to_le_bytes());
        bytes[16..20].copy_from_slice(&self.options.effort.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.profile_bytes.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.raw_bytes.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.max_output_bytes.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.max_log_bytes.to_le_bytes());
        bytes[48..52].copy_from_slice(&self.max_records.to_le_bytes());
        bytes[52..56].copy_from_slice(&self.max_held_bytes.to_le_bytes());
        bytes[56..58].copy_from_slice(&self.max_held_buffers.to_le_bytes());
        bytes[60..64].copy_from_slice(&self.max_elapsed_ms.to_le_bytes());
        for (i, digest) in [
            self.source_binding,
            self.output_binding,
            self.raw_sha256,
            self.profile_sha256,
            self.build_sha256,
        ]
        .iter()
        .enumerate()
        {
            bytes[64 + i * 32..96 + i * 32].copy_from_slice(digest);
        }
        bytes
    }
    fn decode(bytes: &[u8; CONFIG_BYTES]) -> Result<Self> {
        if bytes[10] > 1 || bytes[11] != 0 || bytes[58..60] != [0; 2] {
            return Err(ERROR.into());
        }
        let value = Self {
            info: Info {
                width: u32le(&bytes[0..4]),
                height: u32le(&bytes[4..8]),
                bit_depth: u16le(&bytes[8..10]),
            },
            options: Options {
                lossless: bytes[10] != 0,
                distance: f32::from_bits(u32le(&bytes[12..16])),
                effort: u32le(&bytes[16..20]),
            },
            profile_bytes: u32le(&bytes[20..24]),
            raw_bytes: u64le(&bytes[24..32]),
            max_output_bytes: u64le(&bytes[32..40]),
            max_log_bytes: u64le(&bytes[40..48]),
            max_records: u32le(&bytes[48..52]),
            max_held_bytes: u32le(&bytes[52..56]),
            max_held_buffers: u16le(&bytes[56..58]),
            max_elapsed_ms: u32le(&bytes[60..64]),
            source_binding: bytes[64..96].try_into().unwrap(),
            output_binding: bytes[96..128].try_into().unwrap(),
            raw_sha256: bytes[128..160].try_into().unwrap(),
            profile_sha256: bytes[160..192].try_into().unwrap(),
            build_sha256: bytes[192..224].try_into().unwrap(),
        };
        value.validate()?;
        Ok(value)
    }
    pub(crate) fn binding(&self, nonce: [u8; 16]) -> Result<Digest> {
        self.validate()?;
        let mut hash = Context::new(&SHA256);
        hash.update(b"PeerBrushJxlStreamingV1\0");
        hash.update(&nonce);
        hash.update(&self.encode());
        Ok(hash.finish().as_ref().try_into().unwrap())
    }
}
fn u16le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().unwrap())
}
fn u32le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}
fn u64le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rectangle {
    pub id: u64,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Rectangle {
    pub(crate) fn bytes(self, configuration: &Configuration) -> Result<u64> {
        if self.id == 0
            || self.width == 0
            || self.height == 0
            || self.width > 2048
            || self.height > 2048
            || self.x % 8 != 0
            || self.y % 8 != 0
            || self
                .x
                .checked_add(self.width)
                .is_none_or(|n| n > configuration.info.width)
            || self
                .y
                .checked_add(self.height)
                .is_none_or(|n| n > configuration.info.height)
            || (self.width % 8 != 0 && self.x + self.width != configuration.info.width)
            || (self.height % 8 != 0 && self.y + self.height != configuration.info.height)
        {
            return Err(ERROR.into());
        }
        let count = u64::from(self.width)
            * u64::from(self.height)
            * 4
            * u64::from(configuration.info.bit_depth / 8);
        if count > MAX_RECT_BYTES {
            return Err(ERROR.into());
        }
        Ok(count)
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Message {
    Start(Configuration),
    Ready,
    NeedRectangle(Rectangle),
    RectangleData {
        id: u64,
        bytes: u64,
        sha256: Digest,
    },
    Release {
        id: u64,
    },
    Segment {
        offset: u64,
        bytes: u32,
        sha256: Digest,
    },
    Seek {
        position: u64,
    },
    Finalized {
        position: u64,
    },
    Continue {
        request: u64,
    },
    Done {
        bytes: u64,
        records: u32,
    },
    Failure {
        code: u16,
    },
    Cancel {
        code: u16,
    },
}
impl Message {
    fn kind(self) -> u8 {
        match self {
            Self::Start(_) => 1,
            Self::Ready => 2,
            Self::NeedRectangle(_) => 3,
            Self::RectangleData { .. } => 4,
            Self::Release { .. } => 5,
            Self::Segment { .. } => 6,
            Self::Seek { .. } => 7,
            Self::Finalized { .. } => 8,
            Self::Continue { .. } => 9,
            Self::Done { .. } => 10,
            Self::Failure { .. } => 11,
            Self::Cancel { .. } => 12,
        }
    }
    fn prefix(self) -> ([u8; CONFIG_BYTES], usize) {
        let mut p = [0; CONFIG_BYTES];
        let n = match self {
            Self::Start(c) => {
                p = c.encode();
                CONFIG_BYTES
            }
            Self::Ready => 0,
            Self::NeedRectangle(r) => {
                p[..8].copy_from_slice(&r.id.to_le_bytes());
                for (i, n) in [r.x, r.y, r.width, r.height].iter().enumerate() {
                    p[8 + i * 4..12 + i * 4].copy_from_slice(&n.to_le_bytes());
                }
                24
            }
            Self::RectangleData { id, bytes, sha256 } => {
                p[..8].copy_from_slice(&id.to_le_bytes());
                p[8..16].copy_from_slice(&bytes.to_le_bytes());
                p[16..48].copy_from_slice(&sha256);
                48
            }
            Self::Segment {
                offset,
                bytes,
                sha256,
            } => {
                p[..8].copy_from_slice(&offset.to_le_bytes());
                p[8..12].copy_from_slice(&bytes.to_le_bytes());
                p[16..48].copy_from_slice(&sha256);
                48
            }
            Self::Release { id } => {
                p[..8].copy_from_slice(&id.to_le_bytes());
                8
            }
            Self::Seek { position } | Self::Finalized { position } => {
                p[..8].copy_from_slice(&position.to_le_bytes());
                8
            }
            Self::Continue { request } => {
                p[..8].copy_from_slice(&request.to_le_bytes());
                8
            }
            Self::Done { bytes, records } => {
                p[..8].copy_from_slice(&bytes.to_le_bytes());
                p[8..12].copy_from_slice(&records.to_le_bytes());
                16
            }
            Self::Failure { code } | Self::Cancel { code } => {
                p[..2].copy_from_slice(&code.to_le_bytes());
                8
            }
        };
        (p, n)
    }
    pub(crate) fn payload(self) -> (u64, Option<Digest>) {
        match self {
            Self::Start(c) => (u64::from(c.profile_bytes), Some(c.profile_sha256)),
            Self::RectangleData { bytes, sha256, .. } => (bytes, Some(sha256)),
            Self::Segment { bytes, sha256, .. } => (u64::from(bytes), Some(sha256)),
            _ => (0, None),
        }
    }
    fn bounded(self) -> Result<()> {
        match self {
            Self::Start(c) => c.validate(),
            Self::RectangleData { id, bytes, .. }
                if id == 0 || bytes == 0 || bytes > MAX_RECT_BYTES =>
            {
                Err(ERROR.into())
            }
            Self::Segment { bytes, .. } if bytes == 0 || bytes as usize > BUFFER_BYTES => {
                Err(ERROR.into())
            }
            Self::Failure { code } | Self::Cancel { code } if !(1..=4).contains(&code) => {
                Err(ERROR.into())
            }
            _ => Ok(()),
        }
    }
}
fn prefix_bytes(kind: u8) -> Result<usize> {
    match kind {
        1 => Ok(CONFIG_BYTES),
        2 => Ok(0),
        3 => Ok(24),
        4 | 6 => Ok(48),
        5 | 7 | 8 | 9 | 11 | 12 => Ok(8),
        10 => Ok(16),
        _ => Err(ERROR.into()),
    }
}
fn parse(kind: u8, p: &[u8; CONFIG_BYTES]) -> Result<Message> {
    let message = match kind {
        1 => Message::Start(Configuration::decode(p)?),
        2 => Message::Ready,
        3 => Message::NeedRectangle(Rectangle {
            id: u64le(&p[..8]),
            x: u32le(&p[8..12]),
            y: u32le(&p[12..16]),
            width: u32le(&p[16..20]),
            height: u32le(&p[20..24]),
        }),
        4 => Message::RectangleData {
            id: u64le(&p[..8]),
            bytes: u64le(&p[8..16]),
            sha256: p[16..48].try_into().unwrap(),
        },
        5 => Message::Release { id: u64le(&p[..8]) },
        6 => {
            if p[12..16] != [0; 4] {
                return Err(ERROR.into());
            }
            Message::Segment {
                offset: u64le(&p[..8]),
                bytes: u32le(&p[8..12]),
                sha256: p[16..48].try_into().unwrap(),
            }
        }
        7 => Message::Seek {
            position: u64le(&p[..8]),
        },
        8 => Message::Finalized {
            position: u64le(&p[..8]),
        },
        9 => Message::Continue {
            request: u64le(&p[..8]),
        },
        10 => {
            if p[12..16] != [0; 4] {
                return Err(ERROR.into());
            }
            Message::Done {
                bytes: u64le(&p[..8]),
                records: u32le(&p[8..12]),
            }
        }
        11 | 12 => {
            if p[2..8] != [0; 6] {
                return Err(ERROR.into());
            }
            if kind == 11 {
                Message::Failure {
                    code: u16le(&p[..2]),
                }
            } else {
                Message::Cancel {
                    code: u16le(&p[..2]),
                }
            }
        }
        _ => return Err(ERROR.into()),
    };
    message.bounded()?;
    Ok(message)
}

pub(crate) struct Frame {
    nonce: [u8; 16],
    binding: Digest,
    sequence: u64,
    message: Message,
}
pub(crate) struct VerifiedFrame(Frame);
impl Frame {
    pub(crate) fn message(&self) -> Message {
        self.message
    }
    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }
    pub(crate) fn nonce(&self) -> [u8; 16] {
        self.nonce
    }
    pub(crate) fn read(input: &mut dyn Read, check: &dyn Fn() -> Result<()>) -> Result<Self> {
        let mut h = [0; HEADER_BYTES];
        read_exact(input, &mut h, check)?;
        if &h[..8] != MAGIC || h[9..16] != [0; 7] || h[76..80] != [0; 4] {
            return Err(ERROR.into());
        }
        let prefix = prefix_bytes(h[8])?;
        let length = u32le(&h[72..76]) as u64;
        if length < prefix as u64 || length > MAX_RECT_BYTES + 48 {
            return Err(ERROR.into());
        }
        let mut p = [0; CONFIG_BYTES];
        read_exact(input, &mut p[..prefix], check)?;
        let message = parse(h[8], &p)?;
        if length != prefix as u64 + message.payload().0 {
            return Err(ERROR.into());
        }
        Ok(Self {
            nonce: h[16..32].try_into().unwrap(),
            sequence: u64le(&h[32..40]),
            binding: h[40..72].try_into().unwrap(),
            message,
        })
    }
    /// Bulk callbacks may stage only provisional bytes. Preflight the frame with
    /// State first, then accept this opaque completion only after hash validation.
    /// `scratch` must already carry the caller's real resident-pool admission.
    pub(crate) fn complete(
        self,
        input: &mut dyn Read,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<()>,
        consume: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<VerifiedFrame> {
        copy_payload(input, scratch, self.message.payload(), check, consume)?;
        Ok(VerifiedFrame(self))
    }
}
pub(crate) fn write_frame(
    output: &mut dyn Write,
    nonce: [u8; 16],
    binding: Digest,
    sequence: u64,
    message: Message,
    payload: &mut dyn Read,
    scratch: &mut [u8],
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    message.bounded()?;
    let (prefix, n) = message.prefix();
    let count = message.payload();
    let length = u32::try_from(n as u64 + count.0).map_err(|_| ERROR)?;
    let mut h = [0; HEADER_BYTES];
    h[..8].copy_from_slice(MAGIC);
    h[8] = message.kind();
    h[16..32].copy_from_slice(&nonce);
    h[32..40].copy_from_slice(&sequence.to_le_bytes());
    h[40..72].copy_from_slice(&binding);
    h[72..76].copy_from_slice(&length.to_le_bytes());
    write_all(output, &h, check)?;
    write_all(output, &prefix[..n], check)?;
    copy_payload(payload, scratch, count, check, &mut |bytes| {
        write_all(output, bytes, check)
    })?;
    check()?;
    output.flush().map_err(|_| ERROR.to_string())?;
    check()
}
fn copy_payload(
    input: &mut dyn Read,
    scratch: &mut [u8],
    (mut count, expected): (u64, Option<Digest>),
    check: &dyn Fn() -> Result<()>,
    consume: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    if scratch.is_empty() || scratch.len() > BUFFER_BYTES {
        return Err(ERROR.into());
    }
    let mut hash = Context::new(&SHA256);
    while count != 0 {
        let take = (count.min(scratch.len() as u64)) as usize;
        read_exact(input, &mut scratch[..take], check)?;
        hash.update(&scratch[..take]);
        consume(&scratch[..take])?;
        check()?;
        count -= take as u64;
    }
    if expected.is_some_and(|expected| hash.finish().as_ref() != expected) {
        return Err(ERROR.into());
    }
    check()
}
fn read_exact(
    input: &mut dyn Read,
    mut bytes: &mut [u8],
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    while !bytes.is_empty() {
        check()?;
        let n = input.read(bytes).map_err(|_| ERROR.to_string())?;
        if n == 0 || n > bytes.len() {
            return Err(ERROR.into());
        }
        bytes = &mut bytes[n..];
    }
    check()
}
fn write_all(
    output: &mut dyn Write,
    mut bytes: &[u8],
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    while !bytes.is_empty() {
        check()?;
        let n = output.write(bytes).map_err(|_| ERROR.to_string())?;
        if n == 0 || n > bytes.len() {
            return Err(ERROR.into());
        }
        bytes = &bytes[n..];
    }
    check()
}
pub(crate) fn exact_eof(input: &mut dyn Read, check: &dyn Fn() -> Result<()>) -> Result<()> {
    check()?;
    let mut b = [0];
    if input.read(&mut b).map_err(|_| ERROR.to_string())? != 0 {
        return Err(ERROR.into());
    }
    check()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Parent,
    Child,
}
#[derive(Clone, Copy)]
enum Turn {
    Start,
    Ready,
    Child,
    Ack(u64),
    Data(Rectangle),
    Terminal,
}
#[derive(Clone)]
pub(crate) struct State {
    configuration: Configuration,
    nonce: [u8; 16],
    binding: Digest,
    next: [u64; 2],
    turn: Turn,
    held: [Option<(u64, u64)>; MAX_HELD_BUFFERS as usize],
    held_bytes: u64,
    last_id: u64,
    position: u64,
    highest: u64,
    finalized: u64,
    records: u32,
    log_bytes: u64,
    failed: bool,
    success: bool,
}
impl State {
    pub(crate) fn new(configuration: Configuration, nonce: [u8; 16]) -> Result<Self> {
        let binding = configuration.binding(nonce)?;
        Ok(Self {
            configuration,
            nonce,
            binding,
            next: [0; 2],
            turn: Turn::Start,
            held: [None; MAX_HELD_BUFFERS as usize],
            held_bytes: 0,
            last_id: 0,
            position: 0,
            highest: 0,
            finalized: 0,
            records: 0,
            log_bytes: 0,
            failed: false,
            success: false,
        })
    }
    pub(crate) fn binding(&self) -> Digest {
        self.binding
    }
    pub(crate) fn next_sequence(&self, direction: Direction) -> u64 {
        self.next[usize::from(direction == Direction::Child)]
    }
    pub(crate) fn successful(&self) -> bool {
        self.success && !self.failed
    }
    /// All transport errors latch the operation. Retrying an uncertain frame on
    /// the same pipes is forbidden; a new operation needs fresh ownership/nonce.
    pub(crate) fn receive(
        &mut self,
        direction: Direction,
        input: &mut dyn Read,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<()>,
        consume: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<Message> {
        let result = (|| {
            let frame = Frame::read(input, check)?;
            self.complete(direction, frame, input, scratch, check, consume)
        })();
        if result.is_err() {
            self.failed = true;
            self.success = false;
        }
        result
    }
    pub(crate) fn complete(
        &mut self,
        direction: Direction,
        frame: Frame,
        input: &mut dyn Read,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<()>,
        consume: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<Message> {
        let result = (|| {
            self.preflight(direction, &frame)?;
            let message = frame.message;
            let verified = frame.complete(input, scratch, check, consume)?;
            self.accept(direction, verified)?;
            Ok(message)
        })();
        if result.is_err() {
            self.failed = true;
            self.success = false;
        }
        result
    }
    pub(crate) fn send(
        &mut self,
        direction: Direction,
        message: Message,
        output: &mut dyn Write,
        payload: &mut dyn Read,
        scratch: &mut [u8],
        check: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let result = (|| {
            let sequence = self.next_sequence(direction);
            let frame = Frame {
                nonce: self.nonce,
                binding: self.binding,
                sequence,
                message,
            };
            self.preflight(direction, &frame)?;
            write_frame(
                output,
                self.nonce,
                self.binding,
                sequence,
                message,
                payload,
                scratch,
                check,
            )?;
            self.accept(direction, VerifiedFrame(frame))
        })();
        if result.is_err() {
            self.failed = true;
            self.success = false;
        }
        result
    }
    pub(crate) fn preflight(&mut self, direction: Direction, frame: &Frame) -> Result<()> {
        if self.failed {
            return Err(ERROR.into());
        }
        let result = self.clone().transition(direction, frame);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub(crate) fn accept(&mut self, direction: Direction, frame: VerifiedFrame) -> Result<()> {
        if self.failed {
            return Err(ERROR.into());
        }
        let result = self.transition(direction, &frame.0);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn transition(&mut self, direction: Direction, frame: &Frame) -> Result<()> {
        let index = usize::from(direction == Direction::Child);
        if matches!(self.turn, Turn::Terminal)
            || frame.nonce != self.nonce
            || frame.binding != self.binding
            || frame.sequence != self.next[index]
        {
            return Err(ERROR.into());
        }
        let next = self.next[index].checked_add(1).ok_or(ERROR)?;
        let message = frame.message;
        message.bounded()?;
        if matches!(message, Message::Cancel { .. })
            && direction == Direction::Parent
            && !matches!(self.turn, Turn::Start)
        {
            self.turn = Turn::Terminal;
            self.failed = true;
            self.next[index] = next;
            return Ok(());
        }
        if matches!(message, Message::Failure { .. })
            && direction == Direction::Child
            && !matches!(self.turn, Turn::Start)
        {
            self.turn = Turn::Terminal;
            self.failed = true;
            self.next[index] = next;
            return Ok(());
        }
        self.turn = match (self.turn, direction, message) {
            (Turn::Start, Direction::Parent, Message::Start(c))
                if c.encode() == self.configuration.encode() =>
            {
                Turn::Ready
            }
            (Turn::Ready, Direction::Child, Message::Ready) => Turn::Ack(frame.sequence),
            (Turn::Ack(request), Direction::Parent, Message::Continue { request: received })
                if request == received =>
            {
                Turn::Child
            }
            (Turn::Child, Direction::Child, Message::NeedRectangle(rectangle)) => {
                let bytes = rectangle.bytes(&self.configuration)?;
                if rectangle.id != self.last_id.checked_add(1).ok_or(ERROR)?
                    || self.held.iter().filter(|n| n.is_some()).count()
                        >= usize::from(self.configuration.max_held_buffers)
                    || self
                        .held_bytes
                        .checked_add(bytes)
                        .is_none_or(|n| n > u64::from(self.configuration.max_held_bytes))
                {
                    return Err(ERROR.into());
                }
                Turn::Data(rectangle)
            }
            (
                Turn::Data(rectangle),
                Direction::Parent,
                Message::RectangleData { id, bytes, .. },
            ) if id == rectangle.id && bytes == rectangle.bytes(&self.configuration)? => {
                let slot = self.held.iter_mut().find(|n| n.is_none()).ok_or(ERROR)?;
                *slot = Some((id, bytes));
                self.held_bytes = self.held_bytes.checked_add(bytes).ok_or(ERROR)?;
                self.last_id = id;
                Turn::Child
            }
            (Turn::Child, Direction::Child, Message::Release { id }) => {
                let slot = self
                    .held
                    .iter_mut()
                    .find(|n| n.is_some_and(|(held, _)| held == id))
                    .ok_or(ERROR)?;
                let (_, bytes) = slot.take().ok_or(ERROR)?;
                self.held_bytes = self.held_bytes.checked_sub(bytes).ok_or(ERROR)?;
                Turn::Ack(frame.sequence)
            }
            (Turn::Child, Direction::Child, Message::Segment { offset, bytes, .. }) => {
                let end = offset.checked_add(u64::from(bytes)).ok_or(ERROR)?;
                if offset != self.position
                    || offset < self.finalized
                    || end > self.configuration.max_output_bytes
                {
                    return Err(ERROR.into());
                }
                self.records = self.records.checked_add(1).ok_or(ERROR)?;
                self.log_bytes = self
                    .log_bytes
                    .checked_add(u64::from(bytes) + 48)
                    .ok_or(ERROR)?;
                if self.records > self.configuration.max_records
                    || self.log_bytes > self.configuration.max_log_bytes
                {
                    return Err(ERROR.into());
                }
                self.position = end;
                self.highest = self.highest.max(end);
                Turn::Ack(frame.sequence)
            }
            (Turn::Child, Direction::Child, Message::Seek { position }) => {
                if position < self.finalized || position > self.configuration.max_output_bytes {
                    return Err(ERROR.into());
                }
                self.position = position;
                Turn::Ack(frame.sequence)
            }
            (Turn::Child, Direction::Child, Message::Finalized { position }) => {
                if position < self.finalized || position > self.highest {
                    return Err(ERROR.into());
                }
                self.finalized = position;
                Turn::Ack(frame.sequence)
            }
            (Turn::Child, Direction::Child, Message::Done { bytes, records }) => {
                if self.held_bytes != 0
                    || self.held.iter().any(Option::is_some)
                    || bytes == 0
                    || bytes != self.highest
                    || bytes != self.finalized
                    || records != self.records
                {
                    return Err(ERROR.into());
                }
                self.success = true;
                Turn::Terminal
            }
            _ => return Err(ERROR.into()),
        };
        self.next[index] = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io::Cursor};
    fn digest(bytes: &[u8]) -> Digest {
        ring::digest::digest(&SHA256, bytes)
            .as_ref()
            .try_into()
            .unwrap()
    }
    fn configuration() -> Configuration {
        Configuration {
            info: Info {
                width: 64,
                height: 16,
                bit_depth: 16,
            },
            options: Options::default(),
            profile_bytes: 132,
            raw_bytes: 64 * 16 * 8,
            max_output_bytes: 1024 * 1024,
            max_log_bytes: 2 * 1024 * 1024,
            max_records: 128,
            max_held_bytes: 4096,
            max_held_buffers: 2,
            max_elapsed_ms: 120000,
            source_binding: [2; 32],
            output_binding: [3; 32],
            raw_sha256: [4; 32],
            profile_sha256: digest(&[5; 132]),
            build_sha256: [6; 32],
        }
    }
    fn frame(state: &State, direction: Direction, message: Message) -> Frame {
        Frame {
            nonce: state.nonce,
            binding: state.binding,
            sequence: state.next_sequence(direction),
            message,
        }
    }
    fn deliver(
        a: &mut State,
        b: &mut State,
        direction: Direction,
        message: Message,
        payload: &[u8],
    ) {
        let mut wire = vec![];
        let mut scratch = [0; 127];
        a.send(
            direction,
            message,
            &mut wire,
            &mut Cursor::new(payload),
            &mut scratch,
            &|| Ok(()),
        )
        .unwrap();
        let mut input = Cursor::new(wire);
        let mut received = vec![];
        assert_eq!(
            b.receive(
                direction,
                &mut input,
                &mut scratch,
                &|| Ok(()),
                &mut |bytes| {
                    received.extend_from_slice(bytes);
                    Ok(())
                }
            )
            .unwrap(),
            message
        );
        assert_eq!(received, payload);
        exact_eof(&mut input, &|| Ok(())).unwrap();
    }
    fn handshake() -> (State, State) {
        let config = configuration();
        let mut a = State::new(config, [7; 16]).unwrap();
        let mut b = a.clone();
        deliver(
            &mut a,
            &mut b,
            Direction::Parent,
            Message::Start(config),
            &[5; 132],
        );
        deliver(&mut a, &mut b, Direction::Child, Message::Ready, &[]);
        deliver(
            &mut a,
            &mut b,
            Direction::Parent,
            Message::Continue { request: 0 },
            &[],
        );
        (a, b)
    }
    fn ack(a: &mut State, b: &mut State) {
        let request = a.next_sequence(Direction::Child) - 1;
        deliver(a, b, Direction::Parent, Message::Continue { request }, &[]);
    }
    #[test]
    fn full_binary_operation_roundtrips_multiple_held_rectangles_seek_segments_and_exact_eof() {
        let (mut a, mut b) = handshake();
        for id in 1..=2 {
            let rect = Rectangle {
                id,
                x: (id as u32 - 1) * 8,
                y: 0,
                width: 8,
                height: 8,
            };
            deliver(
                &mut a,
                &mut b,
                Direction::Child,
                Message::NeedRectangle(rect),
                &[],
            );
            let bytes = [id as u8; 512];
            deliver(
                &mut a,
                &mut b,
                Direction::Parent,
                Message::RectangleData {
                    id,
                    bytes: 512,
                    sha256: digest(&bytes),
                },
                &bytes,
            );
        }
        for id in [2, 1] {
            deliver(
                &mut a,
                &mut b,
                Direction::Child,
                Message::Release { id },
                &[],
            );
            ack(&mut a, &mut b);
        }
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Seek { position: 8 },
            &[],
        );
        ack(&mut a, &mut b);
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Segment {
                offset: 8,
                bytes: 4,
                sha256: digest(&[8; 4]),
            },
            &[8; 4],
        );
        ack(&mut a, &mut b);
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Seek { position: 0 },
            &[],
        );
        ack(&mut a, &mut b);
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Segment {
                offset: 0,
                bytes: 8,
                sha256: digest(&[9; 8]),
            },
            &[9; 8],
        );
        ack(&mut a, &mut b);
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Finalized { position: 12 },
            &[],
        );
        ack(&mut a, &mut b);
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Done {
                bytes: 12,
                records: 2,
            },
            &[],
        );
        assert!(a.successful() && b.successful());
        assert!(a
            .preflight(
                Direction::Child,
                &frame(&a, Direction::Child, Message::Ready)
            )
            .is_err());
        assert!(!a.successful());
        assert!(exact_eof(&mut Cursor::new([0]), &|| Ok(())).is_err());
    }
    #[test]
    fn malformed_reserved_nonce_binding_sequence_lengths_and_hashes_fail_closed_before_authority() {
        let config = configuration();
        let fresh = State::new(config, [7; 16]).unwrap();
        let mut wire = vec![];
        write_frame(
            &mut wire,
            fresh.nonce,
            fresh.binding,
            0,
            Message::Start(config),
            &mut Cursor::new([5; 132]),
            &mut [0; 64],
            &|| Ok(()),
        )
        .unwrap();
        for at in [
            0,
            8,
            9,
            16,
            32,
            40,
            72,
            76,
            HEADER_BYTES + 11,
            HEADER_BYTES + 58,
            HEADER_BYTES + CONFIG_BYTES,
        ] {
            let mut damaged = wire.clone();
            damaged[at] ^= 0x80;
            let mut state = fresh.clone();
            assert!(
                state
                    .receive(
                        Direction::Parent,
                        &mut Cursor::new(damaged),
                        &mut [0; 64],
                        &|| Ok(()),
                        &mut |_| Ok(())
                    )
                    .is_err(),
                "byte{at}"
            );
            assert!(state
                .receive(
                    Direction::Parent,
                    &mut Cursor::new(&wire),
                    &mut [0; 64],
                    &|| Ok(()),
                    &mut |_| Ok(())
                )
                .is_err());
        }
        for end in [
            0,
            1,
            79,
            80,
            HEADER_BYTES + CONFIG_BYTES - 1,
            wire.len() - 1,
        ] {
            let mut state = fresh.clone();
            assert!(state
                .receive(
                    Direction::Parent,
                    &mut Cursor::new(&wire[..end]),
                    &mut [0; 64],
                    &|| Ok(()),
                    &mut |_| Ok(())
                )
                .is_err());
        }
    }
    #[test]
    fn declared_16000_and_callback_bounds_are_offset_proofs_without_dense_allocation() {
        let mut c = configuration();
        c.info = Info {
            width: 16000,
            height: 16000,
            bit_depth: 16,
        };
        c.raw_bytes = 2_048_000_000;
        c.max_held_bytes = MAX_HELD_BYTES;
        c.max_held_buffers = MAX_HELD_BUFFERS;
        c.max_elapsed_ms = MAX_ELAPSED_MS;
        c.validate().unwrap();
        let encoded = c.encode();
        assert_eq!(Configuration::decode(&encoded).unwrap(), c);
        let rect = Rectangle {
            id: 1,
            x: 0,
            y: 0,
            width: 2048,
            height: 2048,
        };
        assert_eq!(rect.bytes(&c).unwrap(), MAX_RECT_BYTES);
        for rect in [
            Rectangle {
                width: 2049,
                ..rect
            },
            Rectangle {
                x: u32::MAX,
                ..rect
            },
            Rectangle { x: 1, ..rect },
            Rectangle { height: 1, ..rect },
            Rectangle { id: 0, ..rect },
        ] {
            assert!(rect.bytes(&c).is_err());
        }
        let edge = Rectangle {
            id: 1,
            x: 15992,
            y: 15992,
            width: 8,
            height: 8,
        };
        assert!(edge.bytes(&c).is_ok());
        for field in 0..9 {
            let mut bad = c;
            match field {
                0 => bad.raw_bytes -= 1,
                1 => bad.info.width += 1,
                2 => bad.max_elapsed_ms += 1,
                3 => bad.max_records = MAX_RECORDS + 1,
                4 => bad.max_output_bytes = MAX_OUTPUT_BYTES + 1,
                5 => bad.max_held_bytes += 1,
                6 => bad.max_held_buffers += 1,
                7 => bad.profile_bytes = 4 * 1024 * 1024 + 1,
                _ => bad.options.distance = f32::NAN,
            }
            assert!(bad.validate().is_err());
        }
    }
    #[test]
    fn held_registry_reuse_wrong_release_ack_and_finalized_rewrites_refuse() {
        let (a, b) = handshake();
        for message in [
            Message::Release { id: 1 },
            Message::Continue { request: 0 },
            Message::Done {
                bytes: 1,
                records: 0,
            },
            Message::NeedRectangle(Rectangle {
                id: 2,
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            }),
        ] {
            let mut state = a.clone();
            assert!(state
                .preflight(Direction::Child, &frame(&state, Direction::Child, message))
                .is_err());
        }
        let mut a = a;
        let mut b = b;
        let rect = Rectangle {
            id: 1,
            x: 0,
            y: 0,
            width: 16,
            height: 8,
        };
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::NeedRectangle(rect),
            &[],
        );
        let mut bad = a.clone();
        assert!(bad
            .preflight(
                Direction::Parent,
                &frame(
                    &bad,
                    Direction::Parent,
                    Message::RectangleData {
                        id: 2,
                        bytes: 1024,
                        sha256: [0; 32]
                    }
                )
            )
            .is_err());
        deliver(
            &mut a,
            &mut b,
            Direction::Parent,
            Message::RectangleData {
                id: 1,
                bytes: 1024,
                sha256: digest(&[0; 1024]),
            },
            &[0; 1024],
        );
        let mut bad = a.clone();
        assert!(bad
            .preflight(
                Direction::Child,
                &frame(&bad, Direction::Child, Message::NeedRectangle(rect))
            )
            .is_err());
        let mut bad = a.clone();
        assert!(bad
            .preflight(
                Direction::Child,
                &frame(
                    &bad,
                    Direction::Child,
                    Message::Done {
                        bytes: 1,
                        records: 0
                    }
                )
            )
            .is_err());
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Release { id: 1 },
            &[],
        );
        ack(&mut a, &mut b);
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Segment {
                offset: 0,
                bytes: 4,
                sha256: digest(&[1; 4]),
            },
            &[1; 4],
        );
        ack(&mut a, &mut b);
        deliver(
            &mut a,
            &mut b,
            Direction::Child,
            Message::Finalized { position: 4 },
            &[],
        );
        ack(&mut a, &mut b);
        for message in [
            Message::Seek { position: 3 },
            Message::Finalized { position: 3 },
            Message::Finalized { position: 5 },
            Message::Segment {
                offset: 0,
                bytes: 1,
                sha256: [0; 32],
            },
        ] {
            let mut state = a.clone();
            assert!(state
                .preflight(Direction::Child, &frame(&state, Direction::Child, message))
                .is_err());
        }
    }
    #[test]
    fn cancellation_interrupted_short_transport_and_consumer_errors_are_sticky() {
        let (state, _) = handshake();
        struct Interrupted(Cell<usize>);
        impl Read for Interrupted {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                self.0.set(self.0.get() + 1);
                Err(io::ErrorKind::Interrupted.into())
            }
        }
        let mut input = Interrupted(Cell::new(0));
        let mut failed = state.clone();
        assert!(failed
            .receive(
                Direction::Child,
                &mut input,
                &mut [0; 64],
                &|| Ok(()),
                &mut |_| Ok(())
            )
            .is_err());
        assert_eq!(input.0.get(), 1);
        let mut failed = state.clone();
        assert_eq!(
            failed
                .send(
                    Direction::Child,
                    Message::Seek { position: 0 },
                    &mut vec![],
                    &mut io::empty(),
                    &mut [0; 64],
                    &|| Err("explicit cancellation".into())
                )
                .unwrap_err(),
            "explicit cancellation"
        );
        let mut fresh = State::new(configuration(), [7; 16]).unwrap();
        let mut wire = vec![];
        write_frame(
            &mut wire,
            fresh.nonce,
            fresh.binding,
            0,
            Message::Start(configuration()),
            &mut Cursor::new([5; 132]),
            &mut [0; 64],
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(
            fresh
                .receive(
                    Direction::Parent,
                    &mut Cursor::new(wire),
                    &mut [0; 64],
                    &|| Ok(()),
                    &mut |_| Err("owned staging failed".into())
                )
                .unwrap_err(),
            "owned staging failed"
        );
        let mut c = state.clone();
        let mut d = state;
        deliver(
            &mut c,
            &mut d,
            Direction::Parent,
            Message::Cancel { code: 1 },
            &[],
        );
        assert!(!c.successful() && !d.successful());
    }
    #[test]
    fn arbitrary_small_fixed_headers_never_panic_or_collect_unbounded_payload() {
        let mut seed = 0x7812_59a3u32;
        for _ in 0..1000 {
            let mut bytes = [0u8; 512];
            for byte in &mut bytes {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *byte = (seed >> 24) as u8;
            }
            let mut cursor = Cursor::new(bytes);
            let _ = Frame::read(&mut cursor, &|| Ok(()));
            assert!(cursor.position() <= HEADER_BYTES as u64);
        }
    }
}
