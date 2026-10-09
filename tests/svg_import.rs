use peerbrush::{
    engine::{Document, Engine, Scope},
    image_import, layer_clipboard, psd, server,
    source::{Content, Source},
    svg,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

const ART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="48" viewBox="0 0 64 48">
<title>Editable test artwork</title><metadata><author xmlns="urn:example">Synthetic artwork</author></metadata>
<defs><linearGradient id="tone"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient>
<rect id="tile" width="12" height="12" fill="#ff0000"/><clipPath id="left"><rect width="32" height="48"/></clipPath></defs>
<rect width="64" height="20" fill="url(#tone)"/><use href="#tile" x="4" y="26" opacity="0.5"/>
<path d="M40 26 C40 22 58 22 58 26 L58 42 A4 4 0 0 1 54 46 L44 46 Z" fill="#00ff00"/>
<rect y="21" width="64" height="2" clip-path="url(#left)" fill="#ffffff"/></svg>"##;

struct File(PathBuf);
impl File {
    fn new(extension: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "peerbrush-svg-{}.{}",
            uuid::Uuid::new_v4(),
            extension
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn source(markup: &str) -> Source {
    let (width, height) = svg::dimensions(markup).unwrap();
    Source {
        width,
        height,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content::Svg { svg: markup.into() },
    }
}
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(96, 80, depth).unwrap();
    e.doc.layers[0]
        .pixels
        .set16(85, 70, [12345, 23457, 34569, 60001]);
    e
}
fn edit(e: &mut Engine, commands: &[Value]) {
    e.edit("human", commands, Some(e.doc.revision), None, "SVG artwork")
        .unwrap();
}
fn source_format(bytes: &[u8]) -> u64 {
    let start = bytes.windows(4).position(|w| w == b"PBR1").unwrap() + 4;
    let mut text = Vec::new();
    flate2::read::ZlibDecoder::new(&bytes[start..])
        .read_to_end(&mut text)
        .unwrap();
    serde_json::from_slice::<Value>(&text).unwrap()["format"]
        .as_u64()
        .unwrap()
}
fn without_resources(bytes: &[u8]) -> Vec<u8> {
    let color = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let start = 30 + color;
    let size = u32::from_be_bytes(bytes[start..start + 4].try_into().unwrap()) as usize;
    let mut plain = bytes[..start].to_vec();
    plain.extend_from_slice(&[0; 4]);
    plain.extend_from_slice(&bytes[start + 4 + size..]);
    plain
}

#[test]
fn static_paths_gradients_clips_local_use_and_alpha_render_at_both_depths() {
    let definition = source(ART);
    definition.validate().unwrap();
    for depth in [8, 16] {
        let pixels = definition.render(64, 48, depth).unwrap();
        assert_eq!(pixels.depth, depth);
        assert_eq!(pixels.get(8, 30), [255, 0, 0, 128]);
        assert_eq!(pixels.get(20, 21), [255; 4]);
        assert_eq!(pixels.get(40, 21), [0; 4]);
        assert_eq!(pixels.get(48, 34), [0, 255, 0, 255]);
        let a = pixels.get(2, 8);
        let b = pixels.get(61, 8);
        assert!(a[0] > 240 && a[2] < 20 && b[2] > 240 && b[0] < 20);
        assert_eq!(pixels.get16(8, 30), [65535, 0, 0, 128 * 257]);
    }
    let roundtrip: Source = serde_json::from_value(json!(definition)).unwrap();
    assert_eq!(roundtrip, definition);
}

#[test]
fn ambiguous_viewports_and_oversized_isolated_surfaces_reject_before_rendering() {
    assert!(
        svg::dimensions("<svg width='100%' height='100%'><rect width='8' height='8'/></svg>")
            .unwrap_err()
            .contains("viewBox")
    );
    assert_eq!(svg::dimensions("<svg width='100%' height='100%' viewBox='0 0 64 48'><rect width='8' height='8'/></svg>").unwrap(),(64,48));
    let markup="<svg xmlns='http://www.w3.org/2000/svg' width='1024' height='1024'><g opacity='.5'><g opacity='.5'><rect x='-2048' y='-2048' width='5120' height='5120' fill='red'/></g></g></svg>";
    assert!(svg::dimensions(markup).is_ok());
    let definition = source(markup);
    assert!(definition
        .render(1024, 1024, 16)
        .unwrap_err()
        .contains("memory"));
}

#[test]
fn nonvisual_metadata_cannot_bypass_supported_vector_validation() {
    for body in [
        "<metadata><path id='hidden' d='M0 0h8v8z' filter='url(#unsupported)'/></metadata><use href='#hidden'/>",
        "<metadata><linearGradient id='hidden'><stop offset='0' stop-color='red'/></linearGradient></metadata><rect width='8' height='8' fill='url(#hidden)'/>",
        "<title><clipPath id='hidden'><rect width='8' height='8'/></clipPath></title><rect width='8' height='8' clip-path='url(#hidden)'/>",
    ] {
        let markup = format!("<svg xmlns='http://www.w3.org/2000/svg' width='8' height='8'>{body}</svg>");
        assert!(svg::dimensions(&markup).unwrap_err().contains("metadata"));
    }
    assert!(svg::dimensions("<svg xmlns='http://www.w3.org/2000/svg' width='8' height='8'><metadata><editor:note xmlns:editor='urn:editor'>Original authoring data</editor:note></metadata><rect width='8' height='8' fill='red'/></svg>").is_ok());
}

#[test]
fn unsupported_or_ambiguous_svg_visuals_and_external_resources_reject_explicitly() {
    for body in [
        "<script>alert(1)</script>","<animate attributeName=\"opacity\"/>",
        "<text x=\"1\" y=\"5\">Needs fonts</text>","<image href=\"data:image/png;base64,AA==\"/>",
        "<image href=\"file:///artwork.png\"/>","<foreignObject/>","<filter id=\"f\"/>",
        "<style>rect {fill:red}</style>","<pattern id=\"p\"/>",
        "<rect width=\"8\" height=\"8\" filter=\"url(#f)\"/>",
        "<rect width=\"8\" height=\"8\" style=\"mix-blend-mode:multiply\"/>",
        "<rect width=\"8\" height=\"8\" style=\"width:4px\"/>",
        "<rect width=\"8\" height=\"8\" vector-effect=\"non-scaling-stroke\"/>",
        "<rect width=\"8\" height=\"8\" fill=\"url(https://example.invalid/tone.svg#g)\"/>",
        "<use href=\"missing.svg#x\"/>","<use href=\"#missing\"/>",
        "<rect id=\"x\"/><circle id=\"x\"/>",
        "<defs><g id=\"a\"><use href=\"#b\"/></g><g id=\"b\"><use href=\"#a\"/></g></defs>",
        "<rect id=\"shape\" width=\"8\" height=\"8\"/><rect width=\"8\" height=\"8\" fill=\"url(#shape)\"/>",
        "<path d=\"M0 0 invalid\"/>","<rect width=\"NaN\" height=\"8\"/>",
        "<g transform=\"scale(100000)\"><g transform=\"scale(100000)\"><rect width=\"8\" height=\"8\"/></g></g>",
    ] {
        let markup=format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\">{body}</svg>");
        assert!(svg::dimensions(&markup).is_err(),"Unsupported source accepted: {body}");
    }
    for markup in [
        "<svg/>",
        "<!DOCTYPE svg [<!ENTITY x 'red'>]><svg width='8' height='8'><rect fill='&x;'/></svg>",
        "<?xml-stylesheet href='external.css'?><svg width='8' height='8'/>",
    ] {
        assert!(svg::dimensions(markup).is_err());
    }
    assert!(svg::dimensions(&" ".repeat(svg::MARKUP_LIMIT + 1)).is_err());
    let mut repeated = String::from(
        "<svg width='16' height='16'><defs><g id='x'><rect width='1' height='1'/></g></defs>",
    );
    for _ in 0..1500 {
        repeated.push_str("<use href='#x'/>");
    }
    repeated.push_str("</svg>");
    assert!(svg::dimensions(&repeated).is_err());
}

#[test]
fn import_retains_source_native_human_samples_single_undo_and_standard_psd12_pixels() {
    let file = File::new("svg", ART.as_bytes());
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let before = e.doc.export_png().unwrap();
        let human = e.doc.layers[0].pixels.rgba16();
        let command =
            json!({"op":"image.import","path":file.0,"x":7,"y":9,"name":"Vector artwork"});
        let preview = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
        edit(&mut e, &[command]);
        assert_eq!(e.undo.len(), 1);
        assert_eq!(e.doc.layers[0].source, Some(source(ART)));
        assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (7, 9));
        assert_eq!(e.doc.layers[1].pixels.rgba16(), human);
        assert_eq!(preview.export_png().unwrap(), e.doc.export_png().unwrap());
        let encoded = psd::encode(&e.doc).unwrap();
        assert_eq!(source_format(&encoded), 12);
        let loaded = psd::decode(&encoded).unwrap();
        assert!(!loaded.read_only);
        assert_eq!(loaded.layers[0].source, e.doc.layers[0].source);
        assert_eq!(loaded.export_png().unwrap(), e.doc.export_png().unwrap());
        let standard = psd::decode(&without_resources(&encoded)).unwrap();
        assert!(!standard.read_only);
        assert!(standard.layers.iter().all(|l| l.source.is_none()));
        assert_eq!(
            image::load_from_memory(&standard.export_png().unwrap())
                .unwrap()
                .into_rgba16(),
            image::load_from_memory(&e.doc.export_png().unwrap())
                .unwrap()
                .into_rgba16()
        );
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before);
        e.redo("human").unwrap();
        assert_eq!(e.doc.layers[0].source, Some(source(ART)));
    }
}

