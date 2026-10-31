# Production candidate and Store reconciliation — unreleased

After promotion, this PB-154 change prepares tested, exact-source release candidates on a push to `main`, using the existing three-platform release workflow. A manual run still requires an exact source SHA matching its workflow ref. Every native job runs formatting, Rust tests, Clippy and packaging regressions before building. Inspection requires Windows x64, macOS arm64 and Linux x64 archives with identical corresponding source, version and executable source commit. These are Actions candidates; this workflow does not publish releases, deploy the website or submit to a store. Execution of the revised main trigger remains to be verified.

The final `production-readiness.json` report reopens and validates the real archives against committed source, checks the final inventory and `SHA256SUMS`, and rejects old-binary relabelling or packaging corrections. Each platform reports `candidate_verified`, native signing evidence, `ready: false` and `public_availability: unverified`. The existing archive inspector checks signing metadata and hashes; that alone cannot establish native signature or installation acceptance. The report deliberately has no path to `production_ready: true` until those missing verification and protected publication adapters exist. Sigstore signing remains an explicit manual candidate option; it does not replace native signing/notarization.

The informational `publication_phases` field partitions the actual assessed blockers without changing them. `prepublication_blockers` retains native/installed verification, protected approval, source/version and immutability blockers; unknown future blockers conservatively remain here. `postpublication_checks` contains only the existing `published-assets-and-website-readback-not-configured` item when present. Actual public asset and deployed-site readback follow publication, so a future executor must distinguish them from the conditions required before its first write. Both lists together preserve every current blocker. The original flat list, all readiness fields and `--require-ready` remain unchanged. This field is not an eligibility Boolean, approval, native evidence or permission to publish, and supplied JSON cannot waive the in-process inspection.

The same partition is included in common four/five/six-record assessments, generated DEB/DMG handoff reports and standalone DEB qualification. It does not enter the strict public asset inventory, Store auxiliary inventory, native qualification/attestation or website schemas. Existing immutable report files cannot be rewritten to add the field; regenerate into a fresh output when needed. No executor, durable hosted publication-state adapter, configured protected approval or installed-acceptance authority is supplied by this reporting change. Current final release signing/installation, Store access/state/public availability, website eligibility/deployment and update-delivery gates remain open; see the channel contracts below.

`--previous-inventory` compares against a retained publication inventory: an already published version must keep its exact source SHA and every existing asset filename/hash. Without that reconciled input, version immutability is explicitly unverified. `--require-ready` exits unsuccessfully while production gates remain blocked. Existing output reports cannot be replaced with a different decision. No tag, release file or history entry is overwritten.

Pending gates are installed Windows distribution/signing, signed and notarized Mac DMG installation, final installed Ubuntu qualification and authenticated update delivery, protected publication approval, and public asset/website readback. The existing reviewed Apple signing workflow retains its source pin and protected environment; this change neither repoints nor activates it. Windows Store automation retains its existing disabled variable. No credential, environment or branch-protection setting changes are part of this implementation.

The Ubuntu native job additionally qualifies final DEB bytes and consumes its freshly produced private installation receipt before uploading the candidate. This uses the selected portable/source inputs rather than trusting the package's report. Its separate public qualification report retains false readiness and explicit desktop, real-version migration, interrupted-update, signing, repository and publication limits. Receipt matching is a binding check; native execution provenance comes from the immediately preceding successful test in the same trusted job.

The aggregate job downloads that DEB candidate from the same successful workflow, selects its actual SHA-256 independently of reports, and reopens it against the exact verified Linux portable, source and public maintainer. `production_readiness.py --directory VERIFIED_PORTABLE --include-deb SELECTED_DEB --deb-sha256 SELECTED_DEB_SHA256 --portable-sha256 SELECTED_LINUX_PORTABLE_SHA256 --maintainer 'PUBLIC NAME <PUBLIC EMAIL>' --commit SOURCE_SHA --version VERSION --output NEW_CANDIDATE_DIRECTORY` creates a fresh, nonoverlapping output directory. It preserves the four verified portable/source inventory records and appends one inspected DEB record and its matching `SHA256SUMS` line. Existing candidates are never overwritten; interrupted copies remain for inspection and require a new output directory.

Only those five files and regenerated inventory, checksums and readiness report enter the aggregate. Builder sidecars, qualification reports, unrelated files and private installation receipts stay behind. The DEB's producer sidecars remain in its separate native Actions artifact; this bounded handoff does not supply the website's final-installer/sidecar adapter. The report keeps portable Linux status separate from installed-candidate content verification. Matching metadata does not authenticate native execution: same-workflow job provenance is required separately, and installed acceptance, native signatures, protected approval, update ownership and public availability remain unverified. Four-record inspection remains supported; inspecting a five-record directory requires the explicit DEB/portable hashes and maintainer again. This handoff does not include final DMG or MSIX records and cannot enable publication.

