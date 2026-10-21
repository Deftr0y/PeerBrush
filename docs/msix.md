# Windows installation and MSIX

This is packaging support for the native x64 Windows application. It is separate from the existing Windows/Linux/macOS portable ZIPs. A successful package build is not a Store submission, a signed sideload installer, or evidence of an installed-runtime test.

## Release provenance

The 0.26 candidate uses application version 0.26.0 and maps to MSIX 1.26.0.0. Build all three platforms from one reviewed final source commit with matching embedded source. Preserve earlier tags, artifacts and signing receipts. Before Store submission, verify the actual current Partner Center package version and require a strictly newer candidate; the proposed mapping alone does not establish upgrade eligibility.

## Installed behavior

- Start menu entry uses the existing PeerBrush logo and application name.
- Windows manages package registration, updates, and uninstall. Store-distributed packages use Store updates; there is no in-app self-updater, scheduled task, service, or downloaded code execution.
- PSD and PSB register as available **Open with** handlers. Registration does not take over the user's default editor. Optional image handlers are explicit build choices using `--associate .png` (repeat for supported types). Do not add types without the maintainer's decision.
- Explorer activation passes `--open "%1"`. Paths are parsed as OS arguments, without a shell. Subsequent launches use the existing authenticated local connection to open independent project tabs. Existing unsaved work, unsupported-PSD protection, native bit depth and image-content choices are preserved.
- For the packaged Windows app, state is stored under `%USERPROFILE%\.peerbrush\state`, outside the immutable/versioned installation and AppData virtualization. Discovery remains `%USERPROFILE%\.peerbrush\mcp.json`. These user-owned files intentionally survive uninstall. Portable defaults are unchanged; an initial package install does not silently move/delete portable recovery data. `--state-dir` can choose a separate writable workspace.

## Build requirements and identity

Use Windows x64, Python 3.11+, stable Rust, and a Windows 11-era SDK (10.0.22000 or later) containing MakeAppx.exe. The package targets Windows 10 build 19041 and later. The only restricted capability is `runFullTrust`, required by this classic desktop application. Store approval remains a separate gate.

From a **clean, committed checkout**, use the existing source/license-aware portable builder first:

```powershell
python scripts/package.py --check-source
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --locked
python scripts/test_package.py
python scripts/test_msix.py
python scripts/build_release.py
python scripts/test_msix_runtime_tests.py
python scripts/package.py
python scripts/package_msix.py --portable dist/PeerBrush-0.26.0-Windows-x64.zip --development-identity
```

The explicit local test identity is `PeerBrush.LocalTest` / `CN=PeerBrush Local Test`. It is unregistered development metadata, not an asserted publisher identity. The output is **unsigned** and is not directly installable through an ordinary double-click. No certificate, signing key, trusted-root installation, Developer Mode switch, unsigned-install OID, package registration, account creation or Store upload is performed.

The reserved PeerBrush product identity is now configured in `packaging/msix/identity.store.json`: Name `Deftroy.PeerBrush`, Publisher `CN=438C1F79-CBE8-4F97-8C50-F9AC34C7D2AC`, PublisherDisplayName `Deftroy`. `packaging/msix/product.store.json` records the supplied package family `Deftroy.PeerBrush_wszhgmjq5tjta`, Store ID `9PNR6T1Q2ZL9`, package SID and intended first package version `1.25.0.0`. These public values are configuration, not proof of account verification, certification or publication.

Pass `--identity packaging/msix/identity.store.json --expected-package-family Deftroy.PeerBrush_wszhgmjq5tjta`. The exact Publisher string derives the supplied publisher ID `wszhgmjq5tjta`; on Windows, the build verifies the family with the native `PackageFamilyNameFromId` API. Placeholder identities, wrong families and accidental development-identity use fail closed. The example file remains a template for other explicitly authorized products; do not change this product identity between updates.

