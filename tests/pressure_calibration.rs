use peerbrush::{
    brush::Settings,
    engine::{Document, Engine, Scope},
    psd,
    tool_settings::Store,
};
use serde_json::json;

#[test]
fn calibrated_pressure_matches_explicit_shared_force_at_native_depths_and_reopens() {
    for depth in [8, 16] {
        for mask in [false, true] {
            for op in ["paint", "smudge", "clone", "heal"] {
                let mut calibrated = Engine::new();
                calibrated.doc = Document::new_depth(96, 64, depth).unwrap();
                for y in 0..64 {
                    for x in 0..96 {
                        calibrated.doc.layers[0].pixels.set16(
                            x,
                            y,
                            [12001 + x as u16 * 251, 22003 + y as u16 * 113, 32007, 51013],
                        );
                    }
                }
                let layer = calibrated.doc.layers[0].id.clone();
                if mask {
                    calibrated
                        .edit(
                            "human",
                            &[json!({"op":"mask.add","layer":layer,"value":183})],
                            None,
                            None,
                            "Mask",
                        )
                        .unwrap();
                }
                calibrated.undo.clear();
                let before = calibrated.doc.clone();
                let mut explicit = Engine::new();
                explicit.doc = before.clone();
                // Binary-exact endpoints isolate calibration from reassociation rounding.
                let forces = [0.25_f32, 0.375, 0.5, 0.75];
                let mapped = forces.map(|force| ((force - 0.25) / 0.5).clamp(0., 1.));
                let command = json!({"op":op,"layer":layer,"mask":mask,"points":[[45,30],[55,31],[65,33],[75,35]],"source":[20,20],"color":[245,70,10,255],"radius":6,"hardness":0.7,"pressure_min":0.25,"pressure_max":0.75,"pressure_gamma":1.6,"pressures":forces});
                let preview =
                    Engine::preview_edits(before.clone(), std::slice::from_ref(&command)).unwrap();
                calibrated
                    .edit(
                        "human",
                        std::slice::from_ref(&command),
                        Some(before.revision),
                        None,
                        "Calibrated pressure",
                    )
                    .unwrap();
                let mut equivalent = command;
                equivalent["pressure_min"] = json!(0.);
                equivalent["pressure_max"] = json!(1.);
                equivalent["pressures"] = json!(mapped);
                explicit
                    .edit("human", &[equivalent], None, None, "Explicit pressure")
                    .unwrap();
                assert_eq!(
                    calibrated.doc.export_png().unwrap(),
                    explicit.doc.export_png().unwrap(),
                    "{depth} {mask} {op}"
                );
                assert_eq!(
                    calibrated.doc.export_png().unwrap(),
                    preview.export_png().unwrap()
                );
                assert_eq!(calibrated.undo.len(), 1);
                let reopened = psd::decode(&psd::encode(&calibrated.doc).unwrap()).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.bit_depth, depth);
                assert_eq!(
                    reopened.export_png().unwrap(),
                    calibrated.doc.export_png().unwrap()
                );
                calibrated.undo("human").unwrap();
                assert_eq!(
                    calibrated.doc.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                assert_eq!(
                    calibrated.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
            }
        }
    }
}
#[test]
fn invalid_calibration_and_reserved_strokes_preserve_sources_and_history() {
    for (min, max) in [(0.8, 0.2), (0.4, 0.4), (0.9, 0.905), (-0.1, 1.), (0., 1.1)] {
        assert!(Settings::from_command(&json!({"pressure_min":min,"pressure_max":max})).is_err());
    }
    assert!(Settings::from_command(&json!({"pressure_min":0.99,"pressure_max":1.})).is_ok());
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.doc = Document::new_depth(32, 24, depth).unwrap();
        e.doc.layers[0]
            .pixels
            .set16(2, 2, [12345, 23457, 34569, 65535]);
        let before = e.doc.export_png().unwrap();
        let layer = e.doc.layers[0].id.clone();
        let valid = json!({"op":"paint","layer":layer,"points":[[16,12]],"pressures":[0.5],"pressure_min":0.2,"pressure_max":0.8});
        let mut invalid = valid.clone();
        invalid["pressure_max"] = json!(0.1);
        assert!(e
            .edit(
                "human",
                &[valid.clone(), invalid],
                None,
                None,
                "Invalid batch"
            )
            .is_err());
        assert_eq!(e.doc.export_png().unwrap(), before);
        assert!(e.undo.is_empty());
        e.reserve("agent", "Pixel work", vec![Scope::layer(&layer)])
            .unwrap();
        assert!(e.edit("human", &[valid], None, None, "Reserved").is_err());
        assert_eq!(e.doc.export_png().unwrap(), before);
        assert!(e.undo.is_empty());
    }
}
#[test]
fn calibration_preferences_defer_human_disk_writes_restore_and_protect_external_changes() {
    let root = std::env::temp_dir().join(format!("peerbrush-pressure-{}", peerbrush::engine::id()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("tool-settings.json");
    let mut store = Store::load(path.clone());
    let initial = store.get("brush").unwrap().profile;
    let mut calibrated = initial.clone();
    calibrated.settings.pressure_min = 0.2;
    calibrated.settings.pressure_max = 0.85;
    calibrated.settings.pressure_gamma = 0.7;
    store.human_delta("brush", &initial, &calibrated).unwrap();
    assert!(!path.exists());
    assert_eq!(store.get("brush").unwrap().profile, calibrated);
    assert_eq!(store.state()["pending_save"], true);
    store.flush().unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut reloaded = Store::load(path.clone());
    assert!(reloaded.error.is_none());
    assert_eq!(reloaded.get("brush").unwrap().profile, calibrated);
    let mut newer = calibrated.clone();
    newer.settings.radius = 25.;
    store.human_delta("brush", &calibrated, &newer).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "Continuous controls must not flush every frame"
    );
    let mut external = calibrated.clone();
    external.settings.pressure_gamma = 1.8;
    reloaded.set("brush", external, "human", None).unwrap();
    let changed = std::fs::read(&path).unwrap();
    assert!(store.flush().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    assert_eq!(
        store.get("brush").unwrap().profile,
        newer,
        "Unsaved human preferences remain effective"
    );
    let protected = root.join("protected.json");
    std::fs::write(&protected, b"unsupported preferences").unwrap();
    let mut invalid = Store::load(protected.clone());
    assert!(invalid.error.is_some());
    assert!(invalid.set("brush", calibrated, "human", None).is_err());
    assert_eq!(
        std::fs::read(&protected).unwrap(),
        b"unsupported preferences"
    );
    assert!(!String::from_utf8(changed).unwrap().contains("\"actor\""));
    std::fs::remove_dir_all(root).unwrap();
}