## Private Store submission state

The separate Store-only publisher handoff uses a non-resumable hosted contract:
one first-attempt protected invocation may submit only after its own fresh
confirmed publication and exact public-byte readback. Existing publications and
reruns never regenerate that callback, even if receipts are missing or expired.
Partner Center product/baseline/pending ownership is checked before every write.
Local write-ahead records and sanitized recovery artifacts are diagnostic
evidence; a lost response or runner is a reconciliation hold. This route needs
no separate hosting purchase and remains literally disabled and unqualified.
See [Store automation](store-automation.md) for bounds and owner prerequisites.

The filesystem requirements below apply to the separate, explicitly invoked
operator `submit_store_release.py --submit --state` route and its read-only
reconciler. They are not a Microsoft API hosting requirement.

Store submission remains explicitly disabled while integration is prepared. No persistent private host or storage backend is configured. Temporary runner disks, caches and downloadable artifacts cannot substitute for a durable write-ahead journal. Activating service writes requires a reviewed durable deployment and separate explicit approval.

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

The hosted Store workflow remains disabled. Its fixed isolated first step accepts only a first-attempt manual invocation of the official main Store workflow on a hosted Linux x64 runner. An independently repeated `automation_commit` must equal the event and workflow SHA before checkout uses `github.sha`; the existing release gate then verifies that checkout. The independent release `source_commit` remains separate input data, may differ from the automation SHA, and never selects executable automation. Both selected asset hashes and the release SHA are validated before checkout. Release-event and unknown reusable callers remain blocked until an actual reviewed publisher supplies a complete trust contract.

Hosted `submit=true` is refused before checkout and any credential-bearing step, even if the existing enable variable is set. The final `submit=false` preflight retains the existing Partner Center credentials only for authenticated product/pending/version GETs after exact published-asset validation; no hosted step forwards `--submit` or guesses a state path. This is a guarded source-selection boundary, not an implemented publisher or state host. The unchanged local CLI's durable filesystem journal is already the writer interface; its deployment needs a maintainer-selected persistent private host with accepted filesystem/lock semantics, or an explicitly selected remote backend that durably commits every mutation intent before the corresponding service write. No such host or backend is supplied or configured here. Runner temp files, end-of-job uploads, public Actions artifacts and caches are not durable state. Actual host/access decisions, any required persistence adapter, provider/installed/public acceptance and explicit activation approval remain open.

## Website boundary and verification

The reviewed public `site-content` branch contains `public-site/downloads.json`, version history and baked home-page labels. The [public website-output adapter](website-release-output.md) prepares a new immutable external overlay only after actual public release/source/checksum verification. It preserves history and updates home/download/history labels/cache tokens together. Current committed distribution policy selects Store/DMG/DEB; missing native final-installer and Store public-readback adapters remain explicit exclusions, with a blocked report instead of portable fallback. The tool does not deploy or modify site-content. Website changes retain the documented `maintenance/site-*` review and approval boundary.

Offline checks are `scripts/test_store_release_state.py`, `scripts/test_production_readiness.py`, `scripts/test_store_workflow.py` and the existing Store/package tests. The workflow tests execute its actual inline guard and check checkout/credential order; they do not dispatch it. Synthetic archives are structurally inspected; fake service responses exercise status/error boundaries without authentication, real signing, uploading or publishing. Native platform signing/installation, actual Partner Center access, crash/restart on hosted runners and full main-to-public acceptance remain pending.


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

## Optional exact-attempt native completion readback (unreleased)

Keep verification offline by default. Adding the explicit option below to the complete native attestation selection enables two bounded, unauthenticated read-only requests to the official public API, only after this invocation cryptographically authenticates the exact report/source/run:

    --native-job-readback

The selected producer is dispatch-only and first-attempt only. The consumer reads that authenticated run's exact attempt and its jobs, rather than a latest status. Official public repository/source identities, successful run and both named gate/qualify jobs, hosted-runner labels and the ordered successful qualifier steps must agree. Qualify and Attest must be adjacent; later evidence retention and upload must also succeed. No caller success receipt, alternate endpoint, token, log retrieval, dispatch or service mutation is accepted. Inputs are rehashed after readback.

Success records only service-reconciled native job completion. It does not establish installation, public availability or production readiness; all those gates remain closed. Source review of the pinned workflow and its pre-checkout guards remains necessary because API labels and a certificate alone cannot prove what code executed. Offline verification remains available when completion readback is not requested. Eleven prepared fake-GET methods cover exact-attempt failure boundaries and the aggregate caller; actual provider/cryptographic/native execution remains unverified.

