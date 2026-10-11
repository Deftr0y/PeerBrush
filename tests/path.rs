use peerbrush::{
    engine::{Document, Engine, Scope},
    path::Anchor,
    psd,
    source::Content,
};
use serde_json::{json, Value};
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(32, 24, depth).unwrap();
    for y in 0..24 {
        for x in 0..32 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [10001 + x as u16 * 157, 20003 + y as u16 * 193, 40007, 65535],
            );
        }
    }
    e
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], Some(e.doc.revision), None, "Path")
        .unwrap();
}
fn nodes(e: &Engine) -> &[Anchor] {
    let Content::Bezier { nodes, .. } = &e.doc.layers[0].source.as_ref().unwrap().content else {
        panic!("Path source absent")
    };
    nodes
}
fn create(e: &mut Engine) {
    edit(
        e,
        json!({"op":"path.create","nodes":[{"point":[8.,12.],"out_handle":[8.,2.],"in_handle":[8.,22.],"smooth":true},{"point":[24.,12.],"in_handle":[24.,2.],"out_handle":[24.,22.],"smooth":true}],"closed":true,"fill":[12345,23457,34569,65535],"stroke":[0,0,0,0],"stroke_width":0.}),
    );
}
#[test]
fn straight_and_curved_paths_render_analytic_native_pixels_and_restore_sources() {
    for depth in [8, 16] {
        for curved in [false, true] {
            let mut e = fixture(depth);
            let background = e.doc.layers[0].pixels.clone();
            let c = if curved {
                json!({"op":"path.create","nodes":[{"point":[8.,12.],"out_handle":[8.,2.],"in_handle":[8.,22.]},{"point":[24.,12.],"in_handle":[24.,2.],"out_handle":[24.,22.]}],"closed":true,"fill":[12345,23457,34569,65535],"stroke":[0,0,0,0],"stroke_width":0.})
            } else {
                json!({"op":"path.create","nodes":[{"point":[4.5,8.5]},{"point":[28.5,8.5]}],"stroke":[12345,23457,34569,65535],"stroke_width":2.})
            };
            let preview = Engine::preview_edits(e.doc.clone(), &[c.clone()]).unwrap();
            let before = e.doc.export_png().unwrap();
            edit(&mut e, c);
            assert_eq!(e.undo.len(), 1);
            assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
            let expected = if depth == 16 {
                [12345, 23457, 34569, 65535]
            } else {
                [48 * 257, 91 * 257, 135 * 257, 65535]
            };
            let at = if curved { (16, 12) } else { (12, 8) };
            assert_eq!(e.doc.layers[0].pixels.get16(at.0, at.1), expected);
            if curved {
                assert_eq!(e.doc.layers[0].pixels.get16(16, 2)[3], 0);
            }
            assert_eq!(e.doc.layers[1].pixels, background);
            let saved = psd::encode(&e.doc).unwrap();
            let reopened = psd::decode(&saved).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(reopened.layers[0].source, e.doc.layers[0].source);
            assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before);
            e.redo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), reopened.export_png().unwrap());
        }
    }
}
#[test]
fn path_anchor_and_handle_edits_use_document_coordinates_after_affine_placement() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        create(&mut e);
        let id = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"transform","layer":id,"angle":90.,"mirror_x":true,"selection_only":false,"pivot":[16,12]}),
        );
        let before = e.doc.clone();
        let old = peerbrush::path::document_point(&e.doc.layers[0], nodes(&e)[0].point);
        e.undo.clear();
        edit(
            &mut e,
            json!({"op":"path.node.move","layer":id,"index":0,"point":[old[0]+2.,old[1]-1.]}),
        );
        let now = peerbrush::path::document_point(&e.doc.layers[0], nodes(&e)[0].point);
        assert!((now[0] - old[0] - 2.).abs() < 0.00001 && (now[1] - old[1] + 1.).abs() < 0.00001);
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers[0].source, before.layers[0].source);
        e.redo("human").unwrap();
        let before = e.doc.layers[0].source.clone();
        edit(
            &mut e,
            json!({"op":"path.handle.move","layer":id,"index":0,"handle":"out","point":[old[0]+5.,old[1]+3.]}),
        );
        let n = &nodes(&e)[0];
        let a = n.in_handle.unwrap();
        let b = n.out_handle.unwrap();
        let cross =
            (a[0] - n.point[0]) * (b[1] - n.point[1]) - (a[1] - n.point[1]) * (b[0] - n.point[0]);
        assert!(cross.abs() < 0.001);
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers[0].source, before);
        edit(
            &mut e,
            json!({"op":"path.node.convert","layer":id,"index":0,"smooth":false,"reset_handles":true}),
        );
        assert!(nodes(&e)[0].in_handle.is_none());
        edit(
            &mut e,
            json!({"op":"path.node.convert","layer":id,"index":0,"smooth":true}),
        );
        assert!(nodes(&e)[0].smooth);
        edit(
            &mut e,
            json!({"op":"path.update","layer":id,"closed":false}),
        );
        edit(
            &mut e,
            json!({"op":"path.node.add","layer":id,"point":[16,20],"index":1}),
        );
        assert_eq!(nodes(&e).len(), 3);
        edit(
            &mut e,
            json!({"op":"path.node.delete","layer":id,"index":1}),
        );
        assert_eq!(nodes(&e).len(), 2);
        let reopened = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert_eq!(reopened.layers[0].source, e.doc.layers[0].source);
    }
}
#[test]
fn closed_path_selection_and_masks_preserve_native_color_and_previous_selection() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let photo = e.doc.layers[0].id.clone();
        let color = e.doc.layers[0].pixels.clone();
        create(&mut e);
        let path = e.doc.layers[0].id.clone();
        edit(&mut e, json!({"op":"path.to_selection","path":path}));
        assert!(peerbrush::selection::contains_pixel(&e.doc, 16, 12));
        assert!(!peerbrush::selection::contains_pixel(&e.doc, 16, 2));
        let selection = e.doc.selection_coverage.clone();
        let source = e.doc.layers[0].source.clone();
        e.undo.clear();
        edit(
            &mut e,
            json!({"op":"path.to_mask","path":path,"layer":photo,"feather":1.}),
        );
        assert_eq!(e.undo.len(), 1);
        assert_eq!(e.doc.selection_coverage, selection);
        assert_eq!(e.doc.layers[1].pixels, color);
        assert_eq!(e.doc.layers[0].source, source);
        assert!(e.doc.layers[1].mask_value(16, 12) > 0.99);
        assert_eq!(e.doc.layers[1].mask_value(16, 1), 0.);
        e.undo("human").unwrap();
        assert!(e.doc.layers[1].mask.is_none());
        e.redo("human").unwrap();
        let reopened = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
        assert_eq!(reopened.layers[0].source, source);
    }
}
#[test]
fn malformed_locked_reserved_stale_and_budget_exceeding_paths_are_atomic() {
    for guard in [
        "locked",
        "reserved",
        "stale",
        "handle",
        "index",
        "coordinate",
        "budget",
        "source_reserved",
        "selection_reserved",
    ] {
        let mut e = fixture(16);
        create(&mut e);
        let id = e.doc.layers[0].id.clone();
        let photo = e.doc.layers[1].id.clone();
        let mut c = json!({"op":"path.node.move","layer":id,"index":0,"point":[12,12]});
        if guard == "locked" {
            e.doc.layers[0].locked = true;
        }
        if guard == "reserved" {
            e.reserve("other", "Path", vec![Scope::layer(&id)]).unwrap();
        }
        if guard == "stale" {
            c["source_revision"] = json!(e.doc.revision + 1);
        }
        if guard == "handle" {
            c["op"] = json!("path.handle.move");
            c["handle"] = json!("bad");
        }
        if guard == "index" {
            c["index"] = json!(9999);
        }
        if guard == "coordinate" {
            c["point"] = json!([100001, 12]);
        }
        if guard == "budget" {
            c = json!({"op":"path.create","frame":[0,0,8192,8192],"nodes":[{"point":[2,2]}]});
        }
        if guard == "source_reserved" {
            e.reserve("other", "Path", vec![Scope::layer(&id)]).unwrap();
            c = json!({"op":"path.to_mask","path":id,"layer":photo});
        }
        if guard == "selection_reserved" {
            e.reserve("other", "Area", vec![Scope::layer("@selection")])
                .unwrap();
            c = json!({"op":"path.to_selection","path":id});
        }
        let before = serde_json::to_value(&e.doc).unwrap();
        let count = e.undo.len();
        assert!(
            e.edit("human", &[c], None, None, "Rejected path").is_err(),
            "{guard}"
        );
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert_eq!(e.undo.len(), count);
    }
}