#[test]
fn svgz_choices_mixed_batches_locks_reservations_and_stale_sources_are_atomic() {
    let mut compressed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    compressed.write_all(ART.as_bytes()).unwrap();
    let packed = File::new("svgz", &compressed.finish().unwrap());
    let encoded = image_import::Encoded::file(&packed.0).unwrap();
    assert_eq!(encoded.info.format, "SVG");
    assert_eq!(encoded.info.choice, None);
    assert_eq!(
        encoded
            .decode(&json!({}), image_import::BUDGET)
            .unwrap()
            .get(8, 30),
        [255, 0, 0, 128]
    );
    assert!(encoded
        .decode(&json!({"frame":1}), image_import::BUDGET)
        .is_err());
    assert!(encoded.decode(&json!({}), 1).is_err());
    let good = File::new("svg", ART.as_bytes());
    let bad = File::new(
        "svg",
        b"<svg width='8' height='8'><text>Unsupported</text></svg>",
    );
    let png = File::new(
        "png",
        &peerbrush::raster::png16(1, 1, &[12345, 23457, 34569, 65535]).unwrap(),
    );
    let mut e = fixture(8);
    let before = e.doc.export_png().unwrap();
    let revision = e.doc.revision;
    let batch = [
        json!({"op":"image.import","path":png.0}),
        json!({"op":"image.import","path":bad.0}),
    ];
    assert!(e
        .edit("human", &batch, None, None, "Invalid batch")
        .is_err());
    assert_eq!(e.doc.bit_depth, 8);
    assert_eq!(e.doc.export_png().unwrap(), before);
    assert_eq!(e.doc.revision, revision);
    assert!(e.undo.is_empty());
    edit(
        &mut e,
        &[json!({"op":"layer.add","kind":"group","name":"Vector folder"})],
    );
    let folder = e.doc.layers[0].id.clone();
    let command = json!({"op":"image.import","path":good.0,"parent":folder});
    e.doc.layers[0].locked = true;
    let state = e.doc.clone();
    assert!(e
        .edit("human", &[command.clone()], None, None, "Locked import")
        .is_err());
    assert_eq!(e.doc.export_png().unwrap(), state.export_png().unwrap());
    e.doc.layers[0].locked = false;
    e.reserve(
        "agent",
        "svg-test",
        vec![Scope {
            target: Some(folder.clone()),
            rect: None,
        }],
    )
    .unwrap();
    assert!(e
        .edit("human", &[command.clone()], None, None, "Reserved import")
        .is_err());
    e.leases.clear();
    let stale = json!({"op":"image.import","path":good.0,"document_id":"old-project","source_revision":e.doc.revision});
    assert!(e
        .edit("human", &[stale.clone()], None, None, "Stale import")
        .is_err());
    assert!(Engine::preview_edits(e.doc.clone(), &[stale]).is_err());
    edit(
        &mut e,
        &[json!({"op":"image.import","path":png.0}), command],
    );
    assert_eq!(e.doc.bit_depth, 16);
    assert_eq!(
        e.doc
            .layers
            .iter()
            .find(|l| l.source.is_some())
            .unwrap()
            .parent
            .as_deref(),
        Some(folder.as_str())
    );
}

