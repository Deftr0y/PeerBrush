//! Allocation-free DOM preflight (serde's bounded string scratch is separate).
use super::{Admission, Limits};
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use std::{cell::Cell, fmt};

#[derive(Default)]
struct Counts {
    strings: Cell<usize>,
    largest: Cell<usize>,
    fields: Cell<usize>,
}
impl Counts {
    fn string(&self, bytes: usize) -> Result<(), String> {
        self.strings.set(
            self.strings
                .get()
                .checked_add(bytes)
                .ok_or("Source36 string footprint overflow")?,
        );
        self.largest.set(self.largest.get().max(bytes));
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Seed<'a, 'b> {
    admission: &'a Admission<'b>,
    counts: &'a Counts,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_, '_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        self.admission.enter(self.depth).map_err(D::Error::custom)?;
        deserializer.deserialize_any(self)
    }
}
struct Key<'a>(&'a Counts);
impl<'de> DeserializeSeed<'de> for Key<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_str(self)
    }
}
impl<'de> Visitor<'de> for Key<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Source36 object key")
    }
    fn visit_str<E: serde::de::Error>(self, key: &str) -> Result<(), E> {
        self.0.string(key.len()).map_err(E::custom)
    }
    fn visit_string<E: serde::de::Error>(self, key: String) -> Result<(), E> {
        self.visit_str(&key)
    }
}
impl<'de> Visitor<'de> for Seed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded Source36 metadata preflight")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, number: f64) -> Result<(), E> {
        if !number.is_finite() {
            return Err(E::custom("Nonfinite Source36 number"));
        }
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<(), E> {
        self.counts.string(value.len()).map_err(E::custom)
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<(), E> {
        self.visit_str(&value)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        while sequence
            .next_element_seed(Self {
                depth: self.depth + 1,
                ..self
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<(), A::Error> {
        while object.next_key_seed(Key(self.counts))?.is_some() {
            self.counts.fields.set(
                self.counts
                    .fields
                    .get()
                    .checked_add(1)
                    .ok_or_else(|| A::Error::custom("Source36 object footprint overflow"))?,
            );
            object.next_value_seed(Self {
                depth: self.depth + 1,
                ..self
            })?;
        }
        Ok(())
    }
}

/// Counts/identity from a no-DOM pass. This is not canonical/duplicate-key or
/// typed-source authority; the admitted strict parser must still perform those.
pub(crate) struct Stats {
    bytes: usize,
    nodes: usize,
    strings: usize,
    largest: usize,
    fields: usize,
    fingerprint: [u8; 32],
}
fn fingerprint(bytes: &[u8], check: &dyn Fn() -> Result<(), String>) -> Result<[u8; 32], String> {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"PeerBrushSource36MetadataFootprint\0");
    hash.update(&(bytes.len() as u64).to_be_bytes());
    for part in bytes.chunks(65536) {
        check()?;
        hash.update(part);
    }
    check()?;
    Ok(hash.finish().as_ref().try_into().unwrap())
}
impl Stats {
    pub(crate) fn scan(
        bytes: &[u8],
        limits: Limits,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        limits.validate()?;
        check()?;
        if bytes.len() > limits.bytes {
            return Err("Source36 metadata byte budget exceeded".into());
        }
        let counts = Counts::default();
        let admission = Admission {
            remaining: Cell::new(limits.nodes),
            depth: limits.depth,
            check,
        };
        let mut parser = serde_json::Deserializer::from_slice(bytes);
        Seed {
            admission: &admission,
            counts: &counts,
            depth: 1,
        }
        .deserialize(&mut parser)
        .map_err(|e| e.to_string())?;
        parser.end().map_err(|e| e.to_string())?;
        Ok(Self {
            bytes: bytes.len(),
            nodes: limits.nodes - admission.remaining.get(),
            strings: counts.strings.get(),
            largest: counts.largest.get(),
            fields: counts.fields.get(),
            fingerprint: fingerprint(bytes, check)?,
        })
    }
    pub(crate) fn matches(
        &self,
        bytes: &[u8],
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if bytes.len() != self.bytes || fingerprint(bytes, check)? != self.fingerprint {
            return Err("Source36 metadata admission belongs to another byte source".into());
        }
        Ok(())
    }
    pub(crate) fn input_bytes(&self) -> usize {
        self.bytes
    }
    pub(crate) fn nodes(&self) -> usize {
        self.nodes
    }
    pub(crate) fn tree_upper_bytes(&self) -> Result<usize, String> {
        // Vec's minimum/growth slots, value storage, conservative BTree nodes,
        // copied decoded strings, sorting pointers and parser string scratch.
        // This is an explicit accounting envelope, not process/allocator RAM.
        self.nodes
            .checked_mul(4 * std::mem::size_of::<serde_json::Value>())
            .and_then(|n| {
                self.fields
                    .checked_mul(256 + 2 * std::mem::size_of::<usize>())
                    .and_then(|f| n.checked_add(f))
            })
            .and_then(|n| self.strings.checked_mul(2).and_then(|s| n.checked_add(s)))
            .and_then(|n| self.largest.checked_mul(2).and_then(|s| n.checked_add(s)))
            .and_then(|n| n.checked_add(16384))
            .ok_or_else(|| "Source36 metadata footprint overflow".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preflight_counts_empty_contour_arrays_before_any_dom_collection() {
        let bytes = format!(
            "{{\"contours\":[{}]}}",
            std::iter::repeat_n("[]", 10_000)
                .collect::<Vec<_>>()
                .join(",")
        );
        let limits = Limits {
            bytes: bytes.len(),
            nodes: 10_002,
            depth: 8,
        };
        let stats = Stats::scan(bytes.as_bytes(), limits, &|| Ok(())).unwrap();
        assert_eq!(stats.nodes(), 10_002);
        assert!(stats.tree_upper_bytes().unwrap() > stats.input_bytes() * 20);
        assert!(Stats::scan(
            bytes.as_bytes(),
            Limits {
                nodes: 10_001,
                ..limits
            },
            &|| Ok(())
        )
        .is_err());
        stats.matches(bytes.as_bytes(), &|| Ok(())).unwrap();
        let mut other = bytes.into_bytes();
        other[2] ^= 1;
        assert!(stats.matches(&other, &|| Ok(())).is_err());
    }
    #[test]
    fn preflight_keeps_syntax_depth_and_cancellation_gates_without_claiming_canonicality() {
        let limits = Limits {
            bytes: 1024,
            nodes: 1024,
            depth: 2,
        };
        assert!(Stats::scan(br#"{"x":[[]]}"#, limits, &|| Ok(())).is_err());
        assert!(Stats::scan(br#"{"x":null} null"#, limits, &|| Ok(())).is_err());
        assert!(Stats::scan(br#"{"x":null}"#, limits, &|| Err(
            "Canceled footprint".into()
        ))
        .is_err());
        let duplicate = br#"{"x":1,"x":2}"#;
        let stats = Stats::scan(duplicate, limits, &|| Ok(())).unwrap();
        assert_eq!(stats.nodes(), 3);
        assert!(super::super::Canonical::parse(duplicate, limits, &|| Ok(())).is_err());
    }
}
