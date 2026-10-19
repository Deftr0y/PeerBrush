# Update discovery and restart protection (PB-147 development)

This is an unreleased discovery/safety slice. It checks actual official release metadata and protects a future installer restart. It does not download executable packages, install updates or restart PeerBrush. A published filename or checksum is never treated as a verified package signature or a ready-to-install update.

## Current behavior

File → Updates opens a compact panel with the current application version, installation owner, Stable/Preview channel, opt-in daily checks, Check now, actual request/read progress, release notes, errors and Later. Preferences use the existing guarded per-user profile or explicit instance-local workspace as `updates.json`, independently from artwork and document history. Automatic checks are off until enabled. The last attempt is retained across restarts to limit automatic checks to once per day. A failed check replaces any earlier success; offline or malformed responses never mean up to date. Closing the panel defers attention without closing projects.

`peerbrush updates check` performs the same real check without opening a document. `--preview` includes published prereleases. These command-line checks do not change preferences. They report metadata only.

The fixed request is the public GitHub releases collection for `Deftr0y/PeerBrush`, with at most 100 entries and 2 MiB. It uses the existing `ureq` client with TLS enabled, standard certificate/hostname validation and bundled Mozilla trust roots. HTTPS is required; redirects are disabled. Connections and reads have five-second limits and the request has a 25-second total limit. No API credentials or arbitrary endpoint override is accepted.

Discovery excludes drafts/unpublished entries, compares full SemVer precedence and separates stable from prerelease selection. A tag must parse as an exact semantic version, optionally prefixed by `v`. Returned release/asset URLs must be the exact canonical HTTPS paths for this repository and tag; credentials, query/fragment additions, other origins and path tricks are rejected. It bounds assets, filenames, advertised package sizes and displayed notes. Notes render as plain text, with an official release link for full details.

Only uploaded, nonempty packages matching the exact version and supported architecture are listed. Windows x64 and Linux x64 portable ZIPs, the corresponding Linux amd64 DEB and macOS arm64 ZIP/signed-DMG names are recognized. Candidate DMGs are excluded. Package names alone establish no signing status. If a newer release lacks the current platform/architecture, the panel states that its download is unavailable; it never substitutes an older platform package. The checker inspects the latest bounded collection, not an arbitrary `latest` redirect or an invented download path.

## Installation ownership

Windows reads package identity and native `Package.SignatureKind`: Store-signed installations are identified as Microsoft Store-managed, while other packaged installations defer to their installation provider. A package identity alone never establishes Store origin. Unknown signature/provenance results remain unknown. This slice does not invoke Store install APIs, which can terminate the app; a future adapter must separate download/progress from installation and obey the restart gate. See [Microsoft's Store update API](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/package-updates-from-store).

Linux system installation paths and Flatpak context conservatively defer to the package manager. This does not claim a configured APT/Flatpak update repository. Direct distributions and local builds can check releases but have no direct installer configured. No channel competes with its Store or package manager.

## Shared restart protection

`workspace::lifecycle::update_readiness` is a read-only status check. A future verified native installer must recheck exact project/document/revision snapshots through `finish_update_exit`, alongside native UI gesture checks. This path rejects discard decisions, unsaved or externally conflicting sources, active file operations, loading and procedural code. It verifies the complete project set again and closes all projects atomically only after every check succeeds. A failed check leaves every project open. Ordinary human Save / Don't Save / Cancel close behavior is unchanged.

Save, PNG export, complete export batches, recovery checkpoints/discards and shared brush/tool/filter/export preference changes hold runtime activity counters through unlocked preparation and final writes. Counters are shared through engine clones or the workspace, remain outside PSD/history and prevent a previously clean readiness snapshot racing a newly started operation. Once atomic update exit closes the workspace, new guarded file and preference operations are refused. Shared preferences remain usable when a single original project tab closes normally. The native panel additionally reports unfinished strokes, transforms, parameter/text/selection gestures and file dialogs as blockers. There is no install/restart control wired to these APIs in this slice.

## Intended macOS direct installer and remaining trust work

Use standard Sparkle 2 instead of introducing a custom executable-replacement engine. Integration needs an approved, version-pinned Sparkle framework and corresponding notices/source, reviewed existing-key access or separate explicit approval for key creation, an approved Ed25519 public key embedded as `SUPublicEDKey`, and the exact official HTTPS appcast URL in `SUFeedURL`. No approved key/feed is currently configured; this implementation generates/imports no keys and changes no accounts, credentials or trust settings. See [Sparkle's signing/setup requirements](https://sparkle-project.org/documentation/).

Prepare the actual signed feed/archive pipeline before enabling an adapter. Require signed feeds and archive verification before extraction (`SURequireSignedFeed` and `SUVerifyUpdateBeforeExtraction`), monotonic versions and explicit error behavior. Preserve the installed app's expected Developer ID/bundle identity and notarization checks. Add exact audited framework, XPC/helper and framework-symlink paths to the package/signing inspection policy; the current signer intentionally rejects such extra payloads. Do not weaken the policy with general link/framework exceptions. Sign nested code before the app, sign/notarize/staple the final distribution, then recompute immutable final checksums. Test Sparkle's helper permissions, user quit deferral, unsaved projects, failed/interrupted installation and retained last-working app/settings on actual macOS.

Windows direct distribution still needs its approved trusted native signing and installer channel. Store update calls need installed Store-runtime acceptance. Linux needs real package-manager upgrade/repository acceptance. The discovery result cannot enable any of these missing capabilities.

## Verification

Focused tests cover semantic ordering and downgrade prevention, stable/preview selection, unpublished releases, wrong/missing architecture, candidate exclusion, malicious URLs, malformed/oversized/interrupted responses, guarded preference restart/conflicts, strict dirty/discard/source/project/I/O/loading races and UI error/gesture behavior. Existing save/export callbacks explicitly test restart rejection during unlocked file preparation. Run the `updates` and `update_restart` Rust filters plus lifecycle/save/export/recovery suites. Real TLS discovery, native panel rendering and the installed-platform matrix require separate acceptance evidence; no successful install/update is claimed by these source tests.
