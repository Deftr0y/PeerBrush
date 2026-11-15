//! Stage the native paste once for either rendered hover or guarded drop.
use super::*;
use crate::{
    engine::view::{DocumentSnapshot, DocumentView},
    render::native_preview::{render_review, render_review_with, DisplayOutput, Request},
    server::EditReply,
};

pub(in crate::workspace) struct Destination {
    source: DocumentSnapshot,
    cut_seed: Option<NativeSeed>,
    seed: NativeSeed,
    scene: SceneSnapshot,
}

pub(in crate::workspace) struct PreparedDestination {
    capture: Destination,
    edit: PreparedNative,
    session: Arc<SourceSession>,
    cut: Option<Cut>,
}

pub(crate) struct NativeTransferPreview<'a> {
    destination: &'a PreparedDestination,
    edge: u32,
    check: &'a dyn Fn() -> Result<(), String>,
    summary: TransferSummary,
}

impl NativeTransferPreview<'_> {
    pub(crate) fn render_canvas<T: DisplayOutput>(
        self,
        render: impl FnOnce(&Request, &dyn Fn() -> Result<(), String>) -> Result<T, String>,
    ) -> Result<(TransferSummary, T), String> {
        let to = self.destination;
        let transient = render_review_with(
            &to.capture.scene, &to.edit, to.session.clone(), None, self.edge, None, false, None,
            self.check, true, render,
        )?;
        let (_, output, _) = transient.into_parts();
        Ok((self.summary, output))
    }

    pub(in crate::workspace) fn render_image(self) -> Result<TransferImage, String> {
        let to = self.destination;
        let transient = render_review(
            &to.capture.scene, &to.edit, to.session.clone(), None, self.edge, None, false, None,
            self.check,
        )?;
        let frame = transient.frame();
        let grid = frame.key();
        let pixels = frame.copy_rgba8(&mut || (self.check)())?;
        Ok(TransferImage {
            summary: self.summary, width: grid.width, height: grid.height, rect: grid.rect,
            pixels: TransferPixels::Native(pixels),
        })
    }
}

enum Cut {
    Resident(Vec<Value>),
    Native {
        seed: NativeSeed,
        edit: PreparedNative,
    },
}

impl Cut {
    fn abort(self, session: &SourceSession) -> Option<String> {
        match self {
            Self::Resident(_) => None,
            Self::Native { edit, .. } => edit.abort(session, &|| session.cleanup_check()).err(),
        }
    }
}

impl Destination {
    pub(in crate::workspace) fn capture(
        from: &Engine,
        to: &Engine,
        r: &Transfer<'_>,
    ) -> Result<Option<Self>, String> {
        let Some(seed) = to.native_seed()? else {
            return Ok(None);
        };
        guard(from, r.source_document, r.source_revision)?;
        guard(to, r.destination_document, r.destination_revision)?;
        to.ensure_write_available()?;
        crate::layer_clipboard::check_copy_source(engine::view::current(from))?;
        if seed.document().metadata().read_only() {
            return Err("This PSD is read-only".into());
        }
        Ok(Some(Self {
            source: from.doc_snapshot(),
            cut_seed: if r.move_layers {
                from.native_seed()?
            } else {
                None
            },
            seed,
            scene: to.capture_scene(None)?.into_native()?,
        }))
    }

    fn validate(&self, from: &Engine, to: &Engine, r: &Transfer<'_>) -> Result<(), String> {
        guard(from, r.source_document, r.source_revision)?;
        guard(to, r.destination_document, r.destination_revision)?;
        if !self
            .source
            .view()
            .same_observation(engine::view::current(from))
        {
            return Err("Transfer source changed; observe both projects again".into());
        }
        self.seed.validate(to)?;
        to.ensure_write_available()?;
        if r.move_layers {
            check_task(from, r.actor, r.source_task)?;
        }
        check_task(to, r.actor, r.destination_task)
    }