#[test]
fn vector_updates_transforms_and_internal_clipboard_preserve_editable_source() {
    let mut e = fixture(16);
    edit(
        &mut e,
        &[json!({"op":"source.add","source":source(ART),"x":7,"y":9})],
    );
    let target = e.doc.layers[0].id.clone();
    let original = e.doc.export_png().unwrap();
    let revised = source(&ART.replace("#00ff00", "#ffff00"));
    let command = json!({"op":"source.update","layer":target,"source":revised,"document_id":e.doc.id,"source_revision":e.doc.revision});
    let preview = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
    edit(&mut e, &[command]);
    assert_eq!(preview.export_png().unwrap(), e.doc.export_png().unwrap());
    assert_ne!(original, e.doc.export_png().unwrap());
    e.undo("human").unwrap();
    assert_eq!(original, e.doc.export_png().unwrap());
    let snapshot = layer_clipboard::copy(&e.doc, &[target.clone()]).unwrap();
    let mut destination = Document::new_depth(96, 80, 16).unwrap();
    layer_clipboard::paste(&mut destination, &snapshot, "").unwrap();
    assert!(destination
        .layers
        .iter()
        .any(|l| l.source == Some(source(ART))));
    edit(
        &mut e,
        &[json!({"op":"transform","layer":target,"angle":90.,"selection_only":false})],
    );
    assert_eq!(
        e.doc.layers[0].source.as_ref().unwrap().content,
        source(ART).content
    );
    e.undo("human").unwrap();
    assert_eq!(original, e.doc.export_png().unwrap());
    let huge = source(ART);
    assert!(svg::render(ART, [huge.width, huge.height], huge.matrix, 8192, 4096, 16).is_err());
}

#[test]
fn protocol_inspection_discovery_and_source_feedback_include_svg_and_coordinates() {
    let file = File::new("svg", ART.as_bytes());
    let shared = Arc::new(Mutex::new(fixture(16)));
    let info = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_image_info","arguments":{"path":file.0}}}),
    );
    assert_eq!(info["result"]["isError"], false);
    let details: Value =
        serde_json::from_str(info["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(details["format"], "SVG");
    assert_eq!(
        (details["width"].as_u64(), details["height"].as_u64()),
        (Some(64), Some(48))
    );
    assert!(shared.lock().unwrap().undo.is_empty());
    let catalog = server::capabilities();
    assert!(catalog["image_import"]["extensions"]
        .as_array()
        .unwrap()
        .contains(&json!("svg")));
    let mut e = shared.lock().unwrap();
    edit(
        &mut e,
        &[json!({"op":"image.import","path":file.0,"x":11,"y":13})],
    );
    let state = e.state();
    assert_eq!(state["layers"][0]["source"]["content"]["kind"], "svg");
    assert_eq!(state["layers"][0]["bounds"], json!([11, 13, 75, 61]));
}
