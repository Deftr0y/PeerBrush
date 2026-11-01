use super::*;
use crate::{
    engine::{scene::RasterCollector, Document, Engine, History},
    large_document::{DiskBudget, Limits as StoreLimits, Store, TileSpec},
    raster::{backed::Plane, Raster, Surface},
};

struct Sources {
    originals: [Vec<Surface>; 3],
    prepared: Vec<Surface>,
    output: Vec<Surface>,
    store: Option<Store>,
    path: std::path::PathBuf,
    pool: ResidentBudget,
    disk: DiskBudget,
    calls: usize,
    fail: bool,
}
impl Sources {
    fn new() -> Self {
        Self::with_resident_limit(128 * 1024 * 1024)
    }
    fn with_resident_limit(bytes: usize) -> Self {
        let pool = ResidentBudget::new(bytes).unwrap();
        let disk = DiskBudget::new(32 * 1024 * 1024, 4).unwrap();
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "peerbrush-metadata-inverse-{}",
                crate::engine::id()
            ));
        let store = Store::create_with_disk_budget(
            &path,
            pool.clone(),
            disk.clone(),
            StoreLimits {
                disk_bytes: 32 * 1024 * 1024,
                max_blocks: 64,
                max_resident_blocks: 8,
                manifest_bytes: 65536,
            },
        )
        .unwrap();
        Self {
            originals: std::array::from_fn(|_| Vec::new()),
            prepared: Vec::new(),
            output: Vec::new(),
            store: Some(store),
            path,
            pool,
            disk,
            calls: 0,
            fail: false,
        }
    }
    fn index(side: Side) -> usize {
        match side {
            Side::Before => 0,
            Side::After => 1,
            Side::Current => 2,
        }
    }
    fn stage(&self, raster: &Raster) -> Surface {
        if raster.tiles.is_empty() && raster.samples16.is_empty() {
            return Surface::zero(raster.width, raster.height, raster.depth).unwrap();
        }
        Surface::from_plane(
            Plane::from_resident(raster, self.store.as_ref().unwrap().clone(), &mut || Ok(()))
                .unwrap(),
        )
        .unwrap()
    }
    fn source(&self, source: Source<'_>) -> Result<&Surface> {
        let (values, reference) = match source {
            Source::Original(side, reference) => (&self.originals[Self::index(side)], reference),
            Source::Prepared(reference) => (&self.prepared, reference),
        };
        let surface = values
            .get(reference.id.checked_sub(1).ok_or("Bad fixture ordinal")? as usize)
            .ok_or("Missing side-qualified fixture source")?;
        if (surface.width(), surface.height(), surface.depth())
            != (reference.width, reference.height, reference.depth)
        {
            return Err("Fixture source shape differs".into());
        }
        Ok(surface)
    }
    fn capture(&mut self, document: &Document, side: Side) -> Document36 {
        struct Collector<'a> {
            sources: &'a mut Sources,
            side: Side,
        }
        impl RasterCollector for Collector<'_> {
            fn resident(&mut self, raster: &Raster) -> Result<RasterRef36> {
                let surface = self.sources.stage(raster);
                let values = &mut self.sources.originals[Sources::index(self.side)];
                values.push(surface);
                Ok(reference(
                    raster.width,
                    raster.height,
                    raster.depth,
                    values.len(),
                ))
            }
            fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
                Err("Fixture only captures actual resident oracle sources".into())
            }
        }
        Document36::capture(
            document,
            &mut Collector {
                sources: self,
                side,
            },
        )
        .unwrap()
    }
}

