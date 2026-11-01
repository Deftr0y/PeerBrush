use super::{chunks::Segments, Data, Part, Slot, DEPTH};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{
    ser::{SerializeMap, SerializeSeq},
    Serialize, Serializer,
};
use serde_json::Value;
use std::{
    fmt,
    io::{self, Write},
};

pub(super) fn at<'a>(mut value: &'a Value, path: &[Part<'_>]) -> Option<&'a Value> {
    for part in path {
        value = match part {
            Part::Key(key) => value.get(*key)?,
            Part::Index(index) => value.get(*index)?,
            Part::End => return None,
        };
    }
    Some(value)
}
pub(super) struct Node<'a> {
    pub value: &'a Value,
    pub slots: &'a [Slot],
    pub path: [Part<'a>; DEPTH],
    pub length: usize,
}
impl Serialize for Node<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let path = &self.path[..self.length];
        if let Some(slot) = self
            .slots
            .iter()
            .find(|slot| slot.length == self.length && slot.path[..slot.length] == *path)
        {
            if !self.value.is_null() {
                return Err(serde::ser::Error::custom("Sealed response slot changed"));
            }
            return match &slot.data {
                Data::Png(bytes) => serializer.collect_str(&Base64(bytes)),
                Data::JsonText(value) => serializer.collect_str(&JsonText(value)),
            };
        }
        if !self
            .slots
            .iter()
            .any(|slot| slot.length > self.length && slot.path[..self.length] == *path)
        {
            return self.value.serialize(serializer);
        }
        if self.length == DEPTH {
            return Err(serde::ser::Error::custom(
                "Response path depth exceeds its bound",
            ));
        }
        match self.value {
            Value::Object(values) => {
                let mut map = serializer.serialize_map(Some(values.len()))?;
                for (key, value) in values {
                    let mut path = self.path;
                    path[self.length] = Part::Key(key);
                    map.serialize_entry(
                        key,
                        &Node {
                            value,
                            slots: self.slots,
                            path,
                            length: self.length + 1,
                        },
                    )?;
                }
                map.end()
            }
            Value::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for (index, value) in values.iter().enumerate() {
                    let mut path = self.path;
                    path[self.length] = Part::Index(index);
                    seq.serialize_element(&Node {
                        value,
                        slots: self.slots,
                        path,
                        length: self.length + 1,
                    })?;
                }
                seq.end()
            }
            _ => Err(serde::ser::Error::custom(
                "Response slot has no containing metadata",
            )),
        }
    }
}
struct Base64<'a>(&'a Segments);
impl fmt::Display for Base64<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut carry = [0; 3];
        let mut used = 0;
        let mut output = [0; 16 * 1024];
        for mut part in self.0.parts() {
            if used > 0 {
                let n = (3 - used).min(part.len());
                carry[used..used + n].copy_from_slice(&part[..n]);
                used += n;
                part = &part[n..];
                if used == 3 {
                    let n = STANDARD
                        .encode_slice(carry, &mut output)
                        .map_err(|_| fmt::Error)?;
                    f.write_str(std::str::from_utf8(&output[..n]).map_err(|_| fmt::Error)?)?;
                    used = 0;
                }
            }
            while part.len() >= 3 {
                let n = part.len().min(12 * 1024) / 3 * 3;
                let written = STANDARD
                    .encode_slice(&part[..n], &mut output)
                    .map_err(|_| fmt::Error)?;
                f.write_str(std::str::from_utf8(&output[..written]).map_err(|_| fmt::Error)?)?;
                part = &part[n..];
            }
            if !part.is_empty() {
                carry[..part.len()].copy_from_slice(part);
                used = part.len();
            }
        }
        if used > 0 {
            let n = STANDARD
                .encode_slice(&carry[..used], &mut output)
                .map_err(|_| fmt::Error)?;
            f.write_str(std::str::from_utf8(&output[..n]).map_err(|_| fmt::Error)?)?;
        }
        Ok(())
    }
}
struct JsonText<'a>(&'a Value);
impl fmt::Display for JsonText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Output<'a, 'b>(&'a mut fmt::Formatter<'b>);
        impl Write for Output<'_, '_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
                self.0
                    .write_str(text)
                    .map_err(|_| io::Error::other("Response text formatting failed"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Output(f), self.0).map_err(|_| fmt::Error)
    }
}
pub(super) struct Count {
    length: usize,
    maximum: usize,
}
impl Count {
    pub fn new(maximum: usize) -> Self {
        Self { length: 0, maximum }
    }
    pub fn bytes(&self) -> usize {
        self.length
    }
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.length = self
            .length
            .checked_add(bytes.len())
            .filter(|n| *n <= self.maximum)
            .ok_or_else(|| io::Error::other("Response JSON exceeds its transport bound"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
