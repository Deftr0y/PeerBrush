# Production candidate and Store reconciliation — unreleased

After promotion, this PB-154 change prepares tested, exact-source release candidates on a push to `main`, using the existing three-platform release workflow. A manual run still requires an exact source SHA matching its workflow ref. Every native job runs formatting, Rust tests, Clippy and packaging regressions before building. Inspection requires Windows x64, macOS arm64 and Linux x64 archives with identical corresponding source, version and executable source commit. These are Actions candidates; this workflow does not publish releases, deploy the website or submit to a store. Execution of the revised main trigger remains to be verified.

The final `production-readiness.json` report reopens and validates the real archives against committed source, checks the final inventory and `SHA256SUMS`, and rejects old-binary relabelling or packaging corrections. Each platform reports `candidate_verified`, native signing evidence, `ready: false` and `public_availability: unverified`. The existing archive inspector checks signing metadata and hashes; that alone cannot establish native signature or installation acceptance. The report deliberately has no path to `production_ready: true` until those missing verification and protected publication adapters exist. Sigstore signing remains an explicit manual candidate option; it does not replace native signing/notarization.

`--previous-inventory` compares against a retained publication inventory: an already published version must keep its exact source SHA and every existing asset filename/hash. Without that reconciled input, version immutability is explicitly unverified. `--require-ready` exits unsuccessfully while production gates remain blocked. Existing output reports cannot be replaced with a different decision. No tag, release file or history entry is overwritten.

Pending gates are installed Windows distribution/signing, signed and notarized Mac DMG installation, final installed Ubuntu qualification and authenticated update delivery, protected publication approval, and public asset/website readback. The existing reviewed Apple signing workflow retains its source pin and protected environment; this change neither repoints nor activates it. Windows Store automation retains its existing disabled variable. No credential, environment or branch-protection setting changes are part of this implementation.

The Ubuntu native job additionally qualifies final DEB bytes and consumes its freshly produced private installation receipt before uploading the candidate. This uses the selected portable/source inputs rather than trusting the package's report. Its separate public qualification report retains false readiness and explicit desktop, real-version migration, interrupted-update, signing, repository and publication limits. Receipt matching is a binding check; native execution provenance comes from the immediately preceding successful test in the same trusted job.

The aggregate job downloads that DEB candidate from the same successful workflow, selects its actual SHA-256 independently of reports, and reopens it against the exact verified Linux portable, source and public maintainer. `production_readiness.py --directory VERIFIED_PORTABLE --include-deb SELECTED_DEB --deb-sha256 SELECTED_DEB_SHA256 --portable-sha256 SELECTED_LINUX_PORTABLE_SHA256 --maintainer 'PUBLIC NAME <PUBLIC EMAIL>' --commit SOURCE_SHA --version VERSION --output NEW_CANDIDATE_DIRECTORY` creates a fresh, nonoverlapping output directory. It preserves the four verified portable/source inventory records and appends one inspected DEB record and its matching `SHA256SUMS` line. Existing candidates are never overwritten; interrupted copies remain for inspection and require a new output directory.

Only those five files and regenerated inventory, checksums and readiness report enter the aggregate. Builder sidecars, qualification reports, unrelated files and private installation receipts stay behind. The DEB's producer sidecars remain in its separate native Actions artifact; this bounded handoff does not supply the website's final-installer/sidecar adapter. The report keeps portable Linux status separate from installed-candidate content verification. Matching metadata does not authenticate native execution: same-workflow job provenance is required separately, and installed acceptance, native signatures, protected approval, update ownership and public availability remain unverified. Four-record inspection remains supported; inspecting a five-record directory requires the explicit DEB/portable hashes and maintainer again. This handoff does not include final DMG or MSIX records and cannot enable publication.

## Private Store submission state

The existing MSIX writer still validates a published stable release, exact Store identity, package provenance and checksums, current published version and absence of a pending submission. Actual submission additionally requires an absolute `--state` file inside an existing durable private directory. Before release downloads or authentication, the tool rejects supplied receipt paths inside any Git checkout, including independently cloned release-source inputs and worktrees identified by a `.git` file. It also rejects symbolic links, junctions and reparse points throughout the supplied path. Locking and receipt writes recheck these boundaries. The directory and receipt must stay outside public source, release packages and Actions artifacts.

