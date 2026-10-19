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

The signer verifies the input against the exact committed source policy, then performs these steps:

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

## Portable regression checks

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
