# Website release output preparation â€” unreleased

`scripts/prepare_site_release.py` prepares a reviewable overlay for the existing public `site-content` output. It does not publish a GitHub release, modify a branch, submit to a store, deploy Pages, configure credentials or change environment protections. Its public network operations are unauthenticated GETs. The private website builder remains separate.

The explicit inputs are a canonical stable `vA.B.C` tag, full source SHA, the verified final `release-inventory.json`, an exact reviewed site-content commit, and reviewed public notes/caution JSON. The selected source checkout must match the release SHA and Cargo version. The tool resolves the public tag to that SHA, rejects draft/prerelease/unpublished releases, and accepts only returned GitHub asset URLs with matching names, uploaded state and bounded lengths. It streams public packages/source to temporary files in at most 64 KiB chunks, using a 120-second monotonic deadline checked between blocking network operations and a 15-second per-socket timeout. Only small JSON/checksum reports (at most 2 MiB) are retained in memory. HTTP header/open and chunk-header parsing remain blocking library operations: the between-operation deadline is not a hard wall-clock cancellation guarantee for a peer that dribbles headers. It hashes the actual public bytes, checks any API-provided SHA-256 digest, requires matching published `SHA256SUMS`, compares every corresponding-source file with the committed package source, and reuses the archive inspector. That existing inspector may buffer archive/executable contents during structural validation; this change bounds the transfer layer, not its internal memory use. No downloaded code executes. Only HTTPS GitHub release assets and their bounded redirects to GitHub's explicit asset hosts are accepted; redirect URLs are never logged. Missing or interrupted public downloads cannot become selectable links.

The final inventory is schema1, pinned to one version/source SHA without packaging correction or older-binary relabelling. Existing portable ZIP records retain the exact archive inspector schema. A final installed record additionally uses the common `platform`, `architecture`, `file`, `bytes`, `sha256`, `binary_source_commit` and `source_sha256` fields. The source record contains only `file`, `bytes` and `sha256`. Unknown inventory record fields are rejected; provider/private receipts do not belong in this public metadata contract. The source record is the existing matching source ZIP. Asset names are strictly recognized:

- Portable: `PeerBrush-A.B.C-Windows-x64.zip`, `PeerBrush-A.B.C-Linux-x64.zip`, and `PeerBrush-A.B.C-macOS-arm64[-apple-signed-rN].zip`.
- Final macOS image: `PeerBrush-A.B.C-macOS-arm64-apple-signed-rN-dmg-signed-rN.dmg`, plus its `.verification.json` and `.sha256` assets. The two revision numbers are independently selected by the existing signer.
- Ubuntu 24.04 amd64: `peerbrush_A.B.C-1_amd64.deb`, plus its `.json` and `.sha256` assets. Debian revision `-1` comes from the existing DEB packager, not the Cargo version.

The image and DEB paths bind the current candidate receipts and remain excluded from selectable downloads. Final DEB content is additionally inspected through the existing final-byte validator when an approved public `--maintainer` is supplied. The actual package, matching public Linux portable ZIP, corresponding source, exact version, control fields, file contents/modes and root ownership must agree with the selected inventory; a sidecar cannot substitute for those bytes. Missing approved maintainer or missing public companion ZIP leaves `content_verified:false` with an explicit exclusion. A successful content check still records `ready:false`, `production_ready:false`, unverified installed/public availability and no configured update repository. It never adds a DEB download row or infers signing/installation authority. No production contact has been selected by this preparation. The Mac path retains its native-image verification exclusion. Existing aggregate final-installer handoffs do not grant website eligibility.

Distribution selection comes from `packaging/release-policy.json` **inside the exact release source**, rather than a version-number cutoff. Current policy selects Microsoft Store, macOS DMG and Linux DEB. Consequently current portable-only candidates produce a blocked report, not a quiet portable fallback. A historical/transitional release can explicitly select ZIPs in its own reviewed source policy; the adapter does not synthesize policy for older tags lacking it. Windows Store submission/certification/service Published is never an availability input. There is no Store public-package readback adapter or Store download button in this slice, so that channel remains excluded and unverified.

For eligible published portable records, availability, package structure and native trust are separate. The website description says Portable ZIP and either unsigned or native trust unverified; a signing manifest does not become cryptographic verification. Existing published history and its recorded limitations remain intact.

```sh
# Run from the exact selected release checkout. Notes contain only public prose:
# {"notes":["Reviewed release changes."],"caution":"Recorded limitations."}
python scripts/prepare_site_release.py \
  --tag vA.B.C --commit FULL_SOURCE_SHA --site-commit REVIEWED_SITE_CONTENT_SHA \
  --inventory /verified/release-inventory.json --notes /reviewed/public-notes.json \
  --output /outside-all-checkouts/new-website-candidate
```

For an inventory containing a final DEB, include `--maintainer 'PUBLIC_MAINTAINER_NAME <PUBLIC_MAINTAINER_EMAIL>'` using the independently approved public metadata expected by the package builder and production-readiness tools. This is required verification authority, not an installation or publication switch. The validator uses the existing exact maintainer syntax/control checks; it never reads the DEB's self-declared contact back as approval or invents a production contact.

