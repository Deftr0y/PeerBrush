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

## App Installer candidate files (unreleased)

`scripts/prepare_appinstaller.py` prepares four exclusive candidate files: a versioned `.appinstaller` descriptor, the exact signed PeerBrush MSIX, the exact selected Desktop VCLibs framework APPX/MSIX, and `SHA256SUMS`. It performs no signing, installation, upload, publication or Windows trust/policy change. There is no qualification-report input or success override. The separate unsigned Store path remains unchanged.

Use a clean exact stable source commit and the same independently selected portable ZIP, original unsigned MSIX, final signed MSIX, public identity/family, signer certificate and trusted native tools required by final MSIX verification. The preparer invokes that verification directly against actual package bytes. It reuses the same strict embedded-signature, SHA256 primary signer, single timestamp, exact Publisher/leaf certificate and native exit-zero policy for the framework; a signer's JSON cannot establish trust.

The source-derived PeerBrush manifest currently requires retail x64 `Microsoft.VCLibs.140.00.UWPDesktop` at least `14.0.30704.0`. Select actual approved redistributable media, its independently verified SHA256, the reviewed public signer-leaf SHA256, the applicable approved redistribution license file/hash, and its reviewed immutable HTTPS delivery URI. No media, runtime version, certificate, identity or URL is selected by default. Visual Studio development/testing media availability does not by itself establish redistribution rights. The license file is a bounded private selection, not a grant of permission or an automated legal assessment; its original and owned copy are rechecked and excluded from candidate output. Approval must account for every applicable redistribution/notice condition before invoking the preparer.

The framework's bounded UTF-8 manifest must contain the exact source-required Name/Publisher, one true Framework declaration, x64 architecture, an empty ResourceId and a canonical version meeting the source minimum. Application payloads, additional framework dependencies and ambiguous/unsupported archive layouts are refused. The descriptor records the framework's actual inspected version, rather than its minimum. Actual approved Microsoft media must pass the existing strict native signature policy; unsupported signature structures require investigation, never a relaxed acceptance flag.

```powershell
python -B scripts/prepare_appinstaller.py --source-root "$SOURCE" `
  --commit "$COMMIT" --version "$VERSION" `
  --portable "$WINDOWS_ZIP" --portable-sha256 "$ZIP_SHA256" `
  --unsigned "$UNSIGNED_MSIX" --unsigned-sha256 "$UNSIGNED_SHA256" `
  --package "$SIGNED_MSIX" --package-sha256 "$SIGNED_SHA256" `
  --identity "$REVIEWED_PUBLIC_IDENTITY_JSON" --package-family "$REVIEWED_FAMILY" `
  --certificate-sha256 "$REVIEWED_SIGNER_CERTIFICATE_SHA256" `
  --runtime "$APPROVED_RETAIL_VCLIBS" --runtime-sha256 "$VCLIBS_SHA256" `
  --runtime-certificate-sha256 "$REVIEWED_MICROSOFT_LEAF_SHA256" `
  --runtime-license "$APPROVED_RUNTIME_LICENSE" --runtime-license-sha256 "$LICENSE_SHA256" `
  --descriptor-uri "$IMMUTABLE_DESCRIPTOR_HTTPS_URI" `
  --package-uri "$IMMUTABLE_MSIX_HTTPS_URI" --runtime-uri "$IMMUTABLE_RUNTIME_HTTPS_URI" `
  --signtool "$TRUSTED_SDK_SIGNTOOL" --signtool-sha256 "$SIGNTOOL_SHA256" `
  --powershell "$TRUSTED_PWSH_7_6_5" --powershell-sha256 "$PWSH_SHA256" `
  --work "$FRESH_PRIVATE_WORK" --output "$FRESH_PRIVATE_CANDIDATE_DIRECTORY"
```