#[test]
fn segment_subdivision_preserves_cubic_geometry_and_closed_topology() {
    for depth in [8, 16] {
        for index in [0, 1] {
            let mut e = fixture(depth);
            create(&mut e);
            let id = e.doc.layers[0].id.clone();
            let before = e.doc.clone();
            let old = nodes(&e).to_vec();
            e.undo.clear();
            edit(
                &mut e,
                json!({"op":"path.node.split","layer":id,"index":index,"t":0.5}),
            );
            assert_eq!(nodes(&e).len(), 3);
            let inserted = &nodes(&e)[index + 1];
            assert_eq!(inserted.point, [16., if index == 0 { 4.5 } else { 19.5 }]);
            assert!(inserted.smooth);
            let evaluate = |a: &Anchor, b: &Anchor, t: f32| {
                let u = 1. - t;
                let p = [
                    a.point,
                    a.out_handle.unwrap_or(a.point),
                    b.in_handle.unwrap_or(b.point),
                    b.point,
                ];
                std::array::from_fn::<_, 2, _>(|i| {
                    u * u * u * p[0][i]
                        + 3. * u * u * t * p[1][i]
                        + 3. * u * t * t * p[2][i]
                        + t * t * t * p[3][i]
                })
            };
            for step in 0..=100 {
                let t = step as f32 / 100.;
                let p = evaluate(&old[index], &old[(index + 1) % 2], t);
                let a = &nodes(&e)[index];
                let middle = &nodes(&e)[index + 1];
                let b = &nodes(&e)[(index + 2) % 3];
                let q = if t <= 0.5 {
                    evaluate(a, middle, t * 2.)
                } else {
                    evaluate(middle, b, t * 2. - 1.)
                };
                assert!((p[0] - q[0]).abs() < 0.00001 && (p[1] - q[1]).abs() < 0.00001);
            }
            assert_eq!(e.undo.len(), 1);
            let reopened = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
            assert_eq!(reopened.layers[0].source, e.doc.layers[0].source);
            e.undo("human").unwrap();
            assert_eq!(e.doc.layers[0].source, before.layers[0].source);
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        }
    }
}

