# Release archive contents

`scripts/package-files.json` is the reviewed file list for corresponding source and portable packages. Adding a tracked file under `docs/`, `assets/` or `scripts/` does not put it into a release. The packager reads the policy and selected blobs from one exact Git commit; concurrent working-tree changes cannot change that snapshot.

## Selection and source completeness

The `source` list includes application code, shaders, tests, build metadata/scripts, the patched input adapter, required assets, public guides and optional segmentation integration. Every committed file under `src`, `tests`, `examples`, `integrations` and `vendor`, plus root build/Cargo configuration, must be reviewed into the list; otherwise packaging fails. This prevents newly required code or embedded assets being silently omitted. The `portable` list selects user guides, artwork/fonts and integrations from those source inputs. `readme` generates a focused archive README. The repository README, backlog, contributor coordination, checkpoint evidence and provider deployment mapping stay out of release archives. Application documentation screenshots and bundled app artwork remain source assets. Website code, trailers, showcase screenshots and demo PSD/PNG files are maintained separately and are not application release inputs.

Manifest paths must be portable relative names with no traversal, duplicate/case aliases, reserved Windows names or generated-file collisions. Committed source entries must be regular files. Binary and license inputs reject links, junctions and reparse points. A policy is a selection boundary, not a content review: inspect approved text and media before publication.

## Build and package

Use Python 3.11 or newer and stable Rust. From a clean committed checkout:

```sh
python scripts/package.py --check-source
cargo test --locked
python -B scripts/test_package.py
python scripts/build_release.py
python scripts/package.py
```

CI and release candidates check the committed source policy before compiling. The check reports unlisted compiler inputs without creating archives or requiring a built executable. Review new application files into `scripts/package-files.json` in the same commit that adds them.

macOS packaging additionally requires `--sparkle-archive /absolute/downloads/Sparkle-for-Swift-Package-Manager.zip`; acquire the public official archive separately and verify the committed hash pin. Windows and Linux keep their regular-file layouts.

`--binary`, `--registry` and `--output` select the native executable, build's Cargo registry and output directory. Packaging rejects modified or untracked source inputs. There is no dirty-source release override. Extracted source builds do not need repository metadata; packaging a new release requires committing the reviewed inputs and building from that clean checkout first.

Versioned Windows x64, Linux x64 and macOS arm64 archives contain their matching source ZIP. macOS retains its `.app` bundle. Unreleased v0.3 bundles put the committed ICNS and all verified license/notices inside `Contents/Resources` before signing, so they accompany an app dragged to Applications; source and public support guides remain alongside it. PSD/PSB document declarations use the consent-preserving Alternate handler rank. See [macOS installed integration](macos-integration.md) for native delivery and acceptance limits. Source entries use fixed commit timestamps and stored ZIP entries so all platforms produce identical corresponding source bytes. `release.json` records version, full source commit, executable/source checksums, notice checksums and actual signing configuration. Metadata does not prove an arbitrary local binary was compiled from that commit; the exact-source release workflow provides that separate check.

`peerbrush --build-info` prints the exact bounded public metadata embedded by `build.rs`: version, full Git commit and clean/dirty state. It runs without opening a workspace or reading preferences. There is no environment override for the source SHA. Local provenance calls ignore inherited `GIT_*` overrides and disable filesystem-monitor shortcuts; assume-unchanged and skip-worktree inputs cannot establish a clean label. Git metadata is used only when its project root matches the actual crate root; extracted source inside another checkout cannot inherit that checkout's SHA. A build without its own Git metadata remains valid and reports `unknown`; it cannot be packaged as a verified release build.

Git HEAD, active/packed refs, index, tracked inputs/ignore rules and compiler/content directory watches invalidate cached metadata. These watches exclude build/runtime output trees. `build_release.py` also derives a bounded state fingerprint from Git to force recomputation for root-level additions/deletions. It replaces any caller-provided fingerprint; that environment value only invalidates Cargo's cache and never supplies a source label. Plain `cargo build` can retain a cached label after a new unrelated root-level file appears; release packaging independently rejects untracked inputs and hidden index flags. Always use the trusted release builder from a clean checkout for release candidates.

Package policy version 2 requires exactly one valid embedded record matching the expected version and clean original binary commit, and records it in `release.json`. Packaging compares the staged executable's `--version` and `--build-info` outputs to the actual binary record. Portable inspection checks the record directly without executing downloaded code. Historical source with policy version 1 remains inspectable without retroactively claiming embedded provenance; source containing the new build inputs cannot downgrade its policy to bypass checks. Synthetic tests may skip execution, but cannot skip version 2 provenance, architecture or binary privacy checks.

Executable checks reject recognized credential markers and personal build paths, including common UTF-16 forms. `scripts/build_release.py` remaps checkout, home and tool-cache paths before compiling. An embedded record is a self-label, not cryptographic proof of compilation; trusted exact-source CI, final hashes and actual signing verification remain necessary. Targeted privacy patterns cannot detect every private datum, so actual artifacts still need review.

License collection uses exact notice filenames and explicitly declared license files. Cargo's `.cargo-checksum.json` belongs to [directory/vendor sources](https://doc.rust-lang.org/cargo/reference/source-replacement.html); ordinary registry extraction can omit it. When it is absent for a dependency with required component or embedded-font notices, packaging instead verifies the matching `registry/cache/REGISTRY_ID/NAME-VERSION.crate` SHA-256 against committed `Cargo.lock`. It derives root, declared, font and required component notice paths from that verified archive, compares its Cargo metadata and selected notice bytes with the extracted source, and copies only validated notice bytes. A present but invalid checksum inventory never falls back. Missing cached archives, missing required notices, source mismatches, links, unsafe/duplicate paths and malformed input fail packaging; the fallback does not download or extract anything.

The fallback caps compressed input at 64 MiB, decoded tar data at 128 MiB before tar/PAX parsing, members at 10,000, each selected notice/metadata file at 4 MiB and combined notices at 16 MiB. The decoded archive is bounded in memory; only license/metadata members are read into selected output, and private or unrelated files are excluded. `--registry` still selects the build's extracted registry source root; its matching cache is the sibling `cache` directory with the same registry identifier.