```sh
python scripts/submit_store_release.py --tag vVERSION \
  --source-root /absolute/reviewed-release-source \
  --portable-sha256 VERIFIED_WINDOWS_PORTABLE_SHA256 \
  --msix-sha256 VERIFIED_FINAL_SDK_VALIDATED_MSIX_SHA256 \
  --state /private/store-receipt.json
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

The hosted Store workflow remains disabled. Manual/reusable callers must independently select the exact source SHA, Windows portable hash and final SDK-validated MSIX hash; the workflow checks out trusted automation separately from release-source data. Its release-event listener cannot yet select those inputs and stops before authentication. Even if its variable were later enabled, submitting without a durable private receipt fails before release download or authentication. A reviewed final-inventory adapter, private state persistence adapter and explicit activation approval are still required; uploading the receipt to this public repository's Actions artifacts is not a substitute.

## Website boundary and verification

The reviewed public `site-content` branch contains `public-site/downloads.json`, version history and baked home-page labels. The [public website-output adapter](website-release-output.md) prepares a new immutable external overlay only after actual public release/source/checksum verification. It preserves history and updates home/download/history labels/cache tokens together. Current committed distribution policy selects Store/DMG/DEB; missing native final-installer and Store public-readback adapters remain explicit exclusions, with a blocked report instead of portable fallback. The tool does not deploy or modify site-content. Website changes retain the documented `maintenance/site-*` review and approval boundary.

Offline checks are `scripts/test_store_release_state.py`, `scripts/test_production_readiness.py` and the existing Store/package tests. Synthetic archives are structurally inspected; fake service responses exercise status/error boundaries without authentication, real signing, uploading or publishing. Native platform signing/installation, actual Partner Center access, crash/restart on hosted runners and full main-to-public acceptance remain pending.


Unreleased final-DMG verification now has an independent native consumer, `qualify_macos_dmg.py`, described in [Mac signing](macos-signing.md#independent-final-dmg-qualification-unreleased). It reconstructs expected content from independently selected signed ZIP/source bytes and repeats actual native trust, read-only mount and whole-device cleanup checks rather than accepting a signer receipt. Fresh exclusive sanitized qualification stays false for installation/production readiness. Final DMG/MSIX aggregate handoff, protected native-job authentication, signing activation and public website eligibility remain separate unfinished adapters; this addition cannot make a missing package ready.

## Final-DMG candidate handoff (unreleased)

Before the first publication, select the signed/stapled Mac ZIP together with Windows/Linux portable archives, and build their exact-source four-record seed with `verify_release.py`. Add the independently inspected final DEB through the existing handoff. Then append the independently selected final DMG to a fresh external directory:

```sh
python -B scripts/production_readiness.py --source-root "$RELEASE_SOURCE" \
  --directory "$FIVE_RECORD_CANDIDATE" --include-dmg "$FINAL_DMG" \
  --dmg-sha256 "$DMG_SHA256" --macos-portable-sha256 "$SIGNED_ZIP_SHA256" \
  --dmg-qualification "$NATIVE_QUALIFICATION" --dmg-qualification-sha256 "$QUALIFICATION_SHA256" \
  --macos-team-id "$PUBLIC_TEAM_ID" --deb-sha256 "$DEB_SHA256" \
  --portable-sha256 "$LINUX_ZIP_SHA256" --maintainer "$PUBLIC_MAINTAINER" \
  --commit "$RELEASE_COMMIT" --version "$RELEASE_VERSION" \
  --previous-inventory "$PREVIOUS_PUBLISHED_INVENTORY" --output "$FRESH_FINAL_CANDIDATE"
