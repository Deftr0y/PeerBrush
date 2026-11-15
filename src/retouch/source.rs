//! Snapshot sampling of raw planes, composed masks and merged artwork.
use crate::{
    color_profile::prepared::{DisplayPlan, DisplayStatus},
    compositor::native::{
        prepare_filters_with_layers, LayerWindow, Native16, Native8, Plan, Stored, Topology,
    },
    edit_bounds::{
        input::{InputFrame, PreparedSamples, RasterInput, RetouchInput},
        output::BuiltTiles,
    },
    engine::{
        native::{LayerMetadata, NativeDocument},
        scene::{SceneIdentity, SceneSnapshot},
    },
    large_document::{PinnedBytes, ScratchBytes},
    raster::{
        backed::{Region, MAX_REGION_PIXELS},
        project16,
    },
    workspace::source_runtime::SourceSession,
};
use std::sync::Arc;

pub(super) enum Source<'a, 'b> {
    Raster(&'b RasterInput<'a>),
    Mask {
        document: &'a NativeDocument,
        layer: LayerMetadata<'a>,
    },
    Merged {
        topology: &'b Arc<Topology<'a>>,
        stored: Option<&'b Stored<'a>>,
        display: Option<&'b DisplayPlan>,
    },
}

pub(super) fn finish<'d, 's>(
    result: Result<BuiltTiles<'d, 's>, String>,
    cleanup: Result<(), String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => {
            Err(format!("{reason}; retouch source retirement: {cleanup}"))
        }
        (Ok(built), Err(reason)) => {
            let cleanup = match built {
                BuiltTiles::Unchanged => Ok(()),
                BuiltTiles::Draft(draft) => draft.abort(),
            };
            Err(match cleanup {
                Ok(()) => reason,
                Err(cleanup) => format!("{reason}; retouch output retirement: {cleanup}"),
            })
        }
    }
}

pub(super) fn with_source<'d, 's>(
    source: &RetouchInput<'_>,
    session: &SourceSession,
    check: &mut dyn FnMut() -> Result<(), String>,
    run: impl FnOnce(
        &Source<'_, '_>,
        &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<BuiltTiles<'d, 's>, String>,
) -> Result<BuiltTiles<'d, 's>, String> {
    match source {
        RetouchInput::Raster(input) => run(&Source::Raster(input), check),
        RetouchInput::ComposedMask { document, layer } => run(
            &Source::Mask {
                document,
                layer: *layer,
            },
            check,
        ),
        RetouchInput::Merged(document) => {
            crate::raster::CanvasLimits::Native
                .check(document.metadata().width(), document.metadata().height())?;
            let scene = SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: String::new(),
                    document_id: document.metadata().id().into(),
                    revision: document.metadata().revision(),
                },
                (*document).clone(),
                vec![],
                None,
                None,
            )?;
            let topology = Topology::new(&scene)?;
            let display = if document.metadata().depth() == 8 {
                Some(DisplayPlan::prepare(
                    document.metadata().profile(),
                    false,
                    document.budget(),
                    check,
                )?)
            } else {
                None
            };
            let stored = if Stored::composite_needed(&topology, None) {
                Some(Stored::prepare_composite(topology.clone(), session, None, check)?)
            } else {
                None
            };
            let result = run(
                &Source::Merged {
                    topology: &topology,
                    stored: stored.as_ref(),
                    display: display.as_ref(),
                },
                check,
            );
            let cleanup = stored.map_or(Ok(()), |s| s.retire(&mut || session.cleanup_check()));
            finish(result, cleanup)
        }
    }
}