    pub(in crate::workspace) fn prepare(
        mut self,
        pair: &TransferPair,
        r: &Transfer<'_>,
        check_cancelled: &dyn Fn() -> Result<(), String>,
    ) -> Result<PreparedDestination, String> {
        check_cancelled()?;
        let session = Arc::new(native_dispatch::begin_edit_scene(
            &pair.destination,
            &self.scene,
        )?);
        let check = || check_cancelled().and_then(|_| session.check());
        let commands = [json!({"op":"layer.paste","layer":r.target})];
        let descriptor = native::CommandDescriptor::from_normalized(
            self.seed.document(),
            r.destination_revision,
            &commands,
            &check,
        )?;
        let captured = pair.with(|from, to| {
            check_cancelled()?;
            self.validate(from, to, r)?;
            to.capture_native(
                &self.seed,
                descriptor,
                r.actor,
                Some(r.destination_revision),
                Some(r.destination_document),
            )
        })?;
        let (cut, edit) = match self.source.view() {
            DocumentView::Resident(source) => {
                let snapshot = crate::layer_clipboard::copy(source, r.layers)?;
                let cut = r
                    .move_layers
                    .then(|| crate::layer_clipboard::cut_commands(source, r.layers))
                    .transpose()?;
                (
                    cut.map(Cut::Resident),
                    native::worker::prepare_resident_layer_paste(
                        captured, &snapshot, &session, check_cancelled,
                    ),
                )
            }
            DocumentView::Native(source) => {
                let mut work = Work::new(4_000_000)?;
                let snapshot = LayerCopyPlan::capture(source, r.layers, &mut work, &check)?;
                let cut = if let Some(seed) = self.cut_seed.take() {
                    let commands = snapshot.cut_commands(&mut work, &check)?;
                    let descriptor = native::CommandDescriptor::from_normalized(
                        seed.document(),
                        r.source_revision,
                        &commands,
                        &check,
                    )?;
                    let captured = pair.with(|from, to| {
                        self.validate(from, to, r)?;
                        from.capture_native(
                            &seed,
                            descriptor,
                            r.actor,
                            Some(r.source_revision),
                            Some(r.source_document),
                        )
                    })?;
                    let edit = native::worker::prepare(captured, &session, check_cancelled)
                        .map_err(|failure| with_cleanup(failure.reason, failure.cleanup_error))?;
                    Some(Cut::Native { seed, edit })
                } else {
                    None
                };
                (
                    cut,
                    native::worker::prepare_layer_paste(captured, &snapshot, &session, check_cancelled),
                )
            }
        };
        let edit = match edit {
            Ok(edit) => edit,
            Err(failure) => {
                return Err(with_cleanup(
                    with_cleanup(failure.reason, failure.cleanup_error),
                    cut.and_then(|cut| cut.abort(&session)),
                ))
            }
        };
        Ok(PreparedDestination {
            capture: self,
            edit,
            session,
            cut,
        })
    }
}

fn delete_source(from: &mut Engine, commands: &[Value], r: &Transfer<'_>) -> Result<(), String> {
    from.edit(
        r.actor,
        commands,
        Some(r.source_revision),
        r.source_task,
        "Move layers to another project",
    )
    .map(drop)
}

