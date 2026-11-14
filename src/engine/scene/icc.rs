//! Source36 only: compact canonical ICC bytes, without numeric-array aliases.
use super::MetadataPayload;
use base64::{display::Base64Display, engine::general_purpose::STANDARD, Engine as _};
use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, sync::Arc};

pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_ENCODED: usize = MAX_BYTES.div_ceil(3) * 4;

pub fn serialize<S: Serializer>(
    profile: &Option<MetadataPayload<Vec<u8>>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serialize_bounded(profile, serializer, MAX_BYTES, "Source36 ICC profile exceeds its decoded byte bound")
}

fn serialize_bounded<S: Serializer>(
    asset: &Option<MetadataPayload<Vec<u8>>>,
    serializer: S,
    limit: usize,
    oversized: &str,
) -> Result<S::Ok, S::Error> {
    struct Text<'a>(&'a [u8]);
    impl Serialize for Text<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_str(&Base64Display::new(self.0, &STANDARD))
        }
    }
    match asset {
        None => serializer.serialize_none(),
        Some(bytes) if bytes.len() <= limit => serializer.serialize_some(&Text(bytes)),
        Some(_) => Err(serde::ser::Error::custom(oversized)),
    }
}

pub fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<MetadataPayload<Vec<u8>>>, D::Error> {
    deserialize_bounded(deserializer, MAX_BYTES)
}

fn deserialize_bounded<'de, D: Deserializer<'de>>(
    deserializer: D,
    limit: usize,
) -> Result<Option<MetadataPayload<Vec<u8>>>, D::Error> {
    struct OptionalAsset(usize);
    impl<'de> Visitor<'de> for OptionalAsset {
        type Value = Option<MetadataPayload<Vec<u8>>>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("null or a bounded canonical STANDARD base64 asset")
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: Deserializer<'de>>(self, input: D) -> Result<Self::Value, D::Error> {
            struct Asset(usize);
            impl Visitor<'_> for Asset {
                type Value = MetadataPayload<Vec<u8>>;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("a bounded canonical STANDARD base64 string")
                }
                fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
                    decode(text, self.0).map(MetadataPayload::from).map_err(E::custom)
                }
            }
            input.deserialize_str(Asset(self.0)).map(Some)
        }
    }
    deserializer.deserialize_option(OptionalAsset(limit))
}

