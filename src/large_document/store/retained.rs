//! Additional explicit checkpoint selections share one physical source owner.
use super::*;

impl Store {
    pub(super) fn read_selected(
        &self,
        selected: CommitRef,
        anchor: Option<CommitRef>,
        check: Check<'_>,
    ) -> Result<Snapshot> {
        let expected = self.state()?.expected;
        self.check_source(expected)?;
        let mut offset = FILE_HEADER;
        let mut last = None;
        let mut anchored = anchor.is_none();
        while offset < selected.end {
            check()?;
            let record = read_record(&self.0.file, offset, selected.end, self.0.limits, check)?;
            if &record.kind == b"TILE" {
                let block = self.block_record(&record)?;
                let mut state = self.state()?;
                if state.blocks.len() >= self.0.limits.max_blocks
                    || state.blocks.insert_reserved(block.digest, block)?.is_some()
                {
                    return Err(Error::Corrupt);
                }
            } else {
                let snapshot = self.manifest_record(&record, check)?;
                if anchor.is_some_and(|old| old.end == snapshot.commit.end) {
                    if anchor != Some(snapshot.commit) {
                        return Err(Error::Changed);
                    }
                    anchored = true;
                }
                last = Some(snapshot);
            }
            offset = record.end;
        }
        let snapshot = last.ok_or(Error::Corrupt)?;
        if !anchored || snapshot.commit != selected || offset != selected.end {
            return Err(Error::Corrupt);
        }
        check()?;
        self.check_source(expected)?;
        Ok(snapshot)
    }

    pub(super) fn select_later(&self, selected: CommitRef, check: Check<'_>) -> Result<Snapshot> {
        let write = self.0.access.write()?;
        let (expected, head, indexed) = {
            let state = self.state()?;
            let head = state.head.ok_or(Error::Corrupt)?;
            let indexed = state.indexed.unwrap_or(head);
            if !state.readonly
                || state.poisoned
                || selected.store != self.0.id
                || selected.source != head.source
                || selected.end > state.expected.bytes
                || selected.end > self.0.limits.disk_bytes
            {
                return Err(Error::Corrupt);
            }
            if selected.end <= indexed.end {
                drop(state);
                drop(write);
                return self.snapshot(selected, check);
            }
            (state.expected, head, indexed)
        };
        check()?;
        self.check_source(expected)?;
        let header_bytes = self
            .0
            .root
            .as_os_str()
            .len()
            .checked_add(self.0.path.as_os_str().len())
            .and_then(|n| n.checked_add(std::mem::size_of::<Inner>() + 512))
            .and_then(|n| n.checked_mul(2))
            .ok_or(Error::Budget)?;
        let _header = self
            .0
            .budget
            .reserve_bytes(header_bytes, 64 * 1024 * 1024)?;
        let count = preflight_index(&self.0.file, selected.end, self.0.limits, check)?;
        let mut blocks = Index::new();
        blocks.reserve_to(count, &self.0.budget, self.0.limits.max_blocks)?;
        // A private validation owner borrows duplicate handles from the same
        // locked file. It cannot escape as a new source or acquire disk quota.
        let candidate = Store(Inner::new(Backing {
            root: self.0.root.clone(),
            directory: self.0.directory.try_clone()?,
            directory_id: self.0.directory_id,
            path: self.0.path.clone(),
            file: self.0.file.try_clone()?,
            identity: self.0.identity,
            id: self.0.id,
            limits: self.0.limits,
            budget: self.0.budget.clone(),
            disk: None,
            access: Access::default(),
            state: Mutex::new(State {
                expected,
                blocks,
                head: None,
                indexed: Some(selected),
                readonly: true,
                poisoned: false,
            }),
            cache: Mutex::new(Index::new()),
            may_be_published: AtomicBool::new(true),
        }));
        let mut snapshot = candidate.read_selected(selected, Some(indexed), check)?;
        check()?;
        self.check_source(expected)?;
        let mut prepared = candidate.state()?;
        let mut state = self.state()?;
        if state.poisoned
            || !state.readonly
            || state.expected != expected
            || state.head != Some(head)
            || state.indexed.unwrap_or(head) != indexed
            || state
                .blocks
                .iter()
                .any(|(key, value)| prepared.blocks.get(key) != Some(value))
        {
            return Err(Error::Changed);
        }
        let replacement = std::mem::replace(&mut prepared.blocks, Index::new());
        let old = std::mem::replace(&mut state.blocks, replacement);
        state.indexed = Some(selected);
        drop(state);
        drop(prepared);
        drop(old);
        // Preserve publication head, readonly status and original owner identity.
        snapshot.store = self.clone();
        Ok(snapshot)
    }
}
