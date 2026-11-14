//! Borrowed hierarchy selection shared by clipboard snapshots and native copies.
use crate::engine::view::DocumentView;

pub(crate) struct Forest {
    roots: [bool; 100],
    included: [bool; 100],
    preorder: [usize; 100],
    length: usize,
}
impl Forest {
    pub(crate) fn contains(&self, i: usize) -> bool {
        self.included[i]
    }
    pub(crate) fn is_root(&self, i: usize) -> bool {
        self.roots[i]
    }
    pub(crate) fn roots(&self) -> impl Iterator<Item = usize> + '_ {
        self.preorder[..self.length]
            .iter()
            .copied()
            .filter(|&i| self.roots[i])
    }
    pub(crate) fn nodes(&self) -> impl Iterator<Item = usize> + '_ {
        self.preorder[..self.length]
            .iter()
            .copied()
            .filter(|&i| self.included[i])
    }
    pub(crate) fn new(doc: DocumentView<'_>, ids: &[String]) -> Result<Self, String> {
        if ids.is_empty() || ids.len() > 100 || doc.layer_count() > 100 {
            return Err("Choose 1–100 layers to copy".into());
        }
        let mut selected = [false; 100];
        for id in ids {
            selected[doc
                .layers()
                .position(|l| l.id() == id)
                .ok_or("A selected layer no longer exists")?] = true;
        }
        let mut roots = selected;
        let mut included = selected;
        for (i, layer) in doc.layers().enumerate() {
            let mut parent = layer.parent();
            let mut depth = 0;
            while let Some(id) = parent {
                if depth == 16 {
                    return Err("Group nesting limit exceeded".into());
                }
                depth += 1;
                let p = doc
                    .layers()
                    .position(|l| l.id() == id && l.kind() == "group")
                    .ok_or("Invalid group parent")?;
                if selected[p] {
                    roots[i] = false;
                    included[i] = true;
                }
                parent = doc.layer(p).unwrap().parent();
            }
        }
        for (i, layer) in doc.layers().enumerate() {
            if let Some(id) = layer.clip_to() {
                let base = doc
                    .layers()
                    .position(|l| l.id() == id)
                    .ok_or("Clipping base no longer exists")?;
                if included[i] != included[base] {
                    return Err(
                        "Copy the base and its clipped layers together, or release clipping first"
                            .into(),
                    );
                }
            }
        }
        fn walk(
            doc: DocumentView<'_>,
            parent: Option<&str>,
            depth: usize,
            out: &mut [usize; 100],
            count: &mut usize,
        ) -> Result<(), String> {
            for (i, layer) in doc
                .layers()
                .enumerate()
                .filter(|(_, l)| l.parent() == parent)
            {
                if depth > 16 || *count >= doc.layer_count() {
                    return Err("Invalid group hierarchy".into());
                }
                out[*count] = i;
                *count += 1;
                if layer.kind() == "group" {
                    walk(doc, Some(layer.id()), depth + 1, out, count)?;
                }
            }
            Ok(())
        }
        let mut result = Self {
            roots,
            included,
            preorder: [0; 100],
            length: 0,
        };
        walk(doc, None, 0, &mut result.preorder, &mut result.length)?;
        if result.length != doc.layer_count() {
            return Err("Invalid group hierarchy".into());
        }
        Ok(result)
    }
}
