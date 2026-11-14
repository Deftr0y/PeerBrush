//! Source-independent folder planning under the shared 100-layer/16-parent limits.
use crate::engine::view::DocumentView;

pub(crate) struct Plan {
    pub(crate) parent: Option<usize>,
    roots: [bool; 100],
    order: [usize; 101],
    length: usize,
}
impl Plan {
    pub(crate) fn root(&self, index: usize) -> bool {
        self.roots[index]
    }

    pub(crate) fn new(doc: DocumentView<'_>, ids: &[String]) -> Result<Self, String> {
        if doc.read_only() {
            return Err("This PSD is read-only".into());
        }
        let n = doc.layer_count();
        if n >= 100 {
            return Err("Initial version supports up to 100 layers".into());
        }
        if ids.len() > 100 {
            return Err("Choose 1–100 layers to copy".into());
        }
        let mut selected = [false; 100];
        for id in ids {
            let index = doc
                .layers()
                .position(|l| l.id() == id)
                .ok_or("A selected layer no longer exists")?;
            selected[index] = true;
        }
        let parent = |index: usize| -> Result<Option<usize>, String> {
            doc.layer(index)
                .unwrap()
                .parent()
                .map(|id| {
                    doc.layers()
                        .position(|l| l.id() == id && l.kind() == "group")
                        .ok_or_else(|| "Invalid group parent".into())
                })
                .transpose()
        };
        let mut chains = [[usize::MAX; 16]; 100];
        let mut depths = [0; 100];
        let mut roots = selected;
        let mut included = selected;
        for i in 0..n {
            let mut at = parent(i)?;
            while let Some(p) = at {
                if depths[i] == 16 {
                    return Err("Group nesting limit exceeded".into());
                }
                chains[i][depths[i]] = p;
                depths[i] += 1;
                if selected[p] {
                    roots[i] = false;
                    included[i] = true;
                }
                at = parent(p)?;
            }
            if included[i]
                && std::iter::once(i)
                    .chain(chains[i][..depths[i]].iter().copied())
                    .any(|j| doc.layer(j).unwrap().locked())
            {
                return Err("Unlock the selected layer and its containing folders first".into());
            }
            if let Some(base) = doc.layer(i).unwrap().clip_to() {
                // Membership of later siblings is checked after all chains exist.
                if doc.find_layer(base).is_none() {
                    return Err("Clipping base no longer exists".into());
                }
            }
        }
        for (i, layer) in doc.layers().enumerate() {
            if let Some(base) = layer.clip_to() {
                let base = doc.layers().position(|l| l.id() == base).unwrap();
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
            order: &mut [usize; 100],
            count: &mut usize,
        ) -> Result<(), String> {
            for (i, layer) in doc
                .layers()
                .enumerate()
                .filter(|(_, l)| l.parent() == parent)
            {
                if depth > 16 {
                    return Err("Group nesting limit exceeded".into());
                }
                if *count >= doc.layer_count() {
                    return Err("Invalid group hierarchy".into());
                }
                order[*count] = i;
                *count += 1;
                if layer.kind() == "group" {
                    walk(doc, Some(layer.id()), depth + 1, order, count)?;
                }
            }
            Ok(())
        }
        let mut preorder = [0; 100];
        let mut count = 0;
        walk(doc, None, 0, &mut preorder, &mut count)?;
        if count != n {
            return Err("Invalid group hierarchy".into());
        }
        let first = preorder[..n].iter().copied().find(|&i| roots[i]);
        let common = first.and_then(|first| {
            chains[first][..depths[first]].iter().copied().find(|p| {
                (0..n)
                    .filter(|&i| roots[i])
                    .all(|i| chains[i][..depths[i]].contains(p))
            })
        });
        let mut anchor = first.unwrap_or(0);
        if first.is_some() {
            while parent(anchor)? != common {
                anchor = parent(anchor)?.ok_or("Group anchor is outside its common parent")?;
            }
        }
        let new_depth = common.map_or(0, |p| depths[p] + 1);
        for i in 0..n {
            if included[i] {
                let root = if roots[i] {
                    i
                } else {
                    *chains[i][..depths[i]]
                        .iter()
                        .find(|&&p| roots[p])
                        .ok_or("Selected root missing")?
                };
                if depths[i] - depths[root] + new_depth + 1 > 16 {
                    return Err("Group nesting limit exceeded".into());
                }
            }
        }
        let at = (0..anchor).filter(|&i| !included[i]).count();
        let mut plan = Self {
            parent: common,
            roots,
            order: [0; 101],
            length: n + 1,
        };
        let mut out = 0;
        for i in (0..n).filter(|&i| !included[i]).take(at) {
            plan.order[out] = i;
            out += 1;
        }
        plan.order[out] = n;
        out += 1;
        for i in preorder[..n].iter().copied().filter(|&i| included[i]) {
            plan.order[out] = i;
            out += 1;
        }
        for i in (0..n).filter(|&i| !included[i]).skip(at) {
            plan.order[out] = i;
            out += 1;
        }
        if out != plan.length {
            return Err("Group order does not cover the document".into());
        }
        Ok(plan)
    }

    /// Move original owners in place; no raster or source payload is cloned.
    pub(crate) fn apply<T>(&self, layers: &mut Vec<T>, folder: T) -> Result<(), String> {
        if layers.len() + 1 != self.length {
            return Err("Group plan belongs to another layer roster".into());
        }
        layers
            .try_reserve_exact(1)
            .map_err(|_| "Folder allocation refused")?;
        layers.push(folder);
        let mut current: [usize; 101] = std::array::from_fn(|i| i);
        for (i, &wanted) in self.order[..self.length].iter().enumerate() {
            let at = (i..self.length)
                .find(|&j| current[j] == wanted)
                .ok_or("Group order is not a permutation")?;
            layers.swap(i, at);
            current.swap(i, at);
        }
        Ok(())
    }
}
