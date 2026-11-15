//! Independent inspection of an inventoried startup file. No Store, Snapshot,
//! inferred newest selector or writable document authority escapes this module.
use super::*;
use crate::workspace::source_runtime::SourceRetirement;

impl ParkedSource {
    pub(crate) fn inspect_unselected(
        permit: &SourceRetirement<'_>,
        check: &dyn Fn() -> std::result::Result<(), String>,
    ) -> std::result::Result<Self, String> {
        let input = permit.inspection()?;
        let header_bytes = input
            .path
            .as_os_str()
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<Backing>() + 4096))
            .ok_or("Unselected source metadata overflow")?;
        let header = input
            .resident
            .reserve_bytes(header_bytes, header_bytes)
            .map_err(|e| e.to_string())?;
        let _scratch = input
            .resident
            .reserve_bytes(CHUNK + 4096, CHUNK + 4096)
            .map_err(|e| e.to_string())?;
        let mut guard = || {
            check()
                .and_then(|_| permit.check_namespace())
                .map_err(|_| Error::Cancelled)
        };
        let mut inspect = || -> Result<Store> {
            // Opening the actual data owner and acquiring its OS lock excludes
            // live owners from an earlier runtime as well as current readers.
            validate_ancestors(&input.path)?;
            let directory = open_directory(&input.path)?;
            let directory_id =
                crate::file_safety::identity(&directory).map_err(|_| Error::Changed)?;
            let path = input.path.join("blocks.pbt");
            regular(&path)?;
            let file = source_options().open(&path)?;
            file.try_lock().map_err(|_| Error::Busy)?;
            let identity = crate::file_safety::identity(&file).map_err(|_| Error::Changed)?;
            if (directory_id, identity) != input.filesystem {
                return Err(Error::Changed);
            }
            let limits = input.limits.validate()?;
            let expected = stamp(&file)?;
            if expected.bytes < FILE_HEADER || expected.bytes > limits.disk_bytes {
                return Err(Error::Budget);
            }
            let mut prefix = [0; FILE_HEADER as usize];
            read_exact_at(&file, 0, &mut prefix)?;
            if &prefix[..8] != FILE_MAGIC || prefix[8..] != input.identity {
                return Err(Error::Changed);
            }
            let disk = input.disk.retain_file(&path, &file)?;
            let count = preflight_index(&file, expected.bytes, limits, &mut guard)?;
            let mut blocks = Index::new();
            blocks.reserve_to(count, &input.resident, limits.max_blocks)?;
            let store = Store(Inner::new(Backing {
                root: input.path.clone(),
                directory,
                directory_id,
                path,
                file,
                identity,
                id: input.identity,
                limits,
                budget: input.resident.clone(),
                disk: Some(disk),
                access: Access::default(),
                state: Mutex::new(super::super::State {
                    expected,
                    blocks,
                    head: None,
                    indexed: None,
                    readonly: true,
                    poisoned: false,
                }),
                cache: Mutex::new(Index::new()),
                may_be_published: AtomicBool::new(true),
            }));
            store.check_source(expected)?;
            let mut at = FILE_HEADER;
            while at < expected.bytes {
                guard()?;
                let record = read_record(&store.0.file, at, expected.bytes, limits, &mut guard)?;
                if &record.kind == b"TILE" {
                    let block = store.block_record(&record)?;
                    if store
                        .state()?
                        .blocks
                        .insert_reserved(block.digest, block)?
                        .is_some()
                    {
                        return Err(Error::Corrupt);
                    }
                } else {
                    // This private chain boundary is validation evidence only.
                    // It cannot become a caller's selected/publication head.
                    drop(store.manifest_record(&record, &mut guard)?);
                }
                at = record.end;
            }
            if at != expected.bytes || store.state()?.blocks.len() != count {
                return Err(Error::Corrupt);
            }
            guard()?;
            store.check_source(expected)?;
            Ok(store)
        };
        let store = inspect().map_err(|e| e.to_string())?;
        permit.check(
            input.identity,
            input.filesystem,
            &input.resident,
            &input.disk,
        )?;
        check()?;
        Ok(Self {
            store: Some(store),
            custody: None,
            _inspection: Some(header),
        })
    }
}