Output must be a new path under an existing parent directory outside every registered source worktree, without symlinks/junctions/reparse points. Existing outputs are never replaced. Paths are rechecked after verification immediately before creation. The new directory is 0700 with current-user ownership checked on POSIX; Windows inherits its parent ACL, so choose a private parent directory there. Verification happens before output creation. When no platform is eligible, exit status 2 and `website-output.json` explain exclusions, with no public-site overlay. Otherwise the overlay contains exactly seven files: downloads JSON, home HTML/JS, shared platform/history JS, and full-history HTML/JS. The candidate report binds the source/site commits, output file hashes and canonical public `release_inventory_sha256`, retains unverified Store availability, and explicitly records no deployment and no production-readiness assessment. The inventory digest covers sorted-key compact JSON containing only the public source/asset metadata. Keep operational candidate output outside public source; do not commit the report.

New entries pin the canonical public inventory digest as `releaseInventorySha256`, both in history and, when selected, in the top-level latest fields. Reconciliation may add a delayed platform only from that unchanged inventory after repeating public byte/source/checksum verification for every selectable platform. Notes, dates, source, caution, inventory digest, checksum/source URLs and every existing platform row must remain identical. Removing or replacing a row fails. Existing entries without an inventory pin remain immutable; this tool cannot retroactively assign them a guessed digest. Adding assets absent from the original inventory requires a separately reviewed versioned manifest/checksum contract, not replacement of published files. Store and installed-package eligibility remain blocked by their separate adapters.

An identical selected latest entry is idempotent. shortcut: availability amendments cover the current latest release only; historical amendments need separate exact-source validation of both the amended release and retained latest before they may generate output. This prevents an older target from demoting latest or carrying forward unavailable latest downloads without rechecking them. Existing top-level latest fields must match a retained history entry; new-release downgrades relative to any retained stable release and replacement/removal of immutable assets fail. The renderer updates the schema2 data together with baked home platform rows, checksums, label, caution, release link, fallback label and versioned documentation links. Release script/feed cache URLs include a deterministic numeric `layout` token derived from the complete downloads JSON, so a delayed platform changes the token without inventing a new application version. Every expected cache/doc replacement count is checked. Unexpected HTML/JS layout fails for review instead of silently preparing a partial update. It preserves the current GitHub-only rendering contract and missing-platform behavior; it does not invent Store support.

Review and apply only the seven-file overlay on a documented `maintenance/site-*` branch based on the selected site-content commit, preserving every other site file/asset. The existing protected, manually dispatched Pages workflow remains unchanged. After separately authorized deployment, the [read-only deployed-site reader](website-readback.md) can compare the expected seven files and actual cache URLs and independently reverify public downloads. The future publisher must attach public `release-inventory.json` to that release, matching the prepared inventory digest; this is not an available 0.30 asset today. Older prepared reports without that binding must be regenerated, never assigned a guessed digest. This command does not claim that live readback or main-to-public production publication is complete.

Tests use synthetic committed packages, mocked public HTTP responses and the seven public site output files from `edab369181b8ed43874e82a5cd705c94146dc5eb`. They exercise unavailable assets, public byte/checksum/source tampering, URL/redirect limits, receipt rejection, policy without portable fallback, immutable output/history, delayed latest availability, historical-target rejection, pinned-inventory rejection, cache renewal, idempotence and complete static/dynamic display updates. Focused tests also inspect actual synthetic ar/xz/tar DEB bytes, preserve installed-policy exclusions and reject altered payload/source/control/version, missing authority/companion assets and forged readiness. No live new-release download, native installation/signing or deployed website readback is performed by these tests.

The public release/asset fields follow the [GitHub Releases API](https://docs.github.com/en/rest/releases/releases) and [release assets API](https://docs.github.com/en/rest/releases/assets). See [production automation](production-automation.md) and the [release maintenance playbook](release-maintenance.md) for the remaining publication and approval gates.

## Typed final-DMG qualification companion (unreleased)

The explicit signed Mac candidate handoff now supplies the actual selected
strict public qualification as FINAL_DMG.qualification.json, plus the existing
canonical FINAL_DMG.sha256 digest/name companion. This is a qualification report,
not a signer verification receipt. It contains only the existing bounded public
qualification schema; private signing journals, CLI/root/bundle inputs and raw
provider responses are excluded. These two companion assets are separate from
the common six-record public package/source inventory and its checksum list.
The future publisher must explicitly retain them with the exact selected DMG.
No publisher or deployment is activated by preparing this contract.

When that typed qualification asset is present, website preparation downloads
and structurally inspects the actual signed Mac ZIP before processing the DMG.
It then reuses the final-DMG binding validator on the actual public image/ZIP,
source, selected hashes and strict report. Missing companion ZIPs remain
explicit exclusions. A malformed, forged, changed or overclaiming qualification
fails, and simultaneous typed qualification and legacy signer-verification
assets are refused as ambiguous. A malformed typed report never falls back to
the legacy receipt path. Existing releases that supply only the legacy signer
sidecar retain their previous recognition and exclusion behavior.

Successful matching records asset_binding_verified:true only. The website does
not authenticate the native producer, repeat Apple checks or establish image
contents from an unauthenticated report; content_verified:false, ready:false,
production_ready:false and unverified installed/public availability remain.
No DMG download row is created. Real native authentication is required by the
candidate replacement command itself, and separate installed acceptance and
website eligibility work remain necessary. The shared published reader also
serves deployed readback; this public binding is not a deployment or readiness
authority. Prepared fake-HTTP/native-command tests require combined execution.
