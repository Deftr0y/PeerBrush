# Verified local Windows update transaction

Implementation is in progress. Production activation remains disabled and no feed,
publisher certificate or runtime trust default is supplied. Component tests do not
qualify a genuine signed installation, launch, interruption or recovery.

## Installation boundary

The direct route submits a verified local MSIX and explicit local runtime through
[`PackageManager.AddPackageAsync`](https://learn.microsoft.com/en-us/uwp/api/windows.management.deployment.packagemanager.addpackageasync).
Its inputs are file URIs made from the already held files. Stable and immutable
descriptor equality, complete signature/payload validation, exact hashes, named
file identities and write/delete denial precede submission. The descriptor is
evidence; it is never passed to an installation API for another network fetch.
Forward deployment neither forces application shutdown nor downgrades versions.
Only the separately verified previous main package can use the rollback flag.
The documented `AddPackageAsync` overload accepts `None` or forced shutdown,
so recovery uses the 19041
[`AddPackageByUriAsync`](https://learn.microsoft.com/en-us/uwp/api/windows.management.deployment.packagemanager.addpackagebyuriasync)
options API with
[`ForceUpdateFromAnyVersion`](https://learn.microsoft.com/en-us/uwp/api/windows.management.deployment.addpackageoptions.forceupdatefromanyversion).
Its URI is the same checked local MSIX file, and unsigned/development, forced
shutdown and deferred registration options are explicitly refused. No network
input or descriptor is passed. Rollback does not force a shared framework downgrade.

This route requires a signed direct MSIX installation. Microsoft documents
[different code-update paths for App Installer associations](https://learn.microsoft.com/en-us/windows/msix/non-store-developer-updates).
An existing App Installer association therefore does not silently enter this
route. Legacy association migration remains separately held. Store ownership and
unsigned/development registration stay outside the direct-package authority.
The serialized source records distinguish the new route explicitly; old records
retain their App Installer interpretation and wire representation.

## Surviving owner and final source

Windows [`LockFileEx`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex)
does not transfer a parent's byte-range lock to its child. A separate
[`CreateFile` sharing gate](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)
therefore blocks new profile admission while the old parent still owns its normal
settings lock. The surviving owner must reacquire that normal lock after actual
kernel process termination. Closing a pipe, observing a PID in JSON, a timeout or
an application callback is not a substitute for that process event.

The live handshake binds the named-pipe peer to an owned kernel process object,
creation time, package identity and verified executable. Local pipe names reject
remote clients and all frames are bounded. A nonce or command-line argument alone
grants no authority.

The initial staging backup does not become the final profile. The final copy is
published under the shared source/registry/Store writer transaction, after UI
preferences are flushed. Update exit cannot discard dirty work. Only the final
acknowledgement after writer sealing may authorize the surviving owner to proceed
after kernel exit. Owned byte/identity/absence checks continue across every
boundary. Unknown or contradictory observations hold the original profile.

## Health, recovery and subsequent updates

Registration success is not launch health. The selected installed executable and
corresponding source must match their verified signed payload, and an authenticated
fresh package process must exercise an isolated copy of the final profile. The
normal profile remains protected. The owner pins the isolated namespace throughout
the probe and compares every copied/absent allowlisted preference byte after its
actual process exit. A migrated or rewritten clone cannot establish compatible
recovery; this bounded path requires no profile migration. An observed failed and exited health child can
take the compatible previous-package path; an uncertain live child must hold.
Recovery must prove the previous installed payload and its actual health before
normal admission. Foreign profile changes are retained, never overwritten with
an earlier backup.

Durable intent precedes every deployment. Native terminal results are drained
under ownership, including cancellation. An interrupted intent is evidence of
uncertainty and cannot manufacture a new apply or recovery permit. Completion
records, exact recovery-media promotion and cache retirement require fresh native
identity and profile checks. A subsequent upgrade must capture the then-current
healthy package/profile; it cannot reuse the first transition's source proof.
Historical signed archives are retained. Before a final copy/seal or apply record
exists, an authenticated live-parent cancellation can archive only its owned
intent/active marker and keep the staged media. A lost pipe or later cancellation
cannot take that path. Unknown outcomes continue to hold.

The live owner now consumes the final seal and real kernel exit, reacquires the
profile lock, applies exact local bytes, launches the isolated installed GUI health
consumer and uses the same guarded transaction for compatible previous-package
recovery. The probe exercises the production workspace for three native frames;
it does not establish calibrated rendering, tablet or arbitrary document fidelity.
Its process must prove package identity and the logical-to-physical original and
isolated profile mappings. A copied owner must prove that it is outside package
identity and any parent lifetime job; inability to establish either fact refuses
before parent exit.

Successful live completion prepares fresh verified CURRENT media, preserves old
CURRENT and completed transition archives, then retires the active marker by
checked file-handle identity. Unknown interrupted state holds before migration.
There is no automatic retry under an unobserved provider, no archive deletion and
no claim that a terminal JSON record alone authorizes recovery. Eight retained
owner directories/transitions are a bounded limit pending reviewed archive
management.

Native component replay and genuine isolated signed-package qualification are
still required. The UI caller is connected behind the unchanged production gate. It requires a
human restart click and an untouched blank workspace. Users close projects through
the ordinary explicit close flow first; saved files do not authorize losing live
undo/redo. The atomic transaction repeats that restriction under all engine,
registry and Store writer locks. Arbitrary live-history restart persistence is
outside this bounded path. Health probes suspend release checks and use isolated
server/preferences/cache paths.
Signing/runtime facts, classic packaged-process launch identity, package file
virtualization and surviving owner execution must be demonstrated with actual
signed fixtures before activation.
