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
Only the separately verified previous main package can use the rollback flag;
rollback does not force a shared framework downgrade.

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
normal profile remains protected. An observed failed and exited health child can
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
Historical signed archives are retained.

The implemented state-machine, final-profile, gate, process and framing components
must be connected to the live owner and health consumer and pass native replay
before installation can be offered. Genuine signing/runtime facts and isolated
signed-package qualification remain independent requirements.
