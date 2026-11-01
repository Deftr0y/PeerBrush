//! Canonical, bounded metadata only. No Surface I/O or native tile payloads.
use serde::{
    de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor},
    ser::{Error as _, SerializeMap, SerializeSeq},
    Serialize,
};
use serde_json::{Number, Value};
use std::{cell::Cell, fmt, io::Write};
#[path = "canonical36_stats.rs"]
mod stats;
pub(crate) use stats::Stats;

pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub bytes: usize,
    pub nodes: usize,
    pub depth: usize,
}
impl Limits {
    pub(crate) fn validate(self) -> Result<(), String> {
        if self.bytes == 0
            || self.bytes > MAX_BYTES
            || self.nodes == 0
            || self.nodes > 8_388_608
            || self.depth == 0
            || self.depth > 128
        {
            return Err("Source36 metadata admission differs".into());
        }
        Ok(())
    }
}

struct Admission<'a> {
    remaining: Cell<usize>,
    depth: usize,
    check: &'a dyn Fn() -> Result<(), String>,
}
impl Admission<'_> {
    fn enter(&self, depth: usize) -> Result<(), String> {
        (self.check)()?;
        if depth > self.depth || self.remaining.get() == 0 {
            return Err("Source36 metadata node/depth budget exceeded".into());
        }
        self.remaining.set(self.remaining.get() - 1);
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Seed<'a, 'b> {
    admission: &'a Admission<'b>,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_, '_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        self.admission.enter(self.depth).map_err(D::Error::custom)?;
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_, '_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded unique Source36 JSON metadata")
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(value)))
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(Number::from(value)))
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("Nonfinite Source36 number"))
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.into()))
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(Seed {
            depth: self.depth + 1,
            ..self
        })? {
            values
                .try_reserve(1)
                .map_err(|_| A::Error::custom("Source36 metadata allocation refused"))?;
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(A::Error::custom("Duplicate Source36 metadata key"));
            }
            let value = object.next_value_seed(Seed {
                depth: self.depth + 1,
                ..self
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

struct Ordered<'a>(&'a Value);
impl Serialize for Ordered<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(object) => {
                let mut fields = Vec::new();
                fields
                    .try_reserve_exact(object.len())
                    .map_err(|_| S::Error::custom("Source36 metadata order allocation refused"))?;
                fields.extend(object.iter());
                fields.sort_unstable_by(|a, b| a.0.cmp(b.0));
                let mut output = serializer.serialize_map(Some(fields.len()))?;
                for (key, value) in fields {
                    output.serialize_entry(key, &Ordered(value))?;
                }
                output.end()
            }
            Value::Array(array) => {
                let mut output = serializer.serialize_seq(Some(array.len()))?;
                for value in array {
                    output.serialize_element(&Ordered(value))?;
                }
                output.end()
            }
            other => other.serialize(serializer),
        }
    }
}

struct Output<'a> {
    bytes: Vec<u8>,
    maximum: usize,
    check: &'a dyn Fn() -> Result<(), String>,
}
impl Write for Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        (self.check)().map_err(std::io::Error::other)?;
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= self.maximum)
            .ok_or_else(|| std::io::Error::other("Source36 metadata byte budget exceeded"))?;
        if end > self.bytes.capacity() {
            let target = end
                .checked_next_power_of_two()
                .unwrap_or(self.maximum)
                .min(self.maximum);
            self.bytes
                .try_reserve_exact(target - self.bytes.len())
                .map_err(|_| std::io::Error::other("Source36 metadata allocation refused"))?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (self.check)().map_err(std::io::Error::other)
    }
}

fn validate_value(value: &Value, admission: &Admission<'_>, depth: usize) -> Result<(), String> {
    admission.enter(depth)?;
    match value {
        Value::Array(array) => {
            for child in array {
                validate_value(child, admission, depth + 1)?;
            }
        }
        Value::Object(object) => {
            for child in object.values() {
                validate_value(child, admission, depth + 1)?;
            }
        }
        Value::Number(number) if number.as_f64().is_some_and(|n| !n.is_finite()) => {
            return Err("Nonfinite Source36 number".into())
        }
        _ => {}
    }
    Ok(())
}