```

All inputs are independently selected public values or private paths; no credential is accepted. Existing Mac ZIP selection cannot be silently replaced. Only the six listed assets, canonical inventory/checksums and sanitized readiness are copied; native signing journals and private receipts stay outside. Reinspection requires the same external hashes/Team and qualification inputs. A same-version published four/five-record set cannot gain a DMG through handoff or direct reinspection; select a new version. Matching complete six-record selections may be reconciled without publication.

Strict schema and digest matching binds native claims to selected bytes, and does not authenticate a native job. Overall `candidate_verified:false`, `asset_binding_verified:true`, unverified native producer/availability and false readiness are deliberate. Protected trusted native-job consumption, actual installation/update acceptance, publication and website installed-download adapters remain required. `--require-ready` returns a failing readiness status. Neither handoff nor local [Store preflight](msix.md#local-store-candidate-preflight-unreleased) publishes or activates a hosted workflow.


## Optional offline authentication of native Mac origin (unreleased)

A matching qualification JSON binds bytes and cannot prove who ran Apple tools. Six-record assessment and DMG handoff can additionally authenticate the exact qualification through `verify_macos_qualification.py`. Independently select the qualification/bundle hashes, a reviewed public trusted-root JSONL snapshot/hash, a reviewed GitHub CLI 2.102.0 executable/hash, the exact main workflow/source commit and invocation run/attempt. No root, CLI, key or signing workflow is downloaded, installed or activated by this consumer. It currently runs on Linux x64; leave the options absent when approved trust inputs do not exist.

Add all of these options together to the existing six-record inspection or handoff command:

```sh
  --native-attestation-bundle "$ATTESTATION_BUNDLE" --native-attestation-sha256 "$BUNDLE_SHA256" \
  --native-trusted-root "$REVIEWED_PUBLIC_ROOT_JSONL" --native-trusted-root-sha256 "$ROOT_SHA256" \
  --native-gh "$REVIEWED_GH_BINARY" --native-gh-sha256 "$GH_BINARY_SHA256" --native-gh-version 2.102.0 \
  --native-workflow-commit "$RELEASE_COMMIT" --native-run-id "$QUALIFICATION_RUN_ID" \
  --native-run-attempt "$QUALIFICATION_RUN_ATTEMPT" --native-verification-work "$FRESH_PRIVATE_WORK"
```

The aggregate itself calls the pinned verifier after source/image/ZIP/report binding. It never accepts a caller's success receipt. Fixed offline verification uses the supplied bundle/root, exact repository and main workflow identity, source/signer digests and hosted-runner policy. Only successful cryptographic output supplies certificate identities, the single report subject and observer timestamp. Numeric repository/owner identifiers also bind the origin. Private copies, originals and report are rehashed afterward; raw certificates, predicates, CLI output and local paths stay outside the public result. The explicitly selected workflow identity is `.github/workflows/macos-qualification.yml@refs/heads/main`; historical Apple signing remains separate.

A verified origin still leaves `native_job_conclusion:not-service-reconciled`, installation unperformed, public availability unverified and every readiness flag false. A certificate supplies no macOS or job-success assertion: review the exact workflow source and reconcile its successful native completion separately. Offline roots cannot reveal later revocation/rotation; choose fresh roots through the approved trust process. No approved verifier/root/bundle or real cryptographic execution has been established by the unsigned fixture tests.

See [offline artifact verification](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline), [the CLI verification contract](https://cli.github.com/manual/gh_attestation_verify) and [the pinned CLI security release](https://github.com/cli/cli/releases/tag/v2.102.0). The initial prepared tests cover exact policy, rejected forged/malformed/stale claims, private immutable copies, subprocess interruption/output/time limits and the actual aggregate caller. Their combined execution remains pending.

## Dormant native Mac qualification producer (unreleased)

The prepared macos-qualification.yml accepts one reviewed stable main commit, a successful first-attempt official main run and immutable artifact ID, independently selected outer/ZIP/DMG hashes and exact revisioned filenames, and a public Developer ID Team identifier. It is disabled unless PEERBRUSH_NATIVE_QUALIFICATION_ENABLED is explicitly enabled. Activation and dispatch require approval; preparing the workflow does not enable it.

Both jobs run a fixed isolated inline guard before checkout: input/source/workflow SHA must agree with the exact main invocation. Repository scripts cannot select executable source. The read-only gate job tests that source without attestation authority. Only the dependent hosted macOS ARM64 qualify job requests OIDC/attestation authority. It acquires exactly the two selected signed files, independently qualifies the final mounted DMG, then attests only its generated bounded report. The qualification and Sigstore bundle are the only uploaded evidence. No app execution, installation, signing, notarization or release publication occurs.

Artifact acquisition checks official public repository/run/source metadata and a pinned immutable artifact digest. One bounded storage redirect drops authorization; exact two-entry extraction rejects links, traversal, extra files and mismatched inner hashes. Raw tokens, signed locations and tool output stay outside receipts. Actual native qualification and cryptographic verification still require real selected inputs.

The existing Apple signing workflow retains its historical 0.26 source pin. A separately reviewed and approved signing stage must first supply the final ZIP and DMG for the same new stable main commit in one flat two-file artifact. A ZIP-only artifact or a directory containing private signer receipts is insufficient. Do not change that historical pin or enable qualification to fabricate current inputs. No trust roots, signer keys or remote permissions have been configured by this preparation.

Ten prepared offline methods exercise actual inline invocation guards, exact metadata, extraction/immutability, bounded transport and token separation. Their execution and real native/attestation acceptance remain pending. All aggregate installation/public/production readiness gates remain false.