fn decode(text: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    // Both lengths are checked before allocating or decoding any payload byte.
    if text.len() > limit.div_ceil(3) * 4 || text.len() % 4 != 0 {
        return Err("Source36 ICC encoded length or padding is invalid");
    }
    let padding = text
        .as_bytes()
        .iter()
        .rev()
        .take(2)
        .take_while(|&&b| b == b'=')
        .count();
    let decoded = text.len() / 4 * 3 - padding;
    if decoded > limit {
        return Err("Source36 ICC profile exceeds its decoded byte bound");
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(decoded)
        .map_err(|_| "Source36 ICC allocation refused")?;
    if bytes.capacity() > limit {
        return Err("Source36 ICC allocation exceeds its byte bound");
    }
    bytes.resize(decoded, 0);
    let written = STANDARD
        .decode_slice(text, &mut bytes)
        .map_err(|_| "Source36 ICC is not canonical STANDARD base64")?;
    if written != decoded {
        return Err("Source36 ICC decoded length differs");
    }
    Ok(bytes)
}

pub(super) mod file_info {
    use super::*;
    use crate::psd::file_info::MAX_BYTES;

    pub fn serialize<S: Serializer>(
        asset: &Option<MetadataPayload<Vec<u8>>>, serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serialize_bounded(asset, serializer, MAX_BYTES, "Source36 Photoshop File Info exceeds 1 MiB")
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<MetadataPayload<Vec<u8>>>, D::Error> {
        deserialize_bounded(deserializer, MAX_BYTES)
            .map_err(|_| serde::de::Error::custom("Invalid or oversized Source36 Photoshop File Info"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct Profile {
        #[serde(with = "super")]
        bytes: Option<MetadataPayload<Vec<u8>>>,
    }
    #[test]
    fn source36_field_is_compact_while_legacy_profile_wire_and_metadata_owner_remain_exact() {
        let mut document = crate::engine::Document::new(1, 1).unwrap();
        document.icc_profile = Some(Arc::new(vec![0, 1, 127, 128, 255]));
        struct Collector;
        impl super::super::RasterCollector for Collector {
            fn resident(
                &mut self,
                r: &crate::raster::Raster,
            ) -> Result<super::super::RasterRef36, String> {
                Ok(super::super::RasterRef36 {
                    id: 1,
                    width: r.width,
                    height: r.height,
                    depth: r.depth,
                    retained: None,
                })
            }
            fn surface(
                &mut self,
                _: &crate::raster::Surface,
            ) -> Result<super::super::RasterRef36, String> {
                Err("Unexpected surface".into())
            }
        }
        let metadata = super::super::Document36::capture(&document, &mut Collector).unwrap();
        let value = serde_json::to_value(&metadata).unwrap();
        assert_eq!(value["icc_profile"], "AAF/gP8=");
        let restored: super::super::Document36 = serde_json::from_value(value).unwrap();
        assert_eq!(
            restored.icc_profile.as_deref(),
            document.icc_profile.as_deref()
        );
        assert!(serde_json::to_value(&document).unwrap()["icc_profile"].is_array());
        let pool = crate::large_document::ResidentBudget::new(128 * 1024).unwrap();
        let charge =
            crate::psd::memory36::MetadataCharge::reserve(&pool, 64 * 1024, &|| Ok(())).unwrap();
        let admitted = crate::psd::memory36::Admitted::from_parts(Arc::new(restored), charge);
        let copy = admitted.clone();
        drop(admitted);
        assert_eq!(pool.used(), 64 * 1024);
        assert_eq!(copy.icc_profile.as_deref(), document.icc_profile.as_deref());
        drop(copy);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn exact_native_bytes_and_shared_source_owner_survive_canonical_roundtrip() {
        for n in [0, 1, 2, 3, 4, 5, 16, 257, MAX_BYTES] {
            let bytes = Arc::new(
                (0..n)
                    .map(|i| (i.wrapping_mul(197) + 31) as u8)
                    .collect::<Vec<_>>(),
            );
            let source = Profile {
                bytes: Some(bytes.as_ref().clone().into()),
            };
            let encoded = serde_json::to_vec(&source).unwrap();
            assert!(source.bytes.as_ref().unwrap().shared().is_err());
            let restored: Profile = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(restored.bytes.as_deref(), Some(bytes.as_ref()));
            assert_eq!(serde_json::to_vec(&restored).unwrap(), encoded);
            assert_eq!(bytes.len(), n);
            if n == MAX_BYTES {
                assert_eq!(encoded.len(), MAX_ENCODED + 12);
            }
        }
        let none: Profile = serde_json::from_str("{\"bytes\":null}").unwrap();
        assert!(none.bytes.is_none());
        assert_eq!(serde_json::to_string(&none).unwrap(), "{\"bytes\":null}");
    }
    #[test]
    fn first_excess_decoded_byte_is_refused_even_when_encoded_length_matches_the_maximum() {
        assert_eq!(MAX_ENCODED, 5_592_408);
        let mut excess = "A".repeat(MAX_ENCODED);
        excess.replace_range(MAX_ENCODED - 1.., "=");
        assert_eq!(
            decode(&excess, MAX_BYTES).unwrap_err(),
            "Source36 ICC profile exceeds its decoded byte bound"
        );
        excess.push('A');
        assert!(decode(&excess, MAX_BYTES).is_err());
        let oversized = Profile {
            bytes: Some(vec![0; MAX_BYTES + 1].into()),
        };
        assert!(serde_json::to_vec(&oversized).is_err());
    }
    #[test]
    fn numeric_aliases_noncanonical_padding_and_invalid_utf8_are_refused() {
        for input in [
            "AA", "AB==", "AAB=", "A===", "AAAA====", "AA==\n", "-_==", "éé",
        ] {
            let json = serde_json::json!({"bytes":input});
            assert!(serde_json::from_value::<Profile>(json).is_err(), "{input}");
        }
        assert!(serde_json::from_str::<Profile>("{\"bytes\":[0,1,2]}").is_err());
        assert!(serde_json::from_slice::<Profile>(b"{\"bytes\":\"\xff\"}").is_err());
    }
}
