use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("peerbrush-cli-test-{}", crate::engine::id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let root = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            root.parent().unwrap(),
            std::fs::canonicalize(std::env::temp_dir()).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn streamed_cli_output_and_image_paths_match_the_existing_value_projection() {
    let state = Fixture::new();
    let bytes: Vec<u8> = (0..1024 * 1024 + 7).map(|n| (n * 37) as u8).collect();
    let input = json!({"ok":true,"result":{"document":{"label":"Native \u{1f642}\n\"\\","data":"ordinary data remains metadata"},"images":[{"coordinate_space":"document","data":STANDARD.encode(&bytes),"document_rect":[-2,3,11,10],"height":7,"mime_type":"image/png","path":"replaced old metadata","revision":17,"width":13}],"revision":17}});
    let mut out = Vec::new();
    let mut error = Vec::new();
    assert!(matches!(
        consume(
            input.to_string().as_bytes(),
            &state.0,
            &mut out,
            &mut error,
            &mut || Ok(())
        )
        .unwrap(),
        CliStatus::Success
    ));
    assert!(error.is_empty());
    let actual: Value = serde_json::from_slice(&out).unwrap();
    let path = actual["result"]["images"][0]["path"].as_str().unwrap();
    assert_eq!(Path::new(path).parent().unwrap(), state.0.join("previews"));
    assert_eq!(Path::new(path).extension().unwrap(), "png");
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    let mut expected = input;
    expected["result"]["images"][0]
        .as_object_mut()
        .unwrap()
        .remove("data");
    expected["result"]["images"][0]["path"] = json!(path);
    assert_eq!(
        out,
        format!("{}\n", serde_json::to_string_pretty(&expected).unwrap()).as_bytes()
    );
    assert_eq!(
        std::fs::read_dir(state.0.join("previews")).unwrap().count(),
        1
    );
}
#[test]
fn malformed_truncated_base64_and_cancellation_never_publish_or_emit_partial_json() {
    let state = Fixture::new();
    for input in [
        r#"{"ok":true,"result":{"images":[{"data":"AA=="}]}}tail"#,
        r#"{"ok":true,"result":{"images":[{"data":"AA=A"}]}}"#,
        r#"{"ok":true,"result":{"images":[{"data":"AA=="},{"data":"!"}]}}"#,
        r#"{"ok":true,"result":{"images":[{"data":"AA=="}]}"#,
        r#"{"ok":true,"result":{"images":[{"data":null}]}}"#,
        r#"{"ok":true,"result":{"images":[{"data":"AA==","data":"AA=="}]}}"#,
        r#"{"ok":true,"x":"\uD800"}"#,
    ] {
        let (mut out, mut error) = (Vec::new(), Vec::new());
        assert!(consume(
            input.as_bytes(),
            &state.0,
            &mut out,
            &mut error,
            &mut || Ok(())
        )
        .is_err());
        assert!(out.is_empty());
        assert!(error.is_empty());
        assert!(
            !state.0.join("previews").exists()
                || std::fs::read_dir(state.0.join("previews"))
                    .unwrap()
                    .next()
                    .is_none()
        );
    }
    let (mut out, mut error) = (Vec::new(), Vec::new());
    assert!(consume(
        b"{\"ok\":true}".as_slice(),
        &state.0,
        &mut out,
        &mut error,
        &mut || Err("canceled".into())
    )
    .is_err());
    assert!(out.is_empty());
}
#[test]
fn failure_status_unicode_escaping_empty_arrays_and_long_keys_keep_wire_semantics() {
    let state = Fixture::new();
    for input in [
        json!({"ok":false,"error":"Exact \u{1f642}\nerror"}),
        json!({"ok":false,"error":null}),
        json!({"ok":true,"result":{"images":[],"unknown":{"a very long metadata key that stays fully streamed":"escaped \u{e9}\t"}}}),
    ] {
        let (mut out, mut error) = (Vec::new(), Vec::new());
        let status = consume(
            input.to_string().as_bytes(),
            &state.0,
            &mut out,
            &mut error,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(
            out,
            format!("{}\n", serde_json::to_string_pretty(&input).unwrap()).as_bytes()
        );
        if input["ok"] == false {
            assert!(matches!(status, CliStatus::Failed));
            assert_eq!(
                error,
                format!(
                    "PeerBrush: {}\n",
                    input["error"].as_str().unwrap_or("Command failed")
                )
                .as_bytes()
            );
        } else {
            assert!(matches!(status, CliStatus::Success));
            assert!(error.is_empty());
        }
    }
}