The Store requires a positive first version component and zero fourth component. The default permanent monotonic mapping is `Cargo major + 1 . minor . patch . 0`: app **0.25.0** becomes MSIX **1.25.0.0**, and app 1.0.0 becomes MSIX 2.0.0.0. Portable app versions remain unchanged. Store candidates reject overflow and prerelease Cargo versions. If an established package already has another versioning scheme, pass `--package-version A.B.C.0` and `--previous-version W.X.Y.0`; the new version must be strictly greater. For every real upgrade, supply the previous published MSIX version. Do not reuse a version for changed binary/source content or change identity between updates.

Only the exact `--development-identity` may package `A.B.C-dev.N` builds, projecting the numeric core through the same mapping: `0.30.0-dev.1` becomes local-test MSIX `1.30.0.0`. The receipt and embedded corresponding source retain `0.30.0-dev.1`. Different development iterations may share that numeric package version, so use separate output directories; this supports CI packaging checks, not a development update feed. The local identity cannot upgrade or replace the Store identity. Other prerelease forms are unsupported, and explicit package-version overrides do not make a prerelease eligible for Store packaging.

Example for an update, after reading the actual current package version in Partner Center:

```powershell
python scripts/package_msix.py --portable dist/PeerBrush-0.26.0-Windows-x64.zip --identity packaging/msix/identity.store.json --expected-package-family Deftroy.PeerBrush_wszhgmjq5tjta --portable-sha256 VERIFIED_PORTABLE_SHA256 --package-version 1.26.0.0 --previous-version ACTUAL_CURRENT_STORE_VERSION --output dist/store-candidate
```

The Windows MSVC toolchain can import VCRUNTIME140.dll; final native import inspection remains required. The manifest declares Microsoft.VCLibs.140.00.UWPDesktop (minimum 14.0.30704.0), which the Store resolves; a separately authorized sideload test must also supply the matching x64 Microsoft runtime framework. Review the final executable imports and the build SDK's runtime manifest, and verify launch on a clean machine without Visual Studio. Do not assume the build runner's globally installed runtime exists on end-user machines.

For production, `--portable-sha256` must be the verified SHA256 of the deliberately selected portable input, from the matching release inventory or freshly verified build. Self-declared notice hashes do not independently prove the completeness of downloaded third-party crate notices; the trusted build and license review remain required.

This still produces an unsigned local file only. The Microsoft Store signs accepted submissions. Direct distribution instead needs separately authorized trusted signing and a defined update distribution channel; neither is created here.

## Validation and privacy

The packager accepts only the same-commit, x64 Windows portable artifact, with its complete matching GPL source archive and notices. It validates exact source/portable files, executable and notice hashes, native architecture, archive paths and allowlisted payload. Tracked modifications, stale outputs, unlisted material, mismatched versions/commits and missing notices are rejected. MakeAppx schema/semantic validation stays enabled. Final verification checks every staged byte, OPC-encoded ZIP entry name, and each 64 KiB SHA-256 block-map entry. It emits the package, SHA256 checksum and factual build report. It refuses to overwrite an existing result.

The small Start/Store PNGs are resized derivatives of `assets/peerbrush-logo.png`; no new branding or slogan is introduced. The installed package retains the matching GPL source and third-party notices. Review the actual artifacts and current license obligations before publication, as with portable releases.

CI keeps the existing three-platform portable matrix and adds unsigned Windows local-test packaging. The release-candidate workflow can also prepare a real-identity candidate when `build_store_msix` is explicitly selected. It reads the exact public identity and expected family from the committed Store configuration, so the selected source revision determines the identity. No repository secrets or identity variables are required. Supply `previous_store_version` for every update; an optional `store_package_version` selects an already-reviewed version scheme. The workflow never submits to the Store or changes signing, account, certificate, trust or repository settings. This is a build artifact, never a published release. Running `scripts/test_msix.py` elsewhere exercises synthetic staging and tamper fixtures only; it does not claim Windows SDK or installed-package coverage.

## External AI/MCP compatibility

The app registers the **absolute package-family-scoped execution alias**:

