# macOS Developer ID signing and notarization

This is a prepared, explicit native workflow for direct distribution of the arm64 `PeerBrush.app`. It does not create an Apple account, install credentials, configure a runner, publish a release, or prove that any existing download is signed. Running it requires a separately authorized Mac and Apple submission. Portable tests use synthetic packages and mocked subprocesses; real signing, notarization, Gatekeeper and native launch tests remain release gates.

## Prerequisites and secure setup

- An active Apple Developer Program team and its real, public ten-character Team ID.
- A valid **Developer ID Application** certificate and corresponding private key already usable in the signing user's Keychain. Apple Development and Developer ID Installer certificates are unsuitable for this app bundle.
- The exact certificate's 40-hex-character SHA-1 fingerprint. This is a public identity selector, not a signature hash algorithm. Inspect the existing identities with `security find-identity -v -p codesigning` on the authorized Mac and select the correct team. No fallback chooses the first identity.
- A working, already-configured `notarytool` Keychain profile. The user performs any credential creation/import and initial authentication through Apple's secure local setup separately. Pass the profile's local name and `--keychain` with the absolute path of its existing file-based Keychain. The script selects that Keychain for every notary operation. Never put passwords, certificate private keys, app-specific passwords or API keys in repository files, command arguments, chat, screenshots or build logs.
- Current supported macOS/Xcode command-line tools providing `codesign`, `ditto`, `notarytool`, `stapler` and `spctl`, Python 3.11+, and network access to Apple's timestamp, notarization and ticket services.
- An exact committed source checkout matching the package, the original unsigned macOS ZIP, and its independently verified SHA-256 from the approved build/release inventory.

The script never invokes credential imports, `store-credentials`, keychain unlocks, security trust changes, partition-list changes, API-key creation or account configuration. A keychain permission prompt requires the appropriate user action; this workflow does not bypass it. Do not run the signer as `sudo`.

## Source and release identity

Build and inspect all three platforms from the same reviewed source commit using [packaging](packaging.md). Keep licenses, notices and the matching GPL source ZIP alongside the app. The source checksum and all portable files remain byte-identical through signing; the signed executable's checksum necessarily changes.

Signing appends an explicit packaging label, for example `PeerBrush-0.26.0-macOS-arm64-apple-signed-r1.zip`. It does not change the app version or source commit. An existing `-repacked-YYYYMMDD` label is retained before `-apple-signed-r1`. Increase the revision for a deliberately new signing artifact; existing signed files and receipts are never replaced.

Do not use this label to conceal application changes. A new application version requires a new reviewed source commit and fresh native binaries. An earlier version's signature cannot certify a new version. `--binary-source-commit` is valid only for an actual packaging-only correction whose complete application/build inputs still match the original binary. Signing metadata cannot establish that an arbitrary local executable was built from a commit; retain the exact-source native build evidence.

## Ordered native workflow

1. Commit and review the intended release sources through the authorized contributor workflow. Build, test and package the matching native executable. Inspect the unsigned three-platform matrix with `scripts/verify_release.py`. Resolve native/runtime failures before signing.
2. Review the exact input ZIP's release manifest, source and checksum. Identify the existing Developer ID identity, Team ID and notary profile on the Mac. Preserve the original unsigned ZIP.
3. Select a fresh private work directory and a separate final output directory, both outside the source checkout. The work directory preserves the submitted bytes and job identifier for recovery. Do not share it or include it in release assets.
4. Run the signer only when signing and uploading the app to Apple are authorized. In the example below the shell variables must already contain the actual reviewed values; none is a credential:

```sh
python3 -B scripts/sign_macos.py --help

python3 -B scripts/sign_macos.py \
  --archive "${UNSIGNED_ARCHIVE:?select the verified unsigned macOS ZIP}" \
  --expected-sha256 "${UNSIGNED_SHA256:?use the independently verified input SHA-256}" \
  --commit "${SOURCE_COMMIT:?select the exact full source commit}" \
  --version "${APP_VERSION:?select the matching app version}" \
  --team-id "${APPLE_TEAM_ID:?select the actual Apple Team ID}" \
  --identity "${DEVELOPER_ID_SHA1:?select the existing certificate fingerprint}" \
  --profile "${NOTARY_PROFILE:?select the existing Keychain profile name}" \
  --keychain "${NOTARY_KEYCHAIN:?select the absolute existing file-based Keychain path}" \
  --revision 1 \
  --work "${SIGNING_WORK:?select a fresh private directory outside the checkout}" \
  --output "${SIGNED_OUTPUT:?select a separate directory outside the checkout}"
```

