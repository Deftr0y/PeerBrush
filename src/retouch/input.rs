//! Clone/heal over an explicit captured paint plane or raw paint mask step.
//! Merged artwork and composed masks require their separate prepared provider.
use crate::{
    brush::{input::LocalStroke, Settings},
    edit_bounds::{
        input::{restrict_frame_tile, restrict_tile, InputFrame, RasterInput},
        output::{stage_frame_with, BuiltTiles, TileOutput},
    },
    workspace::source_runtime::SourceSession,
};

pub(crate) fn stage_raster<'d, 's>(
    input: &RasterInput<'d>,
    source: &RasterInput<'d>,
    stroke: &LocalStroke,
    settings: Settings,
    offset: [f32; 2],
    heal_radius: Option<u32>,
    frame: Option<&InputFrame>,
    session: &'s SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    session.check()?;
    check()?;
    stroke.validate_frame(input, frame)?;
    if input.binding().generation() != source.binding().generation()
        || !input.pool().same_pool(source.pool())
        || input.document().metadata().read_only()
        || input.layer_metadata().locked()
    {
        return Err(
            "Native retouch requires the same captured scene and an editable unlocked target"
                .into(),
        );
    }
    if (!input.is_mask() && input.layer_metadata().kind() != "paint")
        || (!source.is_mask() && source.layer_metadata().kind() != "paint")
        || input.is_mask() != source.is_mask()
    {
        return Err(
            "Retouch paint layers together or prepare explicit raw paint mask steps".into(),
        );
    }
    let mut guarded = || {
        session.check()?;
        check()
    };
    let plan = stroke.retouch(input, settings, offset, heal_radius, &mut guarded)?;
    let selection = input.selection()?;
    let render = |key,
                  before: &crate::large_document::PinnedBytes,
                  check: &mut dyn FnMut() -> Result<(), String>| {
        let reads = plan.tile_reads(key)?;
        let shift = frame.map(|f| f.geometry().shift()).unwrap_or([0, 0]);
        let destination = reads
            .destination
            .map(|area| {
                input.prepare_halo(
                    [
                        area[0] - shift[0],
                        area[1] - shift[1],
                        area[2] - shift[0],
                        area[3] - shift[1],
                    ],
                    check,
                )
            })
            .transpose()?;
        let samples = source.prepare_samples(reads.source_queries, check)?;
        let mut after = plan.render_tile(
            key,
            before,
            check,
            |x, y| {
                destination
                    .as_ref()
                    .ok_or("Native healing destination halo is not prepared")?
                    .pixel16(x - shift[0], y - shift[1])
            },
            |x, y| samples.sample16(x, y),
        )?;
        if selection.has_coverage() {
            let area = if let Some(frame) = frame {
                frame.tile_document_rect(key)?
            } else {
                input.tile_document_rect(key)?
            };
            let coverage = selection.prepare(area, check)?;
            after = if let Some(frame) = frame {
                restrict_frame_tile(input, frame, key, before, &after, &coverage, check)?
            } else {
                restrict_tile(input, key, before, &after, &coverage, check)?
            };
        }
        Ok(after)
    };
    if let Some(frame) = frame {
        return stage_frame_with(input, frame, session, &mut guarded, |key, before, check| {
            if plan
                .tiles()
                .binary_search_by_key(&(key.1, key.0), |&(x, y)| (y, x))
                .is_err()
            {
                Ok(before.clone())
            } else {
                render(key, before, check)
            }
        })
        .map(BuiltTiles::Draft);
    }
    let mut output = TileOutput::new(input, session)?;
    let result = (|| {
        for &key in plan.tiles() {
            guarded()?;
            let before = input.prepare_tile(key, &mut guarded)?;
            let after = render(key, before.bytes(), &mut guarded)?;
            output.replace_tile(&before, after, &mut guarded)?;
        }
        guarded()
    })();
    if let Err(mut reason) = result {
        if let Err(cleanup) = output.abort() {
            reason.push_str("; native retouch cleanup retained inventory: ");
            reason.push_str(&cleanup);
        }
        return Err(reason);
    }
    output.finish(&mut guarded).map_err(|refused| {
        let mut reason = refused.reason().to_owned();
        if let Err(cleanup) = refused.abort() {
            reason.push_str("; native retouch cleanup retained inventory: ");
            reason.push_str(&cleanup);
        }
        reason
    })
}
