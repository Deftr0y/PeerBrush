//! IRIDAS/SpeedGrade LOOK baked table. Shader definitions remain source data.
use crate::color_profile::clut::{Clut, MAX_GRID};
use roxmltree::{Document, Node, ParsingOptions};
use std::borrow::Cow;

pub(super) fn parse(bytes: &[u8]) -> Result<Clut, String> {
    let source = xml_text(bytes)?;
    let document = Document::parse_with_options(
        source.trim_start_matches('\u{feff}'),
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 8192,
            ..Default::default()
        },
    )
    .map_err(|_| "Invalid or oversized .look XML")?;
    let root = document.root_element();
    if !root.has_tag_name("look")
        || root.tag_name().namespace().is_some()
        || root.attributes().len() != 0
        || document.root().children().any(|child| child.is_pi())
    {
        return Err("Unsupported .look root".into());
    }
    let mut lut = None;
    let (mut shaders, mut metadata, mut mask) = (false, false, false);
    for node in elements(root)? {
        match node.tag_name().name() {
            "LUT" if lut.is_none() => lut = Some(node),
            "shaders" if !shaders => shaders = true,
            "metadata" if !metadata => metadata = true,
            "mask" if !mask && empty_mask(node) => mask = true,
            "mask" => return Err("A .look with a mask requires its original editor".into()),
            _ => return Err("Unknown or duplicate .look section".into()),
        }
    }
    for node in root.descendants().filter(Node::is_element) {
        if node.ancestors().take(65).count() == 65 {
            return Err("The .look XML exceeds its nesting limit".into());
        }
        if node.has_tag_name("mask") && !empty_mask(node) {
            return Err("A .look with a mask requires its original editor".into());
        }
    }
    let mut size = None;
    let mut data = None;
    for node in elements(lut.ok_or("Missing .look LUT section")?)? {
        if node.attributes().len() != 0
            || node
                .children()
                .any(|child| !child.is_text() && !child.is_comment())
        {
            return Err("Invalid .look LUT field".into());
        }
        match node.tag_name().name() {
            "size" if size.is_none() => size = Some(node),
            "data" if data.is_none() => data = Some(node),
            _ => return Err("Unknown or duplicate .look LUT field".into()),
        }
    }
    let mut size_text = size
        .ok_or("Missing .look grid size")?
        .children()
        .filter_map(|child| if child.is_text() { child.text() } else { None });
    let size = size_text
        .next()
        .filter(|_| size_text.next().is_none())
        .unwrap_or("")
        .trim_matches(|c: char| c.is_ascii_whitespace() || matches!(c, '\'' | '"'))
        .parse::<usize>()
        .ok()
        .filter(|value| (2..=MAX_GRID).contains(value))
        .ok_or("A .look table needs 2–33 grid points per axis")?;
    let count = size.pow(3) * 3;
    let data = data.ok_or("Missing .look grid data")?;
    let mut digits = data
        .children()
        .filter_map(|child| child.is_text().then(|| child.text()).flatten())
        .flat_map(str::bytes)
        .filter(|digit| !digit.is_ascii_whitespace() && !matches!(digit, b'\'' | b'"'));
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        let mut word = [0; 4];
        for byte in &mut word {
            *byte = hex(digits.next())? * 16 + hex(digits.next())?;
        }
        let value = f32::from_le_bytes(word);
        if !value.is_finite() {
            return Err("Non-finite .look table value".into());
        }
        values.push(f64::from(value));
    }
    if digits.next().is_some() {
        return Err("Extra data after the .look grid".into());
    }
    Ok(Clut::new([size; 3], values.into_boxed_slice(), true))
}

fn elements<'a, 'input>(
    node: Node<'a, 'input>,
) -> Result<impl Iterator<Item = Node<'a, 'input>>, String> {
    if node.attributes().len() != 0
        || node.children().any(|child| {
            child.is_element() && child.tag_name().namespace().is_some()
                || child.is_text() && child.text().is_some_and(|text| !text.trim().is_empty())
                || child.is_pi()
        })
    {
        return Err("Unsupported .look XML contents".into());
    }
    Ok(node.children().filter(Node::is_element))
}

fn empty_mask(node: Node<'_, '_>) -> bool {
    if node.attributes().len() != 0
        || node
            .children()
            .any(|child| !child.is_text() && !child.is_comment())
    {
        return false;
    }
    let mut text = node
        .children()
        .filter_map(|child| if child.is_text() { child.text() } else { None })
        .map(|text| text.trim_matches(|c: char| c.is_ascii_whitespace() || matches!(c, '\'' | '"')))
        .filter(|text| !text.is_empty());
    // Adobe LOOK presets use a quoted zero for the no-mask sentinel.
    matches!((text.next(), text.next()), (None | Some("0"), None))
}

fn xml_text(bytes: &[u8]) -> Result<Cow<'_, str>, String> {
    if let [0xff, 0xfe, data @ ..] | [0xfe, 0xff, data @ ..] = bytes {
        if data.len() % 2 != 0 {
            return Err("Truncated UTF-16 .look XML".into());
        }
        let words = data.chunks_exact(2).map(|word| {
            let word = [word[0], word[1]];
            if bytes[0] == 0xff {
                u16::from_le_bytes(word)
            } else {
                u16::from_be_bytes(word)
            }
        });
        return char::decode_utf16(words)
            .collect::<Result<String, _>>()
            .map(Cow::Owned)
            .map_err(|_| "Invalid UTF-16 .look XML".into());
    }
    std::str::from_utf8(bytes)
        .map(Cow::Borrowed)
        .map_err(|_| "A .look table requires UTF-8 or BOM-marked UTF-16 XML".into())
}

fn hex(digit: Option<u8>) -> Result<u8, String> {
    digit
        .and_then(|digit| char::from(digit).to_digit(16))
        .map(|value| value as u8)
        .ok_or_else(|| "Truncated or invalid .look hexadecimal data".into())
}