`%LOCALAPPDATA%\Microsoft\WindowsApps\<PackageFamilyName>\peerbrush-mcp.exe`

Arguments remain `mcp --state-dir ABSOLUTE_PATH`. This avoids the versioned WindowsApps install folder, global alias collisions and an external AI client's PATH assumptions. The alias launches the existing Windows executable with inherited stdin/stdout/stderr. Do not replace it with `start`, a shell URL/AUMID activation or a detached process; those can lose redirected pipes. If the alias is absent, Connect AI reports an actionable error and does not write a broken fallback command.

Clicking Connect AI migrates existing PeerBrush registrations to the alias using the existing backup and conflict-preserving mechanism. It never launches an AI provider or stores an API key. Existing clients need their normal reload/restart and tool approval. Moving from portable to installed (or back) intentionally selects the most recently connected canvas; both can coexist with separate state directories. No real AI-client files should be used in automated packaging tests.

MSIX normally redirects newly written AppData files, so a successful write can be invisible to another app. This manifest retains filesystem and registry virtualization without any directory exemptions. It does not request `unvirtualizedResources`. Normal `.codex`, `.cursor`, `.gemini` and `.peerbrush` paths under the user profile are outside AppData virtualization.

For Claude Desktop and any custom client paths inside AppData, on all supported Windows versions, Connect AI leaves the client file untouched and directs the user to copy the manual configuration. Symlinks and junctions are resolved before checking visibility, and uncertain destinations fail closed. Both behavior and cross-process file visibility still require real Windows QA.

## Required installed Windows QA

Perform these in an authorized disposable Windows test profile/VM, after the package has an approved installation/signing route. This checklist is not permission to create or trust certificates, enable Developer Mode or install unsigned executable packages.

1. Validate with the Windows SDK and Windows App Certification Kit. Inspect identity, version, restricted capabilities, embedded source/notices and signature status. Record exact commit/package SHA256, Windows build and SDK version.
2. Install through the approved route, launch from Start, verify the native logo/window and normal non-elevated runtime. Exercise drawing, PSD/PSB save/reopen, cancellation, 16-bit data, recoveries, and project switching.
3. Use **Open with** on spaced/Unicode PSD/PSB paths while the app is closed, already open, minimized, and holding unsaved work. Multi-select files, repeat an existing file, cancel an image-content dialog, and try a missing/unsupported file. Verify existing projects are unchanged and errors visible. Ensure unrelated default handlers were not changed. Also run portable and installed PeerBrush simultaneously and try opening the same PSD from both; verify the existing file-ownership lock is visible across package boundaries and competing writers are prevented. This cross-package temporary-directory behavior is an explicit QA gate, not an assumed pass.
4. Launch a QA window with a disposable, spaced/Unicode state directory. Run the read-only two-client pipe check against the installed family alias:

   ```powershell
   python scripts/test_msix_runtime.py --executable "$env:LOCALAPPDATA\Microsoft\WindowsApps\ACTUAL_PACKAGE_FAMILY\peerbrush-mcp.exe" --state-dir "C:\QA\PeerBrush state 作品"
   ```

   The script checks initialize, JSON-only stdout, tools/list, actual authenticated read-only project-list calls from both clients, and a repeated first-client call after the second connects. Static adapter replies alone cannot pass. The separate runtime regression suite rejects empty/invalid connection data, stale endpoints and incorrect tokens using disposable state. These checks establish the application bridge, not native GUI appearance. It does not launch the GUI, install a package or touch client configurations. Separately exercise the actual supported AI clients, unavailable/disabled aliases and client restarts in the disposable profile.
5. Verify Connect AI backups/idempotence and external visibility in that disposable profile: normal Codex/Cursor/Gemini paths; Claude manual fallback on Windows 10 and 11; custom CODEX_HOME under AppData. Check an ordinary unpackaged process sees any file reported as registered. Test portable-to-installed migration and switching back.
6. With two authorized same-identity packages and increasing versions, update while preserving documents and settings. Restart the external client without recopying its alias path. Confirm old versioned install paths are unnecessary and rollback/downgrade is not silently enabled.
7. Uninstall. Confirm package-owned shortcuts/handlers/aliases are removed and unrelated defaults, documents, original client settings/backups and user-owned state are not erased. A stale client registration should fail clearly until removed/reconnected by the user.

