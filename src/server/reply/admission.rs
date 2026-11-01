//! Count a serialized metadata projection before constructing its owned Value.
use super::chunks::MAX_BYTES;
use serde::{
    ser::{
        self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
        SerializeTupleStruct, SerializeTupleVariant,
    },
    Serialize, Serializer,
};
use std::{cell::Cell, fmt};
const MAX_NODES: usize = 8 * 1024 * 1024;
#[derive(Debug)]
struct Error(String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl ser::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}
fn failure() -> Error {
    Error("Response metadata exceeds its admission bound".into())
}
fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b)
        .filter(|n| *n <= MAX_BYTES)
        .ok_or_else(failure)
}
struct Estimate<'a> {
    depth: usize,
    nodes: &'a Cell<usize>,
}
impl<'a> Estimate<'a> {
    fn node(&self) -> Result<(), Error> {
        let next = self
            .nodes
            .get()
            .checked_add(1)
            .filter(|n| *n <= MAX_NODES && self.depth <= 128)
            .ok_or_else(failure)?;
        self.nodes.set(next);
        Ok(())
    }
    fn leaf(self, extra: usize) -> Result<usize, Error> {
        self.node()?;
        add(128, extra)
    }
    fn group(self, length: Option<usize>, extra: usize) -> Result<Group<'a>, Error> {
        self.node()?;
        if length.is_some_and(|n| n > MAX_NODES) {
            return Err(failure());
        }
        Ok(Group {
            child: Estimate {
                depth: self.depth + 1,
                nodes: self.nodes,
            },
            bytes: add(256, extra)?,
        })
    }
}
struct Group<'a> {
    child: Estimate<'a>,
    bytes: usize,
}
impl Group<'_> {
    fn value<T: Serialize + ?Sized>(&mut self, value: &T, overhead: usize) -> Result<(), Error> {
        let bytes = value.serialize(Estimate {
            depth: self.child.depth,
            nodes: self.child.nodes,
        })?;
        self.bytes = add(self.bytes, add(bytes, overhead)?)?;
        Ok(())
    }
}
macro_rules! scalar {($($name:ident($ty:ty)),*)=>{$(fn $name(self,_value:$ty)->Result<Self::Ok,Self::Error>{self.leaf(0)})*};}
impl<'a> Serializer for Estimate<'a> {
    type Ok = usize;
    type Error = Error;
    type SerializeSeq = Group<'a>;
    type SerializeTuple = Group<'a>;
    type SerializeTupleStruct = Group<'a>;
    type SerializeTupleVariant = Group<'a>;
    type SerializeMap = Group<'a>;
    type SerializeStruct = Group<'a>;
    type SerializeStructVariant = Group<'a>;
    scalar!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_f32(f32),
        serialize_f64(f64),
        serialize_char(char)
    );
    fn serialize_str(self, value: &str) -> Result<usize, Error> {
        self.leaf(value.len().checked_mul(2).ok_or_else(failure)?)
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<usize, Error> {
        let mut group = self.group(Some(value.len()), 0)?;
        for byte in value {
            group.value(byte, 32)?;
        }
        Ok(group.bytes)
    }
    fn serialize_none(self) -> Result<usize, Error> {
        self.leaf(0)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<usize, Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<usize, Error> {
        self.leaf(0)
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<usize, Error> {
        self.leaf(0)
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<usize, Error> {
        self.serialize_str(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<usize, Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<usize, Error> {
        let mut group = self.group(Some(1), variant.len() * 2)?;
        group.value(value, 256)?;
        Ok(group.bytes)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Group<'a>, Error> {
        self.group(len, 0)
    }
    fn serialize_tuple(self, len: usize) -> Result<Group<'a>, Error> {
        self.group(Some(len), 0)
    }
    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<Group<'a>, Error> {
        self.group(Some(len), 0)
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Group<'a>, Error> {
        self.group(Some(len), variant.len() * 2 + 256)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Group<'a>, Error> {
        self.group(len, 0)
    }
    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<Group<'a>, Error> {
        self.group(Some(len), 0)
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Group<'a>, Error> {
        self.group(Some(len), variant.len() * 2 + 256)
    }
    fn collect_str<T: fmt::Display + ?Sized>(self, value: &T) -> Result<usize, Error> {
        struct Text(usize);
        impl fmt::Write for Text {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0 = self
                    .0
                    .checked_add(s.len())
                    .filter(|n| *n <= MAX_BYTES / 2)
                    .ok_or(fmt::Error)?;
                Ok(())
            }
        }
        let mut text = Text(0);
        fmt::write(&mut text, format_args!("{value}")).map_err(|_| failure())?;
        self.leaf(text.0 * 2)
    }
}
macro_rules! sequence {
    ($trait:ident,$method:ident) => {
        impl $trait for Group<'_> {
            type Ok = usize;
            type Error = Error;
            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
                self.value(value, 32)
            }
            fn end(self) -> Result<usize, Error> {
                Ok(self.bytes)
            }
        }
    };
}
sequence!(SerializeSeq, serialize_element);
sequence!(SerializeTuple, serialize_element);
sequence!(SerializeTupleStruct, serialize_field);
sequence!(SerializeTupleVariant, serialize_field);
impl SerializeMap for Group<'_> {
    type Ok = usize;
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.value(key, 256)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.value(value, 0)
    }
    fn end(self) -> Result<usize, Error> {
        Ok(self.bytes)
    }
}
macro_rules! structure {
    ($trait:ident) => {
        impl $trait for Group<'_> {
            type Ok = usize;
            type Error = Error;
            fn serialize_field<T: Serialize + ?Sized>(
                &mut self,
                key: &'static str,
                value: &T,
            ) -> Result<(), Error> {
                self.value(key, 256)?;
                self.value(value, 0)
            }
            fn end(self) -> Result<usize, Error> {
                Ok(self.bytes)
            }
        }
    };
}
structure!(SerializeStruct);
structure!(SerializeStructVariant);
pub(super) fn bound<T: Serialize + ?Sized>(value: &T) -> Result<usize, String> {
    value
        .serialize(Estimate {
            depth: 0,
            nodes: &Cell::new(0),
        })
        .map_err(|e| e.to_string())
}
