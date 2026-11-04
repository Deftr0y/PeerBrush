use super::*;

pub(in crate::retained) struct FolderPlan<'a> {
    pub layers: Vec<LayerPlan<'a>>,
    _charge: Reservation,
}

pub(in crate::retained) fn belongs(
    document: &NativeDocument,
    layer: LayerMetadata<'_>,
    root: &str,
    work: &mut Work,
) -> Result<bool, String> {
    work.claim(1)?;
    if layer.id() == root {
        return Ok(true);
    }
    let mut parent = layer.parent();
    for _ in 0..=16 {
        work.claim(1)?;
        let Some(id) = parent else {
            return Ok(false);
        };
        if id == root {
            return Ok(true);
        }
        let mut next = None;
        for candidate in document.metadata().layers() {
            work.claim(1)?;
            if candidate.id() == id {
                next = Some(candidate);
                break;
            }
        }
        parent = next
            .ok_or("Native folder parent no longer exists")?
            .parent();
    }
    Err("Native folder hierarchy exceeds the supported depth".into())
}

/// Hidden descendants participate. Container masks keep their own source and
/// placement; the default pivot comes only from current non-container alpha.
pub(in crate::retained) fn folder<'a>(
    document: &'a NativeDocument,
    id: &str,
    command: &Value,
    work: &mut Work,
    check: &dyn Fn() -> SourceResult<()>,
) -> Result<FolderPlan<'a>, String> {
    check().map_err(|e| e.to_string())?;
    let root = lookup(document, id)?;
    if root.kind() != "group" {
        return Err("Native folder target is not a group".into());
    }
    let count = document.metadata().layers().len();
    let bytes = count
        .checked_mul(std::mem::size_of::<LayerPlan<'_>>() + 128)
        .and_then(|n| n.checked_add(4096))
        .ok_or("Native folder plan overflow")?;
    let charge = document
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut layers = Vec::new();
    layers
        .try_reserve_exact(count)
        .map_err(|_| "Native folder plan allocation refused")?;
    if layers
        .capacity()
        .checked_mul(std::mem::size_of::<LayerPlan<'_>>())
        .is_none_or(|n| n > charge.bytes())
    {
        return Err("Native folder plan exceeds metadata admission".into());
    }
    if command["mask"] == true {
        if let Some(plan) = layer(document, id, command, work, check)? {
            layers.push(plan);
        }
        return Ok(FolderPlan {
            layers,
            _charge: charge,
        });
    }
    let projective = crate::projective::parse(command)?;
    let warp = crate::warp::parse(command)?;
    for node in document.metadata().layers() {
        check().map_err(|e| e.to_string())?;
        if !belongs(document, node, id, work)? {
            continue;
        }
        if node.locked() {
            return Err("A layer inside the folder is locked".into());
        }
        if (projective.is_some() || warp.is_some())
            && (node.source().is_some() || !matches!(node.kind(), "group" | "paint" | "fill"))
        {
            return Err("Rasterize editable text/vector or adjustment children at their native precision before folder Distort, Perspective or Warp".into());
        }
    }
    if document.metadata().selection().is_some() && command["selection_only"] != false {
        return Err("Native selected-folder transaction is not prepared".into());
    }
    let default_pivot = if command["op"] != "move"
        && command["pivot"].is_null()
        && projective.is_none()
        && warp.is_none()
    {
        let mut bounds: Option<[i32; 4]> = None;
        for node in document.metadata().layers() {
            check().map_err(|e| e.to_string())?;
            if !belongs(document, node, id, work)? || matches!(node.kind(), "group" | "adjustment")
            {
                continue;
            }
            let pixels = node.pixels();
            let b = crate::transform::native::content_bounds(
                document.source(pixels.slot())?,
                document.budget(),
                document.disk_budget(),
                work,
                check,
            )?
            .unwrap_or([0, 0, pixels.width() as i32, pixels.height() as i32]);
            let [x, y] = node.origin();
            let b = [b[0] + x, b[1] + y, b[2] + x, b[3] + y];
            bounds = Some(bounds.map_or(b, |a| {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            }));
        }
        let b = bounds.unwrap_or([
            0,
            0,
            document.metadata().width() as i32,
            document.metadata().height() as i32,
        ]);
        Some([
            (f64::from(b[0]) + f64::from(b[2])) / 2.,
            (f64::from(b[1]) + f64::from(b[3])) / 2.,
        ])
    } else {
        None
    };
    for node in document.metadata().layers() {
        check().map_err(|e| e.to_string())?;
        if belongs(document, node, id, work)? {
            if let Some(plan) =
                layer_with_pivot(document, node.id(), command, default_pivot, work, check)?
            {
                layers.push(plan);
            }
        }
    }
    if layers.is_empty() {
        layers.push(unchanged(document, root)?);
    }
    Ok(FolderPlan {
        layers,
        _charge: charge,
    })
}