/// Bytes and parsed metadata have bounded, explicit owners. This token proves
/// canonical JSON framing only; typed semantic/roster validation is mandatory.
pub(crate) struct Canonical {
    bytes: Vec<u8>,
    value: Value,
}
impl Canonical {
    pub(crate) fn parse(
        bytes: &[u8],
        limits: Limits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::decode(bytes, limits, true, check)
    }
    fn decode(
        bytes: &[u8],
        limits: Limits,
        require_canonical: bool,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        limits.validate()?;
        check()?;
        if bytes.len() > limits.bytes {
            return Err("Source36 metadata byte budget exceeded".into());
        }
        let admission = Admission {
            remaining: Cell::new(limits.nodes),
            depth: limits.depth,
            check,
        };
        let mut parser = serde_json::Deserializer::from_slice(bytes);
        let value = Seed {
            admission: &admission,
            depth: 1,
        }
        .deserialize(&mut parser)
        .map_err(|e| e.to_string())?;
        parser.end().map_err(|e| e.to_string())?;
        let mut output = Output {
            bytes: Vec::new(),
            maximum: limits.bytes,
            check,
        };
        serde_json::to_writer(&mut output, &Ordered(&value)).map_err(|e| e.to_string())?;
        if require_canonical && output.bytes != bytes {
            return Err("Source36 metadata is not canonical JSON".into());
        }
        check()?;
        Ok(Self {
            bytes: output.bytes,
            value,
        })
    }
    pub(crate) fn from_value(
        value: Value,
        limits: Limits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        limits.validate()?;
        let admission = Admission {
            remaining: Cell::new(limits.nodes),
            depth: limits.depth,
            check,
        };
        validate_value(&value, &admission, 1)?;
        let mut output = Output {
            bytes: Vec::new(),
            maximum: limits.bytes,
            check,
        };
        serde_json::to_writer(&mut output, &Ordered(&value)).map_err(|e| e.to_string())?;
        check()?;
        drop(value);
        // Enforce the exact same parser/depth/numeric contract on emitted data.
        Self::parse(&output.bytes, limits, check)
    }
    pub(crate) fn from_serializable<T: Serialize>(
        value: &T,
        limits: Limits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        limits.validate()?;
        let mut output = Output {
            bytes: Vec::new(),
            maximum: limits.bytes,
            check,
        };
        serde_json::to_writer(&mut output, value).map_err(|e| e.to_string())?;
        Self::decode(&output.bytes, limits, false, check)
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
    pub(crate) fn into_parts(self) -> (Vec<u8>, Value) {
        (self.bytes, self.value)
    }
    pub(crate) fn core_digest(&self) -> [u8; 32] {
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        hash.update(b"PeerBrushSource36Core\0");
        hash.update(&(self.bytes.len() as u64).to_be_bytes());
        hash.update(&self.bytes);
        hash.finish().as_ref().try_into().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn limits() -> Limits {
        Limits {
            bytes: 4096,
            nodes: 1024,
            depth: 32,
        }
    }

    #[test]
    fn canonical_metadata_keeps_native_numbers_unicode_and_settings_exactly() {
        let value = json!({"z":1.2345678901234567,"a":{"word":65535,"min":-0.0,"text":"שלום\n👩‍🎨"},"n":u64::MAX});
        let canonical = Canonical::from_value(value.clone(), limits(), &|| Ok(())).unwrap();
        let parsed = Canonical::parse(canonical.bytes(), limits(), &|| Ok(())).unwrap();
        assert_eq!(parsed.value(), &value);
        assert_eq!(parsed.core_digest(), canonical.core_digest());
        assert!(std::str::from_utf8(canonical.bytes())
            .unwrap()
            .starts_with("{\"a\":"));
        let mut independent = b"PeerBrushSource36Core\0".to_vec();
        independent.extend_from_slice(&(canonical.bytes().len() as u64).to_be_bytes());
        independent.extend_from_slice(canonical.bytes());
        assert_eq!(
            canonical.core_digest(),
            ring::digest::digest(&ring::digest::SHA256, &independent).as_ref()
        );
    }
    #[test]
    fn duplicate_noncanonical_trailing_and_nonfinite_metadata_is_refused() {
        for bytes in [
            br#"{"a":1,"a":2}"#.as_slice(),
            br#"{"a":{"x":1,"x":1}}"#,
            br#"{"z":0,"a":1}"#,
            br#"{ "a": 1 }"#,
            br#"{"a":1} null"#,
            br#"{"a":1e9999}"#,
            br#"{"a":"\u0061"}"#,
        ] {
            assert!(Canonical::parse(bytes, limits(), &|| Ok(())).is_err());
        }
    }
    #[test]
    fn metadata_bytes_nodes_depth_and_cancellation_are_independent_gates() {
        let value = json!({"a":[1,2,3]});
        for admitted in [
            Limits {
                bytes: 1,
                ..limits()
            },
            Limits {
                nodes: 3,
                ..limits()
            },
            Limits {
                depth: 2,
                ..limits()
            },
        ] {
            assert!(Canonical::from_value(value.clone(), admitted, &|| Ok(())).is_err());
            assert!(Canonical::parse(br#"{"a":[1,2,3]}"#, admitted, &|| Ok(())).is_err());
        }
        let calls = Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            if calls.get() > 4 {
                Err("Canceled metadata fixture".into())
            } else {
                Ok(())
            }
        };
        let error = Canonical::parse(br#"{"a":[1,2,3]}"#, limits(), &check)
            .err()
            .unwrap();
        assert!(error.contains("Canceled metadata fixture"));
    }
}
