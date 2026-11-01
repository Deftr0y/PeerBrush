//! The authenticated stdio bridge forwards one bounded JSON message without
//! collecting image strings. Once output starts, any fault closes the transport.
use serde::Deserialize;
use serde_json::Value;
use std::io::{self, Read, Write};

const MAX_BODY: usize = 512 * 1024 * 1024;
const MAX_ID: usize = 4 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;

#[derive(Debug)]
pub(super) struct Failure {
    pub(super) started: bool,
}
impl Failure {
    pub(super) fn message(&self) -> &'static str {
        "The local MCP response was interrupted or invalid"
    }
}

/// Private responses have a fixed first pair of fields. Validate the complete
/// request binding before a single response byte can reach stdout.
pub(super) fn forward(
    mut input: impl Read,
    output: &mut impl Write,
    length: usize,
    id: &Value,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), Failure> {
    let early = || Failure { started: false };
    if length == 0 || length > MAX_BODY {
        return Err(early());
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, id).map_err(|_| early())?;
    let id = serde_json::to_vec(id).map_err(|_| early())?;
    let pieces: [&[u8]; 3] = [b"{\"id\":", &id, b",\"jsonrpc\":\"2.0\","];
    let prefix_length = pieces.iter().map(|p| p.len()).sum::<usize>();
    if prefix_length >= length {
        return Err(early());
    }
    let mut scratch = [0u8; CHUNK];
    for piece in pieces {
        for expected in piece.chunks(CHUNK) {
            check().map_err(|_| early())?;
            input
                .read_exact(&mut scratch[..expected.len()])
                .map_err(|_| early())?;
            if scratch[..expected.len()] != *expected {
                return Err(early());
            }
        }
    }
    let prefix = pieces[0].chain(pieces[1]).chain(pieces[2]);
    let mut reader = Forward {
        input: prefix.chain(input),
        output,
        remaining: length,
        started: false,
        guard: Guard::default(),
        buffer: [0; CHUNK],
        filled: 0,
        check,
    };
    let result = (|| {
        let mut json = serde_json::Deserializer::from_reader(std::io::BufReader::with_capacity(
            CHUNK,
            &mut reader,
        ));
        serde::de::IgnoredAny::deserialize(&mut json).map_err(|_| ())?;
        json.end().map_err(|_| ())?;
        if reader.remaining != 0 || !reader.guard.complete() {
            return Err(());
        }
        reader.flush().map_err(|_| ())?;
        reader.output.write_all(b"\n").map_err(|_| ())?;
        reader.output.flush().map_err(|_| ())
    })();
    result.map_err(|_| Failure {
        started: reader.started,
    })
}

struct Count(usize);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_ID)
            .ok_or_else(|| invalid("MCP response ID exceeds its bound"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct Forward<'a, R, W> {
    input: R,
    output: &'a mut W,
    remaining: usize,
    started: bool,
    guard: Guard,
    buffer: [u8; CHUNK],
    filled: usize,
    check: &'a mut dyn FnMut() -> Result<(), String>,
}
impl<R: Read, W: Write> Forward<'_, R, W> {
    fn flush(&mut self) -> io::Result<()> {
        (self.check)().map_err(|_| invalid("MCP response canceled"))?;
        if self.filled > 0 {
            // A partial Write failure is conservatively terminal as well.
            self.started = true;
            self.output.write_all(&self.buffer[..self.filled])?;
            self.filled = 0;
        }
        Ok(())
    }
}
impl<R: Read, W: Write> Read for Forward<'_, R, W> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        (self.check)().map_err(|_| invalid("MCP response canceled"))?;
        if self.remaining == 0 {
            let mut extra = [0];
            if self.input.read(&mut extra)? != 0 {
                return Err(invalid("MCP response exceeds framing"));
            }
            return Ok(0);
        }
        let count = output.len().min(self.remaining).min(CHUNK - self.filled);
        let read = self.input.read(&mut output[..count])?;
        if read == 0 {
            return Err(invalid("MCP response is truncated"));
        }
        for &byte in &output[..read] {
            self.guard.byte(byte)?;
        }
        self.buffer[self.filled..self.filled + read].copy_from_slice(&output[..read]);
        self.filled += read;
        self.remaining -= read;
        if self.filled == CHUNK {
            self.flush()?;
        }
        Ok(read)
    }
}

