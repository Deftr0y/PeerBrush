use peerbrush::{
    clipboard::{self, Image},
    engine::{Document, Engine},
};
use serde_json::json;
fn setup() -> (Engine, String) {
    let mut e = Engine::new();
    e.doc = Document::new(32, 24).unwrap();
    let id = e.doc.layers[0].id.clone();
    e.doc.layers[0].pixels.set(5, 6, [200, 70, 20, 255]);
    (e, id)
}
#[test]
fn copy_area_bakes_color_and_mask_then_pastes_at_its_position_with_one_undo() {
    let (mut e, id) = setup();
    e.edit(
        "human",
        &[
            json!({"op":"mask.add","layer":id,"value":128}),
            json!({"op":"effect.add","layer":id,"kind":"invert"}),
        ],
        None,
        None,
        "setup",
    )
    .unwrap();
    e.doc.selection = Some([4, 5, 8, 9]);
    e.undo.clear();
    let before = e.doc.clone();
    let image = Image::copy(&e.doc, &id, false, false).unwrap();
    assert_eq!(
        (image.width, image.height, image.origin),
        (4, 4, Some([4, 5]))
    );
    assert_eq!(&image.bytes[20..24], &[55, 185, 235, 128]);
    let result = e
        .edit("human", &[image.command(&id).unwrap()], None, None, "Paste")
        .unwrap();
    assert_eq!(result["created"].as_array().unwrap().len(), 2);
    let folder = &e.doc.layers[0];
    let pasted = &e.doc.layers[1];
    assert_eq!(folder.kind, "group");
    assert_eq!(pasted.kind, "paint");
    assert_eq!(pasted.parent.as_deref(), Some(folder.id.as_str()));
    assert_eq!(e.doc.layers[2].parent, pasted.parent);
    assert_eq!((pasted.x, pasted.y), (4, 5));
    assert_eq!(pasted.pixels.get(1, 1), [55, 185, 235, 128]);
    assert!(e.doc.selection.is_none());
    assert_eq!(e.undo.len(), 1);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers.len(), 1);
    assert!(e.doc.layers[0].parent.is_none());
    assert_eq!(e.doc.selection, before.selection);
    e.redo("human").unwrap();
    assert_eq!(e.doc.layers.len(), 3);
    let round = peerbrush::psd::decode(&peerbrush::psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(round.layers[1].pixels.get(1, 1), [55, 185, 235, 128]);
}
#[test]
fn external_image_centers_in_selected_folder_and_sibling_paste_keeps_parent() {
    let (mut e, id) = setup();
    e.edit(
        "human",
        &[json!({"op":"layer.add","kind":"group","name":"Folder"})],
        None,
        None,
        "Folder",
    )
    .unwrap();
    let group = e.doc.layers[0].id.clone();
    let image = Image {
        width: 2,
        height: 2,
        bytes: vec![100; 16],
        origin: None,
        samples16: None,
    };
    e.edit(
        "human",
        &[image.command(&group).unwrap()],
        None,
        None,
        "Paste",
    )
    .unwrap();
    let pasted = e.doc.layers[1].id.clone();
    assert_eq!((e.doc.layers[1].x, e.doc.layers[1].y), (15, 11));
    assert_eq!(e.doc.layers[1].parent.as_deref(), Some(group.as_str()));
    e.edit(
        "human",
        &[image.command(&pasted).unwrap()],
        None,
        None,
        "Paste",
    )
    .unwrap();
    assert_eq!(e.doc.layers.len(), 4);
    assert_eq!(e.doc.layers[1].parent.as_deref(), Some(group.as_str()));
    assert!(e
        .doc
        .layers
        .iter()
        .find(|l| l.id == id)
        .unwrap()
        .parent
        .is_none());
}
#[test]
fn paste_obeys_locks_reservations_revisions_and_rolls_back_invalid_pixels() {
    let (mut e, id) = setup();
    let image = Image {
        width: 2,
        height: 2,
        bytes: vec![255; 16],
        origin: None,
        samples16: None,
    };
    let command = image.command(&id).unwrap();
    e.doc.layers[0].locked = true;
    assert!(e
        .edit("human", &[command.clone()], None, None, "Paste")
        .is_err());
    e.doc.layers[0].locked = false;
    let lease = e
        .reserve("AI", "paint", vec![peerbrush::engine::Scope::layer(&id)])
        .unwrap();
    assert!(e
        .edit("human", &[command.clone()], None, None, "Paste")
        .is_err());
    e.leases.retain(|l| l.id != lease.id);
    assert!(e
        .edit(
            "human",
            &[json!({"op":"image.paste","layer":id,"png":"bad"})],
            None,
            None,
            "Paste"
        )
        .is_err());
    assert_eq!(e.doc.layers.len(), 1);
    assert_eq!(e.doc.revision, 0);
    assert!(e.undo.is_empty());
    e.edit(
        "AI",
        &[json!({"op":"layer.update","layer":id,"opacity":0.5})],
        None,
        None,
        "AI",
    )
    .unwrap();
    assert!(e.edit("human", &[command], Some(0), None, "Paste").is_err());
    assert_eq!(e.doc.layers.len(), 1);
}
#[test]
fn merged_copy_uses_composite_and_rejects_an_empty_selection() {
    let (mut e, id) = setup();
    e.doc.selection = Some([4, 5, 8, 9]);
    let copied = Image::copy(&e.doc, &id, false, true).unwrap();
    assert_eq!(
        copied.bytes,
        e.doc.preview(e.doc.selection, 8192, None, false).unwrap().2
    );
    e.doc.selection = Some([4, 5, 4, 9]);
    assert!(Image::copy(&e.doc, &id, false, false).is_err());
    // Validate arbitrary incoming bytes before any source grouping is changed.
    let mut doc = e.doc.clone();
    assert!(clipboard::paste(
        &mut doc,
        &json!({"op":"image.paste","layer":id,"png":"bad"})
    )
    .is_err());
    assert_eq!(doc.layers.len(), 1);
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "Requires a native OS clipboard; manually run with --ignored. Restores prior text/image."]
fn native_image_clipboard_copy_and_external_paste_roundtrip() {
    use std::{
        borrow::Cow,
        sync::{Arc, Mutex},
        time::Duration,
    };
    struct Restore {
        clipboard: arboard::Clipboard,
        image: Option<arboard::ImageData<'static>>,
        text: Option<String>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(image) = self.image.take() {
                let _ = self.clipboard.set_image(image);
            } else if let Some(text) = self.text.take() {
                let _ = self.clipboard.set_text(text);
            } else {
                let _ = self.clipboard.clear();
            }
        }
    }
    let mut native = arboard::Clipboard::new().unwrap();
    let image = native.get_image().ok();
    let text = native.get_text().ok();
    let mut restore = Restore {
        clipboard: native,
        image,
        text,
    };
    let (mut e, id) = setup();
    e.doc.selection = Some([4, 5, 8, 9]);
    let expected = Image::copy(&e.doc, &id, false, false).unwrap();
    let worker = clipboard::Worker::new(eframe::egui::Context::default());
    worker
        .request(clipboard::Request::Copy {
            doc: e.doc.clone(),
            target: id.clone(),
            mask: false,
            merged: false,
        })
        .unwrap();
    worker
        .replies
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .result
        .unwrap();
    let read = restore.clipboard.get_image().unwrap();
    assert_eq!((read.width, read.height), (4, 4));
    assert_eq!(read.bytes.as_ref(), expected.bytes);
    // Emulate a picture copied in another application, with no accompanying text.
    let pixels = vec![
        120, 200, 20, 255, 30, 40, 50, 255, 230, 10, 100, 255, 25, 50, 70, 255, 10, 70, 90, 255,
        50, 200, 30, 255,
    ];
    restore
        .clipboard
        .set_image(arboard::ImageData {
            width: 3,
            height: 2,
            bytes: Cow::Borrowed(&pixels),
        })
        .unwrap();
    let shared = Arc::new(Mutex::new(e));
    worker
        .request(clipboard::Request::Paste {
            shared: shared.clone(),
            target: id,
            revision: 0,
        })
        .unwrap();
    let reply = worker.replies.recv_timeout(Duration::from_secs(5)).unwrap();
    reply.result.unwrap();
    let e = shared.lock().unwrap();
    let layer = e
        .doc
        .layers
        .iter()
        .find(|l| Some(&l.id) == reply.selected.as_ref())
        .unwrap();
    assert_eq!((layer.x, layer.y), (14, 11));
    assert_eq!(layer.pixels.rgba(), pixels);
    assert_eq!(e.undo.len(), 1);
}