#[test]
fn compensation_shares_unchanged_large_settings_and_merges_only_edited_assets() {
    let mut sources = Sources::with_resident_limit(768 * 1024 * 1024);
    let pool = sources.pool.clone();
    let construction = MetadataCharge::reserve(&pool, 128 * 1024 * 1024, &|| Ok(())).unwrap();
    let doc = document(16);
    let raw = sources.capture(&doc, Side::Before);
    sources.originals[1] = sources.originals[0].clone();
    sources.originals[2] = sources.originals[0].clone();
    let mut before = copy_metadata(&raw, &pool, &|| Ok(())).unwrap();
    before = before
        .map_unique(|mut value, _| {
            for index in 0..8 {
                value.layers[0].effects.push(Effect36 {
                    id: format!("asset{index}"),
                    kind: "test".into(),
                    enabled: true,
                    weight: 1.,
                    settings: MetadataPayload::from(
                        serde_json::json!({"asset":"x".repeat(4*1024*1024-512)}),
                    )
                    .bind(&pool, &|| Ok(()))?,
                    clips: vec![],
                });
            }
            value.layers[0].effects.push(Effect36 {
                id: "small".into(),
                kind: "test".into(),
                enabled: true,
                weight: 1.,
                settings: MetadataPayload::from(serde_json::json!({"agent":1,"human":2}))
                    .bind(&pool, &|| Ok(()))?,
                clips: vec![],
            });
            Ok(value)
        })
        .unwrap();
    // Rehome the newly built headers before they enter an immutable capture.
    let before = before
        .rehome_unique(|value, _| value.bind_runtime(&pool, &|| Ok(())))
        .unwrap();
    let after = before
        .copy_runtime(&pool, &|| Ok(()))
        .unwrap()
        .map_unique(|mut value, _| {
            value.layers[0].name = "Task name".into();
            value.layers[0].effects[8].settings =
                MetadataPayload::from(serde_json::json!({"agent":3,"human":2}))
                    .bind(&pool, &|| Ok(()))?;
            Ok(value)
        })
        .unwrap();
    let current = after
        .copy_runtime(&pool, &|| Ok(()))
        .unwrap()
        .map_unique(|mut value, _| {
            value.layers[0].opacity = 0.4;
            value.layers[0].effects[8].settings =
                MetadataPayload::from(serde_json::json!({"agent":3,"human":4}))
                    .bind(&pool, &|| Ok(()))?;
            Ok(value)
        })
        .unwrap();
    drop(construction);
    let retained = pool.used();
    let result = inverse(&before, &after, current, &mut sources, &pool, &|| Ok(())).unwrap();
    assert!(result.report.conflicts.is_empty());
    assert_eq!(result.candidate.layers[0].name, before.layers[0].name);
    assert_eq!(result.candidate.layers[0].opacity, 0.4);
    assert_eq!(
        *result.candidate.layers[0].effects[8].settings,
        serde_json::json!({"agent":1,"human":4})
    );
    for index in 0..8 {
        assert!(result.candidate.layers[0].effects[index]
            .settings
            .same_allocation(&before.layers[0].effects[index].settings));
    }
    assert!(pool.used() < retained + 16 * 1024 * 1024);
    drop(result);
    drop(after);
    drop(before);
}
impl Drop for Sources {
    fn drop(&mut self) {
        self.originals.iter_mut().for_each(Vec::clear);
        self.prepared.clear();
        self.output.clear();
        drop(self.store.take());
        assert_eq!(
            self.pool.used(),
            0,
            "all source and metadata owners must drop before fixture cleanup"
        );
        // This is a fresh private fixture directory, held throughout the test.
        let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let target = std::fs::canonicalize(&self.path).unwrap();
        assert_eq!(target.parent(), Some(parent.as_path()));
        assert!(target
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("peerbrush-metadata-inverse-"));
        std::fs::remove_dir_all(target).unwrap();
    }
}
fn reference(width: u32, height: u32, depth: u16, id: usize) -> RasterRef36 {
    RasterRef36 {
        id: id as u32,
        width,
        height,
        depth,
        retained: None,
    }
}
fn bounds(out: &mut Option<[i32; 4]>, rect: [u32; 4], origin: [i32; 2]) {
    let r = [
        rect[0] as i32 + origin[0],
        rect[1] as i32 + origin[1],
        rect[2] as i32 + origin[0],
        rect[3] as i32 + origin[1],
    ];
    *out = Some(match *out {
        None => r,
        Some(a) => [
            a[0].min(r[0]),
            a[1].min(r[1]),
            a[2].max(r[2]),
            a[3].max(r[3]),
        ],
    });
}
impl Resolver for Sources {
    fn same(&mut self, left: Source<'_>, right: Source<'_>) -> Result<bool> {
        self.calls += 1;
        if self.fail {
            return Err("injected fallible source read".into());
        }
        let (left, right) = (self.source(left)?, self.source(right)?);
        if (left.width(), left.height(), left.depth())
            != (right.width(), right.height(), right.depth())
        {
            return Ok(false);
        }
        // Full padded native planes are pinned/read, even for equal portable IDs
        // and even when content-addressed descriptors happen to be equal.
        for y in 0..left.height().div_ceil(256) {
            for x in 0..left.width().div_ceil(256) {
                let a = left
                    .prepare_tile((x, y), &mut || Ok(()))
                    .map_err(|e| e.to_string())?;
                let b = right
                    .prepare_tile((x, y), &mut || Ok(()))
                    .map_err(|e| e.to_string())?;
                match (a.bytes(), b.bytes()) {
                    (Some(a), Some(b)) if a != b => return Ok(false),
                    (Some(_), None) | (None, Some(_)) => return Ok(false),
                    _ => {}
                }
            }
        }
        Ok(true)
    }
    fn import(&mut self, source: Source<'_>) -> Result<RasterRef36> {
        if self.fail {
            return Err("injected fallible source import".into());
        }
        let surface = self.source(source)?.clone();
        let shape = (surface.width(), surface.height(), surface.depth());
        self.output.push(surface);
        Ok(reference(shape.0, shape.1, shape.2, self.output.len()))
    }
    fn inverse(
        &mut self,
        b: Source<'_>,
        a: Source<'_>,
        c: Source<'_>,
        _: &str,
        _: &str,
        origin: [i32; 2],
    ) -> Result<RasterOutcome> {
        use super::super::super::pixel_inverse::{PixelInverse, TileInverse};
        let (before, after, current) = (
            self.source(b)?.clone(),
            self.source(a)?.clone(),
            self.source(c)?.clone(),
        );
        let kernel = PixelInverse::new(&before, &after, &current, &self.pool, &self.disk)?;
        let mut blocks = current
            .physical_tiles()
            .map(|t| (t.coordinate(), t.reference()))
            .collect::<BTreeMap<_, _>>();
        let mut changed = None;
        let mut conflicts = None;
        for coordinate in kernel.coordinates() {
            match kernel.tile(coordinate, &|| Ok(()))? {
                TileInverse::Unchanged => {}
                TileInverse::Conflict {
                    changed: r,
                    conflicts: c,
                } => {
                    bounds(&mut changed, r, origin);
                    bounds(&mut conflicts, c, origin);
                }
                TileInverse::Merged { pixels, changed: r } => {
                    bounds(&mut changed, r, origin);
                    let block = self
                        .store
                        .as_ref()
                        .unwrap()
                        .put(
                            TileSpec::new(256, 256, current.depth()).unwrap(),
                            &pixels,
                            &mut || Ok(()),
                        )
                        .map_err(|e| e.to_string())?;
                    blocks.insert(coordinate, block);
                }
            }
        }
        let surface = Surface::from_plane(
            Plane::from_blocks(
                current.width(),
                current.height(),
                current.depth(),
                self.store.as_ref().unwrap().clone(),
                blocks,
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.prepared.push(surface);
        Ok(RasterOutcome {
            plane: reference(
                current.width(),
                current.height(),
                current.depth(),
                self.prepared.len(),
            ),
            changed,
            conflicts,
        })
    }
}
fn document(depth: u16) -> Document {
    let mut doc = Document::new(4, 2).unwrap();
    doc.bit_depth = depth;
    doc.layers[0].pixels = Raster::new_depth(4, 2, depth);
    doc
}
fn paint(doc: &mut Document, x: i32, value: u16) {
    if doc.bit_depth == 16 {
        doc.layers[0]
            .pixels
            .set16(x, 0, [value, 12345, 45678, 65535]);
    } else {
        doc.layers[0]
            .pixels
            .set(x, 0, [(value / 257) as u8, 23, 47, 255]);
    }
}
fn run(sources: &mut Sources, b: &Document36, a: &Document36, c: &Document36) -> MetadataResult {
    let pool = sources.pool.clone();
    inverse(
        b,
        a,
        copy_metadata(c, &pool, &|| Ok(())).unwrap(),
        sources,
        &pool,
        &|| Ok(()),
    )
    .unwrap()
}
#[test]
fn disjoint_metadata_and_native_pixels_match_legacy_compensation_at_both_depths() {
    for depth in [8, 16] {
        let mut sources = Sources::new();
        let before = document(depth);
        let mut after = before.clone();
        paint(&mut after, 0, 40000);
        after.layers[0].name = "task".into();
        let mut current = after.clone();
        paint(&mut current, 2, 51001);
        current.layers[0].opacity = 0.31;
        current.layers[0].x = 7;
        current.layers[0].y = 9;
        current.revision = 19;
        let mut oracle = Engine::new();
        oracle.doc = current.clone();
        oracle.undo = vec![History {
            before: before.clone(),
            after: after.clone(),
            actor: "agent".into(),
            label: "Task".into(),
            task: Some("task".into()),
            gesture: None,
            scopes: vec![Scope::layer(&before.layers[0].id)],
            reverted: vec![],
        }];
        oracle.undo_task("human", "agent", "task").unwrap();
        let b = sources.capture(&before, Side::Before);
        let a = sources.capture(&after, Side::After);
        let c = sources.capture(&current, Side::Current);
        // All three portable ordinals are 1; they refer to different native data.
        assert_eq!(b.layers[0].pixels.id, a.layers[0].pixels.id);
        let result = run(&mut sources, &b, &a, &c);
        assert!(result.report.conflicts.is_empty());
        assert!(sources.calls > 0);
        assert_eq!(result.candidate.revision, 19);
        assert_eq!(result.candidate.layers[0].name, b.layers[0].name);
        assert_eq!(result.candidate.layers[0].opacity, 0.31);
        assert_eq!(result.candidate.layers[0].x, 7);
        assert_eq!(result.candidate.layers[0].name, oracle.doc.layers[0].name);
        assert_eq!(
            result.candidate.layers[0].opacity,
            oracle.doc.layers[0].opacity
        );
        assert!(result
            .report
            .scopes
            .iter()
            .any(|s| s.rect == Some([7, 9, 8, 10])));
        let output = &sources.output[result.candidate.layers[0].pixels.id as usize - 1];
        let region = output
            .prepare_region(
                crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        assert_eq!(region.pixel16(0, 0).unwrap(), [0; 4]);
        assert_eq!(
            region.pixel16(2, 0).unwrap(),
            current.layers[0].pixels.get16(2, 0)
        );
        for y in 0..2 {
            for x in 0..4 {
                assert_eq!(
                    region.pixel16(x, y).unwrap(),
                    oracle.doc.layers[0].pixels.get16(x, y)
                );
            }
        }
        drop(region);
        drop(result);
    }
}
#[test]
fn overlap_retains_current_and_reports_precise_native_conflict_without_authority() {
    let mut sources = Sources::new();
    let before = document(16);
    let mut after = before.clone();
    paint(&mut after, 1, 31001);
    let mut current = after.clone();
    paint(&mut current, 1, 32003);
    let b = sources.capture(&before, Side::Before);
    let a = sources.capture(&after, Side::After);
    let c = sources.capture(&current, Side::Current);
    let result = run(&mut sources, &b, &a, &c);
    assert_eq!(result.report.conflicts.len(), 1);
    assert_eq!(result.report.conflicts[0].rect, Some([1, 0, 2, 1]));
    assert_eq!(sources.originals[2][0].key(), sources.output[0].key());
}
#[test]
fn removed_layer_imports_retained_mask_and_typed_clips_in_exact_order() {
    use crate::engine::scene::{Coverage36, EffectClip36};
    let mut sources = Sources::new();
    let mut before = document(16);
    paint(&mut before, 0, 12347);
    let mut b = sources.capture(&before, Side::Before);
    // Every original is a real held Source, even when the metadata graph uses
    // extra retained/mask/coverage locations with equal native content.
    let surface = sources.originals[0][0].clone();
    for _ in 0..4 {
        sources.originals[0].push(surface.clone());
    }
    let r = |id| reference(4, 2, 16, id);
    b.layers[0].pixels.retained = Some(Box::new(Retained36 {
        pixels: r(2),
        matrix: [1., 0., 0., 1., 0., 0.],
        projective: None,
        warps: Vec::new(),
    }));
    b.layers[0].mask = Some(Mask36 {
        enabled: true,
        linked: false,
        steps: vec![MaskStep36 {
            id: "mask".into(),
            kind: "raster".into(),
            enabled: true,
            weight: 1.,
            value: 1.,
            pixels: r(3),
            settings: serde_json::json!({}).into(),
        }],
    });
    b.layers[0].effects = vec![Effect36 {
        id: "effect".into(),
        kind: "liquify".into(),
        enabled: true,
        weight: 1.,
        settings: serde_json::json!({"strokes":[{}]}).into(),
        clips: vec![EffectClip36 {
            stroke: 0,
            coverage: Coverage36 {
                mask: r(4),
                bounds: [0, 0, 4, 2],
                origin: [0, 0],
                contours: vec![].into(),
            },
        }],
    }];
    b.selection_previous = Some(Coverage36 {
        mask: r(5),
        bounds: [0, 0, 4, 2],
        origin: [0, 0],
        contours: vec![].into(),
    });
    b.visit_rasters(|_| Ok(())).unwrap();
    let mut a = crate::engine::scene::wire_copy(&b);
    a.layers.clear();
    a.selection_previous = None;
    let c = crate::engine::scene::wire_copy(&a);
    let result = run(&mut sources, &b, &a, &c);
    assert!(result.report.conflicts.is_empty());
    assert_eq!(result.candidate.visit_rasters(|_| Ok(())).unwrap(), 5);
    assert_eq!(sources.output.len(), 5);
    assert!(result.candidate.layers[0].pixels.retained.is_some());
}
#[test]
fn later_selection_and_dependencies_refuse_instead_of_clobbering() {
    let mut sources = Sources::new();
    let before = document(8);
    let mut after = before.clone();
    after.selection = Some([0, 0, 2, 1]);
    let mut current = after.clone();
    current.selection = Some([1, 0, 3, 1]);
    let b = sources.capture(&before, Side::Before);
    let a = sources.capture(&after, Side::After);
    let c = sources.capture(&current, Side::Current);
    let result = run(&mut sources, &b, &a, &c);
    assert_eq!(result.candidate.selection, current.selection);
    assert_eq!(result.report.conflicts[0].field, "selection");
    drop(result);
    sources.output.clear();
    let mut b = b;
    let mut a = a;
    let mut c = c;
    b.selection = None;
    a.selection = None;
    c.selection = None;
    let mut created = crate::engine::scene::wire_copy(&a.layers[0]);
    created.id = "task-folder".into();
    created.kind = "group".into();
    created.pixels.id = 2;
    a.layers.push(crate::engine::scene::wire_copy(&created));
    c.layers.push(created);
    sources.originals[1].push(sources.originals[1][0].clone());
    sources.originals[2].push(sources.originals[2][0].clone());
    let mut child = crate::engine::scene::wire_copy(&c.layers[0]);
    child.id = "human-child".into();
    child.parent = Some("task-folder".into());
    child.pixels.id = 3;
    c.layers.push(child);
    sources.originals[2].push(sources.originals[2][0].clone());
    let result = run(&mut sources, &b, &a, &c);
    assert!(result
        .candidate
        .layers
        .iter()
        .any(|l| l.id == "task-folder"));
    assert!(result
        .report
        .conflicts
        .iter()
        .any(|c| c.field == "layer.dependencies"));
}

#[test]
fn common_order_compensation_preserves_later_insertions_and_refuses_later_reorder() {
    for conflict in [false, true] {
        let mut sources = Sources::new();
        let mut before = document(8);
        before
            .layers
            .push(crate::engine::Layer::new("second", "paint", 4, 2));
        let mut after = before.clone();
        after.layers.swap(0, 1);
        let mut current = after.clone();
        current.layers.insert(
            1,
            crate::engine::Layer::new("human insertion", "paint", 4, 2),
        );
        if conflict {
            current.layers.swap(0, 2);
        }
        let b = sources.capture(&before, Side::Before);
        let a = sources.capture(&after, Side::After);
        let c = sources.capture(&current, Side::Current);
        let result = run(&mut sources, &b, &a, &c);
        assert_eq!(result.candidate.layers[1].name, "human insertion");
        if conflict {
            assert!(result
                .report
                .conflicts
                .iter()
                .any(|c| c.reason == "Order changed after this task"));
            assert_eq!(result.candidate.layers[0].id, current.layers[0].id);
        } else {
            assert!(result.report.conflicts.is_empty());
            assert_eq!(result.candidate.layers[0].id, before.layers[0].id);
            assert_eq!(result.candidate.layers[2].id, before.layers[1].id);
        }
    }
}

#[test]
fn settings_with_ref_shaped_values_are_metadata_and_later_independent_settings_survive() {
    let mut sources = Sources::new();
    let before = document(16);
    let mut b = sources.capture(&before, Side::Before);
    let mut a = sources.capture(&before, Side::After);
    let mut c = sources.capture(&before, Side::Current);
    let settings = serde_json::json!({"gamma":1.0,"white":1.0,"opaque":{"id":1,"width":4,"height":2,"depth":16,"retained":null}});
    let effect = Effect36 {
        id: "levels".into(),
        kind: "levels".into(),
        enabled: true,
        weight: 1.,
        settings: settings.into(),
        clips: vec![],
    };
    b.filters.push(crate::engine::scene::wire_copy(&effect));
    a.filters.push(crate::engine::scene::wire_copy(&effect));
    c.filters.push(effect);
    let mut changed = crate::engine::scene::wire_copy(a.filters[0].settings.value());
    changed["gamma"] = serde_json::json!(1.7);
    a.filters[0].settings = changed.into();
    let mut changed = crate::engine::scene::wire_copy(c.filters[0].settings.value());
    changed["gamma"] = serde_json::json!(1.7);
    changed["white"] = serde_json::json!(0.82);
    c.filters[0].settings = changed.into();
    let result = run(&mut sources, &b, &a, &c);
    assert!(result.report.conflicts.is_empty());
    assert_eq!(sources.output.len(), 1);
    assert_eq!(
        result.candidate.filters[0].settings["gamma"],
        serde_json::json!(1.0)
    );
    assert_eq!(
        result.candidate.filters[0].settings["white"],
        serde_json::json!(0.82)
    );
    assert_eq!(result.candidate.filters[0].settings["opaque"]["id"], 1);
}

#[test]
fn changed_project_frame_with_later_work_and_locked_filter_changes_fail_closed() {
    let mut sources = Sources::new();
    let before = document(8);
    let mut after = before.clone();
    after.width = 5;
    let mut current = after.clone();
    current.name = "later human".into();
    let b = sources.capture(&before, Side::Before);
    let a = sources.capture(&after, Side::After);
    let c = sources.capture(&current, Side::Current);
    let result = run(&mut sources, &b, &a, &c);
    assert_eq!(result.report.conflicts[0].field, "document.frame");
    assert_eq!(result.candidate.width, 5);
    assert_eq!(result.candidate.name, "later human");
    drop(result);
    sources.output.clear();
    let mut a = crate::engine::scene::wire_copy(&b);
    let mut c = crate::engine::scene::wire_copy(&b);
    let effect = Effect36 {
        id: "filter".into(),
        kind: "gamma".into(),
        enabled: true,
        weight: 1.,
        settings: serde_json::json!({}).into(),
        clips: vec![],
    };
    a.filters.push(crate::engine::scene::wire_copy(&effect));
    c.filters.push(effect);
    c.layers[0].locked = true;
    let result = run(&mut sources, &b, &a, &c);
    assert!(result
        .report
        .conflicts
        .iter()
        .any(|c| c.field == "document.filters"));
    assert_eq!(result.candidate.filters.len(), 1);
    assert!(result.candidate.layers[0].locked);
}
#[test]
fn source_failure_cancellation_and_workspace_refusal_restore_exact_admission() {
    let mut sources = Sources::new();
    let before = document(8);
    let mut after = before.clone();
    paint(&mut after, 0, 30000);
    let b = sources.capture(&before, Side::Before);
    let a = sources.capture(&after, Side::After);
    let c = sources.capture(&after, Side::Current);
    let pool = sources.pool.clone();
    let baseline = pool.used();
    sources.fail = true;
    let error = match inverse(
        &b,
        &a,
        copy_metadata(&c, &pool, &|| Ok(())).unwrap(),
        &mut sources,
        &pool,
        &|| Ok(()),
    ) {
        Ok(_) => panic!("Expected fallible source refusal"),
        Err(error) => error,
    };
    assert!(error.contains("injected"));
    assert_eq!(pool.used(), baseline);
    sources.fail = false;
    let current = copy_metadata(&c, &pool, &|| Ok(())).unwrap();
    assert!(inverse(&b, &a, current, &mut sources, &pool, &|| Err(
        "cancel".into()
    ))
    .is_err());
    assert_eq!(pool.used(), baseline);
    let small = ResidentBudget::new(2 * 1024 * 1024).unwrap();
    let current = copy_metadata(&c, &small, &|| Ok(())).unwrap();
    assert!(inverse(&b, &a, current, &mut sources, &small, &|| Ok(())).is_err());
    assert_eq!(small.used(), 0);
}

#[test]
fn prepared_current_plane_and_before_original_import_from_distinct_namespaces() {
    for human_pixels in [false, true] {
        let mut sources = Sources::new();
        let mut before = document(16);
        before.layers[0]
            .pixels
            .set16(3, 0, [12347, 23459, 34571, 0]);
        let mut after = before.clone();
        paint(&mut after, 0, 40001);
        let mut current = after.clone();
        if human_pixels {
            paint(&mut current, 2, 50003);
        }
        let mut b = sources.capture(&before, Side::Before);
        let a = sources.capture(&after, Side::After);
        let c = sources.capture(&current, Side::Current);
        let mut original = Raster::new_depth(4, 2, 16);
        original.set16(1, 0, [101, 303, 505, 0]);
        let original = sources.stage(&original);
        sources.originals[0].push(original.clone());
        b.layers[0].pixels.retained = Some(Box::new(Retained36 {
            pixels: reference(4, 2, 16, 2),
            matrix: [1., 0., 0., 1., 0., 0.],
            projective: None,
            warps: vec![],
        }));
        let result = run(&mut sources, &b, &a, &c);
        assert!(result.report.conflicts.is_empty());
        if human_pixels {
            assert!(result.candidate.layers[0].pixels.retained.is_none());
            assert_eq!(sources.output.len(), 1);
        } else {
            let retained = result.candidate.layers[0].pixels.retained.as_ref().unwrap();
            assert_eq!(retained.pixels.id, 2);
            assert_eq!(sources.output[1].key(), original.key());
        }
        let region = sources.output[0]
            .prepare_region(
                crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        assert_eq!(region.pixel16(3, 0).unwrap(), [12347, 23459, 34571, 0]);
        assert_eq!(region.pixel16(0, 0).unwrap(), [0; 4]);
        if human_pixels {
            assert_eq!(
                region.pixel16(2, 0).unwrap(),
                current.layers[0].pixels.get16(2, 0)
            );
        }
    }
}
