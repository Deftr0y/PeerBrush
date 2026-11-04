//! Charged, immutable layer-tree clipboard policy. No scene is reconstructed as
//! resident pixels and no structural edit is published by this view.
use crate::{
    engine::native::{DocumentKey, LayerMetadata, Metadata, NativeDocument, RasterSlot},
    large_document::Reservation,
    transform::native::Work,
};

pub(crate) struct LayerCopyPlan {
    document: NativeDocument,
    roots: Vec<usize>,
    members: Vec<usize>,
    preorder: Vec<usize>,
    _charge: Reservation,
}
impl LayerCopyPlan {
    pub(crate) fn source(&self) -> &NativeDocument {
        &self.document
    }
    pub(crate) fn key(&self) -> DocumentKey {
        self.document.key()
    }
    pub(crate) fn roots(&self) -> &[usize] {
        &self.roots
    }
    /// Legacy snapshots retain storage order; paste emits hierarchy preorder.
    pub(crate) fn members(&self) -> &[usize] {
        &self.members
    }
    pub(crate) fn preorder(&self) -> &[usize] {
        &self.preorder
    }
    pub(crate) fn layer(&self, index: usize) -> Result<LayerMetadata<'_>, String> {
        if !self.members.contains(&index) {
            return Err("Layer is outside the captured clipboard tree".into());
        }
        self.document
            .metadata()
            .layer(index)
            .ok_or_else(|| "Captured clipboard layer is missing".into())
    }
    pub(crate) fn capture(
        source: &NativeDocument,
        ids: &[String],
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if ids.is_empty() || ids.len() > 100 {
            return Err("Choose 1-100 layers to copy".into());
        }
        let metadata = source.metadata();
        let count = metadata.layers().len();
        let bytes = count
            .checked_mul(5 * std::mem::size_of::<usize>() + 4)
            .and_then(|n| n.checked_add(4096))
            .ok_or("Clipboard tree admission overflow")?;
        let charge = source
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut requested = flags(count)?;
        let mut included = flags(count)?;
        let mut ranks = indices(count)?;
        ranks.resize(count, usize::MAX);
        let mut roots = indices(count)?;
        let mut members = indices(count)?;
        let mut preorder = indices(count)?;
        for id in ids {
            let index =
                lookup(metadata, id, work, check)?.ok_or("A selected layer no longer exists")?;
            requested[index] = true;
        }
        for (index, selected) in requested.iter().copied().enumerate() {
            if selected {
                let mut parent = metadata
                    .layer(index)
                    .ok_or("Clipboard source layer is missing")?
                    .parent();
                let mut selected_parent = false;
                for depth in 0..=16 {
                    let Some(id) = parent else { break };
                    let at = lookup(metadata, id, work, check)?
                        .ok_or("Clipboard source parent is missing")?;
                    if requested[at] {
                        selected_parent = true;
                        break;
                    }
                    parent = metadata
                        .layer(at)
                        .ok_or("Clipboard source parent is missing")?
                        .parent();
                    if depth == 16 && parent.is_some() {
                        return Err("Group nesting limit exceeded".into());
                    }
                }
                if !selected_parent {
                    roots.push(index);
                }
            }
        }
        let mut next = 0usize;
        rank(metadata, None, 0, &mut ranks, &mut next, work, check)?;
        if ranks.iter().any(|n| *n == usize::MAX) {
            return Err("Clipboard source hierarchy is disconnected".into());
        }
        roots.sort_by_key(|index| ranks[*index]);
        for index in 0..count {
            check()?;
            let mut at = Some(index);
            let mut member = false;
            for depth in 0..=16 {
                let Some(index) = at else { break };
                work.claim(1)?;
                if roots.contains(&index) {
                    member = true;
                    break;
                }
                at = metadata
                    .layer(index)
                    .ok_or("Clipboard source layer is missing")?
                    .parent()
                    .map(|id| {
                        lookup(metadata, id, work, check)?
                            .ok_or_else(|| String::from("Clipboard source parent is missing"))
                    })
                    .transpose()?;
                if depth == 16 && at.is_some() {
                    return Err("Group nesting limit exceeded".into());
                }
            }
            included[index] = member;
            if member {
                members.push(index);
            }
        }
        for (index, layer) in metadata.layers().enumerate() {
            if let Some(id) = layer.clip_to() {
                let base = lookup(metadata, id, work, check)?
                    .ok_or("Clipboard clipping base is missing")?;
                if included[index] != included[base] {
                    return Err(
                        "Copy the base and its clipped layers together, or release clipping first"
                            .into(),
                    );
                }
            }
        }
        if members.is_empty() || members.len() > 100 || roots.is_empty() {
            return Err("Clipboard needs a valid layer tree within the 100-layer limit".into());
        }
        preorder.extend(members.iter().copied());
        preorder.sort_by_key(|index| ranks[*index]);
        let capacity =
            (roots.capacity() + members.capacity() + preorder.capacity() + ranks.capacity())
                * std::mem::size_of::<usize>()
                + requested.capacity()
                + included.capacity();
        if capacity > charge.bytes() {
            return Err("Clipboard tree buffers exceed admission".into());
        }
        check()?;
        Ok(Self {
            document: source.clone(),
            roots,
            members,
            preorder,
            _charge: charge,
        })
    }
    pub(crate) fn check_cut(
        &self,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.document.metadata().read_only() {
            return Err("This PSD is read-only".into());
        }
        for index in &self.members {
            unlocked(self.document.metadata(), *index, work, check)?;
        }
        Ok(())
    }
}
fn flags(count: usize) -> Result<Vec<bool>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| "Clipboard tree flags allocation refused")?;
    result.resize(count, false);
    Ok(result)
}
fn indices(count: usize) -> Result<Vec<usize>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| "Clipboard tree index allocation refused")?;
    Ok(result)
}
fn lookup(
    metadata: Metadata<'_>,
    id: &str,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<usize>, String> {
    for (index, layer) in metadata.layers().enumerate() {
        check()?;
        work.claim(1)?;
        if layer.id() == id {
            return Ok(Some(index));
        }
    }
    Ok(None)
}
fn rank(
    metadata: Metadata<'_>,
    parent: Option<&str>,
    depth: usize,
    ranks: &mut [usize],
    next: &mut usize,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    for (index, layer) in metadata.layers().enumerate() {
        check()?;
        work.claim(1)?;
        if layer.parent() == parent {
            if depth > 16 {
                return Err("Group nesting limit exceeded".into());
            }
            if ranks[index] != usize::MAX {
                return Err("Clipboard hierarchy repeats a layer".into());
            }
            ranks[index] = *next;
            *next += 1;
            if layer.kind() == "group" {
                rank(
                    metadata,
                    Some(layer.id()),
                    depth + 1,
                    ranks,
                    next,
                    work,
                    check,
                )?;
            }
        }
    }
    Ok(())
}
fn unlocked(
    metadata: Metadata<'_>,
    index: usize,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let mut at = Some(index);
    for depth in 0..=16 {
        let Some(index) = at else { return Ok(()) };
        check()?;
        work.claim(1)?;
        let layer = metadata
            .layer(index)
            .ok_or("Destination layer or folder no longer exists")?;
        if layer.locked() {
            return Err("Unlock the selected layer and its containing folders first".into());
        }
        at = layer
            .parent()
            .map(|id| {
                lookup(metadata, id, work, check)?
                    .ok_or_else(|| String::from("Destination parent is missing"))
            })
            .transpose()?;
        if depth == 16 && at.is_some() {
            return Err("Group nesting limit exceeded".into());
        }
    }
    Ok(())
}

pub(crate) struct PastePlacement {
    destination: DocumentKey,
    selected: Option<RasterSlot>,
    at: usize,
    parent: Option<RasterSlot>,
    wrap_base: Option<RasterSlot>,
    wrap_members: Vec<RasterSlot>,
    _charge: Reservation,
}
impl PastePlacement {
    pub(crate) fn destination(&self) -> DocumentKey {
        self.destination
    }
    pub(crate) fn selected(&self) -> Option<RasterSlot> {
        self.selected
    }
    pub(crate) fn insertion(&self) -> usize {
        self.at
    }
    pub(crate) fn parent(&self) -> Option<RasterSlot> {
        self.parent
    }
    pub(crate) fn wrap_base(&self) -> Option<RasterSlot> {
        self.wrap_base
    }
    pub(crate) fn wrap_members(&self) -> &[RasterSlot] {
        &self.wrap_members
    }
    pub(crate) fn new(
        destination: &NativeDocument,
        source: &LayerCopyPlan,
        target: &str,
        work: &mut Work,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        let metadata = destination.metadata();
        if metadata.read_only() {
            return Err("This PSD is read-only".into());
        }
        let count = metadata.layers().len();
        let found = lookup(metadata, target, work, check)?;
        if found.is_none() && !target.is_empty() && count > 0 {
            return Err(
                "Paste destination no longer exists; choose a layer or folder again".into(),
            );
        }
        let selected = found.or_else(|| (count > 0).then_some(0));
        if let Some(index) = selected {
            unlocked(metadata, index, work, check)?;
        }
        let selected_layer = selected
            .map(|index| metadata.layer(index).ok_or("Paste destination is missing"))
            .transpose()?;
        let wrap =
            selected_layer.is_some_and(|layer| layer.kind() != "group" && layer.parent().is_none());
        if count + source.members.len() + usize::from(wrap) > 100 {
            return Err("Initial version supports up to 100 layers".into());
        }
        let bytes = count
            .checked_mul(std::mem::size_of::<RasterSlot>() + 64)
            .and_then(|n| n.checked_add(4096))
            .ok_or("Clipboard placement admission overflow")?;
        let charge = destination
            .budget()
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut wrap_members = Vec::new();
        wrap_members
            .try_reserve_exact(count)
            .map_err(|_| "Clipboard clipping unit allocation refused")?;
        let (mut at, mut parent, mut wrap_base) = (0, None, None);
        if let Some(index) = selected {
            let current = metadata
                .layer(index)
                .ok_or("Paste destination is missing")?;
            if wrap {
                let id = current.clip_to().unwrap_or(current.id());
                let base =
                    lookup(metadata, id, work, check)?.ok_or("Paste clipping base is missing")?;
                wrap_base = Some(
                    metadata
                        .layer(base)
                        .ok_or("Paste clipping base is missing")?
                        .pixels()
                        .slot(),
                );
                at = usize::MAX;
                for (index, layer) in metadata.layers().enumerate() {
                    check()?;
                    work.claim(1)?;
                    if layer.parent() == current.parent()
                        && (layer.id() == id || layer.clip_to() == Some(id))
                    {
                        at = at.min(index);
                        wrap_members.push(layer.pixels().slot());
                    }
                }
                if at == usize::MAX {
                    return Err("Paste clipping unit is empty".into());
                }
                at += 1;
            } else if current.kind() == "group" {
                at = index + 1;
                parent = Some(current.pixels().slot());
            } else {
                at = index;
                parent = current
                    .parent()
                    .map(|id| {
                        lookup(metadata, id, work, check)?
                            .and_then(|index| metadata.layer(index))
                            .map(|layer| layer.pixels().slot())
                            .ok_or_else(|| String::from("Paste parent is missing"))
                    })
                    .transpose()?;
            }
        }
        if wrap_members.capacity() * std::mem::size_of::<RasterSlot>() > charge.bytes() {
            return Err("Clipboard clipping unit exceeds admission".into());
        }
        check()?;
        Ok(Self {
            destination: destination.key(),
            selected: selected_layer.map(|layer| layer.pixels().slot()),
            at,
            parent,
            wrap_base,
            wrap_members,
            _charge: charge,
        })
    }
}