The descriptor uses bounded 16 KiB XML, the base 2017 namespace and exact x64 package identities. The [native App Installer text contract](https://learn.microsoft.com/en-us/uwp/schemas/appinstallerschema/element-appinstaller#remarks) requires `encoding="UTF-8"` with literal ASCII and no escape characters. Values needing Unicode or XML attribute escaping are explicitly unsupported by this descriptor preparer; Publisher names are never rewritten. General signed-MSIX verification retains Unicode/escaped XML identity support, so a package can pass that verification while remaining unsupported by the narrower App Installer format. Actual native descriptor acceptance is still unverified. App 0.30.0 maps to main-package/descriptor version 1.30.0.0 and filename `PeerBrush-1.30.0.0-Windows-x64.appinstaller`. URI inputs must be canonical bounded ASCII HTTPS locations ending in the exact selected filenames, without credentials, query/fragment additions or path aliases. Syntax checks do not establish remote immutability, redirect behavior, availability or returned bytes. Native verification and companion checksums establish these local selections only: the App Installer schema embeds neither these media hashes nor the selected leaf-certificate pins for a future download.

No `UpdateSettings`, on-launch/background scheduling, downgrade override, repair or fallback URI is emitted. This omits automatic-update configuration; it does not globally disable existing OS/provider/CSP/user policies or establish protection for unsaved artwork. Association still uses the existing `Package.SignatureKind`/`GetAppInstallerInfo` classification. Store-signed installations remain Store-managed. Future direct install/update activation must bind the approved channel identity, reuse live UI blockers and `finish_update_exit`, retain Later without discarding projects, and verify actual native interruption/recovery behavior before activation.

Original and owned inputs, public identity, license, source and trusted tool bytes are rechecked before and after exclusive output copies. Only the four named files may remain in candidate output; private native stdout, signer metadata, identity/license snapshots and work files are excluded. A failure can retain an incomplete private directory for diagnosis. It is not a publishable artifact or readiness authority. Descriptor preparation does not establish separate executable Authenticode signing, trusted producer origin, successful installation, public availability or production readiness. Unavailable/unverified installers must not be advertised as ready. No production direct identity, signing provider or feed is configured by this slice.

Ten offline methods are prepared for actual CLI/byte-validator paths and fake native boundaries: exact output and original preservation, valid signed-MSIX Unicode/XML-escaping Publisher refusal before candidate output with exact original preservation, canonical URI refusal, missing inputs/source/tool pins, runtime identity/layout/native version, archive/manifest bounds, strict signature failures, late original/owned/license/identity/source/tool changes, output mutation/no overwrite, and redacted CLI failure/no report bypass. This is prepared coverage, not executed test or cryptographic evidence. Combined tests and actual Windows parser, approved-media verification, clean-machine VCLibs install/upgrade, Store/direct coexistence, settings/file opening, unsaved-work deferral and interrupted-update acceptance remain required.

Primary contracts: [App Installer descriptor creation](https://learn.microsoft.com/en-us/windows/msix/app-installer/how-to-create-appinstaller-file), [dependency identity/version matching](https://learn.microsoft.com/en-us/uwp/schemas/appinstallerschema/element-dependencies), [optional update settings](https://learn.microsoft.com/en-us/uwp/schemas/appinstallerschema/element-update-settings), [update prompt and later silent application behavior](https://learn.microsoft.com/en-us/windows/msix/app-installer/update-settings), [Desktop Bridge runtime delivery and SDK scope](https://learn.microsoft.com/en-us/troubleshoot/developer/visualstudio/cpp/libraries/c-runtime-packages-desktop-bridge), [downloaded descriptor delivery and protocol policy](https://learn.microsoft.com/en-us/windows/msix/app-installer/app-installer-file-overview).


## Executable and direct-MSIX signing producer (unreleased)

`scripts/sign_windows_msix.py` prepares the complete direct-candidate chain from an exact-source Windows portable ZIP: sign the owned executable, verify signing-only byte equivalence and its native signature, build an unsigned MSIX containing that executable, sign the MSIX, and independently qualify both final signatures. The original portable archive and unsigned Store builder remain unchanged. This is a prepared, unexecuted route; signing is disabled unless `--approve-existing-certificate-signing` is explicitly supplied after protected approval of the public inputs and existing native host.

The supported provider route is one already configured certificate/private-key provider exposed through **CurrentUser/My**. Select its exact uppercase `--certificate-sha1` locator and independently reviewed `--certificate-sha256`; the SHA1 locator does not select the signing digest. A fixed read-only .NET store probe must match the exact public Publisher and leaf SHA256 and find its existing private-key handle before signing. No PFX, password, private-key file, enrollment, certificate import, machine-store selection or trust/policy change is supported. SignPath and Azure-specific provider bridges are not implemented. Their enrollment and any future bridge require separate approval.

All paths, hashes and source/version selections are explicit; native `signtool.exe`, `pwsh.exe` 7.6.5 and `MakeAppx.exe` bytes must be independently selected. SignTool uses SHA256, the explicitly approved HTTPS RFC3161 `--timestamp-uri`, and the existing certificate locator. Native failures, warnings, output/time overruns, missing timestamps or different leaf certificates fail the chain. Tools, original/owned inputs and exact committed source are rechecked; private native output stays in the private work directory. No signing command is registered in ordinary CI or release-candidate jobs; those jobs register only the offline tests.

The x64 PE32+ comparison permits changes only to the four-byte checksum, eight-byte certificate directory and one certificate table appended after zero alignment padding. It preserves every original header, section, gap and overlay byte. The bounded table must be at EOF, contain one revision-2 PKCS7 WIN_CERTIFICATE and one canonical DER sequence, and have only zero alignment padding; malformed, multiple, interior or trailing table data is refused. This comparison does not rely on the native SIP image digest, which can omit bytes. Separate SignTool and fixed native CMS/provider checks establish signature policy. See the [PE certificate-table contract](https://learn.microsoft.com/windows/win32/debug/pe-format#the-attribute-certificate-table-image-only) and [SignTool signing, removal and verification contracts](https://learn.microsoft.com/windows/win32/seccrypto/signtool).

The signed payload's `release.json` is reconstructed from the exact portable input, with the current executable SHA256 and `windows_executable_signing` unsigned/signed hashes. These fields are content metadata, never signature authority. GPL source, notices, application manifest, image assets and complete block-map coverage remain mandatory. Only package signing may add `AppxSignature.p7x` and its content-type declaration; all existing unsigned-MSIX payload and block-map bytes must remain identical.

After approval, invoke the producer with `--source-root`, `--portable`, `--commit`, `--version`, public `--identity`, `--package-family`, `--portable-sha256`, the certificate/timestamp selections above, all three native tool paths and their `--*-sha256` values, a positive `--revision`, and separate fresh private `--work` and `--output` directories outside every checkout. The approval switch is required; there are no provider defaults. Output contains the original unsigned package with signed PE, final signed MSIX, `SHA256SUMS` and an explicitly unready candidate report. Never resume by appending another signature to a failed work directory; preserve it and select fresh work after reconciliation.

For independent read-only verification and descriptor preparation, pass `--require-executable-signature` to `qualify_signed_msix.py` and `prepare_appinstaller.py`. They derive the actual PE from the package, compare it with the exact source-bound portable original, and verify its Publisher, leaf certificate, timestamp and signature separately from the package. No signer report is accepted. Their legacy default still permits the older explicitly unready unsigned-PE qualification for retained acceptance fixtures; that mode does not meet the direct-release executable-signing gate.

`test_windows_executable_signing.py` prepares offline fixtures for byte-level PE and certificate-table refusal, real source/payload/block-map validation through the complete producer and descriptor caller, native-command order and certificate selection, failure/mutation guards, output exclusion and disabled defaults. These fixtures do not sign files or establish native trust. Actual approved identity/provider/HTTPS timestamp service, Windows VERSIONINFO resource integration, trusted native-job attestation, clean installation/upgrade/runtime, update interruption/recovery, unsaved-work protection and public availability remain open. The existing embedded clean build version/commit is preserved; Windows product/version resources are not yet generated. Reports keep producer authentication unverified, installed acceptance unperformed and both readiness values false. No provider, feed, workflow privilege, Store submission or publication is activated.
