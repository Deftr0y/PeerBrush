//! Shared image-paste hierarchy, locking and document-coordinate policy.
use crate::engine::Scope;
use serde_json::Value;

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub(super) const MAX_FILE_URI: usize = 3 * 4096 + 32;

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub(super) fn local_file_uri(uri: &str) -> Result<std::path::PathBuf, String> {
    let invalid = "Clipboard contains an invalid or nonlocal file URI";
    if uri.len() > MAX_FILE_URI || uri.bytes().any(|b| !b.is_ascii_graphic() || matches!(b, b'?' | b'#' | b'\\')) {
        return Err(invalid.into());
    }
    let (scheme, rest) = uri.split_once("://").ok_or(invalid)?;
    let (host, _) = rest.split_once('/').ok_or(invalid)?;
    if !scheme.eq_ignore_ascii_case("file") || (!host.is_empty() && !host.eq_ignore_ascii_case("localhost")) {
        return Err(invalid.into());
    }
    let mut decoded = Vec::new();
    let mut input = rest[host.len()..].bytes();
    while let Some(byte) = input.next() {
        let byte = if byte == b'%' {
            let digit = |byte: u8| (byte as char).to_digit(16).map(|value| value as u8).ok_or(invalid);
            (digit(input.next().ok_or(invalid)?)? << 4) | digit(input.next().ok_or(invalid)?)?
        } else { byte };
        if byte.is_ascii_control() || decoded.len() == 4096 {
            return Err(invalid.into());
        }
        decoded.push(byte);
    }
    if !decoded.starts_with(b"/") || decoded.starts_with(b"//") {
        return Err(invalid.into());
    }
    Ok(String::from_utf8(decoded).map_err(|_| invalid)?.into())
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn local_file_uris(offer: &[u8]) -> Result<Vec<std::path::PathBuf>, String> {
    let offer = std::str::from_utf8(offer).map_err(|_| "Clipboard file list is not UTF-8")?;
    let mut paths = Vec::new();
    for line in offer.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
        if paths.len() == super::MAX_PASTE_FILES {
            return Err("Paste up to 16 image files at a time".into());
        }
        paths.push(local_file_uri(line)?);
    }
    Ok(paths)
}

#[cfg(test)]
mod file_uri_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn native_file_lists_accept_local_escaped_paths_and_refuse_partial_batches() {
        let offer = b"# copied files\r\nfile:///tmp/a%20b.png\r\nfile://LOCALHOST/tmp/Lumi%C3%A8re%23.png\r\n";
        assert_eq!(local_file_uris(offer).unwrap(), [PathBuf::from("/tmp/a b.png"), PathBuf::from("/tmp/Lumière#.png")]);
        for invalid in ["file://remote/tmp/image.png", "file:///tmp/no%2.png", "file:///tmp/no%00.png",
            "file:///tmp/not%FFutf8.png", "file:////server/share.png", "https://localhost/tmp/image.png",
            "file:///tmp/image.png?query", "file:///tmp/a b.png", "file://relative.png"]
        {
            let offer = format!("file:///tmp/valid.png\n{invalid}\n");
            assert!(local_file_uris(offer.as_bytes()).is_err(), "{invalid}");
        }
        let too_many = "file:///tmp/valid.png\n".repeat(17);
        assert!(local_file_uris(too_many.as_bytes()).unwrap_err().contains("16"));
    }
}

