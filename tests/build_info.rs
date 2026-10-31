#[test]
fn build_info_matches_the_embedded_record_without_opening_a_workspace() {
    let prefix = b"{\"format\":\"PeerBrushBuildInfoV1\",";
    assert_eq!(
        std::fs::read(env!("CARGO_BIN_EXE_peerbrush"))
            .unwrap()
            .windows(prefix.len())
            .filter(|bytes| *bytes == prefix)
            .count(),
        1,
        "About and CLI must share one embedded provenance record"
    );
    let unused = std::env::temp_dir().join(format!(
        "peerbrush-build-info-unused-{}",
        peerbrush::engine::id()
    ));
    assert!(!unused.exists());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_peerbrush"))
        .arg("--build-info")
        .env("TEMP", &unused)
        .env("TMP", &unused)
        .env("TMPDIR", &unused)
        .env(
            "PEERBRUSH_BUILD_COMMIT",
            "0000000000000000000000000000000000000000",
        )
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(!unused.exists());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        peerbrush::build_info::json()
    );
}
#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;
