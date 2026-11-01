# Native save publication admission (unreleased)

Native Source36 staging and a user destination coexist. Persistent source bytes,
every observed previous-save file and new destination/backup growth therefore
share one admission ledger. Existing files are counted once by physical file
identity across targets. A pending operation reserves only its prospective new
backup and destination temporary. A closed filesystem outcome may atomically
transfer that growth into observed remaining files; it does not reserve the same
bytes twice or claim that physical free space increased.

Before any external temporary or backup is created, the actual SourceSession
checks the completed save's exact session and global pools, retained source
binding, complete candidate length and SHA, and the live destination inventory.
It exclusively writes, syncs, reads back and publishes an immutable intent in its
held source namespace. The record binds the source namespace and operation,
candidate/core/render bindings, destination parent and backup-directory
identities, expected destination version, every original named quota fact and
the exact prospective temporary/backup names. Its checksum provides integrity;
filesystem/profile/instance admission supplies authority.

Dropping a ticket, losing a response, partial writing or ambiguous publication
keeps all debt. A complete pending record can rebuild conservative quota after
restart, but cannot reconstruct a live writer, destination lease or editable
source. Partial, foreign, unknown or ambiguous records remain held and refuse
fresh admission. Decoded paths never authorize writes or deletion. A native
filesystem writer's sealed outcome is required to settle live debt; raw paths,
counts, hashes and decoded records cannot implement that production proof.

Publication has a separate closing boundary. A begun save request preallocates
its commit data and validates its exact Control/run/document/revision before
the final Engine fence. That fence must independently compare the current save,
source key/revision/path/version and saved revision. It consumes the prepared
request while serializing against instance stopping, without filesystem I/O or
allocation on success. A resulting permit belongs to one source operation and
one intent only. It may check that intent's completion under the still-held
profile and file-operation guards while closing; ordinary source sessions,
factories and cursors continue refusing new work. Retirement-only checks never
grant publication authority.

The actual exclusive zero-byte temporary or backup has a separate sealed
creation proof. Its physical identity and exact planned role/name/parent are
durably registered before the first payload byte; a live token gates replay.
Decoding that record cannot reconstruct a token or adopt a name/hash match.

Before commit, the operation precreates an exclusive fixed 256 KiB outcome slot
and admits its buffers, metadata, file bytes and quota-transfer workspace. It
fills that existing handle forward only. The final complete checksum/footer
selects the outcome; sync and complete readback precede in-memory selection and
the atomic accounting transfer. Empty/truncated outcome records linked to a
valid intent retain full pending growth and block competing destination writes
while native checkpoint reads remain available. Unknown intents or ambiguous
chains remain refused. Complete crash records are read-only quota evidence,
without resumed publication/deletion authority.

Each subsequent target intent binds its previous closed checksum. Reconstruction
validates the complete chain, preserved backup identities and current named
facts before counting the latest state. A changed live target or retained
temporary blocks another write instead of silently forgetting uncertain debt.
Read-only pending reconstruction conservatively counts an old external identity
and a newly reused internal identity separately; it cannot retire the internal
file or convert an existing pending ticket into a settled one.

This is not an activated save route. Destination writer/Engine integration and
native interruption acceptance remain required. Existing source and canvas
limits remain unchanged. Windows directory power-loss durability is not
qualified by the current no-op parent-directory sync helper. Published
source/history/recovery garbage collection remains closed.

## New destinations

Before even a versions directory or save lock is created, the source session
admits the destination against the actual private source namespace, current held
parent ancestry, managed staging names and known backing FileIds. The native lease
consumes this proof and retains its operation and directory handles through the
save receipt. It never recanonicalizes a raw caller path as new write authority.
POSIX directory/lock/quota creation uses the admitted directory descriptors;
Windows retains deny-delete ancestor and versions-directory name pins. Namespace
checks repeat immediately before mutation. These are live observations and
relative-object operations, not a POSIX atomic ancestry/rename comparison claim.
The operation owner must not escape into long-lived document file ownership.

A completed Source36 candidate already owns the exact scene and all qualified
standard/source bytes. The destination writer requires that candidate, its exact
source session, a held destination lease and frozen previous-version inventory.
A durable publication intent fixes the destination, candidate SHA/length and
same-directory temporary name before any external payload is created.

The writer creates the temporary exclusively under a held parent directory. It
syncs the empty file and parent, then registers the actual zero-byte FileId in the
source journal. A planned name or matching file digest cannot substitute for that
registration. Failure retains the file and the full admitted disk debt.

Replay is append-only through a charged 64 KiB buffer. Every boundary checks the
session, lease, actual named/held temporary, source candidate and registered
creation. The entire temporary is synced and SHA-checked before the Engine fence.
The fence consumes only an already prepared commit token; it performs no file I/O,
large hashing or new admission. Publication uses a no-clobber operation. A foreign
new destination is retained and reported as a conflict.

After publication the held file is rehashed and its named identity reobserved.
The private outcome receipt retains the plan, lease/inventory and physical file
holds, but does not borrow the committed permit. The journal can therefore consume
that permit after finishing its already admitted receipt. Normal new source
operations still refuse workspace closing. Only the exact precommitted operation
may finish. A renamed temporary remains a live destination FileId, never a retired
file refund. Drop does not unlink external files or refund uncertain debt.

The current abort receipt reports the retained temporary as live debt; it does
not claim verified retirement. Only a complete retained candidate can settle
that debt; partial, foreign or uncertain files keep the full pending reservation.
A remaining temporary blocks the next write to that same target. Historical
backups are never deleted by abort.

## Ordinary access rules

Existing-source backup copying uses held descriptors and refuses encrypted files
before plaintext staging. Windows compares owner and primary group, DACL presence
and null-versus-empty state, ordered active ACEs and source descriptor stability.
A backup freezes the copied DACL against its new directory inheritance. Only the
`INHERITED_ACE` origin marker may differ from the source; permission masks, SID,
ACE type, order and every other flag remain exact. No token privilege is enabled.
SACLs and a complete security-descriptor copy are not claimed. Unreadable or
unpreservable ordinary access rules fail before payload copying.

Linux checks held owner/group/mode and access ACL bytes. macOS checks held
owner/group/mode and the external ACL representation. Platform execution evidence
is required in addition to source review.

## Existing destinations: unresolved replacement contract

The component above accepts only an absent destination. It does not change the
ordinary legacy Save path. Windows replacement APIs and POSIX `rename` do not
compare an expected destination FileId atomically. A last check followed by a
replace cannot guarantee that a raced foreign file is untouched.

A separate, unadopted relocation proposal would journal a third quarantine name,
move the old destination there with no-clobber semantics, verify the expected
held ID and bytes, and publish the candidate with another no-clobber move. If a
foreign file raced into the first move, restoration would also be no-clobber;
occupied names and all uncertain files would remain retained. That proposal has
an unbounded missing-destination window after a process crash or power loss.
Filesystem watchers and cloud-sync clients can observe the move/create sequence.
A durable phase record can support later exact-ID recovery but cannot bound that
absence or undo external watcher effects. It is not activated or accepted as the
ordinary Save contract.

Atomic replacement with an exact backup capture preserves a no-gap filename but
can move a raced foreign version to that backup. This is a different contract and
must not be described as destination-ID compare-and-replace. Until a replacement
contract is reviewed and qualified, existing-target native Save remains a gate;
new-name no-clobber Save As is the available safe primitive, not full Save readiness.
