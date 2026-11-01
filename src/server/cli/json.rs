use super::files::{Files, Spool};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::{
    cmp::Ordering,
    io::{BufRead, Write},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy, PartialEq)]
enum Place {
    Root,
    Result,
    Images,
    Image,
    Ok,
    Error,
    Other,
}
struct Input<'a, R> {
    reader: &'a mut R,
    bytes: u64,
    check: &'a mut dyn FnMut() -> Result<()>,
}
impl<R: BufRead> Input<'_, R> {
    fn peek(&mut self) -> Result<Option<u8>> {
        self.reader
            .fill_buf()
            .map(|b| b.first().copied())
            .map_err(|_| "CLI response interrupted".into())
    }
    fn next(&mut self) -> Result<u8> {
        let byte = self.peek()?.ok_or("CLI response is truncated")?;
        self.reader.consume(1);
        self.bytes += 1;
        if self.bytes > 512 * 1024 * 1024 {
            return Err("CLI response exceeds its byte bound".into());
        }
        if self.bytes % 16384 == 0 {
            (self.check)()?;
        }
        Ok(byte)
    }
    fn space(&mut self) -> Result<()> {
        while self.peek()?.is_some_and(|b| b" \r\n\t".contains(&b)) {
            self.next()?;
        }
        Ok(())
    }
    fn expect(&mut self, byte: u8) -> Result<()> {
        if self.next()? != byte {
            return Err("Invalid CLI response JSON".into());
        }
        Ok(())
    }
}
fn write(out: &mut dyn Write, bytes: &[u8]) -> Result<()> {
    out.write_all(bytes)
        .map_err(|_| "CLI staging output failed".into())
}
fn escaped(out: &mut dyn Write, bytes: &[u8]) -> Result<()> {
    for &b in bytes {
        match b {
            b'"' => write(out, b"\\\"")?,
            b'\\' => write(out, b"\\\\")?,
            b'\n' => write(out, b"\\n")?,
            b'\r' => write(out, b"\\r")?,
            b'\t' => write(out, b"\\t")?,
            8 => write(out, b"\\b")?,
            12 => write(out, b"\\f")?,
            0..=31 => {
                let hex = b"0123456789abcdef";
                write(
                    out,
                    &[
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        hex[(b / 16) as usize],
                        hex[(b % 16) as usize],
                    ],
                )?;
            }
            _ => write(out, &[b])?,
        }
    }
    Ok(())
}
fn quoted(out: &mut dyn Write, bytes: &[u8]) -> Result<()> {
    write(out, b"\"")?;
    escaped(out, bytes)?;
    write(out, b"\"")
}
fn indent(out: &mut dyn Write, depth: usize) -> Result<()> {
    write(out, &[b' '; 258][..depth * 2])
}
fn field(out: &mut dyn Write, depth: usize, first: &mut bool, key: &[u8]) -> Result<()> {
    if !*first {
        write(out, b",")?;
    }
    *first = false;
    write(out, b"\n")?;
    indent(out, depth + 1)?;
    quoted(out, key)?;
    write(out, b": ")
}
fn synthetic_path(
    out: &mut dyn Write,
    depth: usize,
    first: &mut bool,
    path: Option<&str>,
    inserted: &mut bool,
    order: Ordering,
) -> Result<()> {
    if !*inserted && order == Ordering::Greater {
        if let Some(path) = path {
            field(out, depth, first, b"path")?;
            quoted(out, path.as_bytes())?;
            *inserted = true;
        }
    }
    Ok(())
}