## Exact release invocation before executable checkout (unreleased)

Every candidate build/inspection job now runs fixed isolated inline validation before checkout. The optional Sigstore job repeats it before downloading artifacts or signing. Independently selected full source SHA must equal the event SHA and workflow-file SHA in the official repository, with the exact release workflow/ref identity. Checkouts select github.sha only after that guard. Repository release_gate.py still checks exact source plus manifest/lock versions afterward; it cannot grant authority to different code checked out before validation.

Main pushes remain main-only. Manual candidates from the same selected branch or tag remain supported; foreign source/workflow/repository, pull-request events and mismatched refs reject before executable source or artifacts are consumed. Signing stays opt-in and no release publication, dispatch, protected setting or credential change occurs. Three offline methods execute the actual inline guards and verify their ordering. Combined replay remains pending.


## Store MSIX auxiliary handoff (unreleased)

The optional release-candidate Store build now has a separate content handoff.
After the common candidate is inspected, the inspect job downloads the
same-workflow unsigned Store artifact and independently reopens its MSIX,
producer JSON and checksum sidecar against the selected Windows portable,
committed source and canonical Store identity. Explicit inventory, portable,
MSIX and report hashes are required. Existing strict four/five/six-record
common inventories and their SHA256SUMS remain unchanged.

handoff_store_msix.py creates a fresh private directory outside Git containing
exactly the MSIX, original producer JSON, a canonical checksum sidecar and
store-candidate-inventory.json. Its manifest binds the common inventory and
corresponding-source hash, package version and operator-selected previous Store
version. Unknown files, input/source changes, partial copies, links/reparse
points and existing outputs reject acceptance; failed partial output is retained.
An optional independently hashed previous auxiliary inventory forbids same-version
replacement or downgrade and prevents lowering the retained Store baseline.
This is local supplied-inventory comparison, not service reconciliation.

The application-to-Store mapping remains (major+1).minor.patch.0. The standalone
MSIX builder still supports an explicit version, but this handoff and Store
submission only support the canonical source mapping. The release workflow
rejects an explicit differing mapping before packaging. A different approved
mapping needs a shared reviewed source/version contract across these consumers.

The verified-store-msix-content artifact is separate from verified-release-candidate.
It does not enter the common checksum/site inventory or existing Sigstore signing
job. Content binding grants no native SDK producer authentication, installation,
Store certification/submission, public availability or production readiness.
All those authority/readiness fields remain explicitly unverified or false.
No workflow dispatch, Store authentication, publication or signing configuration
is enabled by this change.


## Current-source Mac signing producer (unreleased, dormant)

`macos-release-signing.yml` prepares the missing signed ZIP/final DMG pair for the
existing native qualification workflow. It neither replaces nor changes the
historical 0.26 Apple workflow. Both new jobs require an independently repeated
full source SHA equal to the official main event and workflow SHA, a first
manual attempt, hosted macOS ARM64 and the missing-by-default
`PEERBRUSH_APPLE_RELEASE_SIGNING_ENABLED` variable. Neither approval settings nor
this variable are configured by the source change. Do not enable or dispatch it
without explicit approval and verification of the existing `apple-signing`
environment's required-reviewer, ref, credential and concurrency protections.

The credential-free job requires a clean stable Cargo/lock version, current
native tools, formatting, engine/codec/protocol tests and Clippy before building
and inspecting its exact unsigned Mac ZIP. The dependent protected job verifies
the selected same-workflow ZIP hash before importing credentials. It reuses the
reviewed temporary-keychain setup with only its lifetime extended from 7200 to
10800 seconds for the 150-minute two-format job; the historical workflow is
unchanged. Each signer retains its existing bounded upload/wait operations.
It uses the existing `sign_macos.py` ZIP then DMG
paths, with separate original notarization journals and explicit format/revision
arguments. No key, account, certificate, permission or protected environment is
created. This is a Mac producer; three-platform release acceptance remains a
separate production prerequisite.

Only two exact hash-read-back files enter `macos-signed-pair-RUN_ID-1`: the
revisioned `-apple-signed-rN.zip` and its `-dmg-signed-rN.dmg`. Checksums and signer
receipts are not extra members of this pair artifact. The upload's public
artifact ID/digest and both inner digests are recorded for independent selection;
the consumer still re-downloads and hashes the artifact and both files. Its
strict run metadata now requires this producer's workflow path (the documented
bare or `@main` representation), manual event and exact pair artifact name,
alongside the existing official source/repository, successful first attempt and
immutable ID/digest checks. Other workflows and recovery artifacts cannot stand
in for the pair.