The signer accepts only a canonical stable `A.B.C` release version; every prerelease/internal development candidate fails before any provider access, signing or resumable work. It verifies the input against the exact committed source policy, then performs these steps. Unreleased v0.3 bundles include the icon, document declarations and installed license/notices described in [macOS integration](macos-integration.md) before sealing. Signing must preserve these bytes; do not add them to an already signed app. Historical 0.26 artifacts and protected signing workflow pins retain their existing source/layout.

- Confirm a valid existing Developer ID Application identity belonging to the explicit team, and validate the existing notary profile's access.
- Sign the `.app` with hardened runtime and a secure timestamp. Only the reviewed single-executable bundle is supported. There is no `--deep` signing and no entitlement exception such as debugger access, disabled library validation or unsigned executable memory.
- Verify the Apple Developer ID certificate requirement and Team ID, bundle identifier, runtime option, timestamp and empty entitlements.
- Create a submission ZIP with Apple's `ditto`, submit it once with the existing profile, save its ID, and wait up to 30 minutes. Require `Accepted` status and a matching notary log. Any log issue, including a warning, blocks final output for review. The log SHA-256 must match the original submitted ZIP, not the later distribution ZIP.
- Staple and validate the ticket on the app. Require `spctl` to report `Notarized Developer ID`.
- Preserve the signed/stapled bytes, update the executable hash, and build a new final ZIP with the same GPL source and notices. Extract that final ZIP into a fresh directory and repeat `codesign`, entitlement, `stapler validate` and Gatekeeper checks before emitting the archive, SHA-256 file and verification receipt.

`release.json` records unsigned archive/executable hashes, signed executable hash, source hash, submission ZIP hash, team/certificate selectors, accepted submission ID, packaging revision and exact signing-metadata hashes. The final ZIP's hash is outside the ZIP to avoid a circular checksum. The receipt explicitly leaves native UI launch and fresh quarantined-download testing unperformed.

## Interrupted submissions

If notarization is still pending, a wait times out, or later ticket/Gatekeeper checks fail, preserve the work directory and rerun the identical command with `--resume`. The script checks the saved input, configuration, immutable submitted ZIP and signed payload, then waits for the same submission. It does not sign or upload again. `Invalid` or `Rejected` is never treated as success; resolve the actual issue before deliberately preparing another revision.

If the upload outcome is ambiguous and no submission ID was saved, stop and reconcile the existing `notarytool history` and logs locally. Do not repeatedly submit or edit `state.json` to force progress. Likewise, a partial output/receipt write requires checking the existing artifact and output permissions before choosing a new output location; no overwrite option is provided.

Raw Apple logs may contain local paths or account details. Review them privately and retain only the necessary sanitized release evidence. They are not bundled in the archive.

## Strict metadata inventory and ticket preservation

The only newly permitted regular files are:

- `PeerBrush.app/Contents/_CodeSignature/CodeResources`, the resource seal
- `PeerBrush.app/Contents/CodeResources`, the app's stapled ticket

Both require recorded SHA-256 hashes and bounded size. Extra helpers, frameworks, symlinks, provisioning profiles, AppleDouble entries, arbitrary `_CodeSignature` files or changed portable content fail inspection. The main Mach-O contains its embedded signature. Apple's [Security implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_codesigning/lib/notarization.cpp) reads the app ticket from the second path as a regular file. The workflow deliberately supports that layout and fails closed if it changes; it does not silently drop a ticket or add a broad metadata exception. Final re-extracted `stapler validate` is mandatory even after the staging app passed.