fn string<R: BufRead>(
    input: &mut Input<'_, R>,
    mut emit: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    input.expect(b'"')?;
    loop {
        let byte = input.next()?;
        match byte {
            b'"' => return Ok(()),
            0..=31 => return Err("Invalid CLI response string".into()),
            b'\\' => {
                let byte = input.next()?;
                match byte {
                    b'"' | b'\\' | b'/' => emit(&[byte])?,
                    b'b' => emit(&[8])?,
                    b'f' => emit(&[12])?,
                    b'n' => emit(b"\n")?,
                    b'r' => emit(b"\r")?,
                    b't' => emit(b"\t")?,
                    b'u' => {
                        let high = unicode(input)?;
                        let scalar = if (0xd800..=0xdbff).contains(&high) {
                            input.expect(b'\\')?;
                            input.expect(b'u')?;
                            let low = unicode(input)?;
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return Err("Invalid CLI response unicode pair".into());
                            }
                            0x10000 + ((high as u32 - 0xd800) << 10) + (low as u32 - 0xdc00)
                        } else {
                            high as u32
                        };
                        let c =
                            char::from_u32(scalar).ok_or("Invalid CLI response unicode scalar")?;
                        let mut utf8 = [0; 4];
                        emit(c.encode_utf8(&mut utf8).as_bytes())?;
                    }
                    _ => return Err("Invalid CLI response escape".into()),
                }
            }
            0x20..=0x7f => emit(&[byte])?,
            _ => {
                let length = match byte {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => return Err("Invalid CLI response UTF-8".into()),
                };
                let mut utf8 = [0; 4];
                utf8[0] = byte;
                for b in &mut utf8[1..length] {
                    *b = input.next()?;
                }
                std::str::from_utf8(&utf8[..length]).map_err(|_| "Invalid CLI response UTF-8")?;
                emit(&utf8[..length])?;
            }
        }
    }
}
fn unicode<R: BufRead>(input: &mut Input<'_, R>) -> Result<u16> {
    let mut value = 0u16;
    for _ in 0..4 {
        let digit = match input.next()? {
            b @ b'0'..=b'9' => b - b'0',
            b @ b'a'..=b'f' => b - b'a' + 10,
            b @ b'A'..=b'F' => b - b'A' + 10,
            _ => return Err("Invalid CLI response unicode escape".into()),
        };
        value = (value << 4) | digit as u16;
    }
    Ok(value)
}
struct Key {
    bytes: [u8; 32],
    length: usize,
    emitted: bool,
}
impl Key {
    fn is(&self, name: &[u8]) -> bool {
        self.length == name.len() && self.bytes[..self.length] == *name
    }
    fn order(&self) -> Ordering {
        self.bytes[..self.length.min(32)].cmp(b"path")
    }
}
struct Parser<'a, R> {
    input: Input<'a, R>,
    files: &'a mut Files,
    reason: &'a mut dyn Write,
    nodes: u64,
    ok: Option<bool>,
    reason_string: bool,
}
impl<R: BufRead> Parser<'_, R> {
    fn node(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > 8 * 1024 * 1024 {
            return Err("CLI response exceeds its node bound".into());
        }
        Ok(())
    }
    fn key(
        &mut self,
        out: &mut dyn Write,
        depth: usize,
        first: &mut bool,
        path: Option<&str>,
        inserted: &mut bool,
    ) -> Result<Key> {
        self.node()?;
        let mut key = Key {
            bytes: [0; 32],
            length: 0,
            emitted: false,
        };
        string(&mut self.input, |chunk| {
            let count = (32 - key.length.min(32)).min(chunk.len());
            let start = key.length.min(32);
            key.bytes[start..start + count].copy_from_slice(&chunk[..count]);
            key.length = key
                .length
                .checked_add(chunk.len())
                .ok_or("CLI key length overflow")?;
            if key.length > 32 {
                if !key.emitted {
                    synthetic_path(out, depth, first, path, inserted, key.order())?;
                    if !*first {
                        write(out, b",")?;
                    }
                    *first = false;
                    write(out, b"\n")?;
                    indent(out, depth + 1)?;
                    write(out, b"\"")?;
                    escaped(out, &key.bytes)?;
                    key.emitted = true;
                }
                escaped(out, &chunk[count..])?;
            }
            Ok(())
        })?;
        if key.emitted {
            write(out, b"\": ")?;
        }
        Ok(key)
    }
    fn value(&mut self, out: &mut dyn Write, place: Place, depth: usize) -> Result<()> {
        self.node()?;
        if depth > 128 {
            return Err("CLI response exceeds its nesting bound".into());
        }
        self.input.space()?;
        let byte = self.input.peek()?.ok_or("CLI response is truncated")?;
        if place == Place::Root && byte != b'{' {
            return Err("Invalid PeerBrush CLI response envelope".into());
        }
        if place == Place::Image && byte != b'{' {
            return Err("Missing image data".into());
        }
        if place == Place::Ok && !matches!(byte, b't' | b'f') {
            return Err("Invalid CLI response status".into());
        }
        match byte {
            b'{' => self.object(out, place, depth),
            b'[' => self.array(out, place, depth),
            b'"' => {
                write(out, b"\"")?;
                let reason = place == Place::Error;
                if reason {
                    self.reason_string = true;
                }
                let error = &mut self.reason;
                string(&mut self.input, |bytes| {
                    escaped(out, bytes)?;
                    if reason {
                        write(*error, bytes)?;
                    }
                    Ok(())
                })?;
                write(out, b"\"")
            }
            b't' | b'f' | b'n' => {
                let word: &[u8] = match byte {
                    b't' => b"true",
                    b'f' => b"false",
                    _ => b"null",
                };
                for &b in word {
                    self.input.expect(b)?;
                }
                if place == Place::Ok {
                    self.ok = Some(byte == b't');
                }
                write(out, word)
            }
            b'-' | b'0'..=b'9' => {
                let mut bytes = [0; 128];
                let mut count = 0;
                while self
                    .input
                    .peek()?
                    .is_some_and(|b| !b" \r\n\t,]}".contains(&b))
                {
                    if count == bytes.len() {
                        return Err("CLI response number exceeds its bound".into());
                    }
                    bytes[count] = self.input.next()?;
                    count += 1;
                }
                let value: serde_json::Number = serde_json::from_slice(&bytes[..count])
                    .map_err(|_| "Invalid CLI response number")?;
                serde_json::to_writer(out, &value).map_err(|_| "CLI number staging failed".into())
            }
            _ => Err("Invalid CLI response JSON".into()),
        }
    }
    fn object(&mut self, out: &mut dyn Write, place: Place, depth: usize) -> Result<()> {
        self.input.expect(b'{')?;
        write(out, b"{")?;
        let image = if place == Place::Image {
            Some(self.files.image()?)
        } else {
            None
        };
        let path = image.as_ref().map(|(_, path)| path.as_str());
        let mut inserted = false;
        let mut first = true;
        let mut data = false;
        let mut seen = 0u8;
        self.input.space()?;
        while self.input.peek()? != Some(b'}') {
            let key = self.key(out, depth, &mut first, path, &mut inserted)?;
            self.input.space()?;
            self.input.expect(b':')?;
            self.input.space()?;
            let special = if place == Place::Root && key.is(b"ok") {
                Some((1, Place::Ok))
            } else if place == Place::Root && key.is(b"result") {
                Some((2, Place::Result))
            } else if place == Place::Root && key.is(b"error") {
                Some((4, Place::Error))
            } else if place == Place::Result && key.is(b"images") {
                Some((8, Place::Images))
            } else {
                None
            };
            if let Some((bit, _)) = special {
                if seen & bit != 0 {
                    return Err("Duplicate CLI response envelope field".into());
                }
                seen |= bit;
            }
            if image.is_some() && key.is(b"data") {
                if data {
                    return Err("Duplicate CLI image data".into());
                }
                data = true;
                self.node()?;
                let writer = self.files.image_writer(image.as_ref().unwrap().0);
                decode(&mut self.input, writer)?;
            } else if image.is_some() && key.is(b"path") {
                self.value(&mut std::io::sink(), Place::Other, depth + 1)?;
            } else {
                if !key.emitted {
                    synthetic_path(out, depth, &mut first, path, &mut inserted, key.order())?;
                    field(out, depth, &mut first, &key.bytes[..key.length])?;
                }
                self.value(
                    out,
                    special.map_or(Place::Other, |(_, place)| place),
                    depth + 1,
                )?;
            }
            self.input.space()?;
            match self.input.peek()? {
                Some(b',') => {
                    self.input.next()?;
                    self.input.space()?;
                    if self.input.peek()? == Some(b'}') {
                        return Err("Invalid trailing CLI object comma".into());
                    }
                }
                Some(b'}') => break,
                _ => return Err("Invalid CLI response object".into()),
            }
        }
        self.input.expect(b'}')?;
        if image.is_some() {
            if !data {
                return Err("Missing image data".into());
            }
            synthetic_path(
                out,
                depth,
                &mut first,
                path,
                &mut inserted,
                Ordering::Greater,
            )?;
        }
        if !first {
            write(out, b"\n")?;
            indent(out, depth)?;
        }
        write(out, b"}")
    }
    fn array(&mut self, out: &mut dyn Write, place: Place, depth: usize) -> Result<()> {
        self.input.expect(b'[')?;
        if place == Place::Images {
            self.files.ensure_directory()?;
        }
        write(out, b"[")?;
        let mut first = true;
        self.input.space()?;
        while self.input.peek()? != Some(b']') {
            if !first {
                write(out, b",")?;
            }
            first = false;
            write(out, b"\n")?;
            indent(out, depth + 1)?;
            self.value(
                out,
                if place == Place::Images {
                    Place::Image
                } else {
                    Place::Other
                },
                depth + 1,
            )?;
            self.input.space()?;
            match self.input.peek()? {
                Some(b',') => {
                    self.input.next()?;
                    self.input.space()?;
                    if self.input.peek()? == Some(b']') {
                        return Err("Invalid trailing CLI array comma".into());
                    }
                }
                Some(b']') => break,
                _ => return Err("Invalid CLI response array".into()),
            }
        }
        self.input.expect(b']')?;
        if !first {
            write(out, b"\n")?;
            indent(out, depth)?;
        }
        write(out, b"]")
    }
}
fn decode<R: BufRead>(input: &mut Input<'_, R>, out: &mut Spool) -> Result<()> {
    let mut quartet = [0; 4];
    let mut count = 0;
    let mut final_group = false;
    let mut buffer = [0; 8192];
    let mut used = 0;
    string(input, |bytes| {
        for &byte in bytes {
            if final_group {
                return Err("Invalid preview base64 padding".into());
            }
            quartet[count] = byte;
            count += 1;
            if count == 4 {
                let mut decoded = [0; 3];
                let length = STANDARD
                    .decode_slice(quartet, &mut decoded)
                    .map_err(|_| "Invalid preview base64 data")?;
                if used + length > buffer.len() {
                    write(out, &buffer[..used])?;
                    used = 0;
                }
                buffer[used..used + length].copy_from_slice(&decoded[..length]);
                used += length;
                final_group = quartet.contains(&b'=');
                count = 0;
            }
        }
        Ok(())
    })?;
    if count != 0 {
        return Err("Invalid preview base64 length".into());
    }
    write(out, &buffer[..used])
}
pub(super) fn project<R: BufRead>(
    input: &mut R,
    out: &mut dyn Write,
    reason: &mut dyn Write,
    files: &mut Files,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<bool> {
    check()?;
    let mut parser = Parser {
        input: Input {
            reader: input,
            bytes: 0,
            check,
        },
        files,
        reason,
        nodes: 0,
        ok: None,
        reason_string: false,
    };
    parser.value(out, Place::Root, 0)?;
    parser.input.space()?;
    if parser.input.peek()?.is_some() {
        return Err("Trailing CLI response data".into());
    }
    let failed = !parser.ok.ok_or("Missing CLI response status")?;
    if failed && !parser.reason_string {
        write(parser.reason, b"Command failed")?;
    }
    (parser.input.check)()?;
    Ok(failed)
}