pub(crate) struct Layer<'a> {
    pub id: &'a str,
    pub parent: Option<&'a str>,
    pub kind: &'a str,
    pub clip_to: Option<&'a str>,
    pub locked: bool,
}
pub(crate) struct Plan {
    pub selected: Option<usize>,
    pub wrap_base: Option<usize>,
    /// Pasted layer index, after inserting the optional wrapper at index - 1.
    pub index: usize,
    pub parent: Option<String>,
}
impl Plan {
    pub fn new<'a>(
        c: &Value,
        count: usize,
        layer: impl Fn(usize) -> Layer<'a>,
    ) -> Result<Self, String> {
        let context = c["layer"]
            .as_str()
            .ok_or("Choose a layer or folder before pasting")?;
        Self::build(context, count, 1, layer)
    }
    pub fn layers<'a>(
        target: &str,
        count: usize,
        added: usize,
        layer: impl Fn(usize) -> Layer<'a>,
    ) -> Result<Self, String> {
        let context = if target.is_empty() && count > 0 {
            layer(0).id
        } else {
            target
        };
        Self::build(context, count, added, layer)
    }
    fn build<'a>(
        context: &str,
        count: usize,
        added: usize,
        layer: impl Fn(usize) -> Layer<'a>,
    ) -> Result<Self, String> {
        if added == 0 || added > 100 {
            return Err("Clipboard needs 1-100 layers".into());
        }
        let selected = (0..count).find(|&i| layer(i).id == context);
        if selected.is_none() && count > 0 {
            return Err("Paste destination no longer exists".into());
        }
        let unlocked = |index: usize| -> Result<(), String> {
            let mut current = Some(index);
            for _ in 0..=16 {
                let Some(i) = current else { return Ok(()) };
                let l = layer(i);
                if l.locked {
                    return Err("Unlock the destination before pasting".into());
                }
                current = l
                    .parent
                    .map(|id| {
                        (0..count)
                            .find(|&i| layer(i).id == id)
                            .ok_or("Invalid paste destination")
                    })
                    .transpose()?;
            }
            if current.is_some() {
                Err("Group nesting limit exceeded".into())
            } else {
                Ok(())
            }
        };
        let mut plan = Self {
            selected,
            wrap_base: None,
            index: 0,
            parent: None,
        };
        if let Some(index) = selected {
            unlocked(index)?;
            let node = layer(index);
            if node.kind != "group" {
                let id = node.clip_to.unwrap_or(node.id);
                let base = (0..count)
                    .find(|&i| layer(i).id == id)
                    .ok_or("Paste clipping base is missing")?;
                if layer(base).parent != node.parent || layer(base).clip_to.is_some() {
                    return Err("Invalid paste clipping base".into());
                }
                plan.wrap_base = node.parent.is_none().then_some(base);
                plan.parent = node.parent.map(str::to_owned);
                plan.index = count;
                for i in 0..count {
                    let member = layer(i);
                    if member.parent == node.parent
                        && (member.id == id || member.clip_to == Some(id))
                    {
                        if plan.wrap_base.is_some() {
                            unlocked(i)?;
                        }
                        plan.index = plan.index.min(i);
                    }
                }
                plan.index += usize::from(plan.wrap_base.is_some());
            } else {
                plan.index = index + 1;
                plan.parent = Some(node.id.into());
            }
        }
        if count + added + usize::from(plan.wrap_base.is_some()) > 100 {
            return Err("Initial version supports up to 100 layers".into());
        }
        Ok(plan)
    }
    pub fn wraps<'a>(&self, candidate: &Layer<'a>, base: &Layer<'a>) -> bool {
        candidate.parent == base.parent
            && (candidate.id == base.id || candidate.clip_to == Some(base.id))
    }
    pub fn scopes<'a>(&self, count: usize, layer: impl Fn(usize) -> Layer<'a>) -> Vec<Scope> {
        let mut scopes = vec![Scope::layer("@selection")];
        if let Some(base) = self.wrap_base {
            let base = layer(base);
            for i in 0..count {
                let l = layer(i);
                if self.wraps(&l, &base) {
                    scopes.push(Scope::layer(l.id));
                }
            }
        } else if let Some(index) = self.selected {
            scopes.push(Scope::layer(layer(index).id));
            if let Some(parent) = &self.parent {
                if layer(index).id != parent {
                    scopes.push(Scope::layer(parent));
                }
            }
        }
        scopes
    }
}
pub(crate) fn origin(c: &Value, canvas: [u32; 2], image: [u32; 2]) -> Result<[i32; 2], String> {
    if c["origin"].is_null() {
        return Ok([
            (canvas[0] as i32 - image[0] as i32) / 2,
            (canvas[1] as i32 - image[1] as i32) / 2,
        ]);
    }
    let values = c["origin"]
        .as_array()
        .filter(|a| a.len() == 2)
        .ok_or("Invalid paste origin")?;
    let coordinate = |v: &Value| {
        v.as_i64()
            .filter(|n| (-100000..=100000).contains(n))
            .map(|n| n as i32)
            .ok_or("Invalid paste origin")
    };
    Ok([coordinate(&values[0])?, coordinate(&values[1])?])
}