pub(super) enum Samples<'a> {
    Raster(PreparedSamples<'a>),
    Rendered {
        queries: [f32; 4],
        origin: [i32; 2],
        shape: [u32; 2],
        area: [i32; 4],
        bytes: Option<PinnedBytes>,
    },
}
impl Samples<'_> {
    pub(super) fn sample16(&self, x: f32, y: f32) -> Result<[u16; 4], String> {
        let Self::Rendered {
            queries,
            origin,
            shape,
            area,
            bytes,
        } = self
        else {
            let Self::Raster(samples) = self else {
                unreachable!()
            };
            return samples.sample16(x, y);
        };
        if !x.is_finite()
            || !y.is_finite()
            || x < queries[0]
            || y < queries[1]
            || x > queries[2]
            || y > queries[3]
        {
            return Err("Retouch sample is outside its declared source queries".into());
        }
        let [x, y] = [x - origin[0] as f32, y - origin[1] as f32];
        if x <= -1. || y <= -1. || x >= shape[0] as f32 || y >= shape[1] as f32 {
            return Ok([0; 4]);
        }
        super::native::bilinear(x, y, |x, y| {
            if x < 0 || y < 0 || x >= shape[0] as i32 || y >= shape[1] as i32 {
                return Ok([0; 4]);
            }
            if x < area[0] || y < area[1] || x >= area[2] || y >= area[3] {
                return Err("Retouch sample exceeds its prepared rendered footprint".into());
            }
            let bytes = bytes.as_ref().ok_or("Retouch source pixels missing")?;
            let at = ((y - area[1]) as usize * (area[2] - area[0]) as usize
                + (x - area[0]) as usize)
                * 8;
            Ok(std::array::from_fn(|c| {
                u16::from_be_bytes([bytes[at + c * 2], bytes[at + c * 2 + 1]])
            }))
        })
    }
}
impl<'a> Source<'a, '_> {
    pub(super) fn prepare(
        &self,
        input: &RasterInput<'_>,
        frame: Option<&InputFrame>,
        queries: [f32; 4],
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Samples<'a>, String> {
        if let Self::Raster(source) = self {
            let frame = frame
                .filter(|_| input.layer_metadata().id() == source.layer_metadata().id())
                .map(|f| f.for_plane(source))
                .transpose()?;
            return source
                .prepare_samples(queries, frame.as_ref(), check)
                .map(Samples::Raster);
        }
        check()?;
        if queries.iter().any(|v| !v.is_finite() || v.abs() > 800_000.)
            || queries[0] > queries[2]
            || queries[1] > queries[3]
        {
            return Err("Invalid native sampling query footprint".into());
        }
        let (origin, shape, pool) = match self {
            Self::Mask { document, layer } => (
                layer.origin(),
                [layer.pixels().width(), layer.pixels().height()],
                document.budget(),
            ),
            Self::Merged { topology, .. } => (
                [0, 0],
                [
                    topology.scene().metadata().width(),
                    topology.scene().metadata().height(),
                ],
                topology.scene().budget(),
            ),
            Self::Raster(_) => unreachable!(),
        };
        let local = [
            queries[0] - origin[0] as f32,
            queries[1] - origin[1] as f32,
            queries[2] - origin[0] as f32,
            queries[3] - origin[1] as f32,
        ];
        let support = [
            local[0].floor() as i32,
            local[1].floor() as i32,
            local[2].floor() as i32 + 2,
            local[3].floor() as i32 + 2,
        ];
        if support[2] - support[0] > 385 || support[3] - support[1] > 385 {
            return Err("Native bilinear footprint exceeds one complete healing halo".into());
        }
        let area = [
            support[0].max(0),
            support[1].max(0),
            support[2].min(shape[0] as i32),
            support[3].min(shape[1] as i32),
        ];
        let bytes = if area[0] < area[2] && area[1] < area[3] {
            let len = (area[2] - area[0]) as usize * (area[3] - area[1]) as usize * 8;
            let mut bytes = ScratchBytes::new(pool, len, len).map_err(|e| e.to_string())?;
            match self {
                Self::Mask { document, layer } => {
                    let stripe = MAX_REGION_PIXELS / (area[2] - area[0]) as usize;
                    for top in (area[1]..area[3]).step_by(stripe) {
                        let bottom = (top + stripe as i32).min(area[3]);
                        let mask = crate::mask::region::prepare_native(
                            document,
                            *layer,
                            [
                                origin[0] + area[0],
                                origin[1] + top,
                                origin[0] + area[2],
                                origin[1] + bottom,
                            ],
                            check,
                        )?;
                        for y in top..bottom {
                            check()?;
                            for x in area[0]..area[2] {
                                let value = if document.metadata().depth() == 16 {
                                    (mask.value16(x, y)? * 65535.).round() as u16
                                } else {
                                    u16::from(project16(
                                        (mask.value8(x, y)? * 65535.).round() as u16
                                    )) * 257
                                };
                                put(&mut bytes, area, x, y, [value, value, value, 65535]);
                            }
                        }
                    }
                }
                Self::Merged {
                    topology,
                    stored,
                    display,
                } => {
                    // Native8 preview executes CMS on complete canvas rows. Keep
                    // those SIMD/tail boundaries, then retain only the footprint.
                    let full_row = display.is_some_and(|d| d.status() == DisplayStatus::Converted);
                    let left = if full_row { 0 } else { area[0] };
                    let right = if full_row { shape[0] as i32 } else { area[2] };
                    let width = if full_row {
                        shape[0] as usize
                    } else {
                        (area[2] - area[0]) as usize
                    };
                    let stripe = MAX_REGION_PIXELS / width;
                    let mut rows = if display.is_some() {
                        Some((
                            ScratchBytes::new(pool, width * 4, width * 4)
                                .map_err(|e| e.to_string())?,
                            ScratchBytes::new(pool, width * 4, width * 4)
                                .map_err(|e| e.to_string())?,
                        ))
                    } else {
                        None
                    };
                    for top in (area[1]..area[3]).step_by(stripe) {
                        let bottom = (top + stripe as i32).min(area[3]);
                        let region =
                            Region::new([left, top, right, bottom]).map_err(|e| e.to_string())?;
                        let filtered =
                            prepare_filters_with_layers(topology, region, *stored, check)?;
                        let window = if filtered.is_none() {
                            Some(LayerWindow::prepare(topology, region, *stored, check)?)
                        } else {
                            None
                        };
                        if let Some(display) = display {
                            let plan = window
                                .as_ref()
                                .map(|w| {
                                    Plan::<Native8, _>::with_topology(
                                        (*topology).clone(),
                                        w,
                                        region,
                                    )
                                })
                                .transpose()?;
                            let (raw, converted) =
                                rows.as_mut().ok_or("Retouch display rows missing")?;
                            for y in top..bottom {
                                check()?;
                                for (x, p) in raw.chunks_exact_mut(4).enumerate() {
                                    let x = left + x as i32;
                                    p.copy_from_slice(&if let Some(f) = &filtered {
                                        f.pixel(x, y)?
                                    } else {
                                        plan.as_ref()
                                            .ok_or("Retouch compositor missing")?
                                            .pixel(x, y)?
                                    });
                                }
                                display.convert8(raw, converted, check)?;
                                for x in area[0]..area[2] {
                                    put(
                                        &mut bytes,
                                        area,
                                        x,
                                        y,
                                        std::array::from_fn(|c| {
                                            u16::from(converted[(x - left) as usize * 4 + c]) * 257
                                        }),
                                    );
                                }
                            }
                        } else {
                            let plan = window
                                .as_ref()
                                .map(|w| {
                                    Plan::<Native16, _>::with_topology(
                                        (*topology).clone(),
                                        w,
                                        region,
                                    )
                                })
                                .transpose()?;
                            for y in top..bottom {
                                check()?;
                                for x in area[0]..area[2] {
                                    let p = if let Some(f) = &filtered {
                                        f.pixel16(x, y)?
                                    } else {
                                        plan.as_ref()
                                            .ok_or("Retouch compositor missing")?
                                            .pixel(x, y)?
                                    };
                                    put(&mut bytes, area, x, y, p);
                                }
                            }
                        }
                    }
                }
                Self::Raster(_) => unreachable!(),
            }
            Some(bytes.freeze())
        } else {
            None
        };
        check()?;
        Ok(Samples::Rendered {
            queries,
            origin,
            shape,
            area,
            bytes,
        })
    }
}
fn put(bytes: &mut [u8], area: [i32; 4], x: i32, y: i32, pixel: [u16; 4]) {
    let at = ((y - area[1]) as usize * (area[2] - area[0]) as usize + (x - area[0]) as usize) * 8;
    for (out, value) in bytes[at..at + 8].chunks_exact_mut(2).zip(pixel) {
        out.copy_from_slice(&value.to_be_bytes());
    }
}
