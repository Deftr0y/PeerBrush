use peerbrush::{
    effects,
    engine::{Document, Engine},
    psd, server,
};
use serde_json::json;
use std::collections::HashSet;

#[test]
fn catalog_search_respects_stack_names_categories_and_keywords() {
    let kinds = effects::catalog::ENTRIES
        .iter()
        .filter(|entry| entry.supports(false))
        .map(|entry| entry.kind)
        .collect::<HashSet<_>>();
    assert_eq!(kinds, effects::KINDS.iter().copied().collect());
    assert_eq!(
        effects::catalog::ENTRIES
            .iter()
            .map(|entry| entry.kind)
            .collect::<HashSet<_>>()
            .len(),
        effects::catalog::ENTRIES.len()
    );
    let matches = |mask, query| {
        effects::catalog::ENTRIES
            .iter()
            .filter(|entry| entry.matches(mask, query))
            .map(|entry| entry.kind)
            .collect::<Vec<_>>()
    };
    assert_eq!(matches(false, " hUe SATURATION "), ["hsl"]);
    assert_eq!(matches(false, "glow"), ["bloom"]);
    assert_eq!(matches(true, "feather"), ["blur"]);
    assert!(matches(false, "feather").is_empty());
    assert!(matches(true, "bloom").is_empty());
    assert_eq!(matches(false, "Distortion"), ["liquify"]);
    assert!(effects::catalog::ENTRIES
        .iter()
        .all(|entry| effects::catalog::CATEGORIES.contains(&entry.category)));
    assert_eq!(
        server::capabilities()["effect_catalog"],
        effects::catalog::discovery()
    );
}

#[test]
fn every_discovered_effect_is_editable_undoable_and_native_depth_safe() {
    for depth in [8, 16] {
        for mask in [false, true] {
            for entry in effects::catalog::ENTRIES
                .iter()
                .filter(|entry| entry.supports(mask))
            {
                let mut engine = Engine::new();
                engine.doc = Document::new_depth(8, 8, depth).unwrap();
                if depth == 16 {
                    engine.doc.layers[0]
                        .pixels
                        .set16(2, 3, [12347, 33559, 51237, 45679]);
                } else {
                    engine.doc.layers[0].pixels.set(2, 3, [48, 131, 200, 177]);
                }
                let layer = engine.doc.layers[0].id.clone();
                if mask {
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"mask.add","layer":layer})],
                            None,
                            None,
                            "Mask",
                        )
                        .unwrap();
                }
                let before = engine.doc.clone();
                engine.edit("human", &[json!({"op":if mask {"mask.step.add"} else {"effect.add"},"layer":layer,"kind":entry.kind})], None, None, "Catalog effect").unwrap();
                assert_eq!(engine.doc.bit_depth, depth);
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                let saved = psd::decode(&psd::encode(&engine.doc).unwrap()).unwrap();
                assert!(!saved.read_only, "{} mask={mask}", entry.kind);
                assert_eq!(saved.bit_depth, depth);
                if mask {
                    assert_eq!(
                        saved.layers[0]
                            .mask
                            .as_ref()
                            .unwrap()
                            .steps
                            .last()
                            .unwrap()
                            .kind,
                        entry.kind
                    );
                } else {
                    assert_eq!(saved.layers[0].effects.last().unwrap().kind, entry.kind);
                }
                engine.undo("human").unwrap();
                assert_eq!(
                    serde_json::to_value(&engine.doc.layers).unwrap(),
                    serde_json::to_value(&before.layers).unwrap()
                );
            }
        }
    }
}