## Microsoft references

- [Manual desktop packaging](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-manual-conversion)
- [Desktop Visual C++ runtime dependencies](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-prepare)
- [MakeAppx](https://learn.microsoft.com/en-us/windows/msix/package/create-app-package-with-makeappx-tool)
- [Store package/version requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements)
- [Virtualization behavior](https://learn.microsoft.com/en-us/windows/msix/desktop/flexible-virtualization)
- [App capability declarations](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/app-capability-declarations)
- [Package-family alias directories](https://learn.microsoft.com/en-us/sysinternals/downloads/microsoft-store)
- [Execution alias schema](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap5-appexecutionalias)
- [File association schema](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap3-filetypeassociation)
- [Inherited standard handles](https://learn.microsoft.com/en-us/windows/console/getstdhandle)
- [Unsigned test packages and their separate identity constraints](https://learn.microsoft.com/en-us/windows/msix/package/unsigned-package)

## Local Store candidate preflight (unreleased)

Use the existing stable-version mapping and configured public Store identity; independently select the portable, final unsigned MSIX and exact producer-report hashes before authentication:

```sh
python -B scripts/qualify_store_msix.py --source-root "$RELEASE_SOURCE" \
  --portable "$WINDOWS_ZIP" --portable-sha256 "$WINDOWS_ZIP_SHA256" \
  --package "$STORE_MSIX" --package-sha256 "$MSIX_SHA256" \
  --report "$MSIX_PRODUCER_REPORT" --report-sha256 "$PRODUCER_REPORT_SHA256" \
  --commit "$RELEASE_COMMIT" --version "$RELEASE_VERSION" \
  --previous-version "$PREVIOUS_STORE_VERSION" --output "$FRESH_PRIVATE_REPORT_DIRECTORY"
```

The local consumer reuses the actual Store-submission validator: exact committed source and embedded build, source-derived manifest/assets/notices, configured identity/family, complete payload/block/whole-file hashes and strict report consistency. Inputs are bounded identity-checked snapshots and are rechecked after validation. It accepts stable mapped versions above the initial Store baseline and a strictly newer version than the selected previous package. The previous version remains operator-selected until actual service reconciliation.

The exclusive sanitized report stays outside Git checkouts. It confirms content consistency; it does not authenticate SDK execution or its native producer, establish installation or signed direct-download trust, submit a package or verify public availability. Readiness remains false. Native SDK validation belongs to the trusted build job, even when the producer report contains its expected validation label. Actual Store submission retains its separate protected approval and durable private receipt. No account access, authentication, installation, upload or Windows trust change is performed by preflight.

## Final signed direct-distribution bytes (unreleased)

Windows v0.3 requires both Microsoft Store distribution and a direct signed installer. The unsigned Store builder and submission validator retain their existing path. `qualify_signed_msix.py` is a separate read-only final-byte verifier; it does not sign executables/packages, install, publish, create an AppInstaller descriptor or change certificate trust.

On an authorized Windows verification host, independently select the stable source commit, matching portable ZIP, original unsigned MSIX, final `-signed-rN.msix`, every input SHA256, reviewed public identity/family and leaf certificate SHA256. Also select trusted installed Windows SDK SignTool and PowerShell 7.6.5 executable paths and independently reviewed SHA256 values. There are no configured production direct identity, certificate, signing-provider or feed defaults.

```powershell
python -B scripts/qualify_signed_msix.py --source-root "$SOURCE" `
  --commit "$COMMIT" --version "$VERSION" `
  --portable "$WINDOWS_ZIP" --portable-sha256 "$ZIP_SHA256" `
  --unsigned "$UNSIGNED_MSIX" --unsigned-sha256 "$UNSIGNED_SHA256" `
  --package "$SIGNED_MSIX" --package-sha256 "$SIGNED_SHA256" `
  --identity "$REVIEWED_PUBLIC_IDENTITY_JSON" --package-family "$REVIEWED_FAMILY" `
  --certificate-sha256 "$REVIEWED_SIGNER_CERTIFICATE_SHA256" `
  --signtool "$TRUSTED_SDK_SIGNTOOL" --signtool-sha256 "$SIGNTOOL_SHA256" `
  --powershell "$TRUSTED_PWSH_7_6_5" --powershell-sha256 "$PWSH_SHA256" `
  --work "$FRESH_PRIVATE_WORK" --output "$FRESH_PRIVATE_REPORT_DIRECTORY"
```

Both packages must contain the source-derived native executable, source/notices, unchanged manifest, assets and complete block-map coverage. Signing may add only a bounded `AppxSignature.p7x` and its exact content-type declaration; all original payload and block-map bytes remain unchanged. Consequently this route preserves the current portable executable bytes and **does not establish separate Authenticode signing of `peerbrush.exe`**. That signing/provenance pipeline remains a release requirement.

The selected SignTool runs only `verify /pa /all /tw /q`: Default Authentication policy, all embedded signatures, timestamp required, no catalog search. Exit codes 1 and 2 fail; localized output never establishes trust. A fixed profile-free PowerShell probe uses the native Authenticode cmdlet and .NET `SignedCms` only to extract/check bounded metadata. It rejects catalog results, multiple/nested primary signatures, ambiguous timestamps, unknown unsigned attributes, certificate/Publisher differences and malformed or extra output fields. A single SHA256 primary signer and one timestamp signer must agree with the native provider's certificates. Signature metadata alone is insufficient: SignTool must succeed first. Inputs, owned copies, source and selected tools are rehashed afterward.

Transient copies and the exclusive report belong outside every Git checkout. Failures retain private work for diagnosis; do not publish it. Native verification uses the selected trusted SDK/PowerShell installation and the host's existing Windows trust/SIP policy, including any normal certificate/revocation lookups. Binary hashes do not attest the entire installed tool/runtime or machine configuration. No certificate import, private key, provider credential, custom root or trust-setting change is performed. Output-size and deadline checks are polling/acceptance guards: transient output can exceed the budget between polls. They are not a hard disk quota or a sandbox for an untrusted verifier installation.

The report records content and native signature results for these bytes, while native-job origin, executable signing, installed acceptance, AppInstaller/feed/public availability and production readiness stay unverified/false. Offline test processes are fake native verifiers, not cryptographic acceptance. Actual SDK/MSIX SIP probing with a reviewed CA-trusted timestamped package, tamper/missing-timestamp/wrong-publisher cases and clean-machine install/update/interruption/settings/unsaved-work/channel-migration checks remain required. Store and direct package identities, VCLibs delivery, aliases/profile migration and the public feed need explicit review before either channel is activated.

Primary contracts: [SignTool verification options and return codes](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool), [native Authenticode cmdlet and catalog behavior](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.security/get-authenticodesignature?view=powershell-7.5), [PowerShell 7.6.5 provider implementation](https://github.com/PowerShell/PowerShell/blob/v7.6.5/src/System.Management.Automation/security/Authenticode.cs), [SignatureStatus is not independent trust](https://learn.microsoft.com/en-us/dotnet/api/system.management.automation.signaturestatus), [Microsoft MSIX signature envelope](https://github.com/microsoft/msix-packaging/blob/master/src/inc/internal/AppxSignature.hpp), [MSIX signature content type](https://github.com/microsoft/msix-packaging/blob/master/src/msix/pack/ContentTypeWriter.cpp), [SignedCms signer metadata](https://learn.microsoft.com/en-us/dotnet/api/system.security.cryptography.pkcs.signedcms.signerinfos), [timestamping policy](https://learn.microsoft.com/en-us/windows/win32/seccrypto/time-stamping-authenticode-signatures).