#[test]
fn path_protocol_feedback_and_standard_psd_channels_preserve_native_rendering() {
    use peerbrush::server;
    use std::sync::{Arc, Mutex};
    for depth in [8, 16] {
        let mut e = fixture(depth);
        create(&mut e);
        let id = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            json!({"op":"move","layer":id,"dx":2,"dy":1,"selection_only":false}),
        );
        let observed = e.state();
        assert_eq!(
            observed["layers"][0]["path_document_nodes"][0]["point"],
            json!([10., 13.])
        );
        let revision = e.doc.revision;
        let shared = Arc::new(Mutex::new(e));
        let r=server::dispatch(&shared,"edit",&json!({"actor":"artist","expected_revision":revision,"commands":[{"op":"path.handle.move","layer":id,"index":0,"handle":"out","point":[12,5]}]})).unwrap();
        assert_eq!(r["images"][0]["document_rect"], json!([0, 0, 32, 24]));
        assert_eq!(r["images"][0]["mime_type"], "image/png");
        assert!(r["images"][0]["data"]
            .as_str()
            .unwrap()
            .starts_with("iVBOR"));
        let e = shared.lock().unwrap();
        let saved = psd::encode(&e.doc).unwrap();
        let start = 30 + u32::from_be_bytes(saved[26..30].try_into().unwrap()) as usize;
        let size = u32::from_be_bytes(saved[start..start + 4].try_into().unwrap()) as usize;
        let standard = [
            saved[..start].to_vec(),
            vec![0; 4],
            saved[start + 4 + size..].to_vec(),
        ]
        .concat();
        let standard = psd::decode(&standard).unwrap();
        assert!(!standard.read_only);
        assert_eq!(standard.bit_depth, depth);
        assert_eq!(standard.export_png().unwrap(), e.doc.export_png().unwrap());
        assert!(standard.layers[0].source.is_none());
    }
}
