use peerbrush::{
    compatibility::{self, LossCode, Representation, StandardLayout},
    effects::Effect,
    engine::{Document, Engine, Layer, Mask, MaskStep},
    psd,
    raster::Raster,
    server,
    source::{Content, Source},
};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn fixture(depth: u16) -> Document {
    let mut doc = Document::new_depth(8, 6, depth).unwrap();
    for y in 0..6 {
        for x in 0..8 {
            doc.layers[0]
                .pixels
                .set16(x, y, [10001 + x as u16, 20003 + y as u16, 40007, 65535]);
        }
    }
    doc
}

#[test]
fn lossy_saves_require_explicit_acceptance_bound_to_the_current_source() {
    for depth in [8, 16] {
        let root =
            std::env::temp_dir().join(format!("peerbrush-save-loss-{}", peerbrush::engine::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut engine = Engine::new();
        engine.doc = fixture(depth);
        engine.doc.layers[0].effects = vec![effect(true)];
        let document = engine.doc.id.clone();
        let revision = engine.doc.revision;
        let before = serde_json::to_value(&engine.doc).unwrap();
        let shared = Arc::new(Mutex::new(engine));
        let path = root.join("Styled.psd");
        assert!(server::save(&shared, &path).is_err());
        for acceptance in [json!(null), json!(false), json!("true"), json!(1)] {
            let request = json!({"action":"save","actor":"human","path":path,"document_id":document,"expected_revision":revision,"accept_editability_loss":acceptance});
            assert!(server::dispatch(&shared, "document", &request).is_err());
            assert!(!path.exists());
        }
        for request in [
            json!({"action":"save","actor":"human","path":path,"accept_editability_loss":true}),
            json!({"action":"save","actor":"human","path":path,"document_id":document,"expected_revision":revision+1,"accept_editability_loss":true}),
            json!({"action":"save","actor":"human","path":path,"document_id":"other","expected_revision":revision,"accept_editability_loss":true}),
        ] {
            assert!(server::dispatch(&shared, "document", &request).is_err());
            assert!(!path.exists());
        }
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
            before
        );
        assert_eq!(shared.lock().unwrap().saved_revision, 0);
        let report = server::dispatch(
            &shared,
            "document",
            &json!({"action":"save_report","document_id":document,"expected_revision":revision}),
        )
        .unwrap();
        assert!(report["has_editability_losses"].as_bool().unwrap());
        for extension in ["psd", "psb"] {
            let output = root.join(format!("Accepted.{extension}"));
            server::dispatch(&shared,"document",&json!({"action":"save","actor":"human","path":output,"document_id":document,"expected_revision":revision,"accept_editability_loss":true})).unwrap();
            let restored = psd::decode(&std::fs::read(&output).unwrap()).unwrap();
            assert!(!restored.read_only);
            assert_eq!(restored.bit_depth, depth);
            assert_eq!(restored.layers[0].effects.len(), 1);
            assert_eq!(
                restored.export_png().unwrap(),
                shared.lock().unwrap().doc.export_png().unwrap()
            );
        }
        // A completed save grants no continuing permission to later revisions.
        {
            let mut e = shared.lock().unwrap();
            let layer = e.doc.layers[0].id.clone();
            e.edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Later human work"})],
                None,
                None,
                "Later work",
            )
            .unwrap();
        }
        let current_path = shared.lock().unwrap().path.clone().unwrap();
        let saved = std::fs::read(&current_path).unwrap();
        assert!(server::dispatch(&shared,"document",&json!({"action":"save","actor":"human","document_id":document,"expected_revision":revision,"accept_editability_loss":true})).is_err());
        assert_eq!(std::fs::read(&current_path).unwrap(), saved);
        drop(shared);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn save_acceptance_keeps_plain_saves_and_protected_and_overwrite_guards() {
    for depth in [8, 16] {
        let root =
            std::env::temp_dir().join(format!("peerbrush-save-policy-{}", peerbrush::engine::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut engine = Engine::new();
        engine.doc = fixture(depth);
        let shared = Arc::new(Mutex::new(engine));
        let plain = root.join("Plain.psd");
        server::save(&shared, &plain).unwrap();
        let original = std::fs::read(&plain).unwrap();
        let existing = root.join("Existing.psb");
        std::fs::write(&existing, &original).unwrap();
        let (document, revision) = {
            let mut engine = shared.lock().unwrap();
            let layer = engine.doc.layers[0].id.clone();
            engine
                .edit(
                    "human",
                    &[json!({"op":"effect.add","layer":layer,"kind":"invert","settings":{}})],
                    None,
                    None,
                    "Effect requiring review",
                )
                .unwrap();
            (engine.doc.id.clone(), engine.doc.revision)
        };
        let request = json!({"action":"save","path":existing,"actor":"human","document_id":document,"expected_revision":revision,"accept_editability_loss":true});
        assert!(server::dispatch(&shared, "document", &request)
            .unwrap_err()
            .contains("overwrite"));
        assert_eq!(std::fs::read(&existing).unwrap(), original);
        {
            let mut engine = shared.lock().unwrap();
            engine.doc.read_only = true;
            engine.doc.warnings = vec!["Unsupported Photoshop source".into()];
        }
        let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
        let mut protected_request = request;
        protected_request["overwrite"] = json!(true);
        assert!(server::dispatch(&shared, "document", &protected_request)
            .unwrap_err()
            .contains("unsupported"));
        assert_eq!(std::fs::read(&existing).unwrap(), original);
        assert_eq!(std::fs::read(&plain).unwrap(), original);
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
            before
        );
        drop(shared);
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn effect(enabled: bool) -> Effect {
    Effect {
        id: peerbrush::engine::id(),
        kind: "invert".into(),
        enabled,
        weight: 1.,
        settings: json!({}),
    }
}
fn ordinary(bytes: &[u8]) -> Vec<u8> {
    let color = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let at = 30 + color;
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let mut out = bytes[..at].to_vec();
    out.extend([0; 4]);
    out.extend(&bytes[at + 4 + size..]);
    out
}

#[test]
fn report_matches_psd_and_psb_standard_folder_records_without_mutating_sources() {
    for depth in [8, 16] {
        let mut doc = fixture(depth);
        let mut group = Layer::new("Folder", "group", 8, 6);
        group.pixels = Raster::new_depth(8, 6, depth);
        doc.layers[0].parent = Some(group.id.clone());
        doc.layers.insert(0, group);
        let before = serde_json::to_value(&doc).unwrap();
        let report = compatibility::report(&doc).unwrap();
        assert_eq!(report.standard_layout, StandardLayout::LayerTree);
        assert_eq!(report.standard_records, 3);
        assert_eq!(report.standard_layer_count, 2);
        assert!(!report.has_editability_losses);
        assert!(report.losses.is_empty());
        assert_eq!(report.layers[0].standard, Representation::Folder);
        assert_eq!(report.layers[1].standard, Representation::Raster);
        assert_eq!(report.layers[0].standard_bounds, Some([0; 4]));
        assert_eq!(report.layers[1].standard_bounds, Some([0, 0, 8, 6]));
        for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let decoded = psd::decode(&ordinary(&encoded)).unwrap();
            assert!(!decoded.read_only);
            assert_eq!(decoded.layers.len(), 2);
            assert_eq!(decoded.layers[0].kind, "group");
            assert_eq!(
                decoded.layers[1].pixels.rgba16(),
                doc.layers[1].pixels.rgba16()
            );
            assert_eq!(decoded.export_png().unwrap(), doc.export_png().unwrap());
        }
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }
}

#[test]
fn group_effect_report_identifies_collapsed_children_and_disabled_source_only_settings() {
    for depth in [8, 16] {
        for enabled in [false, true] {
            let mut doc = fixture(depth);
            let mut group = Layer::new("Styled folder", "group", 8, 6);
            group.pixels = Raster::new_depth(8, 6, depth);
            group.effects = vec![effect(enabled)];
            let group_id = group.id.clone();
            doc.layers[0].parent = Some(group_id.clone());
            doc.layers.insert(0, group);
            let report = compatibility::report(&doc).unwrap();
            assert!(report.has_editability_losses);
            assert_eq!(report.standard_records, if enabled { 1 } else { 3 });
            assert_eq!(
                report.layers[0].standard,
                if enabled {
                    Representation::BakedFolderRaster
                } else {
                    Representation::Folder
                }
            );
            assert_eq!(
                report.layers[1].standard,
                if enabled {
                    Representation::BakedIntoFolder
                } else {
                    Representation::Raster
                }
            );
            assert_eq!(
                report.layers[1].standard_container,
                if enabled { Some(group_id) } else { None }
            );
            assert!(report.losses.iter().any(|loss| loss.code
                == if enabled {
                    LossCode::FolderBaked
                } else {
                    LossCode::DisabledEffectsPrivate
                }));
            let encoded = psd::encode(&doc).unwrap();
            let decoded = psd::decode(&ordinary(&encoded)).unwrap();
            assert_eq!(decoded.layers.len(), if enabled { 1 } else { 2 });
            assert_eq!(report.standard_layer_count, decoded.layers.len());
            assert_eq!(
                report.layers[0].standard_bounds,
                Some(if enabled { [0, 0, 8, 6] } else { [0; 4] })
            );
            assert_eq!(
                report.layers[1].standard_bounds,
                if enabled { None } else { Some([0, 0, 8, 6]) }
            );
            assert_eq!(
                decoded.layers[0].kind,
                if enabled { "paint" } else { "group" }
            );
            assert_eq!(decoded.export_png().unwrap(), doc.export_png().unwrap());
            let restored = psd::decode(&encoded).unwrap();
            assert_eq!(restored.layers.len(), 2);
            assert_eq!(restored.layers[0].effects[0].enabled, enabled);
        }
    }
}

#[test]
fn filters_and_adjustments_report_the_actual_visible_composite_and_hidden_source_choices() {
    for depth in [8, 16] {
        for filtered in [false, true] {
            let mut doc = fixture(depth);
            if filtered {
                doc.filters = vec![effect(true)];
            } else {
                let mut adjustment = Layer::new("Adjustment", "adjustment", 8, 6);
                adjustment.pixels = Raster::new_depth(8, 6, depth);
                adjustment.effects = vec![effect(true)];
                doc.layers.insert(0, adjustment);
            }
            let report = compatibility::report(&doc).unwrap();
            assert_eq!(
                report.standard_layout,
                if filtered {
                    StandardLayout::CompositeAndHiddenSources
                } else {
                    StandardLayout::CompositeOnly
                }
            );
            assert_eq!(report.standard_records, if filtered { 2 } else { 1 });
            assert!(report.losses.iter().any(|loss| loss.code
                == if filtered {
                    LossCode::FiltersBaked
                } else {
                    LossCode::DocumentFlattened
                }));
            let standard = psd::decode(&ordinary(&psd::encode(&doc).unwrap())).unwrap();
            assert_eq!(standard.layers.len(), if filtered { 2 } else { 1 });
            assert_eq!(report.standard_layer_count, standard.layers.len());
            if filtered {
                assert!(!standard.layers[1].visible);
            }
            assert_eq!(standard.export_png().unwrap(), doc.export_png().unwrap());
        }
    }
}

#[test]
fn editable_vector_masks_effects_and_opacity_losses_are_specific_and_reports_are_source_bound() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = fixture(depth);
        let source = Source {
            width: 8,
            height: 6,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content::Shape {
                shape: "rectangle".into(),
                bounds: [1., 1., 7., 5.],
                points: vec![],
                closed: true,
                fill: [12345, 23457, 34569, 65535],
                stroke: [60001, 50003, 40007, 65535],
                stroke_width: 1.,
            },
        };
        engine
            .edit(
                "human",
                &[json!({"op":"source.add","source":source})],
                None,
                None,
                "Vector source",
            )
            .unwrap();
        let layer = &mut engine.doc.layers[0];
        layer.opacity = 0.5;
        layer.effects = vec![effect(true)];
        layer.mask = Some(Mask {
            enabled: true,
            cache_key: peerbrush::engine::id(),
            steps: vec![MaskStep {
                id: peerbrush::engine::id(),
                kind: "fill".into(),
                enabled: true,
                weight: 1.,
                value: 183.,
                pixels: Raster::new_depth(8, 6, depth),
                settings: json!(null),
            }],
        });
        let before = serde_json::to_value(&engine.doc).unwrap();
        let revision = engine.doc.revision;
        let document = engine.doc.id.clone();
        let shared = Arc::new(Mutex::new(engine));
        let report = compatibility::report(&shared.lock().unwrap().doc).unwrap();
        for code in [
            LossCode::EditableSourceRasterized,
            LossCode::MaskStackBaked,
            LossCode::EffectsBaked,
            LossCode::OpacityQuantized,
        ] {
            assert!(
                report.losses.iter().any(|loss| loss.code == code),
                "{code:?}"
            );
        }
        let request =
            json!({"action":"save_report","document_id":document,"expected_revision":revision});
        let mut observed = server::dispatch(&shared, "document", &request).unwrap();
        assert_eq!(observed["project_id"], shared.lock().unwrap().project_id);
        observed.as_object_mut().unwrap().remove("project_id");
        assert_eq!(observed, serde_json::to_value(&report).unwrap());
        for bad in [
            json!({"action":"save_report"}),
            json!({"action":"save_report","document_id":document,"expected_revision":revision+1}),
            json!({"action":"save_report","document_id":"other","expected_revision":revision}),
        ] {
            assert!(server::dispatch(&shared, "document", &bad).is_err());
        }
        assert_eq!(
            serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
            before
        );
        assert_eq!(shared.lock().unwrap().undo.len(), 1);
        let mut protected = fixture(depth);
        protected.read_only = true;
        protected.warnings = vec!["Unsupported Photoshop feature: lmfx".into()];
        let report = compatibility::report(&protected).unwrap();
        assert!(!report.save_allowed);
        assert_eq!(report.standard_layout, StandardLayout::Protected);
        assert_eq!(report.protected_reasons, protected.warnings);
        assert_eq!(report.standard_records, 0);
    }
}