The portable `verify_release.py` accepts the signed filename and this narrow inventory, still requires the complete matching Windows/macOS/Linux matrix, and still verifies every committed source byte and the final executable/archive hashes. Its signing-verification field says **metadata-and-hashes-only**. A manifest claim or portable test does not establish Apple's cryptographic validation, notarization or Gatekeeper acceptance.

## Required native release validation

After scripted checks, use a fresh user profile or clean Mac/VM to download the exact candidate through a normal browser, extract it, and launch through Finder with normal Gatekeeper/quarantine behavior. Do not disable Gatekeeper or remove quarantine to make a test pass. Verify first launch, saving, PSD open/edit behavior, the MCP integration and any selected optional provider flow. Check offline launch with the stapled ticket where applicable.

Signing and notarization address unidentified-developer and unnotarized-software blocks; they cannot promise that macOS will never show a normal first-open confirmation, permission request, revocation warning or device-policy restriction. Publish only after the actual native evidence and separately authorized release steps in [release maintenance](release-maintenance.md) are complete.

## DMG preparation and remaining release gates

On an actual Mac, use `scripts/package_macos_dmg.py` with an already verified signed/notarized/stapled ZIP and the exact source checkout used by that ZIP:

```sh
python3 -B scripts/package_macos_dmg.py \
  --archive /absolute/candidates/PeerBrush-VERSION-macOS-arm64-apple-signed-r1.zip \
  --expected-sha256 VERIFIED_SIGNED_ZIP_SHA256 \
  --commit FULL_SOURCE_SHA --version VERSION \
  --output /absolute/dmg-candidates --revision 1
```

Output must be outside the checkout. The original signed ZIP/app remains unchanged. The DMG contains the app and drag-to-Applications shortcut, with original source, licenses and support files retained separately in `Documentation and source`. Its mounted app receives the same Developer ID/Team ID, hardened runtime, secure timestamp, entitlement, ticket and Gatekeeper verification as the original ZIP. The DMG itself is an **unsigned, unnotarized preparation candidate**, not a published installer; app notarization is not recorded as image notarization.

`sign_macos.py --format dmg` implements image signing and notarization through the same preflight, existing-keychain selection, submission journal, accepted log/digest verification and resume logic as ZIP signing. ZIP remains the default. Both formats reject prereleases before any provider call, including on resume. With separately authorized provider access, take the independently verified original signed ZIP as input:

```sh
python3 -B scripts/sign_macos.py --format dmg \
  --archive /absolute/candidates/PeerBrush-VERSION-macOS-arm64-apple-signed-r1.zip \
  --expected-sha256 VERIFIED_SIGNED_ZIP_SHA256 \
  --commit FULL_SOURCE_SHA --version VERSION \
  --output /absolute/signed-dmg --work /absolute/dmg-signing-work \
  --team-id APPLE_TEAM_ID --identity DEVELOPER_ID_CERTIFICATE_SHA1 \
  --profile EXISTING_NOTARY_PROFILE --revision 1
```

The tool prepares the original app/source payload, signs and timestamps `submission.dmg`, records its exact hash before upload, and retains the returned submission ID. It requires Accepted status and a matching issue-free notarization log. Timeout, rejection or finalization failure preserves the submitted bytes; `--resume` checks the same configuration, prepared inputs and image hash, then checks the existing submission without resigning or uploading again. Ambiguous submission failures without an ID require manual history reconciliation, never automatic re-submission.

Finalization staples a separate copy, verifies its Developer ID/Team ID, identifier, secure timestamp, ticket and Gatekeeper primary image signature, then verifies `hdiutil` integrity and the complete read-only mounted payload plus the app's signature/ticket/Gatekeeper result. Only afterward are immutable `-dmg-signed-rN.dmg`, final SHA-256 and provenance written. Failed mounted work is retained rather than recursively removed. No second signing system, current 0.26 signing pin, credential, protected approval or publication setting is changed. Actual provider execution remains separately gated; synthetic tests establish the code path, not real Apple acceptance.

Signed image receipts still say `release_ready: false` while drag/install and quarantined-download acceptance remain open. A signed DMG is not automatically accepted by the existing ZIP-only release/website inventory. Public integration must verify the final signed image and checksums, actual platform acceptance and public downloads. Do not rename an unsigned preparation candidate as signed or reuse a pre-signing checksum.

