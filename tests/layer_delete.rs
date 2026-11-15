use peerbrush::engine::{Document, Engine, Layer, Scope};
use serde_json::json;

#[test]
fn folder_deletion_rejects_locked_children_and_rolls_back_other_roots() {
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
        let folder = Layer::new("Folder", "group", 8, 8);
        let folder_id = folder.id.clone();
        let mut child = Layer::new("Locked child", "paint", 8, 8);
        child.parent = Some(folder_id.clone());
        child.locked = true;
        let child_id = child.id.clone();
        let other = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
        e.resident_state_mut().expect("resident fixture").document.layers.extend([folder, child]);
        if depth == 16 {
            for l in &mut e.resident_state_mut().expect("resident fixture").document.layers {
                l.pixels.promote16();
            }
        }
        let before = serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap();
        let commands = [
            json!({"op":"layer.delete","layer":other}),
            json!({"op":"layer.delete","layer":folder_id}),
        ];
        assert!(e
            .edit("human", &commands, Some(0), None, "Delete")
            .unwrap_err()
            .contains("children"));
        assert_eq!(serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(), before);
        assert!(e.resident_state().expect("resident fixture").undo.is_empty());
        e.resident_state_mut().expect("resident fixture").document
            .layers
            .iter_mut()
            .find(|l| l.id == child_id)
            .unwrap()
            .locked = false;
        e.reserve("artist", "Paint child", vec![Scope::layer(&child_id)])
            .unwrap();
        let before = serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap();
        assert!(e
            .edit("human", &commands, Some(0), None, "Delete")
            .unwrap_err()
            .contains("Reserved by artist"));
        assert_eq!(serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(), before);
        assert!(e.resident_state().expect("resident fixture").undo.is_empty());
    }
}
