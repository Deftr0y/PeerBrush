//! Exact native8-to16 source promotion. Scalar coverage is not a color plane.
use crate::{
    engine::native::{DocumentKey, NativeDocument, RasterMetadata, RasterSlot, StagedOutput},
    large_document::{Reservation, ScratchBytes, Store, TileSpec},
    raster::{backed::Plane, Surface, SurfaceKey, TILE},
    retained::transaction::{self, Session},
    transform::native::Work,
};
use std::collections::BTreeMap;

pub(crate) struct Promotion {
    target: RasterSlot,
    input: SurfaceKey,
    output: StagedOutput,
}
impl Promotion {
    pub(crate) fn target(&self) -> RasterSlot {
        self.target
    }
    pub(crate) fn input(&self) -> SurfaceKey {
        self.input
    }
    pub(crate) fn output(&self) -> &StagedOutput {
        &self.output
    }
}
pub(crate) struct Promotions {
    source: DocumentKey,
    entries: Vec<Promotion>,
    // Both the descriptor list and its Vec capacity remain charged until the
    // complete nominal insertion consumer has copied or retired every entry.
    _charge: Reservation,
}
impl Promotions {
    pub(crate) fn source(&self) -> DocumentKey {
        self.source
    }
    pub(crate) fn entries(&self) -> &[Promotion] {
        &self.entries
    }
    pub(crate) fn abort(self, session: &impl Session) -> Result<(), String> {
        let mut failure = None;
        for entry in self.entries {
            if let Err(reason) = entry.output.abort(session, &|| Ok(())) {
                failure = Some(reason);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

/// None promotes the whole destination. A layer-buffer conversion supplies its
/// generation-bound primary slots; masks and originals follow those same roots.
pub(crate) fn stage(
    source: &NativeDocument,
    members: Option<&[RasterSlot]>,
    session: &impl Session,
    work: &mut Work,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Promotions, String> {
    check()?;
    session.check()?;
    if !source.budget().same_pool(&session.resident())
        || !source.disk_budget().same_pool(&session.disk())
    {
        return Err("Clipboard promotion session uses another admission".into());
    }
    if source.metadata().depth() != 8 {
        return Err("Clipboard promotion requires a native8 source".into());
    }
    let mut count = 0usize;
    for layer in source.metadata().layers() {
        if members.is_some_and(|members| !members.contains(&layer.pixels().slot())) {
            continue;
        }
        count = count
            .checked_add(2 * (1 + layer.mask().map_or(0, |m| m.steps().len())))
            .ok_or("Clipboard promotion count overflow")?;
    }
    if let Some(members) = members {
        for target in members {
            source.source(*target)?;
            if !source
                .metadata()
                .layers()
                .any(|layer| layer.pixels().slot() == *target)
            {
                return Err("Clipboard promotion needs primary color slots".into());
            }
        }
    }
    let bytes = count
        .checked_mul(std::mem::size_of::<RasterSlot>() + std::mem::size_of::<Promotion>() + 128)
        .and_then(|n| n.checked_add(4096))
        .ok_or("Clipboard promotion admission overflow")?;
    let charge = source
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut targets = Vec::new();
    targets
        .try_reserve_exact(count)
        .map_err(|_| "Clipboard promotion target allocation refused")?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| "Clipboard promotion owner allocation refused")?;
    if targets.capacity() * std::mem::size_of::<RasterSlot>()
        + entries.capacity() * std::mem::size_of::<Promotion>()
        > charge.bytes()
    {
        return Err("Clipboard promotion lists exceed admission".into());
    }
    let mut add = |pixels: RasterMetadata<'_>| {
        if !targets.contains(&pixels.slot()) {
            targets.push(pixels.slot());
        }
        if let Some(original) = pixels.retained() {
            let slot = original.pixels().slot();
            if !targets.contains(&slot) {
                targets.push(slot);
            }
        }
    };
    for layer in source.metadata().layers() {
        if members.is_some_and(|members| !members.contains(&layer.pixels().slot())) {
            continue;
        }
        add(layer.pixels());
        if let Some(mask) = layer.mask() {
            for step in mask.steps() {
                add(step.pixels());
            }
        }
    }
    let staged = (|| {
        for target in &targets {
            check()?;
            session.check()?;
            let input = source.source(*target)?;
            if input.depth() != 8 {
                return Err("Clipboard color source graph has mixed native depth".into());
            }
            let output = transaction::stage(source, session, check, |store, checked| {
                write(input, store, work, checked)
            })?;
            entries.push(Promotion {
                target: *target,
                input: input.key(),
                output,
            });
        }
        check()?;
        session.check()
    })();
    drop(targets);
    let result = Promotions {
        source: source.key(),
        entries,
        _charge: charge,
    };
    if let Err(reason) = staged {
        return match result.abort(session) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!("{reason}; promoted source retained: {cleanup}")),
        };
    }
    Ok(result)
}
fn write(
    source: &Surface,
    store: &Store,
    work: &mut Work,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Surface, String> {
    let pool = store.resident_budget();
    let disk = store
        .disk_budget()
        .ok_or("Clipboard promotion needs shared disk admission")?;
    if source.depth() != 8 || !source.uses_budget(&pool) || !source.uses_disk_budget(&disk) {
        return Err("Clipboard promotion source admission differs".into());
    }
    let count = source.physical_tiles().count();
    if count == 0 {
        if !source.is_known_zero() {
            return Err("Clipboard promotion source has no complete physical layout".into());
        }
        return Surface::zero(source.width(), source.height(), 16).map_err(|e| e.to_string());
    }
    let index_bytes = count
        .checked_mul(512)
        .and_then(|n| n.checked_add(4096))
        .ok_or("Clipboard promotion index overflow")?;
    let index = pool
        .reserve_bytes(index_bytes, index_bytes)
        .map_err(|e| e.to_string())?;
    let mut blocks = BTreeMap::new();
    let spec = TileSpec::new(TILE as u16, TILE as u16, 16).map_err(|e| e.to_string())?;
    let mut output =
        ScratchBytes::new(&pool, spec.bytes(), spec.bytes()).map_err(|e| e.to_string())?;
    for tile in source.physical_tiles() {
        check().map_err(|e| e.to_string())?;
        work.claim(u64::from(TILE) * u64::from(TILE))?;
        let coordinate = tile.coordinate();
        let input = source
            .prepare_tile(coordinate, &mut || check())
            .map_err(|e| e.to_string())?
            .copy_to_scratch(&pool)
            .map_err(|e| e.to_string())?;
        for (at, (word, byte)) in output.chunks_exact_mut(2).zip(input.iter()).enumerate() {
            if at % 4096 == 0 {
                check().map_err(|e| e.to_string())?;
            }
            word.fill(*byte);
        }
        // Physical presence, hidden RGB and nonlogical padding all participate.
        blocks.insert(
            coordinate,
            store
                .put(spec, &output, &mut || check())
                .map_err(|e| e.to_string())?,
        );
    }
    let result = Surface::from_plane(
        Plane::from_blocks(source.width(), source.height(), 16, store.clone(), blocks)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    drop(index);
    check().map_err(|e| e.to_string())?;
    Ok(result)
}
