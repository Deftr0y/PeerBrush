# Native source retention (unreleased)

Native backing is authoritative retained data, not an evictable cache. No
published-source deletion or automatic startup garbage collection is enabled.
The registered checkpoint path preserves strong selected-prefix owners from an
actual complete capture or load proof. Missing, partial, unparsed, changed or
unregistered recovery markers remain held and prevent a complete checkpoint
census. A census is an observation, never a deletion permit.

| Root | Actual retained owner |
| --- | --- |
| Native current document and immutable clones | Admitted Core, roster, Surface and physical Store owners |
| Native undo/redo and captured history records | Before/after NativeDocument owners in shared records |
| Prepared transaction, previews and generated outputs | Candidate documents, Surface views, Snapshots and FreshFragment tokens |
| Render/save/export work | Immutable scene/source owners and admitted SourceSession through final I/O |
| Registered persistent recovery | Complete selected Snapshots retained by SourceRuntime |
| Unloaded, unknown or incomplete recovery | Held files and fail-closed checkpoint census |

Native tab, clipboard and task roots are not yet centrally switched from the
resident Engine. Their complete live integration remains required before any
published-source retirement policy can be admitted. Source-key counts alone
cannot establish that integration. Resident pixel pins can outlive a surface;
future retirement must additionally inspect live cached pins after establishing
exclusive source ownership. A completed resident buffer is not a new disk-source
or recovery capability.

## Generated transaction outputs

A transaction may prepare several immutable generated fragments while only its
final sources survive. The worker identifies final physical Store ownership
before publication. The final in-memory assignment marks only surviving
fragments with the existing irreversible publication flag. All output views and
unused tokens move to deferred retirement outside the Engine mutex.

Deferred cleanup first drops every intermediate Surface, selected Snapshot and
candidate view, then calls the existing registered SourceSession abort operation
for each unused FreshFragment. An escaped preview or late owner refuses unique
retirement and retains bytes and debt. The publication flag is never cleared;
a published fragment cannot use the unpublished abort route. Dropping a token
never refunds disk or removes files.

## Checkpoint registration

Only the recovery capture and complete recovery loader can construct the opaque
checkpoint proof. It binds the exact marker UUID, whole-file SHA-256, byte length
and ordered complete selected Snapshot roster. Source snapshots must belong to
the actual runtime and carry the conservative publication mark. This mark is a
necessary refusal guard for unpublished fragments, not document or checkpoint
authority on its own.

Registration rechecks the actual borrowed session, inventory file identity,
complete marker SHA and every selected source owner. Snapshot roster allocation
is charged before cloning and remains charged with its owner. Registration does
not select a newer source head, infer offsets, grant editable-document adoption,
or activate recovery dispatch. Repeated identical registration is idempotent;
conflicting evidence fails closed.

The explicit census inventories the actual held source namespace and rechecks
registered marker identities and complete hashes. Unregistered `.pbr` or `.json`
markers, pending records, unexpected files or directories, changed membership
and registration races refuse the observation. Startup recognizes files for
accounting only; a complete capture/load proof must establish registration.
Registered snapshots remain strong owners for the runtime's lifetime. There is
no automatic marker removal, registry pruning, compaction or disk refund.

## Published retirement remains closed

A future explicit retirement implementation must join the complete persistent
marker census with a source-specific admission gate that excludes resolver and
weak-owner resurrection. It must prove final Arc uniqueness and absence of live
pins, validate every known source record, hash, original lineage and exact EOF,
then retire only that held file and its verified empty owned directory. Readonly
tails, partial writes, unknown checkpoints, foreign replacements or uncertain
outcomes retain data and debt. All live current/history/clipboard/task/tab roots
and readers must participate before that gate is usable.

Actual profile, instance and workspace file-operation admission must survive the
whole check and retirement. The retirement-only closing check admits no new
source, checkpoint or publication. Uncertain codec children instead retain a
nominal SessionRetention with the exact admitted operation until known exit.
Windows directory-metadata power-loss durability remains unqualified.
