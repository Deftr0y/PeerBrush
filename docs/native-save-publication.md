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

This is not an activated save route or complete restart settlement. Closed
receipt persistence, linked repeated-save reconstruction, destination writer
integration and native interruption acceptance remain required. Existing source
and canvas limits remain unchanged. Windows directory power-loss durability is
not qualified by the current no-op parent-directory sync helper. Published
source/history/recovery garbage collection remains closed.