Native acceptance still requires a fresh quarantined browser download, Finder drag to Applications, first/offline launch, upgrade/replacement with unsaved-work protection, settings retention, MCP launch path and PSD/PSB opening. Unreleased v0.3 includes the committed icon and verified installed license/notices in `Contents/Resources` before sealing; exact archive inspection requires and verifies them. Corresponding source and public support guides remain alongside the app in the distribution, as described in [installed integration](macos-integration.md). Historical 0.26 bundles retain their original layout. Never insert files into an already sealed app; source implementation and synthetic tests do not establish installed acceptance.

`python3 -B scripts/test_macos_dmg.py` and `scripts/test_sign_macos_dmg.py` use synthetic signed ZIP fixtures and mocked Apple commands, including rejected/pending/interrupted notarization, hash/signature/payload failures, finalization retries and same-submission resume. `python3 -B scripts/test_macos_dmg_native.py` runs actual image creation, verification and read-only mounting on macOS using synthetic unsigned bytes that are never executed. CI and release-candidate tests run the native smoke without credentials. These tests do not claim a real signed image or a completed installation.

## Portable signing regression checks

```sh
python3 -B scripts/test_package.py
python3 -B scripts/test_sign_macos.py
```

These cover exact archive/source preservation, narrow signed inventories, hashes, identity/configuration failures, empty entitlements, native-command construction, notarization rejection/pending/timeout handling, same-submission resume, log mismatches/issues, unexpected signing files, staple mutation, final extracted verification, and non-overwriting outputs. Apple commands are subprocess mocks. Passing these tests is not a native signing or notarization result.

## Apple references

- [Creating distribution-signed code for macOS](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/)
- [Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
- [Migrating to the latest notarization tool](https://developer.apple.com/documentation/technotes/tn3147-migrating-to-the-latest-notarization-tool)
- [Code-signing requirements](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements)
- [Apple DTS: resolving Gatekeeper problems and checking the submission SHA-256](https://developer.apple.com/forums/thread/706379)


## Independent final-DMG qualification (unreleased)

After an explicitly approved signing job produces a signed/notarized/stapled ZIP and final DMG, a separate native consumer can qualify those exact files without signing, submitting, installing or running the app. Independently select the stable source SHA/version, both file SHA256 values and public publisher Team ID. Use absolute paths and a private owned parent outside every Git checkout for fresh work/report directories:

```sh
python scripts/qualify_macos_dmg.py --source-root "$REVIEWED_SOURCE" \
  --archive "$SELECTED_SIGNED_ZIP" --image "$SELECTED_FINAL_DMG" \
  --archive-sha256 "$VERIFIED_ZIP_SHA256" --image-sha256 "$VERIFIED_DMG_SHA256" \
  --commit "$REVIEWED_COMMIT" --version "$STABLE_VERSION" --team-id "$PUBLIC_TEAM_ID" \
  --work "$PRIVATE_PARENT/check-NEW" --output "$PRIVATE_PARENT/report-NEW"
```

The consumer checks committed corresponding source and embedded executable identity, reconstructs every expected app/support file, mode and Applications shortcut directly from the selected ZIP, then rechecks actual app/image publisher trust, timestamps, hardened runtime, tickets and Gatekeeper. Read-only image mounting must expose exactly one selected volume. Cleanup binds the entire BSD device to that exact image, detaches it and confirms removal. Ambiguous/extra volumes or failed cleanup fail qualification and retain work; do not recursively delete unresolved mounts. Both original and private input copies are hashed again after native inspection.

The exclusive `dmg-qualification.json` contains a strict sanitized asset record and factual check results, with installation/public/production readiness still false or unverified. It contains no local paths, private signing information or arbitrary signer-sidecar fields. Native execution authority still needs a successful trusted native job when consumed; matching JSON alone does not authenticate a producer. This leaf consumer is not yet wired into the aggregate/website final-installer adapter. Select the verified signed ZIP as the Mac portable asset before first publication; existing published files remain immutable. No existing Apple workflow source pin or activation setting is changed.
