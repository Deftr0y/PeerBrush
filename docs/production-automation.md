# Production candidate and Store reconciliation — unreleased

PB-154 now prepares tested, exact-source release candidates on a push to `main`, using the existing three-platform release workflow. A manual run still requires an exact source SHA matching its workflow ref. Every native job runs formatting, Rust tests, Clippy and packaging regressions before building. Inspection requires Windows x64, macOS arm64 and Linux x64 archives with identical corresponding source, version and executable source commit. These are Actions candidates; this workflow does not publish releases, deploy the website or submit to a store.

The final `production-readiness.json` report reopens and validates the real archives against committed source, checks the final inventory and `SHA256SUMS`, and rejects old-binary relabelling or packaging corrections. Each platform reports `candidate_verified`, native signing evidence, `ready: false` and `public_availability: unverified`. The existing archive inspector checks signing metadata and hashes; that alone cannot establish native signature or installation acceptance. The report deliberately has no path to `production_ready: true` until those missing verification and protected publication adapters exist. Sigstore signing remains an explicit manual candidate option; it does not replace native signing/notarization.

`--previous-inventory` compares against a retained publication inventory: an already published version must keep its exact source SHA and every existing asset filename/hash. Without that reconciled input, version immutability is explicitly unverified. `--require-ready` exits unsuccessfully while production gates remain blocked. Existing output reports cannot be replaced with a different decision. No tag, release file or history entry is overwritten.

Pending gates are installed Windows distribution/signing, signed and notarized Mac DMG installation, the chosen installed Linux format/update owner, protected publication approval, and public asset/website readback. The existing reviewed Apple signing workflow retains its source pin and protected environment; this change neither repoints nor activates it. Windows Store automation retains its existing disabled variable. No credential, environment or branch-protection setting changes are part of this implementation.

## Private Store submission state

The existing MSIX writer still validates a published stable release, exact Store identity, package provenance and checksums, current published version and absence of a pending submission. Actual submission additionally requires an absolute `--state` file inside an existing durable private directory. The directory and receipt must stay outside public source, release packages and Actions artifacts.

```sh
python scripts/submit_store_release.py --tag vVERSION --state /private/store-receipt.json
# Add --submit only after explicit authorization and Store preflight review.
```

The receipt binds the tag, exact source SHA, package filename, package hash and Store version. An OS lock prevents concurrent receipt writers and releases if the process exits. The writer durably saves create intent before creating a remote draft, saves its ID before upload/metadata changes, and saves commit intent before the remote commit. A disconnected create can succeed without returning an ID; its receipt remains uncertain and blocks another automatic create, even before the service reports a pending submission. Mutating requests are never automatically retried, deleted or replaced. Existing listing, pricing and publish-mode preservation remains in force.

After an interrupted run or during certification, resume observation with:

```sh
python scripts/store_release_state.py --state /private/store-receipt.json
```

This command only performs authenticated service GETs after token acquisition. It checks product/submission identity and expected package version/name, retains its exact receipt, and retries transient GET failures at most three times with one- and two-second delays. Authentication/authorization, missing submissions, unknown statuses and package mismatches fail closed. It never calls create, upload, commit, delete or publish. Unknown create outcomes require manual service reconciliation; the tool does not guess a submission ID or silently reset the journal.

Certification, rejection, publication pending, publishing, service Published and release processing are distinct states. `Published` and `Release` do not establish that the expected package is publicly downloadable in a particular market/device cohort, so `public_availability` stays `unverified`. Public version availability needs a separate readback adapter. A pending, rejected or uncertain channel cannot become a ready website download. The read-only result identifies the channel as `microsoft-store`; other stores require their own reviewed adapter and cannot inherit Microsoft verification.

The [documented Store statuses](https://learn.microsoft.com/en-us/windows/uwp/monetize/get-status-for-an-app-submission) and [submission process](https://learn.microsoft.com/en-us/windows/uwp/monetize/manage-app-submissions) are the basis for this mapping. Commit acceptance starts ingestion; it is not public availability.

The hosted Store workflow remains disabled. Even if its variable were later enabled, submitting without a durable private receipt fails before release download or authentication. A reviewed private state persistence adapter and explicit activation approval are still required; uploading the receipt to this public repository's Actions artifacts is not a substitute.

## Website boundary and verification

The reviewed public `site-content` branch contains `public-site/downloads.json`, version history and baked home-page labels. It is an accessible output source, separate from the private site builder. A future validated update must preserve existing history, use real immutable public asset URLs/checksums, update home/latest/download labels together, and show each unavailable channel accurately. Candidate files and Store service status are insufficient inputs. This implementation prepares no website edit or deployment; those actions retain the documented `maintenance/site-*` review and approval boundary.

Offline checks are `scripts/test_store_release_state.py`, `scripts/test_production_readiness.py` and the existing Store/package tests. Synthetic archives are structurally inspected; fake service responses exercise status/error boundaries without authentication, real signing, uploading or publishing. Native platform signing/installation, actual Partner Center access, crash/restart on hosted runners and full main-to-public acceptance remain pending.
