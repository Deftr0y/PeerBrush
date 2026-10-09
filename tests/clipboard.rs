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
#[path = "support/isolated_clipboard.rs"]
mod isolated_clipboard;
#[cfg(target_os = "windows")]
#[test]
fn windows_clipboard_transports_preserve_pixels_precision_ownership_and_atomic_paste() {
    use std::{
        borrow::Cow,
        sync::{Arc, Mutex},
    };
    if isolated_clipboard::child(
        "windows_clipboard_transports_preserve_pixels_precision_ownership_and_atomic_paste",
    ) {
        return;
    }
    let station = isolated_clipboard::Station::new();
    let mut native = arboard::Clipboard::new().unwrap();
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
    station.receive(&worker.replies).result.unwrap();
    let read = native.get_image().unwrap();
    assert_eq!((read.width, read.height), (4, 4));
    assert_eq!(read.bytes.as_ref(), expected.bytes);
    // Emulate a picture copied in another application, with no accompanying text.
    let pixels = vec![
        120, 200, 20, 255, 30, 40, 50, 255, 230, 10, 100, 255, 25, 50, 70, 255, 10, 70, 90, 255,
        50, 200, 30, 255,
    ];
    native
        .set_image(arboard::ImageData {
            width: 3,
            height: 2,
            bytes: Cow::Borrowed(&pixels),
        })
        .unwrap();
    let shared = Arc::new(Mutex::new(e));
    let document = shared.lock().unwrap().doc.id.clone();
    worker
        .request(clipboard::Request::Paste {
            document,
            shared: shared.clone(),
            target: id,
            revision: 0,
        })
        .unwrap();
    let reply = station.receive(&worker.replies);
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
    drop(e);
    let paste = |target: &str| {
        let e = shared.lock().unwrap();
        worker
            .request(clipboard::Request::Paste {
                document: e.doc.id.clone(),
                shared: shared.clone(),
                target: target.into(),
                revision: e.doc.revision,
            })
            .unwrap();
        drop(e);
        station.receive(&worker.replies)
    };
    let undo = || {
        shared.lock().unwrap().undo("human").unwrap();
    };
    let root = shared
        .lock()
        .unwrap()
        .doc
        .layers
        .iter()
        .find(|l| l.name == "Paint 1")
        .unwrap()
        .id
        .clone();
    undo();
    let baseline = shared.lock().unwrap().doc.clone();

    // An old 24-bit CF_DIB bitmap, without a file header or registered PNG.
    let mut dib = vec![0u8; 40];
    dib[..4].copy_from_slice(&40u32.to_le_bytes());
    dib[4..8].copy_from_slice(&3i32.to_le_bytes());
    dib[8..12].copy_from_slice(&2i32.to_le_bytes());
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&24u16.to_le_bytes());
    for row in pixels.chunks_exact(12).rev() {
        for rgba in row.chunks_exact(4) {
            dib.extend([rgba[2], rgba[1], rgba[0]]);
        }
        dib.extend([0; 3]);
    }
    station.set_raw(clipboard_win::formats::CF_DIB, &dib);
    let reply = paste(&root);
    reply.result.unwrap();
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .find(|l| Some(&l.id) == reply.selected.as_ref())
            .unwrap()
            .pixels
            .rgba(),
        pixels
    );
    undo();

    // Actual registered PNG16 transport promotes an 8-bit destination, including
    // existing pixels/masks, rather than projecting the incoming words to 8 bit.
    let words = vec![
        12347, 23459, 34571, 65535, 44321, 33219, 22117, 40001, 12347, 23459, 34571, 65535, 44321,
        33219, 22117, 40001,
    ];
    let png = peerbrush::raster::png16(2, 2, &words).unwrap();
    station.set_raw(clipboard_win::register_format("PNG").unwrap().get(), &png);
    let reply = paste(&root);
    reply.result.unwrap();
    {
        let e = shared.lock().unwrap();
        assert_eq!(e.doc.bit_depth, 16);
        assert!(e.doc.layers.iter().all(|l| l.pixels.depth == 16));
        let l = e
            .doc
            .layers
            .iter()
            .find(|l| Some(&l.id) == reply.selected.as_ref())
            .unwrap();
        assert_eq!(l.pixels.rgba16(), words);
        assert_eq!((l.x, l.y), (15, 11));
        let loaded = peerbrush::psd::decode(&peerbrush::psd::encode(&e.doc).unwrap()).unwrap();
        assert_eq!(loaded.export_png().unwrap(), e.doc.export_png().unwrap());
    }
    undo();
    assert_eq!(
        shared.lock().unwrap().doc.export_png().unwrap(),
        baseline.export_png().unwrap()
    );
    assert_eq!(shared.lock().unwrap().doc.bit_depth, 8);

    // Explorer-style image file copying uses CF_HDROP, including batches.
    let file =
        std::env::temp_dir().join(format!("peerbrush-clipboard-{}.png", uuid::Uuid::new_v4()));
    let bad =
        std::env::temp_dir().join(format!("peerbrush-clipboard-{}.png", uuid::Uuid::new_v4()));
    std::fs::write(&file, &png).unwrap();
    std::fs::write(&bad, b"invalid PNG").unwrap();
    native.set().file_list(&[&file]).unwrap();
    let reply = paste(&root);
    reply.result.unwrap();
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .find(|l| Some(&l.id) == reply.selected.as_ref())
            .unwrap()
            .pixels
            .rgba16(),
        words
    );
    undo();
    native.set().file_list(&[&file, &file]).unwrap();
    let reply = paste(&root);
    reply.result.unwrap();
    assert_eq!(reply.selected_layers.len(), 2);
    assert_eq!(shared.lock().unwrap().undo.len(), 1);
    assert!(reply.selected_layers.iter().all(|id| shared
        .lock()
        .unwrap()
        .doc
        .layers
        .iter()
        .find(|l| l.id == *id)
        .unwrap()
        .pixels
        .rgba16()
        == words));
    undo();
    native.set().file_list(&[&file, &bad]).unwrap();
    let reply = paste(&root);
    assert!(reply.result.is_err());
    assert_eq!(
        shared.lock().unwrap().doc.export_png().unwrap(),
        baseline.export_png().unwrap()
    );
    assert!(shared.lock().unwrap().undo.is_empty());
    std::fs::remove_file(file).unwrap();
    std::fs::remove_file(bad).unwrap();

    // Internal native words/coordinates survive display transport while owned.
    // An external copy of identical display bytes must supersede that cache.
    let native_copy = {
        let mut e = shared.lock().unwrap();
        e.doc = Document::new_depth(32, 24, 16).unwrap();
        e.doc.layers[0]
            .pixels
            .set16(5, 6, [12347, 23459, 34571, 40001]);
        e.doc.selection = Some([4, 5, 8, 9]);
        Image::copy(&e.doc, &e.doc.layers[0].id, false, false).unwrap()
    };
    let doc = shared.lock().unwrap().doc.clone();
    let root = doc.layers[0].id.clone();
    worker
        .request(clipboard::Request::Copy {
            doc,
            target: root.clone(),
            mask: false,
            merged: false,
        })
        .unwrap();
    station.receive(&worker.replies).result.unwrap();
    let reply = paste(&root);
    reply.result.unwrap();
    {
        let e = shared.lock().unwrap();
        let l = e
            .doc
            .layers
            .iter()
            .find(|l| Some(&l.id) == reply.selected.as_ref())
            .unwrap();
        assert_eq!(l.pixels.rgba16(), *native_copy.samples16.as_ref().unwrap());
        assert_eq!((l.x, l.y), (4, 5));
    }
    undo();
    native
        .set_image(arboard::ImageData {
            width: 4,
            height: 4,
            bytes: Cow::Borrowed(&native_copy.bytes),
        })
        .unwrap();
    let reply = paste(&root);
    reply.result.unwrap();
    let e = shared.lock().unwrap();
    let l = e
        .doc
        .layers
        .iter()
        .find(|l| Some(&l.id) == reply.selected.as_ref())
        .unwrap();
    assert_eq!((l.x, l.y), (14, 10));
    assert_eq!(
        l.pixels.rgba16(),
        native_copy
            .bytes
            .iter()
            .map(|v| *v as u16 * 257)
            .collect::<Vec<_>>()
    );
}

