//! A complete per-target chain is quota evidence, never a resumed writer.
use super::{
    closed_frame::{ClosedFrame, Place},
    frame::{Expected, IntentFrame, Role},
};
use crate::{
    large_document::ResidentBudget,
    psd::memory36::{Admitted, MetadataCharge},
};
use std::sync::Arc;
pub(super) struct Order {
    pub entries: Vec<usize>,
    pub latest: Vec<usize>,
}
pub(super) struct Node<'a> {
    pub frame: &'a IntentFrame,
    pub closed: Option<(&'a ClosedFrame, [u8; 32])>,
}
pub(super) fn continues(
    before: &IntentFrame,
    closed: &ClosedFrame,
    closed_sha: [u8; 32],
    next: &IntentFrame,
) -> Result<(), String> {
    if next.previous != Some(closed_sha)
        || before.target_key != next.target_key
        || before.target != next.target
        || before.parent != next.parent
        || before.backups != next.backups
        || before.backups_identity != next.backups_identity
    {
        return Err("External intent does not continue its exact destination chain".into());
    }
    let mut expected = None;
    for binding in &closed.bindings {
        match binding.place {
            Place::Target => {
                let sha256 = match before.expected {
                    Some(old) if old.identity == binding.fact.identity => old.sha256,
                    _ => before.candidate_sha256,
                };
                expected = Some(Expected {
                    identity: binding.fact.identity,
                    bytes: binding.fact.bytes,
                    modified: binding.fact.modified,
                    sha256,
                });
            }
            Place::Temporary => {
                return Err("Retained external temporary keeps this destination blocked".into())
            }
            Place::BackupOrdinal(ordinal) => {
                let old = before
                    .facts
                    .get(ordinal as usize)
                    .ok_or("Previous backup binding is absent")?;
                preserved(next, &old.name, binding.fact)?;
            }
            Place::CreatedBackup => preserved(
                next,
                before
                    .backup
                    .as_deref()
                    .ok_or("Previous backup name absent")?,
                binding.fact,
            )?,
            Place::RetainedOriginal => preserved(
                next,
                &before
                    .retained_original_name()
                    .ok_or("Previous original name absent")?,
                binding.fact,
            )?,
        }
    }
    if next.expected != expected {
        return Err(
            "Destination changed since its previous closed outcome; bytes and debt retained".into(),
        );
    }
    Ok(())
}
fn preserved(
    next: &IntentFrame,
    name: &std::ffi::OsStr,
    fact: crate::large_document::ExternalFact,
) -> Result<(), String> {
    let at = next
        .facts
        .binary_search_by_key(&fact.identity, |f| f.fact.identity)
        .map_err(|_| "Previous backup is absent from the next inventory")?;
    let now = &next.facts[at];
    if now.role != Role::Backup || now.name != name || now.fact != fact {
        return Err("Previous backup changed across the external intent chain".into());
    }
    Ok(())
}
pub(super) fn order(
    registry: &[Node<'_>],
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Admitted<Order>, String> {
    let count = registry.len();
    let owner = MetadataCharge::reserve(
        pool,
        count
            .checked_mul(8 * std::mem::size_of::<usize>())
            .and_then(|n| n.checked_add(128))
            .ok_or("External chain footprint overflow")?,
        check,
    )?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| "External chain allocation refused")?;
    let mut latest = Vec::new();
    latest
        .try_reserve_exact(count)
        .map_err(|_| "External latest index allocation refused")?;
    let mut seen = Vec::new();
    seen.try_reserve_exact(count)
        .map_err(|_| "External chain visitation allocation refused")?;
    seen.resize(count, false);
    for (root, entry) in registry
        .iter()
        .enumerate()
        .filter(|(_, r)| r.frame.previous.is_none())
    {
        check()?;
        if latest
            .iter()
            .any(|at: &usize| registry[*at].frame.target_key == entry.frame.target_key)
        {
            return Err(
                "Multiple external chain roots name one target; pending debt retained".into(),
            );
        }
        let mut at = root;
        loop {
            check()?;
            if seen[at] {
                return Err("External intent chain is cyclic or duplicated".into());
            }
            seen[at] = true;
            entries.push(at);
            let current = &registry[at];
            let mut next = None;
            if let Some((closed, closed_sha)) = current.closed {
                for (child, following) in registry
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.frame.previous == Some(closed_sha))
                {
                    if next.replace(child).is_some() {
                        return Err("External intent has competing children".into());
                    }
                    continues(current.frame, closed, closed_sha, following.frame)?;
                }
            }
            match next {
                Some(child) => at = child,
                None => {
                    latest.push(at);
                    break;
                }
            }
        }
    }
    if seen.iter().any(|v| !*v) {
        return Err("External chain has a missing or ambiguous previous receipt".into());
    }
    check()?;
    Ok(Admitted::from_parts(
        Arc::new(Order { entries, latest }),
        owner,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::{closed_frame::Binding, frame::NamedFact};
    use super::*;
    fn fact(id: u64, bytes: u64) -> crate::large_document::ExternalFact {
        crate::large_document::ExternalFact {
            identity: (1, id),
            bytes,
            modified: id as u128,
        }
    }
    fn frame(
        id: u8,
        previous: Option<[u8; 32]>,
        expected: Expected,
        candidate: u64,
    ) -> IntentFrame {
        let parent = std::env::current_dir().unwrap();
        let mut facts = vec![
            NamedFact {
                role: Role::Target,
                name: "n.psb".into(),
                fact: crate::large_document::ExternalFact {
                    identity: expected.identity,
                    bytes: expected.bytes,
                    modified: expected.modified,
                },
            },
            NamedFact {
                role: Role::Backup,
                name: "old.psb".into(),
                fact: fact(2, 3),
            },
        ];
        if id != 1 {
            facts.push(NamedFact {
                role: Role::Backup,
                name: "backup-1.psb".into(),
                fact: fact(4, 4),
            })
        }
        facts.sort_unstable_by_key(|f| f.fact.identity);
        IntentFrame {
            id: [id; 16],
            owner: [2; 16],
            operation: [3; 16],
            target: parent.join("n.psb"),
            target_key: [4; 32],
            previous,
            parent: (1, 99),
            backups: parent.join(".peerbrush-versions"),
            backups_identity: (1, 98),
            expected: Some(expected),
            candidate_bytes: candidate,
            candidate_sha256: [6; 32],
            provenance: [7; 32],
            core: [8; 32],
            render: [9; 32],
            temporary: format!("temp-{id}").into(),
            backup: Some(format!("backup-{id}.psb").into()),
            growth: candidate + expected.bytes,
            facts,
        }
    }
    fn first() -> IntentFrame {
        frame(
            1,
            None,
            Expected {
                identity: (1, 1),
                bytes: 4,
                modified: 1,
                sha256: [5; 32],
            },
            8,
        )
    }
    fn second() -> IntentFrame {
        frame(
            2,
            Some([11; 32]),
            Expected {
                identity: (1, 3),
                bytes: 8,
                modified: 3,
                sha256: [6; 32],
            },
            7,
        )
    }
    fn closed() -> ClosedFrame {
        ClosedFrame {
            id: [1; 16],
            owner: [2; 16],
            intent: [10; 32],
            target: [4; 32],
            growth: 12,
            bindings: vec![
                Binding {
                    place: Place::BackupOrdinal(1),
                    fact: fact(2, 3),
                },
                Binding {
                    place: Place::Target,
                    fact: fact(3, 8),
                },
                Binding {
                    place: Place::CreatedBackup,
                    fact: fact(4, 4),
                },
            ],
            retired: vec![(1, 1)],
        }
    }
    #[test]
    fn unordered_chain_selects_one_latest_state_and_keeps_shared_backup_identity() {
        let before = first();
        let after = second();
        let receipt = closed();
        let pool = ResidentBudget::new(65536).unwrap();
        let nodes = [
            Node {
                frame: &after,
                closed: None,
            },
            Node {
                frame: &before,
                closed: Some((&receipt, [11; 32])),
            },
        ];
        let selected = order(&nodes, &pool, &|| Ok(())).unwrap();
        assert_eq!(selected.entries, [1, 0]);
        assert_eq!(selected.latest, [0]);
        assert_eq!(after.facts[0].fact.identity, (1, 2));
        drop(selected);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn next_intent_must_preserve_the_relocated_original_identity_and_name() {
        let before = first();
        let mut receipt = closed();
        receipt.retired.clear();
        receipt.bindings.push(super::super::closed_frame::Binding {
            place: Place::RetainedOriginal,
            fact: fact(1, 4),
        });
        receipt.bindings.sort_unstable_by_key(|b| b.fact.identity);
        receipt.validate(&before, [10; 32]).unwrap();
        let mut after = second();
        assert!(continues(&before, &receipt, [11; 32], &after).is_err());
        after.facts.insert(
            0,
            NamedFact {
                role: Role::Backup,
                name: before.retained_original_name().unwrap(),
                fact: fact(1, 4),
            },
        );
        after.validate().unwrap();
        continues(&before, &receipt, [11; 32], &after).unwrap();
        after.facts[0].name = "different-original.psb".into();
        assert!(continues(&before, &receipt, [11; 32], &after).is_err());
    }
    #[test]
    fn missing_previous_competing_children_cross_target_and_live_version_changes_refuse() {
        let pool = ResidentBudget::new(65536).unwrap();
        let before = first();
        let after = second();
        let receipt = closed();
        assert!(order(
            &[Node {
                frame: &after,
                closed: None
            }],
            &pool,
            &|| Ok(())
        )
        .is_err());
        let mut competing = second();
        competing.id = [3; 16];
        assert!(order(
            &[
                Node {
                    frame: &before,
                    closed: Some((&receipt, [11; 32]))
                },
                Node {
                    frame: &after,
                    closed: None
                },
                Node {
                    frame: &competing,
                    closed: None
                }
            ],
            &pool,
            &|| Ok(())
        )
        .is_err());
        let mut changed = second();
        changed.expected.as_mut().unwrap().identity = (1, 55);
        assert!(continues(&before, &receipt, [11; 32], &changed).is_err());
        let mut other = second();
        other.target_key = [99; 32];
        assert!(continues(&before, &receipt, [11; 32], &other).is_err());
        let mut missing = second();
        missing.facts.retain(|f| f.fact.identity != (1, 4));
        assert!(continues(&before, &receipt, [11; 32], &missing).is_err());
        let mut shared = second();
        shared.facts[0].name = "renamed.psb".into();
        assert!(continues(&before, &receipt, [11; 32], &shared).is_err());
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn pending_unselected_and_retained_temp_never_become_a_new_write_parent() {
        let pool = ResidentBudget::new(65536).unwrap();
        let before = first();
        let after = second();
        assert!(order(
            &[
                Node {
                    frame: &before,
                    closed: None
                },
                Node {
                    frame: &after,
                    closed: None
                }
            ],
            &pool,
            &|| Ok(())
        )
        .is_err());
        let mut receipt = closed();
        receipt.bindings.push(Binding {
            place: Place::Temporary,
            fact: fact(7, 8),
        });
        assert!(continues(&before, &receipt, [11; 32], &after).is_err());
        assert!(order(
            &[Node {
                frame: &before,
                closed: Some((&receipt, [11; 32]))
            }],
            &pool,
            &|| Err("cancel".into())
        )
        .is_err());
        assert_eq!(pool.used(), 0);
        assert!(order(
            &[Node {
                frame: &before,
                closed: Some((&receipt, [11; 32]))
            }],
            &ResidentBudget::new(1).unwrap(),
            &|| Ok(())
        )
        .is_err());
    }
}
