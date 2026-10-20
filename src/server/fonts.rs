//! Bounded, path-free font discovery shared by RPC, CLI and MCP.
use crate::fonts::{self, Catalog, Face, TextFont};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    List {
        #[serde(default)]
        search: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    Prepare {
        font: TextFont,
    },
    Refresh,
}
fn page_size() -> usize {
    32
}

fn bounds(search: &str, offset: usize, limit: usize) -> Result<(), String> {
    if search.len() > 512
        || search.chars().count() > 128
        || search.chars().any(char::is_control)
        || offset > 8192
        || !(1..=128).contains(&limit)
    {
        return Err(
            "Font search requires at most 128 characters, offset 0–8192 and limit 1–128".into(),
        );
    }
    Ok(())
}
fn page(catalog: Catalog, search: &str, offset: usize, limit: usize) -> Result<Value, String> {
    bounds(search, offset, limit)?;
    let search = search.to_lowercase();
    let matches: Vec<&Face> = catalog
        .faces
        .iter()
        .filter(|face| {
            search.is_empty()
                || face.family.to_lowercase().contains(&search)
                || face.postscript.to_lowercase().contains(&search)
        })
        .collect();
    let total = matches.len();
    let faces: Vec<_> = matches.into_iter().skip(offset).take(limit).collect();
    Ok(
        json!({"schema":1,"generation":catalog.generation,"status":catalog.status,
        "fonts":faces,"total":total,"offset":offset,
        "next_offset":(offset.saturating_add(limit) < total).then_some(offset + limit)}),
    )
}

pub(super) fn request(value: &Value) -> Result<Value, String> {
    // Deserialize before starting discovery, including action-specific unknown fields.
    match serde_json::from_value(value.clone()).map_err(|_| "Invalid font request")? {
        Request::List {
            search,
            offset,
            limit,
        } => {
            bounds(&search, offset, limit)?;
            page(fonts::catalog(), &search, offset, limit)
        }
        Request::Prepare { font } => {
            font.validate().map_err(|error| error.to_string())?;
            Ok(match fonts::snapshot(&font) {
                Ok(_) => json!({"schema":1,"ready":true,"font":font}),
                Err(error) => {
                    json!({"schema":1,"ready":false,"status":error,"message":error.to_string(),"font":font})
                }
            })
        }
        Request::Refresh => match fonts::refresh() {
            Ok(()) => Ok(json!({"schema":1,"status":"loading"})),
            Err(error) => Ok(json!({"schema":1,"status":error,"message":error.to_string()})),
        },
    }
}

pub(super) fn tool() -> Value {
    let font = json!({"oneOf":[
        {"type":"string","enum":["regular","medium","semibold"]},
        {"type":"object","properties":{
            "family":{"type":"string","minLength":1,"maxLength":512},
            "postscript":{"type":"string","minLength":1,"maxLength":512},
            "style":{"type":"string","enum":["normal","italic","oblique"]},
            "weight":{"type":"integer","minimum":1,"maximum":1000},
            "stretch":{"type":"integer","minimum":1,"maximum":9},
            "index":{"type":"integer","minimum":0,"maximum":255},
            "sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"}},
         "required":["family","postscript","style","weight","stretch","index","sha256"],"additionalProperties":false}
    ]});
    json!({"name":"peerbrush_fonts","description":"Inspect installed document fonts without changing a project. list returns bounded metadata with exact font references or explicit unsupported reasons; paths and font bytes are private. prepare queues an exact font snapshot; poll until ready before source.add/update. Missing or changed fonts retain saved pixels and require an explicit replacement. refresh rebuilds only the derived catalog. No registration, download, text shaping or automatic substitution.",
        "inputSchema":{"type":"object","properties":{
            "action":{"type":"string","enum":["list","prepare","refresh"]},
            "search":{"type":"string","maxLength":128},"offset":{"type":"integer","minimum":0,"maximum":8192},
            "limit":{"type":"integer","minimum":1,"maximum":128},"font":font},
         "required":["action"],"additionalProperties":false,
         "oneOf":[{"properties":{"action":{"const":"list"}},"not":{"required":["font"]}},
            {"properties":{"action":{"const":"prepare"}},"required":["font"],"not":{"anyOf":[{"required":["search"]},{"required":["offset"]},{"required":["limit"]}]}},
            {"properties":{"action":{"const":"refresh"}},"not":{"anyOf":[{"required":["font"]},{"required":["search"]},{"required":["offset"]},{"required":["limit"]}]}}]}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::{CatalogStatus, FontError};
    use std::sync::Arc;

    #[test]
    fn pagination_preserves_explicit_unsupported_rows_without_paths() {
        let faces: Vec<_> = (0..300)
            .map(|index| Face {
                family: format!("Native family{index:03}"),
                postscript: format!("Native{index:03}"),
                font: None,
                error: Some(FontError::Variable),
            })
            .collect();
        let catalog = Catalog {
            generation: 7,
            status: CatalogStatus::Ready,
            faces: Arc::from(faces),
        };
        let first = page(catalog.clone(), "FAMILY", 0, 128).unwrap();
        assert_eq!(first["fonts"].as_array().unwrap().len(), 128);
        assert_eq!(first["total"], 300);
        assert_eq!(first["next_offset"], 128);
        assert_eq!(first["fonts"][0]["error"], "variable");
        let last = page(catalog.clone(), "Native", 256, 128).unwrap();
        assert_eq!(last["fonts"].as_array().unwrap().len(), 44);
        assert!(last["next_offset"].is_null());
        assert_eq!(page(catalog.clone(), "absent", 0, 32).unwrap()["total"], 0);
        assert!(page(catalog.clone(), "", 8193, 32).is_err());
        assert!(page(catalog.clone(), "", 0, 129).is_err());
        assert!(page(catalog, "\n", 0, 32).is_err());
        assert!(!first.to_string().contains("path"));
    }

    #[test]
    fn invalid_or_path_bearing_queries_fail_before_discovery_and_prepare_uses_exact_snapshot() {
        for value in [
            json!({"action":"list","limit":0}),
            json!({"action":"list","limit":129}),
            json!({"action":"list","offset":8193}),
            json!({"action":"list","search":"x".repeat(129)}),
            json!({"action":"list","search":"\u{0000}"}),
            json!({"action":"list","path":"font.ttf"}),
            json!({"action":"list","font":"regular"}),
            json!({"action":"refresh","search":"x"}),
            json!({"action":"prepare","font":"unknown"}),
            json!({"action":"prepare","font":{"path":"font.ttf"}}),
            json!({"action":"prepare","font":"regular","limit":1}),
            json!({"action":"unknown"}),
        ] {
            assert!(request(&value).is_err(), "{value}");
        }
        assert_eq!(
            request(&json!({"action":"prepare","font":"regular"})).unwrap()["ready"],
            true
        );
        let font = fonts::InstalledFont {
            family: "Fixture".into(),
            postscript: "Fixture-Regular".into(),
            style: "normal".into(),
            weight: 400,
            stretch: 5,
            index: 0,
            sha256: "a".repeat(64),
        };
        let guard = fonts::test_fonts(vec![(font.clone(), Err(FontError::Changed))]);
        let result = request(&json!({"action":"prepare","font":font})).unwrap();
        assert_eq!(result["ready"], false);
        assert_eq!(result["status"], "changed");
        assert_eq!(guard.requests(), 1);
    }
}