Some locked accessibility crates omit their workspace notices from the registry archive. The source inventory carries the original files from the commits recorded in those archives. Portable inspection checks the included notice bytes against the selected source commit, and Nix installs the same files. [AccessKit source](https://github.com/AccessKit/accesskit/tree/405d578cdfd8496ee79020d24cfaac8c29a8a48f) · [AT-SPI source](https://github.com/odilia-app/atspi/tree/f7976706c3c8cff7117fd50d64b4f14693e81e72) · [zbus notice](https://github.com/dbus2/zbus/blob/25876f399b068749c337374505c35e92dc9e5872/LICENSE-MIT)

Policy-listed compiler copyright notices are required from the active build compiler's `rustc --print sysroot` under `share/doc/rust`; install that toolchain's `rust-docs` component before packaging. Missing, empty or linked required runtime notices fail packaging, and archive inspection independently requires their inventory and hashes. Registry crates missing for another target are omitted from the downloaded-dependency inventory; linked dependencies and original license obligations still require review.

Each run stages source, notices and the native ZIP in a fresh temporary directory. Final files are replaced only after validation and collection succeed. Existing extracted folders and unrelated output files are neither copied nor deleted. Failed input or notice checks preserve previous archives.

Ubuntu 24.04 amd64 installation candidates use `scripts/package_deb.py` on the exact same source ZIP. The independently selected portable checksum is required before executable validation. The DEB retains corresponding source/notices and consent-based PSD/PSB desktop integration, with exact tar-header/content inspection and immutable outputs. APT/dpkg owns installation; no signed update repository is configured and receipts explicitly remain unreleased. See [Linux installation scope and native CI acceptance](linux-installation.md).
## macOS DMG candidates

`scripts/package_macos_dmg.py` prepares a compressed, read-only HFS+ DMG from an existing Developer ID signed, notarized and stapled ZIP. It requires the independently verified input SHA-256 and exact source checkout/version, reuses the ZIP/source inspector, and performs native app signature, ticket and Gatekeeper checks. It preserves every original app byte and file mode. The volume contains `PeerBrush.app`, an `Applications` shortcut and a `Documentation and source` folder containing the original GPL source, notices and support material. No installed files or application state are changed.

The script verifies the image with `hdiutil`, mounts it read-only without opening Finder, compares the complete mounted file/directory/link inventory and original bytes/modes, verifies the mounted app again, then detaches it. Final candidate, SHA-256 and provenance receipt are exclusively created and never overwritten; choose a new revision after an existing or incomplete output. Failed work remains in a private temporary staging folder beneath the external output directory for inspection, especially if detachment fails. Do not remove that folder while its image remains mounted.

These are explicitly named `-dmg-candidate-rN.dmg`, with `release_ready: false` and image signing/notarization reported as `not-performed`. `sign_macos.py --format dmg` provides the separately gated Developer ID/timestamp/notarization/stapling path, reusing existing provider preflight and resumable journal logic. It verifies the final signed image and mounted original app/source before writing new final checksums and receipts. Signed DMGs still need installed/quarantined-download acceptance and public release integration; neither candidate nor signed image is automatically accepted by the ZIP-only release/website inventory. See [DMG signing and installation gates](macos-signing.md#dmg-preparation-and-remaining-release-gates). Credential-free macOS CI mounts synthetic unsigned data to check image creation and payload preservation; that test does not establish real publisher signing, notarization or installation acceptance.

## Verification and publication

[Sparkle preparation](sparkle-preparation.md) now supplies the exact dormant macOS framework through an explicit --sparkle-archive input. Only its nine exact links and five reviewed native objects enter the new source-defined package layout. The native signing policy remains unconfigured pending independent macOS audit review, and updater installation remains disabled.

`scripts/test_package.py` exercises tracked and untracked private canaries, stale output, exact nested source, all platform layouts, notice selection, invalid manifests, new compiler inputs, linked inputs and failed builds. `scripts/verify_release.py` requires the three-platform matrix, checks every selected source byte, portable files, recorded notices, executable/source checksums, architecture, macOS bundle identity and archive paths. Unexpected package entries fail inspection.

Run a clean contributor build from the extracted source before publication. Follow [release maintenance](https://github.com/Deftr0y/PeerBrush/blob/main/docs/release-maintenance.md) for media/privacy review, signatures, native verification and authorized publication. Filtering new archives does not remove historical Git blobs or previously published downloads. Archive signatures do not replace Windows Authenticode or Apple signing/notarization.

For an explicitly authorized packaging-only correction, `--binary-source-commit` can retain an existing executable while recording its original full commit. For policy version 2 that SHA must equal the executable's actual clean embedded label. Every application/build input must match that original commit; changed or missing engine inputs reject the correction. The source archive and packaging revision identify the new reviewed policy separately. Preserve executable hashes, publish the correction explicitly and verify replacement assets before removing superseded uploads.

### Dormant Sparkle preparation (unreleased)

The new source-defined macOS layout preserves the complete pinned Sparkle framework, exactly nine relative links, original regular-file/directory modes and full upstream notice inside the app and distribution. Only five native code files and four existing resource seals may change during independently verified nested signing; transformed code must normalize exactly to committed reviewed native hashes. The current native policy is null, so signing is refused before provider access. ZIP extraction, resume/stapling parity and mounted DMG inventory preserve the exact links; every other archive link remains rejected. Credential-free native audit and unsigned bundle/image fixtures are prepared, with no execution or installed update acceptance claimed. See [Sparkle preparation](sparkle-preparation.md).

PB-147 remains open. This prepares dormant packaging and a guarded native component, not a working application updater. Remaining source work includes trusted configured app-owned framework loading, an actual update cycle and progress/release-notes/cancellation UI, verified installation and relaunch outcome, the complete update-specific native termination handoff, and failed/interrupted installation with last-working-app recovery. The final handoff must use the existing atomic finish_update_exit contract; this preparation never calls it, closes a project or invokes an installer block. Supplying a feed, public key or native object policy cannot activate these missing implementations.

## Windows application version resources (unreleased, native checks pending)

The Windows MSVC application build prepares one standard VERSIONINFO resource through the initialized existing Windows SDK compiler, then links the resulting `.res` only into `peerbrush`. No extra crate, tool installation or post-link mutation is used. The compiler and `rcdll.dll` must be present in the selected `WindowsSdkVerBinPath/x64` directory, whose version must match `WindowsSDKVersion`; the prepared host contract accepts the existing SDK 18362 and SDK 26100 layouts. MSVC's normal LINK/CVTRES environment is required. Missing or unsupported selection, failed compilation or missing/bounded-output failure stops the build. Other platforms do not invoke RC; Windows GNU/cross-host resource compilation is not implemented. Native qualification remains pending rather than silently falling back to a resource-less PE.

The checked-in `packaging/windows/version.rc.in` includes public PeerBrush name, graphics-editor description, original/internal executable name, GPL license comment, full Cargo version text and numeric major/minor/patch/zero fixed versions. Prerelease/debug flags follow the Cargo version and build-debug selection. Translation is explicitly English/Unicode. A stable application 0.30.0 has numeric 0.30.0.0; the separate existing MSIX epoch mapping remains 1.30.0.0. No personal checkout, Git branch, signer/account identity or key enters the resource.

`windows_version.py` reuses the PE signing layout and reads the actual bounded RT_VERSION tree and UTF-16 blocks. Portable construction, independent archive inspection and the MSIX/signing-only consumers share that byte validation. New source capability is defined by the committed template and requires `windows_version_resource:1` plus matching metadata. Old source without this capability remains an explicit legacy layout; a report cannot assert or hide the capability. Signing must preserve every existing resource byte. These checks establish metadata/content, never publisher trust or installed readiness.

Prepared fixtures cover numeric/text/flag/translation consistency, missing/duplicate/malformed resources, invalid directories/RVAs/padding/UTF-16 and actual portable/MSIX/signing callers. The Windows-only test additionally reads the actual source-matched Cargo binary through Windows version APIs and records public SDK/tool hashes without paths. Both native CI workflows call the suite after the release build; complete Python discovery retains it locally. Native API availability skips are explicit on other hosts. No compiler, native API, fixture or platform check for this slice has run yet; source review and combined/native qualification are required before claiming completion.

## Package-manager distribution (PB-158, unreleased)

[Issue #59](https://github.com/Deftr0y/PeerBrush/issues/59) is in the next-version scope on `version/0.4`. Package-definition and repository-candidate producers for all four channels, signature attachment/verification tooling, and Scoop/Homebrew/Nix update-ownership integration are in source. Publisher signing, native package qualification and public delivery remain unfinished.

| Channel | Integration to complete | Acceptance still required |
| --- | --- | --- |
| Scoop | Manifest for the reviewed Windows package, immutable versioned URL/hash and package-manager update metadata. | Clean install, upgrade, launch/MCP paths and uninstall with user data preserved. |
| Homebrew | Cask for the reviewed signed/notarized macOS application distribution, with explicit supported architectures and version/hash. | Install, quarantine/Gatekeeper launch, upgrade and uninstall; package-manager update ownership. |
| apt | Signed repository metadata and versioned DEB publication, reusing `scripts/package_deb.py` and existing final-DEB verification. | Repository trust/bootstrap, install/upgrade through apt, dependencies, launch and uninstall. A standalone DEB is not an apt update channel. |
| Nix | Reproducible package expression with pinned source/dependency inputs and required native build/runtime dependencies. | Build, launch/MCP integration and upgrade/rollback on declared supported systems; settings stay outside the immutable store. |

Each channel must identify the actual release version and source commit, retain GPLv3 corresponding source/notices, and use verified artifacts and hashes. Updates for package-manager installations belong to that manager; application update UI must not replace manager-owned files. Preserve documents, preferences and unrelated file associations across installation, upgrades and removal. Package definitions, public availability and actual native acceptance are separate completion steps. Upstream catalog inclusion remains subject to each project's review; do not promise inclusion or publish installation commands before the destination and packages are verified. Coordinate PB-145, PB-147, PB-154 and PB-155, and keep #59 open until the requested channels have verified delivery.

The first installed build in each disposable lifecycle saves editable Shape
fixtures in PSD and PSB at both native depths and writes real tool preferences.
Each checked generation reopens the documents and compares the editable
definitions, native PNG samples and tool settings. After removal, the harness
checks the original file bytes and records fixture digests in its private
receipt. Pre-install sentinel files still check for unwanted installer writes
before the first build can create the real fixtures.

The first selected build must support `discover --connect` and each generation
must support `discover --status`. The harness registers a private Cursor
configuration fixture, verifies the exact backup and preserves an unrelated
server entry. It reconnects through the saved command on each generation,
including Nix rollback, and checks that removal preserves the client files.
This qualifies the saved command through the stdio adapter. Actual Cursor
startup and user approval require separate native client acceptance.

### Scoop and Homebrew definition preparation (unreleased)

`scripts/prepare_package_managers.py` prepares definitions from the public release verifier's inspected assets. It requires a canonical stable tag, the exact source commit/checkout, and a release inventory selected with an independent SHA-256. It reads back the public tag, source, checksums and package bytes before writing anything. The generator can consume verified portable companions even when the website selects Store/DMG/DEB distributions. Website selection policy remains unchanged. Missing required public assets produce no definition for that channel.

The Scoop manifest declares Windows x64, the inspected extraction directory, the CLI executable and a Start menu shortcut. The Apple Silicon Homebrew cask prefers a public signed DMG whose typed qualification binds its bytes to the inspected Developer ID notarized ZIP. It takes the app from the root and support files from `Documentation and source`. A verified notarized ZIP remains usable with its nested layout when there is no bound DMG. The cask exposes the CLI and installs the corresponding source ZIP, GPL license and complete third-party notice tree under its versioned share directory. The receipt records the publisher team and support hashes from the inspected ZIP, and the DMG receipt also records the companion and qualification hashes. Current bundles declare macOS 11. A changed bundle minimum requires reviewing the Homebrew dependency stanza. Definitions contain no preference deletion, file-association takeover or self-update installation hooks.

Contributor preparation, after the selected release bytes are public:

```sh
python scripts/prepare_package_managers.py \
  --inventory /path/to/release-inventory.json \
  --inventory-sha256 VERIFIED_INVENTORY_SHA256 \
  --tag vA.B.C --commit FULL_SOURCE_SHA \
  --output /path/to/new-external-output
```

Use `--channel scoop` or `--channel homebrew` to select one; the default requests both. The output contains `peerbrush.json`, `peerbrush.rb` when their public assets exist, and `package-managers.json` binding all generated hashes to the verified release. Outputs must be new and outside every source worktree; reruns never overwrite an earlier result. Regenerate definitions through this verifier for each new release rather than guessing future archive names or hashes. Package-manager upgrades require reviewed catalog updates; catalog maintenance is not automated by this script.

All output remains unpublished and `release_ready: false`. Archive/hash verification does not establish native signing trust, quarantine acceptance, installed update ownership, a working catalog or installation/upgrade behavior. The runtime ownership observers described below are in source; actual installed upgrade acceptance remains required before release. Signed apt repository publication and qualified reproducible Nix builds remain separate unfinished PB-158 work. No bucket, tap, repository, upstream submission or public installation command is configured here. The format references are the [Scoop manifest specification](https://github.com/ScoopInstaller/Scoop/wiki/App-Manifests) and [Homebrew Cask Cookbook](https://docs.brew.sh/Cask-Cookbook).

### Public Scoop, Homebrew and Nix catalog readback (unreleased)

After authorized delivery to a GitHub catalog, use `scripts/readback_package_catalog.py` from a public-network client. Select the approved public repository, its intended default-branch commit, the exact application source commit and tag, and the reviewed definition receipt digest independently from the catalog under inspection.

```sh
python -B scripts/readback_package_catalog.py \
  --channel scoop --definition /path/to/reviewed-definition \
  --definition-sha256 AUTHENTICATED_DEFINITION_RECEIPT_SHA256 \
  --tag vA.B.C --source-commit FULL_APPLICATION_SOURCE_SHA \
  --repository APPROVED_OWNER/APPROVED_CATALOG --catalog-commit FULL_CATALOG_SHA
```

For Homebrew use `--channel homebrew` and `--team APPROVED_APPLE_TEAM_ID`. The Scoop manifest is `bucket/peerbrush.json`. The cask can be `Casks/peerbrush.rb` or `Casks/p/peerbrush.rb`. For `--channel nix`, publish the files listed in the reviewed definition receipt as the entire root flake tree. Keep the preparation receipt in the private release record. Readback verifies the complete tree, including the source input metadata, directories and executable bits, against the same inventory used by native qualification. The GitHub requests are unauthenticated and read pinned blobs while confirming the approved commit at the default branch before and after the transfer. [GitHub Git references](https://docs.github.com/en/rest/git/refs#get-a-reference) · [GitHub Git trees](https://docs.github.com/en/rest/git/trees#get-a-tree)

The readback also fetches the selected versioned release downloads and verifies their exact hashes. Scoop and Homebrew require both the install archive and corresponding source ZIP. For a Homebrew DMG, readback also verifies the companion notarized ZIP and the DMG qualification receipt against the reviewed definition. Nix requires its pinned source ZIP. The verifier checks the application tag and selected asset metadata again after the transfers, then rechecks the catalog branch.

Keep the JSON result in the private release record. `catalog_publication` records the observed catalog, and `release_downloads_readback` records the downloaded package and source bytes. Publisher trust and native installation and upgrade still require their separate gates. APT uses the repository readback below.

### Native package-manager evidence readback (unreleased)

After the selected runner has written a private native receipt, bind it to the reviewed package definition, exact version and source commit. Supply both receipt hashes from the private release record.

```sh
python3 -B scripts/verify_package_manager_acceptance.py \
  --channel scoop --version A.B.C --source-commit FULL_APPLICATION_SOURCE_SHA \
  --definition /path/to/package-managers.json DEFINITION_SHA256 \
  --native /path/to/private/native-receipt.json NATIVE_RECEIPT_SHA256
```

For Homebrew, supply the approved Developer ID team with `--publisher`. For APT, use the signed candidate receipt as the definition and supply the approved primary fingerprint with `--publisher`. For Nix, select `nix-package.json` and keep its pinned `source-inputs.json` beside it.

For upgrade evidence, supply the independently selected earlier definition hash with `--previous-definition-sha256`. In archived APT mode this selects the historical baseline. The verifier separately checks the current signed receipt's immediate predecessor. For the initial APT repository upgrade from an installed standalone DEB, supply `--standalone-deb-sha256` with the earlier package's independently authenticated digest. The output records the selected lifecycle, including the Nix profile type and rollback result. Keep fresh-install and upgrade evidence separately when both are required. APT and Nix desktop receipts require `--desktop` during native acceptance. The readback requires document launch, native sample retention, and package-specific lifecycle evidence. Publication, public publisher trust and release readiness have separate gates.

### Reconcile all four package channels (unreleased)

After public readback and native qualification, select each channel's public,
clean-install and upgrade readbacks and their hashes from the private release
record. The clean install and upgrade must both qualify the current package.
The upgrade readback names its earlier definition separately.

```sh
python3 -B scripts/verify_package_manager_delivery.py \
  --version A.B.C --source-commit FULL_APPLICATION_SOURCE_SHA \
  --evidence scoop SCOOP_PUBLIC SHA256 SCOOP_INSTALL SHA256 SCOOP_UPGRADE SHA256 \
  --evidence homebrew BREW_PUBLIC SHA256 BREW_INSTALL SHA256 BREW_UPGRADE SHA256 \
  --evidence apt APT_PUBLIC SHA256 APT_INSTALL SHA256 APT_UPGRADE SHA256 \
  --evidence nix NIX_PUBLIC SHA256 NIX_INSTALL SHA256 NIX_UPGRADE SHA256
```

The verifier requires the same exact package filename and matching package and
corresponding source-archive hashes in each channel's public, clean-install
and upgrade readbacks. The public catalogs must agree on the source archive
size. The initial APT generation can use a verified standalone DEB upgrade as
its baseline. It also checks the applicable publisher or signing key, APT metadata validity, the Scoop
desktop launcher and Connect AI checks, and Nix rollback evidence. Nix clean
install and upgrade must reproduce the same flake snapshot, package output and
dependency closure. APT clean install and upgrade must both use the public
HTTPS repository and match the independently verified repository URL, bootstrap
URL and signed `InRelease` digest. Recreate older readbacks that lack the package
filename, source-archive hash, Homebrew DMG companions, APT validity start time or Nix output hashes from
their authenticated inputs.
Keep the resulting JSON private. Reconciliation records the selected observations.
Release authorization and native publisher trust still use their own gates.

### APT repository candidate preparation (unreleased)

`scripts/prepare_apt_repository.py` prepares the `noble/main` amd64 repository tree for the declared Ubuntu 24.04 target. It reuses the final-DEB inspector and its exact validated control fields, requiring the independently selected DEB/portable hashes, stable application version, full source commit and approved public package maintainer. It never executes the downloaded application or installs a package. The selected DEB already contains the verified complete corresponding source and notices.

The fresh external output contains `repository/pool/main/p/peerbrush/`, `repository/dists/noble/Release`, `main/binary-amd64/Packages`, deterministic `Packages.gz`, and SHA-256 by-hash copies of both indexes. Release dates are explicit canonical UTC inputs, with positive validity of at most fourteen days. The generated indexes retain the original dependency fields and bind the installed package filename, size and SHA-256. The final bounded copy rejects changed, truncated or growing package input; a failure cannot emit a successful `apt-repository.json`. Existing outputs are immutable.

Contributor preparation from the exact committed source checkout:

```sh
python scripts/prepare_apt_repository.py \
  --portable /path/to/PeerBrush-VERSION-Linux-x64.zip \
  --portable-sha256 VERIFIED_PORTABLE_SHA256 \
  --package /path/to/peerbrush_VERSION-1_amd64.deb \
  --package-sha256 VERIFIED_DEB_SHA256 \
  --commit FULL_SOURCE_SHA --version VERSION \
  --maintainer 'PUBLIC_MAINTAINER_NAME <PUBLIC_MAINTAINER_EMAIL>' \
  --date YYYY-MM-DDTHH:MM:SSZ --valid-until YYYY-MM-DDTHH:MM:SSZ \
  --output /path/to/new-external-output
```

The unsigned producer prepares the current generation. Signature attachment authenticates its exact Release bytes and can carry forward earlier pool files and by-hash index generations. A publisher must promote Release and index changes atomically, refresh metadata before expiry, and verify actual public bytes. Disposable apt trust/bootstrap, fresh install, real version upgrade, uninstall/retention and manager-owned update acceptance are still required.

The format references are [Debian repository structure](https://wiki.debian.org/DebianRepository/Format) and [apt-secure](https://manpages.debian.org/unstable/apt/apt-secure.8.en.html). Key validation must not rely on `gpgv` alone: its [official documentation](https://www.gnupg.org/documentation/manuals/gnupg/gpgv.html) states that it assumes supplied keys are trustworthy and does not check key expiry or revocation. No signing key or public repository has been configured by this development work.

### APT signature attachment (unreleased, native execution pending)

`scripts/attach_apt_signatures.py` consumes the unsigned candidate, independently selected DEB/portable inputs and externally produced `InRelease`/`Release.gpg`. It invokes no signing operation and accesses no default/private key store. Select the unsigned `apt-repository.json`, both signature files, one ASCII-armored current public certificate and the reviewed native GnuPG executable with independent SHA-256 hashes. The full uppercase primary fingerprint is a separate required input. Publisher approval and certificate freshness must be established independently; supplying a fingerprint is not proof of publisher identity.

The attachment path reconstructs the exact DEB/source/index plan and rejects a noncanonical or changed manifest, extra files, links, special entries and changed repository bytes. Public-certificate inspection rejects private material before import. GnuPG exports a minimal binary public keyring for the selected fingerprint, then imports the export into a second empty home to verify both signatures against the exact Release bytes. Both homes disable default options, agent/dirmngr autostart and automatic key retrieval/import.

The current Ubuntu 24.04 contract accepts v4 RSA (at least 2048 bits), ECDSA (at least 256 bits) and EdDSA (at least 255 bits), with SHA-256/384/512 metadata signatures. Each signature needs one successful native result linked to the selected primary fingerprint. Invalid/revoked/expired or disabled keys, weak/unsupported algorithms, ambiguous results, future signatures and keys/signatures expiring before Release expiry fail. Primary and actual signing-subkey metadata are both checked. Only the supplied certificate's validity/revocations are visible offline; the independently approved current certificate must include the publisher's latest revocations and expiry changes.

Contributor attachment, after producing signatures externally for the exact unsigned Release file:

```sh
python scripts/attach_apt_signatures.py \
  --portable /path/to/PeerBrush-VERSION-Linux-x64.zip \
  --portable-sha256 VERIFIED_PORTABLE_SHA256 \
  --package /path/to/peerbrush_VERSION-1_amd64.deb \
  --package-sha256 VERIFIED_DEB_SHA256 \
  --commit FULL_SOURCE_SHA --version VERSION \
  --maintainer 'PUBLIC_MAINTAINER_NAME <PUBLIC_MAINTAINER_EMAIL>' \
  --unsigned /path/to/unsigned-candidate \
  --unsigned-sha256 VERIFIED_UNSIGNED_MANIFEST_SHA256 \
  --inrelease /path/to/InRelease --inrelease-sha256 VERIFIED_INRELEASE_SHA256 \
  --detached /path/to/Release.gpg --detached-sha256 VERIFIED_DETACHED_SHA256 \
  --public-key /path/to/publisher.asc --public-key-sha256 VERIFIED_PUBLIC_KEY_SHA256 \
  --fingerprint APPROVED_FULL_UPPERCASE_PRIMARY_FINGERPRINT \
  --repository-uri https://packages.example.invalid/peerbrush/apt \
  --tool /path/to/reviewed/gpg --tool-sha256 VERIFIED_GNUPG_SHA256 \
  --initial \
  --output /path/to/new-signed-candidate
```

Replace the reserved example URI with the approved public HTTPS repository base. Supply a lowercase DNS host and optional unescaped path using the default HTTPS port. The producer records the URI without contacting it. `repository/` contains the package, indexes, Release and its two signatures. `bootstrap/` contains `peerbrush.sources` and `peerbrush-archive-keyring.gpg` as separate client setup files. For native acceptance, independently authenticate the publisher, URI and fingerprint and check both bootstrap hashes before installing the files at `/etc/apt/sources.list.d/peerbrush.sources` and `/etc/apt/keyrings/peerbrush-archive-keyring.gpg`. The keyring must be readable by `_apt`. The Deb822 entry scopes trust to the selected keyring and fingerprint, the `noble` suite, the `main` component and `amd64`. Date and expiry checks are explicit. See [Ubuntu 24.04 APT source options](https://manpages.ubuntu.com/manpages/noble/man5/sources.list.5.html).

Use `--initial` for a repository's first publication. For each later candidate, replace it with `--previous /path/to/last-published-candidate AUTHENTICATED_RECEIPT_SHA256`. Obtain the digest of that candidate's `apt-repository.json` from the independently verified publication record. The digest anchors the prior inventory and client trust. The previous candidate must be for the same URI, suite, component and architecture, with an older Release date and an equal or lower version. Attachment and stage verification require the same approved primary fingerprint and exact client keyring, which existing installations already trust. Same-version metadata refreshes must reuse the exact DEB bytes.

The producer copies the previous content-addressed indexes and versioned DEBs, including files that candidate already retained. It verifies each inherited file's size and digest while copying. The new signed Release and its canonical indexes remain byte-for-byte as generated for the new package. Clients can still fetch the immutable URLs from the two earlier index generations once that history exists. This follows [Debian's by-hash repository format](https://wiki.debian.org/DebianRepository/Format#indices_acquisition_via_hashsums_.28by-hash.29). The publisher still needs to stage the complete tree and verify those older URLs across an atomic switch.

Retention is cumulative and the receipt is capped at 512 KiB. A candidate that reaches the cap fails before writing output. Pruning needs a reviewed policy that preserves the supported index and package URLs.

The `PeerBrushSignedAptRepositoryV3` receipt records the predecessor receipt digest for an update, separate repository and bootstrap inventories, input/tool hashes and the selected public fingerprints. Output uses a fresh directory. The producer rechecks repository bytes after signature verification, verifies the copied DEBs and checks metadata validity before writing a successful receipt. Reports keep `release_ready: false`, public availability unverified, and native install/repository-update acceptance unperformed. Real GnuPG and APT acceptance, publisher approval, trusted delivery and key rotation, and atomic publication remain pending.

### APT publication-stage readback (unreleased)

Copy the signed candidate's `repository/` and `bootstrap/` into a dedicated offline stage and freeze it before verification. Keep candidate receipts outside that tree. Independently select the candidate receipt digest, repository URI and primary fingerprint from the release record. For an initial candidate, run:

```sh
python scripts/verify_apt_stage.py \
  --stage /path/to/frozen-stage \
  --receipt /path/to/selected-candidate/apt-repository.json \
  --receipt-sha256 AUTHENTICATED_CANDIDATE_RECEIPT_SHA256 \
  --repository-uri https://packages.example.invalid/peerbrush/apt \
  --fingerprint APPROVED_FULL_UPPERCASE_PRIMARY_FINGERPRINT \
  --initial
```

For an update, replace `--initial` with `--previous /path/to/last-published/apt-repository.json AUTHENTICATED_PREDECESSOR_RECEIPT_SHA256`. The verifier requires each previously published immutable URL and digest to be present in the candidate inventory, then verifies the actual staged bytes and the scoped Deb822 configuration. It rejects unexpected files, links, changed bytes, wrong release pins and expired metadata. Keep its JSON output with the private publication record. `local_stage: verified` covers the observed offline stage only.

After an approved atomic switch, repeat the command from a public-network client with `--public-bootstrap-uri https://packages.example.invalid/peerbrush/client`, replacing the example with the independently approved HTTPS base for the two bootstrap files. The command reads every current and retained repository URL from `--repository-uri` and both client setup files from the bootstrap base. Direct unauthenticated HTTPS GETs verify server identity and require a full, exact-size, matching SHA-256 response. The check reads `InRelease` before and after the other objects to detect a changed signed generation during readback. The endpoint must serve each URL directly, without redirects or content encoding. See [Ubuntu's APT HTTPS transport](https://manpages.ubuntu.com/manpages/noble/man1/apt-transport-https.1.html) for its TLS defaults.

`public_availability: exact-bytes-verified` records a successful observation from that client. Native APT install, upgrade, uninstall, and preservation of documents and settings remain separate acceptance checks.

### APT install and upgrade acceptance (unreleased)

`scripts/test_apt_repository_upgrade.py` can qualify a clean install of any currently valid signed candidate. Use `--install-only STAGE RECEIPT SHA256` in place of both version selections below. If the signed candidate has repository history, also supply `--install-previous RECEIPT SHA256` with its authenticated immediate predecessor. The harness verifies that history without installing the older package. The clean-install receipt binds the predecessor and covers missing-key refusal, installation, purge and user-file preservation.

Upgrade acceptance can start from two real, currently valid signed candidates with increasing application versions and an unchanged approved keyring. Supply the same authenticated receipt pins used for stage verification. The newer candidate must name the older one as its predecessor. Pass `--older-previous RECEIPT SHA256` if the older candidate itself has a predecessor.

Run inside a fresh root Ubuntu 24.04 amd64 Docker container with the DEBs' declared runtime dependencies, Python 3.11 or later, `xdg-utils` and the existing installed-DEB acceptance prerequisites already installed. Set `PEERBRUSH_DISPOSABLE_PACKAGE_TEST=1`, mount inputs read-only and use a private evidence directory. The acceptance run can use `--network none` after preparing that container. For example, inside the container:

```sh
python3 -B scripts/test_apt_repository_upgrade.py \
  --older /inputs/old-stage /inputs/old-receipt.json AUTHENTICATED_OLDER_SHA256 \
  --newer /inputs/new-stage /inputs/new-receipt.json AUTHENTICATED_NEWER_SHA256 \
  --repository-uri https://packages.example.invalid/peerbrush/apt \
  --fingerprint APPROVED_FULL_UPPERCASE_PRIMARY_FINGERPRINT \
  --output /evidence/apt-upgrade.json
```

The harness verifies and copies the complete inventories, uses private APT lists and one local file source with the verified scoped keyring, and checks that APT refuses the source without its key. It installs by package name. For an upgrade it switches the local repository generation atomically, refreshes the indexes and upgrades by package name. Each installation verifies the executable, corresponding source and CLI build provenance. The running build must report `LinuxPackage` and leave in-app update eligibility disabled. A headless launch under the disposable non-root user checks the editable files and preferences. Purge must remove the launcher and MIME registration while user fixtures and default-editor choices remain byte-identical. Its private receipt covers this local transport. Public HTTPS delivery, publisher approval and key rotation have separate gates. [Ubuntu APT source options](https://manpages.ubuntu.com/manpages/noble/man5/sources.list.5.html)

Add `--desktop` to qualify document startup after each selected generation is installed, including an archived baseline. Prepare the disposable container with GIO, Xvfb, xauth, D-Bus, Openbox, wmctrl, xdg-mime and software graphics support. The harness starts the display under the synthetic non-root account, verifies that the installed MIME database recognizes the four Unicode-named PSD and PSB fixtures, and launches the desktop entry authenticated by the selected DEB. It requires separate clean tabs, exact editable sources and native samples, a decodable workspace frame and clean close of the authenticated process. It uses fresh private settings with a synthetic first-launch choice and leaves the fixture's original default editor untouched. Physical desktop acceptance remains separate. [GIO application launch](https://docs.gtk.org/gio/method.AppInfo.launch.html)

Once the older repository metadata has expired, replace `--older` with `--archived-baseline OLD_SIGNED_RECEIPT OLD_SHA256 HISTORICAL_INSTALL_RECEIPT HISTORICAL_INSTALL_SHA256`. The historical install receipt must be the authenticated `PeerBrushAptInstallV1` receipt for that signed package. The harness verifies the old DEB at its immutable pool URL in the current stage, restores it locally as the installed baseline and then upgrades by package name through the fresh signed indexes. Supply `--newer-previous RECEIPT SHA256` if the active stage names a refreshed repository generation as its immediate predecessor. The current signature, date, expiry, scoped keyring and inherited-inventory checks still apply. The private archived-upgrade receipt records both historical and current evidence. [Ubuntu APT command reference](https://manpages.ubuntu.com/manpages/noble/man8/apt-get.8.html)

For an initial repository generation, use `--standalone-baseline DEB VERSION COMMIT DEB_SHA256 EXECUTABLE_SHA256 SOURCE_SHA256` with `--newer STAGE RECEIPT SHA256`. Select the earlier standalone DEB and its identity from the reviewed release record. The harness installs that exact package locally, then upgrades by package name through the signed repository. The repository qualification still needs a separate clean install of the current generation.

After an approved public switch, add `--public-bootstrap-uri https://packages.example.invalid/peerbrush/client` to `--install-only`, `--archived-baseline` or `--standalone-baseline` with the approved bootstrap URL. Use a fresh disposable container with network access and the system CA certificates installed. The harness checks the public inventory, points its isolated APT configuration at the approved HTTPS repository and checks that APT refuses it before the scoped keyring is installed. It hashes the `InRelease` file APT accepted and the exact `.deb` APT downloaded before installing that cached version. For upgrades, APT fetches the newer package over HTTPS after the baseline is installed locally. The native receipt records both HTTPS URLs and the signed metadata digest. Delivery reconciliation compares these fields with the independently verified public readback for each lifecycle that used HTTPS. [Ubuntu APT HTTPS transport](https://manpages.ubuntu.com/manpages/noble/man1/apt-transport-https.1.html)

### Nix update ownership foundation (unreleased)

On Linux, PeerBrush now classifies an observed process image inside a valid default `/nix/store/DIGEST-NAME` object as Nix-managed, including the native executable behind profile or wrapper launch paths. The update panel identifies Nix as the owner of version changes and rollbacks. This uses the OS executable observation and the [standard Nix store-path format](https://nix.dev/manual/nix/2.33/protocols/json/store); it invokes no package-manager process and trusts no environment-supplied ownership label. Lookalike prefixes, malformed digests and lexical escapes do not establish Nix ownership. Custom store roots are not yet recognized. An unavailable Linux executable observation remains Unknown.

This ownership change does not deliver a Nix package or establish an available catalog update. A reproducible package still needs its exact source archive/commit/hash, Cargo.lock-derived dependency inputs, pinned Nixpkgs/native toolchain, required font/graphics/input/runtime dependencies, corresponding source/notices, desktop/CLI/MCP integration and native build/install/upgrade/rollback acceptance. User settings and documents must remain outside the immutable store.

The current native Linux build policy selects Rust 1.92.0, Clang 18.1.3, CMake 3.31.6 and Ninja 1.13.2. A Nix recipe must preserve the reviewed JPEG XL tool checks; an alternate native tuple needs explicit review and qualification. No Nix toolchain or build has been qualified by this ownership change. Source archives without their own Git metadata correctly produce unknown embedded commit provenance; package-source pins must not be substituted into a forged clean build label. The source-package producer described below now supplies Nix definitions; toolchain qualification, runtime acceptance and publication remain unfinished PB-158 work.

Linux Nix Connect AI registration selects a matching conventional or verified custom profile launcher and migrates previous default-store wrapped commands through the existing backed-up client registration path. The running build and observed profile generation establish the match. Selection reads links and metadata. See [Nix profile launch paths](mcp.md#nix-profile-launch-paths-unreleased) for supported layouts. Actual install, upgrade and rollback acceptance remains pending.

### Nix source-package definition preparation (unreleased)

`scripts/prepare_nix_package.py` prepares `flake.nix`, `default.nix`, `package.nix`, the actual Cargo.lock, source-input metadata and reviewed portable support files from a publicly verified exact-source release. It uses the existing public tag/source/checksum/package verifier, so an unpublished or changed release cannot silently supply new definitions. The source must contain the committed Nix template and native build contract. Source URLs and raw ZIP SHA-256/SRI values come from the verified release.

The default expression pins an explicitly selected full lowercase Nixpkgs Git commit for Rust and the runtime closure. `--clang-nixpkgs-commit`, `--cmake-nixpkgs-commit` and `--ninja-nixpkgs-commit` optionally select separate fixed revisions for those build tools. Omitted pins inherit the base revision. The source-input receipt records all effective pins. Every import uses native x86_64-linux with empty user config and overlays. Nix still must fetch and evaluate the revisions. Cargo dependencies use the exported lockfile and fixed registry checksums. Local patched crates remain in the verified complete source archive. Git dependencies and alternate registries require a reviewed fetch adapter.

The flake exposes the same package as `packages.x86_64-linux.peerbrush` and as the default for that system. The receipt includes the hash of `flake.nix`. Once a channel is approved and published, installs from its moving flake reference can receive `nix profile upgrade` updates while each release still pins its own build inputs. An install from a fixed flake revision stays pinned. Native acceptance must exercise an upgrade between two releases from the same channel reference, then roll it back. See the [Nix profile upgrade reference](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-upgrade.html).

Contributor preparation, after the exact release inputs are public:

```sh
python scripts/prepare_nix_package.py \
  --inventory /path/to/release-inventory.json \
  --inventory-sha256 VERIFIED_INVENTORY_SHA256 \
  --tag vA.B.C --commit FULL_SOURCE_SHA \
  --nixpkgs-commit REVIEWED_FULL_NIXPKGS_SHA \
  --output /path/to/new-external-output
```

The derivation uses `buildRustPackage` and disables the top-level CMake and Ninja hooks. Cargo drives the JPEG XL build using the existing `scripts/jxl_build.py` qualifier. The hook supplies a private temporary GITHUB_ENV destination, imports the qualified paths through quoted assignments and removes the temporary file. The exact Rust, Clang, CMake and Ninja checks apply to the selected packages regardless of pin layout. The selected Rust compiler must supply the complete notice set under its sysroot or its own split documentation output. Missing or empty notices fail packaging. [Nixpkgs CMake hook](https://github.com/NixOS/nixpkgs/blob/master/doc/hooks/cmake.section.md), [Nixpkgs Ninja hook](https://github.com/NixOS/nixpkgs/blob/master/doc/hooks/ninja.section.md)

Font/graphics/input libraries, the CLI and the known Nix wrapper accompany the application. The derivation installs the reviewed desktop/MIME definitions with paths inside its output, validates the desktop file and retains portable artwork/fonts/guides. Read-only Nix store references keep the exact source ZIP and complete locked Cargo dependency sources/licenses in the installed closure; required Rust runtime notices are copied separately. The native image receives the existing binary privacy check. No desktop default, user preference, document, AI-client configuration or system trust setting is changed by the derivation.

Source input metadata records the verified release source and selected Nixpkgs/lock/toolchain inputs. It does not forge a Git checkout or override `--build-info`: extracted source correctly reports unknown embedded Git provenance. That is separate from source/closure pin verification. Generated outputs must be fresh and outside every source worktree, remain unpublished and keep `release_ready: false`. Tests are not automatically run by the derivation; the actual essential/native release gate remains mandatory.

Native Nix qualification and public delivery remain pending. The build references are the [Nixpkgs Rust packaging guide](https://github.com/NixOS/nixpkgs/blob/master/doc/languages-frameworks/rust.section.md) and [fixed-revision Git fetch documentation](https://nix.dev/manual/nix/2.35/language/builtins.html#builtins-fetchGit).

### Nix native profile acceptance (unreleased)

For a first release, replace both version selections below with
`--install-only DEFINITION RECEIPT_SHA256`. The harness builds and rebuilds
that candidate, checks the private profile, live MCP and Nix update ownership,
and verifies removal and user-file preservation. Its first-install receipt
leaves upgrade and rollback pending.

Use a disposable x86_64 Linux runner with Python 3.11 or later and a dedicated local Nix 2.35 daemon matching the reviewed CLI. Verify the daemon's configuration independently before running. It needs Linux sandboxing with fallback disabled, local builders, store optimization disabled and access to the fixed build inputs. Run the harness as an ordinary non-root user. Select the resolved Nix executable and supply its independently checked SHA-256. The executable and checked store files must be regular files without hard links. [Nix daemon store](https://nix.dev/manual/nix/2.35/store/types/local-daemon-store.html) · [Sandbox configuration](https://nix.dev/manual/nix/2.35/command-ref/conf-file.html#conf-sandbox)

```sh
PEERBRUSH_DISPOSABLE_NIX_TEST=1 python3 -B scripts/test_nix_profile_upgrade.py \
  --older /path/to/older-definition OLD_RECEIPT_SHA256 \
  --newer /path/to/newer-definition NEW_RECEIPT_SHA256 \
  --nix /path/to/actual/nix --nix-sha256 NIX_EXECUTABLE_SHA256 \
  --output /path/to/private-output/nix-profile-acceptance.json
```

Select two definitions of increasing versions from the verified source preparation above. The harness snapshots only the files and executable bits authenticated by each definition receipt. It builds and checks both generations, compares native rebuilds through `nix build --rebuild`, and installs, upgrades, rolls back and removes PeerBrush in a private profile. A single local flake reference supplies both generations. Each installed generation is checked for its executable, source and dependency closure, reported version and a live MCP call through the stable launcher. The running build must report `NixPackage` and leave in-app update eligibility disabled through installation, upgrade and rollback. The harness also checks byte-for-byte preservation of private preferences and a synthetic document. It writes a fresh receipt under an existing private directory after success. Temporary profile and user state are removed, while realized store objects follow normal Nix retention. [Nix rebuild reference](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-build.html) · [Nix profile upgrade reference](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-upgrade.html)

The harness verifies the installed store closure under the runner's Nix trust configuration. The pinned source ZIP and Cargo vendor directory must be direct references of the installed output. The vendor lockfile must match the reviewed definition, and every locked crates.io entry must resolve to a referenced store directory with matching checksum metadata. The receipt records the closure digest, store object count and crate count for each generation. Rebuild must reproduce the closure, and rollback must restore the original one. [Nix path metadata](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-path-info.html) · [Nix store verification](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-store-verify.html)

On install, upgrade and rollback, the private profile's desktop entry and Photoshop MIME definition must resolve to the selected store output. The harness checks the desktop entry, MIME definition and icon against the pinned source ZIP, allowing the derivation's launcher and icon path substitutions in the desktop entry. Removal must clear both profile links along with the launcher. [Nix profile layout](https://nix.dev/manual/nix/2.35/command-ref/files/profiles.html)

The harness reads the numbered generation selected by the profile link and checks that the on-disk manifest matches `nix profile list --json`. It verifies the active package output and flake references on install and upgrade, the exact original generation on rollback, and an empty later generation on removal. The private receipt records generation numbers and flake snapshot NAR digests. [Nix profile list reference](https://nix.dev/manual/nix/2.35/command-ref/new-cli/nix3-profile-list.html)

Run with `--custom-profile` and a separate receipt to qualify a named profile. Connect AI registers the absolute profile launcher without its directory on PATH. Status then runs through the store wrapper with the profile’s `share` directory in XDG_DATA_DIRS and rediscovers the same launcher. The harness reuses that saved command after upgrade and rollback.

The default run covers the local profile lifecycle headlessly. For desktop acceptance, add `--desktop` and run the Python command under `xvfb-run -a -s '-screen 0 1600x1000x24 -nolisten tcp' dbus-run-session --`. Use the same disposable runner with GIO, Openbox, wmctrl, Xvfb, xauth, a local session bus and software graphics available. The harness starts a private window manager and supplies four Unicode PSD/PSB files to GIO through the selected profile's desktop entry. Install, upgrade and rollback must each open independent clean tabs with exact editable sources and native 8- and 16-bit samples, render a workspace PNG and close the authenticated application normally. The probe starts with a synthetic first-launch preference and uses fresh private settings and runtime directories for each generation. [GIO application launch](https://docs.gtk.org/gio/method.AppInfo.launch.html)

Native execution, default file associations, physical graphics, fonts, dialogs, clipboard, GUI Connect AI and external client reconnect still require qualification. Catalog review, publisher trust and actual public delivery remain separate steps.


### Scoop runtime ownership and MCP paths (unreleased)

Windows process observations now distinguish the generated x64 Scoop layout using its actual version directory and bounded regular installed receipts. The update panel leaves version replacement with Scoop, independently of public release discovery; local receipts establish neither publisher trust nor bucket update availability. MSIX package identity retains precedence. Missing or inconsistent Scoop records remain Unknown.

Connect AI uses a matching `current` launcher rather than retaining a version directory that can disappear after an upgrade. Source regression coverage includes native junction retargeting and backed-up client migration; see [Scoop launch paths](mcp.md#scoop-launch-paths-unreleased) for supported receipt layouts and limits. No Scoop process, install, update, system setting or client configuration is changed by ownership observation. Compilation and regression source do not establish real package installation, publisher qualification, upgrade/removal or AI-client acceptance. The generated definition remains unpublished.

### Scoop native upgrade acceptance (unreleased)

For a first release, replace both version selections below with
`--install-only DEFINITION RECEIPT_SHA256`. This requires the installed build
to report Scoop update ownership and checks the desktop launcher, live MCP,
removal and user-file preservation. Its first-install receipt leaves upgrade pending.

Use a disposable native Windows x64 VM, a signed-in desktop session under a fresh standard user, Windows PowerShell, and 64-bit Python 3.11 or later. The runner uses a reviewed stock Scoop source ZIP selected by independent SHA-256. Scoop uses private app, config and cache paths. It creates and removes a real Start menu shortcut under the test account. Use a fresh test account with local folders and discard the VM when finished. The test downloads the exact release assets over HTTPS through Scoop with hash checking enabled. [Scoop install command](https://github.com/ScoopInstaller/Scoop/blob/master/libexec/scoop-install.ps1) · [Scoop update command](https://github.com/ScoopInstaller/Scoop/blob/master/libexec/scoop-update.ps1)

```powershell
$env:PEERBRUSH_DISPOSABLE_SCOOP_TEST = '1'
python -B scripts/test_scoop_upgrade.py `
  --older C:\QA\older OLD_RECEIPT_SHA256 `
  --newer C:\QA\newer NEW_RECEIPT_SHA256 `
  --scoop-archive C:\QA\scoop-source.zip --scoop-sha256 SCOOP_SOURCE_SHA256 `
  --output C:\QA\private\scoop-acceptance.json
```

Select two generated definitions of increasing versions and independently authenticate both `package-managers.json` receipts. Regenerate older definition receipts that lack `support_files` before native acceptance. Both binaries must support the installation observation, headless MCP commands and rendered workspace capture. The newer build must report Scoop update ownership. The harness installs the older release from a private local bucket, replaces that bucket's manifest, upgrades through Scoop and uninstalls. It checks installed binary, corresponding source, GPL license and third-party notice hashes against the reviewed archive. It also checks live MCP through the same `current` launcher and byte-for-byte preservation of user fixtures. Removal checks the installed version tree and launcher paths. The receipt records whether the older build exposed an owner observation. Public bucket delivery requires independent readback.

Each installed generation must match the hash of its actual cached ZIP. The harness checks that `current` is a junction to the selected version, the executable shim launches that version, and the Start menu shortcut uses `current` as its working directory with no extra arguments. After upgrade it rechecks the previous version's installed payload and receipts before uninstall checks removal of the whole app tree. [Scoop junctions and shims](https://github.com/ScoopInstaller/Scoop/blob/master/lib/install.ps1) · [Scoop cache naming](https://github.com/ScoopInstaller/Scoop/blob/master/lib/core.ps1)

The desktop probe starts the shortcut's verified target and working directory with a private `--state-dir` and private client configuration folders. It supplies four native PSD and PSB files with Unicode names on the launch command. The app must open each path in an independent clean tab, retain the editable sources and export the original native 8- or 16-bit samples. On the first launch, wait for the terminal prompt and click **Connect AI** once in PeerBrush within 120 seconds. The harness checks that the app backs up and migrates the fixture Cursor registration, preserves its unrelated server and creates discovery data. It opens two live MCP sessions through the saved command. For an upgrade, leave Connect AI alone on the second launch. The harness reuses the first registration byte for byte and opens fresh MCP sessions against the upgraded desktop in the same private workspace.

A second probe opens the installed Start menu shortcut through Windows Shell using private AppData and temporary paths. It requires a new process handle for the authenticated installed executable, a live connection in the isolated workspace, a decodable workspace PNG and a clean close through the native window. The disposable account must have no existing default PeerBrush workspace. Offline receipt verification requires both the Shell and Connect AI evidence, so older desktop receipts need a fresh acceptance run. Native execution, publisher trust and reconnect through an actual installed AI client remain pending. [Windows Shell execution API](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw)


### Homebrew runtime ownership and updater exclusion (unreleased)

The macOS update panel now distinguishes the generated arm64 Homebrew cask through a bounded installed receipt and the Caskroom app link resolving to this running bundle. A matching Homebrew installation owns version changes independently of public release discovery. Local records establish neither publisher identity nor tap update availability. See [Homebrew launch paths](mcp.md#homebrew-launch-paths-unreleased) for the supported prefix, receipt and launcher scope.

Homebrew-managed or uncertain app ownership does not acquire built-in updater configuration. Manual native checks re-observe the app, and shared native installed-app verification plus retained verification rechecks refuse package-managed/uncertain targets. Existing signing, source, settings-profile, checkpoint and final-control requirements remain mandatory; the ownership observation supplies no update permission. Human artwork/settings and package-manager records are not repaired or changed by observation. An upgrade or ownership change that invalidates a pending observation remains held.

Source fixtures cover POSIX moved-app/CLI links, independent direct copies, receipt bounds and required-field duplicates, missing/stale records, unsupported architecture, launcher refusal, ambiguous prefixes and human-file preservation. Shared discovery regression source also covers backed-up migration of an older Caskroom command. Windows compile-only checking covers the platform-neutral observer and shared enum/UI callers; macOS-native compilation, POSIX execution, installed Homebrew upgrade/removal/client checks and actual tap publication are still required. The cask definition remains unpublished, and the wider release gate remains open.

### Homebrew native upgrade acceptance (unreleased)

For a first release, replace both version selections below with
`--install-only DEFINITION RECEIPT_SHA256`. This requires the installed app to
report Homebrew update ownership and retains the signing, quarantined launch,
rendered workspace, live MCP and removal checks. Its first-install receipt
leaves upgrade pending.

Use a disposable Apple Silicon Mac with a logged-in standard user, active Gatekeeper, Apple command-line tools, Python 3.11 or later and a clean Homebrew checkout at `/opt/homebrew`. Select the independently reviewed Homebrew commit and Apple Developer ID team. Use a fresh installation with no PeerBrush cask, CLI link, versioned support directory or existing app. The runner uses Homebrew's real prefix for its private fixture tap and installed cask, with a private Applications directory, cache and user fixtures. It installs the exact cask from a fully qualified fixture name and leaves Homebrew's normal tap trust and quarantine checks in place. Approve any normal first-launch dialog for the selected signed app in the disposable session. [Homebrew Tap Trust](https://docs.brew.sh/Tap-Trust) · [Homebrew command reference](https://docs.brew.sh/Manpage)

```sh
PEERBRUSH_DISPOSABLE_HOMEBREW_TEST=1 python3 -B scripts/test_homebrew_upgrade.py \
  --older /path/to/older-definition OLD_RECEIPT_SHA256 \
  --newer /path/to/newer-definition NEW_RECEIPT_SHA256 \
  --team APPLE_TEAM_ID --brew-commit HOMEBREW_COMMIT \
  --output /path/to/private-output/homebrew-acceptance.json
```

Select two increasing notarized release versions with independently authenticated `package-managers.json` receipts. The harness installs the older cask, replaces the same fixture tap's cask definition, upgrades through Homebrew and uninstalls. For both generations it checks the installed binary, corresponding source, licenses, Developer ID, stapled ticket, Gatekeeper assessment, quarantined Launch Services startup, a rendered workspace and live MCP over the stable CLI link. The newer app must report Homebrew update ownership. The fixture verifies settings and document bytes through removal and writes a private receipt after success. Dispose of the runner after any failure. GUI Connect AI and third-party client reconnect, public tap delivery and release authorization remain separate.

For each installed generation the harness also reads Homebrew's structured cask state, hashes the actual ZIP or DMG from its private download cache, and checks the complete corresponding-source and notice tree against the release inventory. Upgrade must retire the older cask's staged files, metadata and support artifacts. Uninstall must clear the installed version and remove the app, CLI link and Caskroom entry. Homebrew may leave an empty parent directory for the versioned support artifacts. [Homebrew JSON queries](https://docs.brew.sh/Querying-Brew) · [Homebrew cache and uninstall commands](https://docs.brew.sh/Manpage)

After the quarantined app has produced or checked the retained fixtures, the harness opens the same installed bundle in a fresh private workspace through Launch Services. It supplies four Unicode-named PSD and PSB files as documents. The app must open separate clean tabs, preserve editable sources and native 8- and 16-bit samples, render the active workspace and quit cleanly. [Apple Launch Services documentation](https://developer.apple.com/documentation/coreservices/launch_services)