impl PreparedDestination {
    pub(in crate::workspace) fn preview<T>(
        self,
        pair: &TransferPair,
        r: &Transfer<'_>,
        edge: u32,
        coarse: Option<u32>,
        check_cancelled: &dyn Fn() -> Result<(), String>,
        render_native: impl Fn(NativeTransferPreview<'_>) -> Result<T, String>,
        emit_coarse: &mut dyn FnMut(T),
    ) -> Result<T, String> {
        let check = || check_cancelled().and_then(|_| self.session.check());
        let rendered = (|| {
            check()?;
            if let Some(Cut::Resident(commands)) = &self.cut {
                let mut draft = pair.with(|from, to| {
                    self.capture.validate(from, to, r)?;
                    from.resident_draft()
                })?;
                delete_source(&mut draft, commands, r)?;
            }
            let metadata = self.edit.preview().metadata();
            let render = |edge| {
                let output = render_native(NativeTransferPreview {
                    destination: &self, edge, check: &check,
                    summary: TransferSummary::from_view(self.edit.preview().into()),
                })?;
                check()?;
                pair.with(|from, to| self.capture.validate(from, to, r))?;
                Ok(output)
            };
            if let Some(coarse) = coarse
                .filter(|&coarse| coarse < edge && metadata.width().max(metadata.height()) > coarse)
            {
                if let Ok(frame) = render(coarse) {
                    emit_coarse(frame);
                }
            }
            render(edge)
        })();
        let cleanup = self
            .edit
            .abort(&self.session, &|| self.session.cleanup_check())
            .err();
        let cut_cleanup = self.cut.and_then(|cut| cut.abort(&self.session));
        let cleanup = match (cleanup, cut_cleanup) {
            (Some(a), Some(b)) => Some(format!("{a}; {b}")),
            (a, b) => a.or(b),
        };
        match (rendered, cleanup) {
            (Ok(image), None) => Ok(image),
            (Ok(_), Some(cleanup)) => {
                Err(format!("Preview source cleanup retained debt: {cleanup}"))
            }
            (Err(reason), cleanup) => Err(with_cleanup(reason, cleanup)),
        }
    }

    pub(in crate::workspace) fn commit(
        self,
        pair: &TransferPair,
        r: &Transfer<'_>,
    ) -> Result<Value, String> {
        let Self {
            capture,
            edit,
            session,
            cut,
        } = self;
        let reply = match EditReply::native(&edit, 1) {
            Ok(reply) => reply,
            Err(reason) => {
                return Err(with_cleanup(
                    with_cleanup(
                        reason,
                        edit.abort(&session, &|| session.cleanup_check()).err(),
                    ),
                    cut.and_then(|cut| cut.abort(&session)),
                ))
            }
        };
        let mut publication = match Publication::prepare(
            &capture.seed,
            edit,
            session.clone(),
            HistoryRequest {
                actor: r.actor,
                label: "Paste layers",
                task: r.destination_task,
                gesture: None,
            },
        ) {
            Ok(publication) => publication,
            Err(reason) => {
                return Err(with_cleanup(
                    reason,
                    cut.and_then(|cut| cut.abort(&session)),
                ))
            }
        };
        if let Some(Cut::Native { seed, edit }) = cut {
            let mut source = match Publication::prepare(
                &seed,
                edit,
                session,
                HistoryRequest {
                    actor: r.actor,
                    label: "Move layers to another project",
                    task: r.source_task,
                    gesture: None,
                },
            ) {
                Ok(source) => source,
                Err(reason) => return publication.finish(Err(reason)),
            };
            let result = pair.with(|from, to| {
                capture.validate(from, to, r)?;
                source.publish_pair(from, &seed, &mut publication, to, &capture.seed, r.actor)?;
                Ok(transfer_reply(from, to, r, reply.data()))
            });
            return publication.finish(source.finish(result));
        }
        let result = pair.with(|from, to| {
            capture.validate(from, to, r)?;
            let checkpoint = if let Some(Cut::Resident(commands)) = &cut {
                let checkpoint = from.resident_checkpoint()?;
                delete_source(from, commands, r)?;
                Some(checkpoint)
            } else {
                None
            };
            if let Err(reason) = publication.publish(to, &capture.seed, r.actor) {
                if let Some(checkpoint) = checkpoint {
                    checkpoint.restore(from)?;
                }
                return Err(reason);
            }
            Ok(transfer_reply(from, to, r, reply.data()))
        });
        publication.finish(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::native::worker::tests::Fixture, raster::backed::Region};

    fn identity(shared: &Shared) -> (String, String, u64, String) {
        let engine = shared.lock().unwrap();
        let view = engine::view::current(&engine);
        (
            engine.project_id.clone(),
            view.id().to_owned(),
            view.revision(),
            view.layer(0).unwrap().id().to_owned(),
        )
    }

    #[test]
    fn native_destination_renders_exact_paste_and_rolls_back_resident_cut_on_refusal() {
        let fixture = Fixture::new();
        let mut document = Document::new_depth(8, 8, 16).unwrap();
        document.layers[0]
            .pixels
            .set16(1, 1, [12347, 23459, 34571, 65535]);
        document.layers[0]
            .pixels
            .set16(6, 6, [54321, 32109, 10987, 0]);
        let source = Arc::new(Mutex::new(Engine::from_document(document)));
        let connection =
            crate::server::start(source.clone(), fixture.path().join("state")).unwrap();
        connection.configure_native_sources(None, true).unwrap();
        let workspace = attach(&source);
        let runtime = workspace.lock().unwrap().source_runtime.clone().unwrap();
        let destination = native_registration::new_blank(
            &workspace,
            8,
            8,
            8,
            &crate::loading::Control::default(),
            None,
        )
        .unwrap();
        let (from, from_doc, from_revision, layer) = identity(&source);
        let (to, to_doc, to_revision, target) = identity(&destination);
        let layers = [layer.clone()];
        let request = Transfer {
            source: &from,
            destination: &to,
            source_document: &from_doc,
            destination_document: &to_doc,
            source_revision: from_revision,
            destination_revision: to_revision,
            layers: &layers,
            target: &target,
            move_layers: true,
            actor: "human",
            source_task: None,
            destination_task: None,
        };
        let disk = runtime.disk().used().unwrap();
        let preview =
            render_transfer_in(&workspace, &request, 8, None, &|| Ok(()), |_| {}).unwrap();
        assert_eq!(
            (preview.width, preview.height, preview.rect),
            (8, 8, [0, 0, 8, 8])
        );
        assert_eq!(preview.summary.bit_depth, 16);
        assert_eq!(&preview.bytes()[36..40], &[48, 91, 135, 255]);
        assert_eq!(source.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(destination.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(runtime.disk().used().unwrap(), disk);

        struct StreamedSample(crate::render::native_preview::Tag, [u8; 4]);
        impl DisplayOutput for StreamedSample {
            fn display_tag(&self) -> crate::render::native_preview::Tag { self.0.clone() }
        }
        let (summary, sample) = render_transfer_with(
            &workspace, &request, 8, None, &|| Ok(()),
            |_| panic!("Disk-backed destination must use the native renderer"),
            |preview| preview.render_canvas(|request, check| {
                assert_eq!((request.key().width, request.key().height), (8, 8));
                let mut sample = [0; 4];
                let complete = request.render_into(&mut || check(), &mut |rows, bytes| {
                    if rows[0] <= 1 && 1 < rows[1] {
                        let offset = ((1 - rows[0]) as usize * 8 + 1) * 4;
                        sample.copy_from_slice(&bytes[offset..offset + 4]);
                    }
                    Ok(())
                })?;
                Ok(StreamedSample(complete.tag, sample))
            }),
            |_| {},
        ).unwrap();
        assert_eq!(summary.bit_depth, 16);
        assert_eq!(sample.1, [48, 91, 135, 255]);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(source.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(destination.lock().unwrap().history_lengths(), [0, 0]);

        let cancellation = native::Cancellation::default();
        let cancelled = render_transfer_in(
            &workspace,
            &request,
            8,
            Some(4),
            &|| cancellation.check(),
            |coarse| {
                assert_eq!((coarse.width, coarse.height), (4, 4));
                cancellation.cancel();
            },
        );
        assert!(cancelled.err().unwrap().contains("cancelled"));
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(source.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(destination.lock().unwrap().history_lengths(), [0, 0]);

        let pair = TransferPair::new(&workspace, &request).unwrap();
        let capture = pair
            .with(|from, to| Destination::capture(from, to, &request))
            .unwrap()
            .unwrap();
        let prepared = capture.prepare(&pair, &request, &|| Ok(())).unwrap();
        let lease = destination
            .lock()
            .unwrap()
            .reserve(
                "other",
                "Review the destination",
                vec![Scope {
                    target: None,
                    rect: None,
                }],
            )
            .unwrap();
        assert!(prepared
            .commit(&pair, &request)
            .unwrap_err()
            .contains("Reserved"));
        assert_eq!(identity(&source).2, from_revision);
        assert_eq!(identity(&destination).2, to_revision);
        assert_eq!(source.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(destination.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        destination
            .lock()
            .unwrap()
            .leases
            .retain(|entry| entry.id != lease.id);

        let moved = transfer_in(&workspace, &request).unwrap();
        assert_eq!(moved["moved"], true);
        assert_eq!(source.lock().unwrap().history_lengths(), [1, 0]);
        let (pasted, key) = {
            let engine = destination.lock().unwrap();
            assert_eq!(engine.history_lengths(), [1, 0]);
            let native = engine.native_document().unwrap();
            let pasted = native
                .metadata()
                .layers()
                .find(|layer| layer.name().ends_with(" copy"))
                .unwrap();
            let surface = native.source(pasted.pixels().slot()).unwrap();
            let tile = surface
                .prepare_region(Region::new([6, 6, 7, 7]).unwrap(), &mut || Ok(()))
                .unwrap();
            assert_eq!(tile.pixel16(6, 6).unwrap(), [54321, 32109, 10987, 0]);
            (pasted.id().to_owned(), surface.key())
        };

        let other = native_registration::new_blank(
            &workspace,
            8,
            8,
            16,
            &crate::loading::Control::default(),
            None,
        )
        .unwrap();
        let (other_project, other_doc, other_revision, other_layer) = identity(&other);
        let copy_layers = [pasted];
        let copy = Transfer {
            source: &to,
            destination: &other_project,
            source_document: &to_doc,
            destination_document: &other_doc,
            source_revision: to_revision + 1,
            destination_revision: other_revision,
            layers: &copy_layers,
            target: &other_layer,
            move_layers: false,
            actor: "human",
            source_task: None,
            destination_task: None,
        };
        let disk = runtime.disk().used().unwrap();
        transfer_in(&workspace, &copy).unwrap();
        assert_eq!(destination.lock().unwrap().history_lengths(), [1, 0]);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        {
            let engine = other.lock().unwrap();
            let native = engine.native_document().unwrap();
            let pasted = native
                .metadata()
                .layers()
                .find(|layer| layer.name().ends_with(" copy copy"))
                .unwrap();
            assert_eq!(native.source(pasted.pixels().slot()).unwrap().key(), key);
        }

        let native_move = Transfer {
            destination_revision: other_revision + 1,
            move_layers: true,
            ..copy
        };
        let source_key = destination.lock().unwrap().native_document().unwrap().key();
        let destination_key = other.lock().unwrap().native_document().unwrap().key();
        let preview =
            render_transfer_in(&workspace, &native_move, 8, None, &|| Ok(()), |_| {}).unwrap();
        assert_eq!(&preview.bytes()[36..40], &[48, 91, 135, 255]);
        let pair = TransferPair::new(&workspace, &native_move).unwrap();
        let capture = pair
            .with(|from, to| Destination::capture(from, to, &native_move))
            .unwrap()
            .unwrap();
        let prepared = capture.prepare(&pair, &native_move, &|| Ok(())).unwrap();
        assert!(runtime.try_checkpoint().unwrap().is_none());
        let lease = other
            .lock()
            .unwrap()
            .reserve(
                "other",
                "Review the destination",
                vec![Scope {
                    target: None,
                    rect: None,
                }],
            )
            .unwrap();
        assert!(prepared
            .commit(&pair, &native_move)
            .unwrap_err()
            .contains("Reserved"));
        for (project, key) in [(&destination, source_key), (&other, destination_key)] {
            let engine = project.lock().unwrap();
            assert_eq!(engine.native_document().unwrap().key(), key);
            assert_eq!(engine.history_lengths(), [1, 0]);
        }
        other
            .lock()
            .unwrap()
            .leases
            .retain(|entry| entry.id != lease.id);

        let result = transfer_in(&workspace, &native_move).unwrap();
        assert_eq!(result["moved"], true);
        {
            let engine = destination.lock().unwrap();
            assert_eq!(engine.history_lengths(), [2, 0]);
            assert!(engine::view::current(&engine)
                .find_layer(&copy_layers[0])
                .is_none());
        }
        {
            let engine = other.lock().unwrap();
            assert_eq!(engine.history_lengths(), [2, 0]);
            let native = engine.native_document().unwrap();
            let copied = result["created_roots"][0].as_str().unwrap();
            let slot = native.layer_slot(copied).unwrap();
            assert_eq!(native.source(slot).unwrap().key(), key);
        }
        assert_eq!(runtime.disk().used().unwrap(), disk);
        crate::server::history_shared(
            &destination,
            "human",
            native::Direction::Undo,
            result["source_revision"].as_u64(),
            Some(&to_doc),
        )
        .unwrap();
        assert!(engine::view::current(&destination.lock().unwrap())
            .find_layer(&copy_layers[0])
            .is_some());
        assert_eq!(other.lock().unwrap().history_lengths(), [2, 0]);
    }
}
