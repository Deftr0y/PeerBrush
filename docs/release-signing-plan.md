# PeerBrush repository privacy and release signing plan

Prepare public source and release packages for Windows signing, macOS signing and notarization, and verified Linux distribution. Preserve GPL v3, complete corresponding source, bundled artwork and fonts, and the Ember palette. Follow [release maintenance](release-maintenance.md) for publication and privacy checks.

Status: corrected v0.2 archives retain original executable bytes. Dated historical rebuilds for development-d70f64e, v0.1.2 and v0.1.4 are published with unchanged application inputs, new executable hashes, matching GPL source and verified website history links. Website source and operational records are maintained separately. The scoped historical audit and targeted artifact correction are complete; original Git history/tags and their automatic source archives are unchanged. Those historical signing deferrals are not the current Mac status: 0.26.0 Mac arm64 is Developer ID signed, notarized and stapled, distributed as a ZIP. Verified Mac/Linux downloads and the website are live; Windows Store 0.26 remains pending, with submission/certification unverified. See [v0.2 correction facts](release-0.2.md) and [historical correction facts](release-packaging-corrections.md).

## High-priority installation and production foundations

Proposed for v0.3 implementation, following the first measurable PB-108 fix: settings/migration, installed distributions and associations, safe updates, then production publishing/Store reconciliation. This explicitly extends the earlier release-unassigned planning scope; it does not establish release readiness. PB-145, PB-146 and PB-147 are expanded existing requests, not new parallel implementations. PB-155 adds persistent settings/migration acceptance; PB-154 owns the end-to-end publishing work. [The backlog](../FOLLOWUPS.MD#high-priority-foundations--installation-updates-and-production-publishing) is authoritative for item status and acceptance. Responsiveness PB-108 remains priority #1.

1. **Installed distribution (PB-145/PB-146/PB-155):** make installed software the default. Provide both Microsoft Store distribution and a direct signed Windows installer, and complete their runtime/install/upgrade/uninstall checks; provide a signed/notarized macOS DMG with PeerBrush.app dragged to Applications. Decide the supported Linux installation format and its update owner before implementation. Register PSD/PSB Open with handlers and icons; the user's OS-supported consent chooses defaults, and upgrades must not reset them. Persist per-user settings outside the installation/package, migrate portable settings with validation/backup, and protect artwork and stable MCP launch paths.
2. **Update experience (PB-147):** record Store/package-managed versus direct-distribution ownership and prevent competing updaters. Specify automatic-check preferences, manual check, release/version/channel information, notifications, progress and restart-later behavior. Unsaved projects and active writes block disruptive replacement/restart. Validate authenticated metadata, trusted signatures and final integrity; define offline/interrupted/error recovery, bounded retries and last-working-installation rollback where supported, including settings compatibility. Acceptance requires native success/failure and unsaved-work tests for each selected channel.
3. **Trusted main-to-production pipeline (PB-154):** after a main push, require tests, exact source/version validation and protected signing/notarization approval before publishing. Keep one immutable source SHA per version, final installers, matching GPLv3 source/third-party notices and checksums computed after signing/stapling. Append website history and update latest/download links only from verified public assets; show per-platform availability and pending channels accurately. Model Windows Store submission, certification delay/rejection, publication and verified public availability as separate states, with reconciliation, idempotent bounded retries and clear status reporting. Keep the artifact/channel contract extensible to future stores without claiming those integrations exist.

Completion is demonstrated by an installed-platform acceptance matrix and one exact-source release progressing from main to verified public downloads/website and independently reported Store state. A delayed/rejected Store channel must not be labelled available or cause a duplicate submission; another ready platform may be published with its status stated explicitly. Preserve branch policies, protected signing approval, GPLv3/source availability and existing version history. Credential values, private operations and approval changes do not belong in public planning documents or this work's implicit authority.

Current boundary: CI and manual release/signing/Pages workflows exist; end-to-end main-push publication and active Store submission automation are not established. Store integration is prepared with submission explicitly disabled; no durable private host or storage backend is configured. The historical checklist below retains earlier completed work and unresolved platform gates; it is not evidence that automation or native acceptance is complete.

## 1 Make packaging include only approved content

`scripts/package.py` uses the explicit file list in `scripts/package-files.json`, fresh temporary staging and committed-source export for clean release inputs. See [release archive contents](packaging.md). The v0.2 correction and dated historical corrections are inspected and published. Original tag history is unchanged.

- [x] Replace broad working-tree copying with reviewed manifests for application packages and corresponding source. Derive release inputs from the exact selected Git commit and reject unexpected inputs.
- [x] Build each package in a fresh staging directory. Reject paths or symbolic links that escape the permitted source tree.
- [x] Include all source, scripts and integrations required to build and run the distributed application, with GPL v3 and dependency/font notices. Review the existing integration directory omission when defining the source manifest.
- [x] Add meaningful packaging checks using synthetic private files and stale output files: they must never enter source archives or binary packages. Verify expected source assets remain included.
- [x] Inspect local QA archive entry lists and extracted contents, including the nested source ZIP.
- [x] Reject recognized credentials and personal build paths in executable inputs; configure Rust path remapping in CI.
- [x] Rebuild historical correction binaries, rerun engine/codec/protocol tests and inspect the final archives before publication. A new signed release still requires its own candidate validation.
- [x] Inspect corrected v0.2 and historical downloads and verify the deployed website output. Historical Git/tag source remains separately scoped.

New-release completion requires production builds from the final reviewed commit; an explicit packaging-only correction can retain original executable bytes after verifying identical application/build inputs: packages contain the intended source and assets, exclude synthetic private material and stale files, and pass inspection of the actual archives.

## 2 Inventory public and private material

- [x] Inventory tracked, ignored and untracked files, reachable Git history, release assets, Actions artifacts and logs, and website output. Record which remote refs and historical artifacts were actually reviewed.
- [x] Classify material by purpose: application source and public contributor documentation; private operations and account records; credentials and signing keys; disposable build, QA and recovery files.
- [x] Scan for credentials and private information without printing values. Review examples, screenshots and videos for connection tokens, personal paths and account information as well as text files.
- [x] Review deployment metadata, outreach material and operational notes individually. A filename or project identifier alone does not establish that a file contains a secret.
- [ ] If credentials have been exposed, revoke or rotate them first. Prepare a separate history and artifact remediation proposal that accounts for forks, clones, caches and existing downloads. Do not rewrite shared history as routine cleanup.

Completion: a private inventory of findings and reviewed surfaces, with a concrete list of files to keep public, relocate, redact or exclude. Do not publish raw audit reports.

## 3 Establish separate storage and access

Keep the application in one public repository. Store private operations outside that checkout; a directory or branch within a public repository is not an access boundary.

| Material | Destination |
| --- | --- |
| Application source, tests, assets, licenses, build scripts, signing workflow definitions and contributor documentation | Public repository |
| Operational notes, account setup records, publication receipts and private audit findings | Separate private workspace; optional private operations repository for records suitable for version control |
| Passwords, API tokens, signing key backups and recovery codes | Password manager, secure key storage or provider-managed hardware; never ordinary Git files |
| Credentials required by release jobs | Protected GitHub environment secrets; short-lived OIDC authentication where supported |
| Build output, generated QA captures, caches and application runtime connection data | Dedicated local or temporary directories excluded from source and packaging |

- [x] Choose the private storage location and access policy before relocating material. Preserve originals until the copy is verified.
- [x] Move or redact classified private files, then review the staged diff. Ignore rules do not remove already tracked files or historic copies.
- [x] Expand ignore rules for local secrets, private configuration, signing keys, QA and recovery material. Keep sanitized examples and required source assets available to contributors.
- [x] Keep private infrastructure details out of public instructions. Public documentation should describe required configuration using placeholders.

Completion: a public checkout containing contributor-facing material, separate private storage, and an explicit history of any remaining exposure to remediate. Credentials need not be in a private repository either.

## 4 Prepare trusted release automation

The existing `.github/workflows/ci.yml` tests, builds and inspects Actions artifacts on pushes and pull requests. The separate release-candidate workflow enforces an exact commit and can sign archives with Sigstore; protected manual Apple signing/notarization is configured for a reviewed source pin. Store-submission tooling remains inactive and untested against Partner Center; automatic main-to-production publication remains planned in PB-154. Keep ordinary CI separate from jobs that can sign or publish a release.

- [ ] Add a release workflow with an explicit version, source commit and architecture matrix. All platforms must use the same revision.
- [ ] Restrict signing and publishing to trusted release refs and protected environments. Pull requests must not receive signing authority or publication credentials.
- [ ] Use minimal job permissions, reviewed actions pinned to immutable revisions, and temporary credential storage with cleanup. Avoid executing untrusted changes in privileged jobs.
- [ ] Make signing failures and missing credentials fail the release job. Keep any unsigned development builds explicitly labelled.
- [ ] Run format, engine, codec and protocol checks before signing. Preserve source provenance and generate final package SHA-256 checksums after signing, notarization and stapling.

Completion: the workflow can prepare and validate a release without publishing it; only trusted release jobs can request signatures. Configure remote environment settings when the concrete workflow is ready for review.

## 5 Select and enroll signing providers

- [ ] Confirm the publisher's registration country and whether the publisher is an individual or a legal organization. Agree on the publisher identity users will see.
- [ ] Apply to SignPath Foundation for Windows signing. Its free program requires acceptance, verified builds, a public code signing policy, defined roles and manual approval of each release. Its certificate identifies SignPath Foundation as publisher.
- [ ] If SignPath is unsuitable or declined, check Azure Artifact Signing eligibility or obtain a quote from a trusted certificate provider with a CI-compatible hardware-backed signing service. Do not assume Azure supports every country or individual publisher.
- [ ] Enroll in the Apple Developer Program and create a Developer ID Application certificate. Store its private key and notarization credentials securely.
- [ ] Prepare provider applications and account configuration before submitting them or incurring charges. Record account details privately; publish only required contributor-facing policies.

Completion: provider acceptance and account access are confirmed, signing identities are agreed, and a test signature can be verified. Applications and enrollment remain dependent on maintainer account access.

Provider requirements: [SignPath Foundation](https://signpath.org/terms.html), [Microsoft signing options](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options), [Apple membership](https://developer.apple.com/support/compare-memberships/).

## 6 Produce signed Windows and notarized macOS packages

- [ ] Windows: set PeerBrush product/version metadata before signing; Authenticode-sign and timestamp the executable and any project-owned executable components. Sign any installer introduced for distribution, then verify the final packaged files.
- [ ] Review the Windows batch launcher and determine whether direct executable launch provides the same supported setup. Avoid making an unsigned script the primary download entry point without testing its behavior under current Windows protections.
- [ ] macOS: create `PeerBrush.app` with its executable, resources, icon and stable bundle identity. Confirm application resource paths and writable runtime data locations work when launched through Finder.
- [ ] Sign macOS executable components and the bundle with Developer ID, hardened runtime and secure timestamps. Submit through `notarytool`, require acceptance, inspect the log, and staple the ticket to the app or DMG. Package the final stapled result; do not try to staple a ZIP itself.
- [ ] Verify Windows signatures with SignTool and macOS signatures, tickets and Gatekeeper assessment with Apple tools. Do not modify sealed binaries or app resources after signing.

Completion: Windows packages have valid trusted signatures and timestamps; macOS packages have valid signatures and accepted notarization, with attached tickets verified.

Windows SmartScreen reputation is separate from signature validity, and new signed files can still warn. EV certificates do not guarantee an immediate bypass. macOS can still show a normal first-open confirmation. Sources: [Microsoft SmartScreen guidance](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation), [Apple notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## 7 Prepare Linux distribution and the Windows Store channel

- [ ] Create a Flatpak manifest with a stable application ID, desktop entry, icons and pinned sources. Preserve notices and source availability.
- [ ] Test PSD/PNG loading and saving, file dialogs, clipboard, GPU rendering, runtime storage, and AI/MCP connectivity under the sandbox. Request only permissions demonstrated to be needed.
- [ ] Prepare a Flathub submission and ownership verification. Flathub signs its published builds; listing verification separately establishes the maintainer's control of the application ID.
- [ ] Keep portable Linux downloads with final checksums and a documented verification method. Detached checksum signatures provide integrity evidence but do not replace operating-system package trust.
- [ ] Complete the required Microsoft Store channel with MSIX filesystem, local MCP and installation acceptance before submission. Keep its package/update ownership separate from the required signed direct installer. Both Windows channels remain subject to native verification and explicit activation approval.

Completion: Flatpak passes native smoke tests and its submission is ready. Store publication and Flathub publication require their respective account access and submission steps.

Sources: [Flathub signing](https://docs.flathub.org/blog/app-safety-layered-approach-source-to-user), [Flathub verification](https://docs.flathub.org/docs/for-app-authors/verification), [Windows Store signing](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options).

## 8 Validate a complete release candidate

- [ ] Download candidate packages through the intended browser/download path on clean Windows, macOS and Linux systems. Preserve Windows download markers and macOS quarantine attributes so installation checks reflect real users.
- [ ] Check publisher identity, signature/timestamp validity, Gatekeeper behavior and offline ticket validation where applicable. Record remaining prompts honestly.
- [ ] Visually verify the native workspace and exercise painting, effects, transforms, layer reordering, 16-bit editing/saving, PSD merged composites and AI connection behavior. Run engine, codec and protocol tests for behavior changes.
- [ ] Review source, packages, logs and screenshots for privacy; compare version, commit, architecture and final checksums across platforms.
- [ ] Confirm that a contributor can build from the supplied source without private files or signing credentials. Local unsigned builds should remain supported.

Completion: each supported platform has documented results. Missing native access or a failed platform is an explicit blocker, not a successful cross-platform release.

## 9 Publish and maintain verified downloads

- [ ] Follow the release-maintenance playbook to publish an authorized new version. Do not silently replace existing binaries or rewrite existing tags.
- [ ] Upload final packages, corresponding source and checksums. Verify the actual public downloads and signatures after upload.
- [ ] Update website home/latest information, Downloads and dated version history from one consistent source. Mark signing status and remaining platform limitations accurately.
- [ ] Update confirmed announcement destinations only within authorized scope. Keep account records and publication receipts in private storage.
- [ ] Establish recurring release checks for expiring credentials, provider access, signature failures and unsigned regressions in the pipeline. Schedule background reminders only if requested.

Completion: every in-scope public destination has been read back and matches the published version, assets and verified signing status.

## Security references

- [GitHub Actions secrets and OIDC](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets)
- [Removing sensitive data from Git history](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository)

The checklist records public implementation requirements. Actual credentials, audit findings, account identities, private storage locations and release receipts belong outside this repository.
