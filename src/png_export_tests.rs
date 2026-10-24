use super::*;
use crate::{
    engine::Document,
    production_export::{self, Control, Format},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pb-png-stream-{}", engine::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn shared(depth: u16) -> Shared {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(260, 79, depth).unwrap();
    for y in 2..77 {
        for x in 3..257 {
            engine.doc.layers[0].pixels.set16(
                x,
                y,
                [
                    12347 + x as u16 * 97,
                    23459 + y as u16 * 193,
                    34571,
                    if (x + y) % 13 == 0 { 0 } else { 45679 },
                ],
            );
        }
    }
    Arc::new(Mutex::new(engine))
}
fn state(shared: &Shared) -> Value {
    let engine = shared.lock().unwrap();
    json!({"doc":engine.doc,"undo":engine.undo.len(),"redo":engine.redo.len(),"saved":engine.saved_revision,"path":engine.path})
}

#[test]
fn streaming_publication_keeps_late_failure_cancellation_source_destination_and_temp_guards() {
    for depth in [8, 16] {
        for failure in [
            "write",
            "cancel",
            "source",
            "destination",
            "temp_bytes",
            "temp_identity",
        ] {
            let temp = Temp::new();
            let shared = shared(depth);
            let path = temp.0.join("output.png");
            let doc = shared.lock().unwrap().doc.clone();
            let original = doc.export_png().unwrap();
            fs::write(&path, &original).unwrap();
            let before = state(&shared);
            let control = Control::default();
            let result = export_derived_to_guarded(
                &shared,
                &path,
                &json!({"document_id":doc.id,"expected_revision":doc.revision,"overwrite":true}),
                &control,
                |doc, writer| {
                    production_export::render_document_to(
                        doc,
                        Format::Png,
                        crate::jpeg_xl::Options::default(),
                        &control,
                        writer,
                    )?;
                    match failure {
                        "write" => return Err("Injected encoder/write failure".into()),
                        "cancel" => control.cancel(),
                        "source" => {
                            shared.lock().unwrap().edit("human",&[json!({"op":"fill","layer":doc.layers[0].id,"rect":[1,2,3,4],"color":[53,97,173,255]})],None,None,"Later human work")?;
                        }
                        "destination" => fs::write(&path, b"Later external destination")
                            .map_err(|e| e.to_string())?,
                        "temp_bytes" | "temp_identity" => {
                            let staged = fs::read_dir(&temp.0)
                                .unwrap()
                                .flatten()
                                .map(|e| e.path())
                                .find(|p| p.extension().is_some_and(|e| e == "tmp"))
                                .unwrap();
                            if failure == "temp_identity" {
                                let bytes = fs::read(&staged).unwrap();
                                fs::rename(&staged, staged.with_extension("original-owned-temp"))
                                    .unwrap();
                                fs::write(&staged, bytes).unwrap();
                            } else {
                                fs::write(&staged, b"Changed temporary bytes").unwrap();
                            }
                        }
                        _ => unreachable!(),
                    }
                    Ok(())
                },
            );
            assert!(result.is_err(), "Accepted {failure}");
            assert_eq!(
                fs::read(&path).unwrap(),
                if failure == "destination" {
                    b"Later external destination".to_vec()
                } else {
                    original
                }
            );
            if failure == "source" {
                let engine = shared.lock().unwrap();
                assert_eq!(engine.doc.revision, doc.revision + 1);
                assert_eq!(engine.undo.len(), 1);
                assert_eq!(
                    engine.doc.layers[0].pixels.get16(1, 2),
                    [53 * 257, 97 * 257, 173 * 257, 65535]
                );
            } else {
                assert_eq!(state(&shared), before);
            }
            assert!(!fs::read_dir(&temp.0)
                .unwrap()
                .flatten()
                .any(|e| e.path().extension().is_some_and(|v| v == "tmp")));
        }
    }
}

#[test]
fn direct_native_png_stream_uses_exact_dimensions_overwrite_retention_and_loaded_source_guards() {
    for depth in [8, 16] {
        let temp = Temp::new();
        let shared = shared(depth);
        let path = temp.0.join("output.PNG");
        let before = state(&shared);
        export(&shared, &path).unwrap();
        let first = fs::read(&path).unwrap();
        let image = image::load_from_memory(&first).unwrap();
        assert_eq!((image.width(), image.height()), (260, 79));
        assert_eq!(
            image.into_rgba16().into_raw(),
            image::load_from_memory(&shared.lock().unwrap().doc.export_png().unwrap())
                .unwrap()
                .into_rgba16()
                .into_raw()
        );
        assert!(export(&shared, &path).unwrap_err().contains("exists"));
        let doc = shared.lock().unwrap().doc.clone();
        export_confirmed_source(&shared, &path, &doc.id, doc.revision).unwrap();
        assert_eq!(fs::read(&path).unwrap(), first);
        assert!(fs::read_dir(temp.0.join(".peerbrush-versions"))
            .unwrap()
            .flatten()
            .any(|e| fs::read(e.path()).is_ok_and(|bytes| bytes == first)));
        assert_eq!(state(&shared), before);
        let loaded = temp.0.join("loaded.png");
        fs::write(&loaded, &first).unwrap();
        open(&shared, &loaded).unwrap();
        let preserved = state(&shared);
        let doc = shared.lock().unwrap().doc.clone();
        assert!(export_confirmed_source(&shared, &loaded, &doc.id, doc.revision).is_err());
        assert_eq!(fs::read(&loaded).unwrap(), first);
        assert_eq!(state(&shared), preserved);
    }
}

#[cfg(unix)]
#[test]
fn streaming_publication_rejects_a_temporary_alias_to_the_held_original_inode() {
    for depth in [8, 16] {
        let temp = Temp::new();
        let shared = shared(depth);
        let path = temp.0.join("output.png");
        let doc = shared.lock().unwrap().doc.clone();
        let original = doc.export_png().unwrap();
        fs::write(&path, &original).unwrap();
        let before = state(&shared);
        let control = Control::default();
        let error = export_derived_to_guarded(
            &shared,
            &path,
            &json!({"document_id":doc.id,"expected_revision":doc.revision,"overwrite":true}),
            &control,
            |doc, writer| {
                production_export::render_document_to(
                    doc,
                    Format::Png,
                    crate::jpeg_xl::Options::default(),
                    &control,
                    writer,
                )?;
                let staged = fs::read_dir(&temp.0)
                    .unwrap()
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| p.extension().is_some_and(|e| e == "tmp"))
                    .unwrap();
                let held = staged.with_extension("original-owned-temp");
                fs::rename(&staged, &held).unwrap();
                std::os::unix::fs::symlink(&held, &staged).unwrap();
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.contains("temporary path changed"));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(state(&shared), before);
        assert!(!fs::read_dir(&temp.0)
            .unwrap()
            .flatten()
            .any(|e| e.path().extension().is_some_and(|v| v == "tmp")));
    }
}