/// IgnoredAny avoids allocating payload strings; this guard independently
/// validates UTF-8 and bounds its nesting scratch before serde sees each byte.
#[derive(Default)]
struct Guard {
    string: bool,
    escape: Escape,
    depth: usize,
    utf8: u8,
    low: u8,
    high: u8,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Escape {
    #[default]
    None,
    Slash,
    Hex {
        remaining: u8,
        word: u16,
        low: bool,
    },
    LowSlash,
    LowU,
}
impl Guard {
    fn byte(&mut self, byte: u8) -> io::Result<()> {
        if self.utf8 > 0 {
            if byte < self.low || byte > self.high {
                return Err(invalid("Invalid MCP UTF-8"));
            }
            self.utf8 -= 1;
            self.low = 0x80;
            self.high = 0xbf;
            return Ok(());
        }
        if byte >= 0x80 {
            if !self.string || self.escape != Escape::None {
                return Err(invalid("Invalid MCP string escape"));
            }
            let (count, low, high) = match byte {
                0xc2..=0xdf => (1, 0x80, 0xbf),
                0xe0 => (2, 0xa0, 0xbf),
                0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
                0xed => (2, 0x80, 0x9f),
                0xf0 => (3, 0x90, 0xbf),
                0xf1..=0xf3 => (3, 0x80, 0xbf),
                0xf4 => (3, 0x80, 0x8f),
                _ => return Err(invalid("Invalid MCP UTF-8")),
            };
            self.utf8 = count;
            self.low = low;
            self.high = high;
            return Ok(());
        }
        if self.string {
            self.escape = match self.escape {
                Escape::None => match byte {
                    b'\\' => Escape::Slash,
                    b'"' => {
                        self.string = false;
                        Escape::None
                    }
                    0..=31 => return Err(invalid("Invalid MCP string control")),
                    _ => Escape::None,
                },
                Escape::Slash => match byte {
                    b'u' => Escape::Hex {
                        remaining: 4,
                        word: 0,
                        low: false,
                    },
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => Escape::None,
                    _ => return Err(invalid("Invalid MCP string escape")),
                },
                Escape::Hex {
                    remaining,
                    word,
                    low,
                } => {
                    let digit = match byte {
                        b'0'..=b'9' => byte - b'0',
                        b'a'..=b'f' => byte - b'a' + 10,
                        b'A'..=b'F' => byte - b'A' + 10,
                        _ => return Err(invalid("Invalid MCP unicode escape")),
                    };
                    let word = (word << 4) | digit as u16;
                    if remaining > 1 {
                        Escape::Hex {
                            remaining: remaining - 1,
                            word,
                            low,
                        }
                    } else if low {
                        if !(0xdc00..=0xdfff).contains(&word) {
                            return Err(invalid("Invalid MCP unicode pair"));
                        }
                        Escape::None
                    } else if (0xd800..=0xdbff).contains(&word) {
                        Escape::LowSlash
                    } else if (0xdc00..=0xdfff).contains(&word) {
                        return Err(invalid("Unpaired MCP unicode escape"));
                    } else {
                        Escape::None
                    }
                }
                Escape::LowSlash => {
                    if byte != b'\\' {
                        return Err(invalid("Unpaired MCP unicode escape"));
                    }
                    Escape::LowU
                }
                Escape::LowU => {
                    if byte != b'u' {
                        return Err(invalid("Unpaired MCP unicode escape"));
                    }
                    Escape::Hex {
                        remaining: 4,
                        word: 0,
                        low: true,
                    }
                }
            };
        } else {
            match byte {
                b'"' => self.string = true,
                b'{' | b'[' => {
                    self.depth += 1;
                    if self.depth > 128 {
                        return Err(invalid("MCP response nesting exceeds its bound"));
                    }
                }
                b'}' | b']' => {
                    self.depth = self
                        .depth
                        .checked_sub(1)
                        .ok_or_else(|| invalid("Invalid MCP nesting"))?;
                }
                _ => (),
            }
        }
        Ok(())
    }
    fn complete(&self) -> bool {
        self.utf8 == 0 && !self.string && self.escape == Escape::None && self.depth == 0
    }
}

pub(super) fn framing<'a>(
    status: u16,
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<usize, Failure> {
    let fail = || Failure { started: false };
    if status != 200 {
        return Err(fail());
    }
    let mut length = None;
    let mut content = false;
    let mut encoding = false;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(fail());
            }
            length = Some(value.parse::<usize>().map_err(|_| fail())?);
        } else if name.eq_ignore_ascii_case("content-type") {
            if content
                || !value
                    .split(';')
                    .next()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
            {
                return Err(fail());
            }
            content = true;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(fail());
        } else if name.eq_ignore_ascii_case("content-encoding") {
            if encoding || !value.eq_ignore_ascii_case("identity") {
                return Err(fail());
            }
            encoding = true;
        }
    }
    length
        .filter(|length| content && *length > 0 && *length <= MAX_BODY)
        .ok_or_else(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn body(data: &str) -> Vec<u8> {
        format!("{{\"id\":17,\"jsonrpc\":\"2.0\",\"result\":{{\"data\":\"{data}\"}}}}").into_bytes()
    }
    #[test]
    fn forwards_large_strings_unicode_and_exact_wire_without_collecting() {
        let body = body(&("a".repeat(CHUNK * 7 + 1) + "🙂é"));
        let mut output = Vec::new();
        forward(
            body.as_slice(),
            &mut output,
            body.len(),
            &Value::from(17),
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(&output[..body.len()], body);
        assert_eq!(output.last(), Some(&b'\n'));
    }
    #[test]
    fn wrong_identity_and_header_faults_emit_no_body_or_private_details() {
        let body = body("secret-token-should-not-leak");
        let mut output = Vec::new();
        let fault = forward(
            body.as_slice(),
            &mut output,
            body.len(),
            &Value::from(18),
            &mut || Ok(()),
        )
        .unwrap_err();
        assert!(!fault.started);
        assert!(output.is_empty());
        assert!(!fault.message().contains("secret"));
        for (status, headers) in [
            (
                401,
                vec![
                    ("Content-Length", "100"),
                    ("Content-Type", "application/json"),
                ],
            ),
            (
                200,
                vec![("Content-Length", "100"), ("Content-Type", "text/html")],
            ),
            (
                200,
                vec![
                    ("Content-Length", "100"),
                    ("Content-Length", "100"),
                    ("Content-Type", "application/json"),
                ],
            ),
            (
                200,
                vec![
                    ("Transfer-Encoding", "chunked"),
                    ("Content-Type", "application/json"),
                ],
            ),
            (
                200,
                vec![
                    ("Content-Length", "100"),
                    ("Content-Type", "application/json"),
                    ("Content-Encoding", "gzip"),
                ],
            ),
        ] {
            assert!(!framing(status, headers).unwrap_err().started);
        }
        assert_eq!(
            framing(
                200,
                [
                    ("Content-Length", "100"),
                    ("Content-Type", "application/json; charset=utf-8")
                ]
            )
            .unwrap(),
            100
        );
    }
    #[test]
    fn partial_invalid_truncated_and_canceled_messages_are_terminal() {
        let valid = body(&"a".repeat(CHUNK * 3));
        let mut cases = vec![valid[..valid.len() - 2].to_vec()];
        let mut invalid_utf8 = valid.clone();
        invalid_utf8[CHUNK * 2] = 0xff;
        cases.push(invalid_utf8);
        let mut syntax = valid.clone();
        *syntax.last_mut().unwrap() = b',';
        cases.push(syntax);
        let mut extra = valid.clone();
        extra.push(b'0');
        cases.push(extra);
        for case in cases {
            let mut output = Vec::new();
            let fault = forward(
                case.as_slice(),
                &mut output,
                valid.len(),
                &Value::from(17),
                &mut || Ok(()),
            )
            .unwrap_err();
            assert!(fault.started);
            assert!(!output.is_empty());
            assert!(!output.ends_with(b"\n"));
        }
        let mut output = Vec::new();
        let delivered = std::cell::Cell::new(false);
        struct CancelSink<'a> {
            bytes: &'a mut Vec<u8>,
            delivered: &'a std::cell::Cell<bool>,
        }
        impl Write for CancelSink<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                self.delivered.set(true);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut sink = CancelSink {
            bytes: &mut output,
            delivered: &delivered,
        };
        let fault = forward(
            valid.as_slice(),
            &mut sink,
            valid.len(),
            &Value::from(17),
            &mut || {
                if delivered.get() {
                    Err("canceled secret".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert!(fault.started);
        assert!(!output.ends_with(b"\n"));
    }
    #[test]
    fn nesting_utf8_boundary_and_transport_write_failure_are_bounded() {
        let mut nested = b"{\"id\":17,\"jsonrpc\":\"2.0\",\"result\":".to_vec();
        nested.extend(std::iter::repeat_n(b'[', 129));
        nested.extend_from_slice(b"0");
        nested.extend(std::iter::repeat_n(b']', 129));
        nested.push(b'}');
        let mut output = Vec::new();
        assert!(
            !forward(
                nested.as_slice(),
                &mut output,
                nested.len(),
                &Value::from(17),
                &mut || Ok(())
            )
            .unwrap_err()
            .started
        );
        for sequence in [
            &[0xe0, 0x80, 0x80][..],
            &[0xed, 0xa0, 0x80],
            &[0xf4, 0x90, 0x80, 0x80],
            &[0xc2],
        ] {
            let mut guard = Guard::default();
            let mut refused = false;
            for &byte in sequence {
                if guard.byte(byte).is_err() {
                    refused = true;
                    break;
                }
            }
            assert!(refused || !guard.complete());
        }
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let body = body("fine");
        assert!(
            forward(
                body.as_slice(),
                &mut Broken,
                body.len(),
                &Value::from(17),
                &mut || Ok(())
            )
            .unwrap_err()
            .started
        );
    }
    #[test]
    fn ignored_strings_still_require_valid_unicode_escape_pairs() {
        for data in [
            r"\uD800",
            r"\uDC00",
            r"\uD800x",
            r"\uD800\u0041",
            r"\uD800\n",
            r"\uD800\uD800",
        ] {
            let body = body(data);
            let mut output = Vec::new();
            assert!(forward(
                body.as_slice(),
                &mut output,
                body.len(),
                &json_id(),
                &mut || Ok(())
            )
            .is_err());
            assert!(output.is_empty());
        }
        let body = body(r"\uD83D\uDE42\u0022\n\u00e9");
        let mut output = Vec::new();
        forward(
            body.as_slice(),
            &mut output,
            body.len(),
            &json_id(),
            &mut || Ok(()),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["result"]["data"], "\u{1f642}\"\n\u{e9}");
    }
    fn json_id() -> Value {
        Value::from(17)
    }
}