#[test]
fn native16_image_paste_promotes_existing_sources_atomically_and_undoes_depth() {
    let (mut e, id) = setup();
    e.edit(
        "human",
        &[
            json!({"op":"mask.add","layer":id,"value":128}),
            json!({"op":"transform","layer":id,"scale_x":0.75,"scale_y":0.75,"selection_only":false}),
        ],
        None,
        None,
        "Sources",
    )
    .unwrap();
    e.undo.clear();
    let before = e.doc.clone();
    assert!(before.layers[0].pixels.retained.is_some());
    let words = vec![12347, 23459, 34571, 40001];
    let image = Image {
        width: 1,
        height: 1,
        bytes: words
            .iter()
            .copied()
            .map(peerbrush::raster::project16)
            .collect(),
        samples16: Some(words.clone()),
        origin: None,
    };
    let mut invalid = image.command(&id).unwrap();
    invalid["origin"] = json!([1.5, 0]);
    assert!(clipboard::paste(&mut e.doc, &invalid).is_err());
    assert_eq!(
        serde_json::to_value(&e.doc).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    e.edit(
        "human",
        &[image.command(&id).unwrap()],
        Some(e.doc.revision),
        None,
        "Paste",
    )
    .unwrap();
    assert_eq!(e.doc.bit_depth, 16);
    assert_eq!(e.doc.layers[1].pixels.rgba16(), words);
    assert!(e.doc.layers.iter().all(|l| l.pixels.depth == 16));
    let old = e.doc.layers.iter().find(|l| l.id == id).unwrap();
    let original = &before.layers[0];
    assert_eq!(
        old.pixels.rgba16(),
        original
            .pixels
            .rgba()
            .iter()
            .map(|v| *v as u16 * 257)
            .collect::<Vec<_>>()
    );
    assert!(old
        .mask
        .as_ref()
        .unwrap()
        .steps
        .iter()
        .all(|s| s.pixels.depth == 16));
    assert!(old
        .pixels
        .retained
        .as_ref()
        .is_some_and(|r| r.pixels.depth == 16));
    e.undo("human").unwrap();
    assert_eq!(e.doc.bit_depth, 8);
    assert_eq!(
        serde_json::to_value(&e.doc.layers).unwrap(),
        serde_json::to_value(&before.layers).unwrap()
    );
}

#[test]
fn image_paste_into_empty_document_never_redirects_a_stale_nonempty_target() {
    let (mut e, id) = setup();
    let image = Image {
        width: 1,
        height: 1,
        bytes: vec![23, 45, 67, 255],
        samples16: None,
        origin: None,
    };
    assert!(e
        .edit(
            "human",
            &[image.command("missing").unwrap()],
            Some(0),
            None,
            "Paste"
        )
        .is_err());
    e.edit(
        "human",
        &[json!({"op":"layer.delete","layer":id})],
        None,
        None,
        "Delete",
    )
    .unwrap();
    let before = e.doc.clone();
    e.edit(
        "human",
        &[image.command(&id).unwrap()],
        Some(e.doc.revision),
        None,
        "Paste",
    )
    .unwrap();
    assert_eq!(e.doc.layers.len(), 1);
    assert!(e.doc.layers[0].parent.is_none());
    assert_eq!(e.doc.layers[0].pixels.rgba(), image.bytes);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers.len(), before.layers.len());
}
