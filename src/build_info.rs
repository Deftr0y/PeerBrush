//! Public build labels; trusted exact-source builds and signing remain separate.
// One storage address and accessor prevent ThinLTO from duplicating About/CLI labels.
static JSON: [u8; include_bytes!(concat!(env!("OUT_DIR"), "/build-info.json")).len()] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/build-info.json"));

#[inline(never)]
pub fn json() -> &'static str {
    std::str::from_utf8(&JSON).expect("Build metadata must be UTF-8")
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_record_is_bounded_public_metadata() {
        let value: serde_json::Value = serde_json::from_str(super::json()).unwrap();
        assert!(super::json().len() < 512);
        assert_eq!(value["format"], "PeerBrushBuildInfoV1");
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["product"], "PeerBrush");
        assert_eq!(value["end"], "PeerBrushBuildInfoEnd");
        assert!(matches!(
            value["tree"].as_str(),
            Some("clean" | "dirty" | "unknown")
        ));
        let commit = value["commit"].as_str().unwrap();
        assert!(
            commit == "unknown"
                || (commit.len() == 40
                    && commit
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        );
    }
}