Workflow reruns are refused and neither signer retries a notarization upload.
On failure, a separate bounded credentials-free recovery tar preserves only
the journal-selected public input, state and submitted payload; ZIP executable
modes are retained. DMG finalization/mount directories are never traversed or
recursively removed. An incomplete tar is never published as a recovery input.
The recovery artifact has seven-day retention and is **not durable guaranteed
storage**: cancellation, runner failure, upload failure or the recovery budget
can prevent it. Reconcile Apple history and preserve the exact work before any
new submission; use the existing local `--resume` path with the original source,
input, identity and revision only after reconciliation. This workflow does not
download or restore a recovery archive automatically. Activation must include an
approved operational recovery procedure; a missing recovery artifact is a
blocker, not permission to start a new submission.

The producer does not dispatch qualification, authenticate its own receipts,
attest native acceptance, install the application, publish assets, submit to a
store, deploy the website or configure an updater. Stable source, approved
provider access/protection and actual signing, qualification, installed/update
and public readback acceptance remain required. Its offline workflow fixtures
are code tests, not real Apple issuance or release evidence.

## Exact reviewed website invocation (unreleased)

The manually dispatched Pages workflow now checks its fixed official main
invocation before executable checkout or any Pages action. Event/source and
workflow SHA must agree; the exact pages workflow/ref and a full independently
reviewed site SHA are required. The main workflow source and site-content source
are intentionally separate commits. Checkout selects the explicit site SHA,
then fetches only refs/heads/site-content and requires its current tip and the
checkout to equal the selected SHA. A moved tip or missing required public
files stops before configuration or artifact upload. The deploy job repeats
the fixed invocation check before its privileged Pages action.

The existing manual trigger, contents-read scope, Pages/OIDC deployment scope,
github-pages protected environment and serialized deployment remain unchanged.
No protection, credential, dispatch, branch or published content is changed by
preparing this boundary. It does not implement release publication or website
eligibility for installed packages. The existing release-source-guard test
entry point and all its current native/release/signing callers also cover the
actual website guard/selection programs offline; they exercise accepted and
refused invocations, exact checkout/current-tip binding, failed fetch and
missing output. These prepared cases require combined execution; they do not
deploy Pages or authenticate a release.

## Explicit signed Mac candidate assembly (unreleased, verification pending)

The current main candidate job still produces an unsigned Mac ZIP. A separately
approved signing/qualification run is not automatically dispatched or consumed
by release.yml. A new explicit local handoff closes the candidate assembly gap:
add --signed-macos-archive SELECTED_SIGNED_ZIP to the existing --include-dmg
command, with its independently selected --macos-portable-sha256 and all existing
DMG/source/DEB selections. The five-record seed must select an unsigned Mac ZIP.
Only that Mac record is replaced in a fresh external six-record candidate;
Windows, Linux, corresponding source and DEB records/bytes stay unchanged. Both
the seed and signed archive are independently inspected against the exact same
reviewed source/version and corresponding source ZIP. Published same-version
replacement/addition and downgrades remain refused; existing outputs are never
overwritten.

This explicit replacement additionally requires every existing native attestation
input and --native-job-readback. The aggregate itself invokes the pinned offline
CLI/root/bundle verifier and then its bounded exact-attempt successful native job
reader. A caller's success JSON or unsigned native report cannot authorize the
replacement. The consumer currently requires Linux x64; no verifier, root,
artifact, credential or signing service is installed or configured. Failure
retains private partial output without a success readiness report.

After successful authentication the new mode adds exactly two public companion
files: SELECTED_DMG.qualification.json, containing the unchanged strict bounded
PeerBrushDmgQualificationV1 report, and SELECTED_DMG.sha256, containing the
canonical image digest/name line. The qualification is correctly typed and is
never renamed to a signer verification receipt. These companions are outside
the unchanged six-asset common inventory; the checksum list still covers those
six assets. Private signing journals, authentication output, trust roots,
verifier executables, attestation bundles and operational paths are not copied.
Original/copied candidates, qualification and selected trust inputs are rehashed
after verification and companion creation before the final report.

Overall candidate_verified, ready and production_ready remain false; installed
acceptance and public availability remain unverified. The website can bind this
typed public report to actual public ZIP/image/source bytes while retaining its
installed-download exclusion. Actual release workflow orchestration, installed
and interruption acceptance, protected publication, public asset reconciliation
and native website eligibility remain unfinished. Offline fixture tests mock
only cryptographic/provider boundaries and do not establish real native trust.
