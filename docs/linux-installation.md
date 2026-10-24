# Ubuntu installation candidates

The first installed Linux target is **Ubuntu 24.04 amd64**, using a DEB with APT/dpkg owning installation and removal. This is a bounded distribution decision for PB-145/PB-146, not a claim of support for every Linux distribution. The existing portable ZIP remains available as a secondary option. A Flatpak/Flathub channel needs separate sandbox, file-dialog, clipboard, GPU and local AI connectivity acceptance before selection.

There is no configured signed APT repository or Linux self-updater. A locally downloaded candidate does not establish automatic updates. Repository signing, authenticated update metadata, notification/restart/rollback behavior and publication remain pending PB-147/PB-154 work. Package receipts explicitly report `release_ready: false`, `signing: not-performed` and `update_repository: not-configured`; neither CI artifacts nor a successful package test make downloads publicly ready.

## Prepare an exact-source candidate

Use the declared Ubuntu 24.04 amd64 build host, Python 3.11+, `dpkg-dev`, `desktop-file-utils`, the normal native Rust/build prerequisites, and an already built matching Linux ZIP. Package preparation does not install anything. From a clean committed checkout:

```sh
python3 -B scripts/test_deb.py
python3 -B scripts/package_deb.py \
  --portable /absolute/candidates/PeerBrush-VERSION-Linux-x64.zip \
  --portable-sha256 VERIFIED_PORTABLE_ZIP_SHA256 \
  --output /absolute/deb-candidates \
  --maintainer 'PUBLIC_MAINTAINER_NAME <PUBLIC_MAINTAINER_EMAIL>'
```

Choose an independently verified input checksum and a public maintainer contact approved for package metadata. Do not supply private account details. The input hash must match before the ELF is executed to check its version. The exact selected Git source snapshot supplies the desktop/MIME files, artwork, version and complete corresponding source; uncommitted replacements cannot enter the package.

The package installs `/usr/bin/peerbrush`, a desktop launcher, icon, PSD/PSB MIME registration, and original source/licenses/support material under `/usr/share/doc/peerbrush`. No maintainer install/remove scripts or default-editor changes are included. `dpkg-shlibdeps` derives linked library requirements, with explicit runtime graphics/input dependencies and the desktop/MIME cache providers. A desktop portal is recommended for native dialogs; actual backend/GPU/clipboard behavior still needs desktop acceptance.

The builder validates the desktop file and reads both data/control tar archives without extracting them. It requires exact file bytes, modes, root ownership, directories and the control file, rejects links/special entries, and preserves the executable at mode 0755. Output directories reject symbolic links/reparse points. Final DEB, checksum and provenance are exclusively created, never replaced. Debian versions map `0.30.0-dev.1` to `0.30.0~dev.1-1`, sorting prereleases before the eventual `0.30.0-1`.

Final candidate qualification independently reopens the selected DEB against the explicitly selected Linux portable hash and clean source checkout. Staging and inspection share one payload mapping. The writer explicitly uses xz for both control and data; the portable inspector accepts only this declared format, with bounded ar/xz/tar reads and no extraction or application execution. It rejects extra members, links, extended/sparse headers, path aliases, unexpected ownership/modes, scripts and changed bytes. Control fields must match the selected public maintainer, source version, exact installed size and declared identity. Native linked dependencies are syntax-checked with mandatory runtime packages; independent dependency correctness still requires the trusted native `dpkg-shlibdeps` build.

## Installation, consent and retention

After separate authorization to install a reviewed candidate on a test system, APT can resolve dependencies for a local DEB. A direct file must be verified against a trusted checksum/signature source first; this unsigned preparation workflow does not provide such a public trust source. Do not install it over the user's working environment as a substitute for isolated acceptance.

The desktop entry offers PeerBrush in **Open With** for Photoshop documents. The user's desktop environment chooses its default editor. The package never writes `mimeapps.list`, invokes a default-setting command, or alters another application's handlers. Its literal `/usr/bin/peerbrush -- %F` launch uses the existing guarded multi-file/project activation, keeping filenames with spaces/Unicode as OS arguments. Actual native file launching and existing-instance behavior on Linux remain separate UI acceptance work.

APT/dpkg removal or purge removes package-owned binaries and desktop/MIME integration. User documents, per-user settings, recovery data and external AI configuration remain outside the installation and are retained. Reinstall must preserve them. Removing retained user state requires a separate explicit user choice; the package does not recursively delete home directories or configure AI clients. PB-155 owns validated preference migration and PB-147 owns compatible update recovery.

## Native CI acceptance and limits

CI builds the real candidate, then invokes `scripts/test_deb_installation.py` inside a fresh Ubuntu 24.04 Docker container. The script refuses host execution: it requires Linux, root inside Docker and `PEERBRUSH_DISPOSABLE_PACKAGE_TEST=1`. Only this disposable container is installed into. It runs the installed executable as a non-root fixture user, verifies PSD/PSB MIME caches/Open With registration, preserves an existing default editor, and exercises upgrade, remove, purge and reinstall while checking user-owned settings/document/default bytes.

The installation check requires the explicitly selected final DEB SHA-256, exact source commit and application version, executable SHA-256 and corresponding-source SHA-256. Both CI callers obtain the executable/source hashes by inspecting the verified portable input against the selected source; they do not take readiness or source claims from the DEB receipt. Before user creation or APT changes, the check verifies the final candidate's bytes, filename, package identity, amd64 architecture and Debian version. It installs a verified copy in the disposable test directory, preserving the read-only host input. Each install, same-binary package upgrade and reinstall must retain the exact executable and corresponding-source bytes, installed package identity, actual `--version` output and canonical clean `--build-info` for that source commit. The original DEB must remain unchanged throughout.

Minimized Ubuntu images can filter documentation during installation. The fixture supplies command-specific dpkg path-includes for `/usr/share/doc/peerbrush` and its contents on fresh installation, upgrade and reinstall, retaining the package's corresponding source and notices for verification. It does not edit system dpkg configuration or change another package's documentation policy. Missing files, links and changed bytes fail with separate fixed-label diagnostics; source verification is never skipped. This fixture preparation does not change how the distributed package respects a user's own installation policy.

On complete success, the check exclusively creates a canonical `PeerBrushDebInstallationV1` JSON receipt under a fresh private runner-temporary directory. The receipt binds the final package hash, version/source commit, executable/source hashes and completed checks. It retains `release_ready: false`, production readiness unassessed, and explicit unperformed desktop, real application-version migration, unsaved-work/update interruption, publisher-signature and public-availability checks. Existing evidence is never overwritten; failure cannot produce a successful receipt. CI does not upload this operational receipt with the DEB candidate or add it to public release inventory. These are results of the trusted exact-source test job, not independent cryptographic attestation or authority to enable website downloads.

Before retaining the candidate, the same native job now invokes the final-byte inspector and `production_readiness.py --deb`, binding that private receipt to independently derived package, executable, corresponding-source and portable hashes. The bounded receipt must have the exact supported fields, completed checks and explicit limits. The public `.qualification.json` contains fixed qualification facts and remaining gates, with no private receipt paths or raw evidence; it keeps `ready: false`, `production_ready: false` and public availability unverified. Matching JSON does not authenticate its producer. The successful immediately preceding disposable test in this trusted job supplies that provenance; an arbitrary uploaded receipt cannot establish installation acceptance.

For an already authorized disposable container, the explicit invocation is:

```sh
python3 -B /checks/test_deb_installation.py \
  --package /packages/peerbrush_VERSION-1_amd64.deb \
  --package-sha256 SELECTED_DEB_SHA256 --commit EXACT_SOURCE_COMMIT \
  --version EXACT_APPLICATION_VERSION --executable-sha256 VERIFIED_EXECUTABLE_SHA256 \
  --source-sha256 VERIFIED_CORRESPONDING_SOURCE_SHA256 \
  --output /private-test-evidence/installation.json
```

The output parent must already exist and be private, with no symbolic-link parents. The workflow uses a private `mktemp` directory mounted separately from read-only package/source inputs. The receipt file is readable by the runner through that private parent even when the container creates it as root. No host package state or installed application is used for these checks.

The upgrade fixture increases the Debian package revision while retaining the same executable. It is private test data, never uploaded. It establishes package lifecycle behavior; it does not establish a real previous-application-version migration, native desktop launch, GPU rendering, dialogs, clipboard, unsaved-project/update interruption protection or a signed update feed. These checks remain explicit gates before installed Linux release readiness. Windows portable tests verify source/staging/tar inspection and skip no native acceptance into a success claim.

Lifecycle phases are reported with fixed labels. Because this scriptless package has no conffiles or configured repository, the fixture does not ask APT to purge an already removed, unresolvable package. It verifies remove followed by local-file reinstall, then purge while installed followed by another local-file reinstall and final purge. Every installed phase rechecks the original executable/source/build identity and every removed phase rechecks integration removal and retained user data. These fixture corrections require a complete native rerun before lifecycle acceptance is claimed.


Unreleased installed document-font discovery loads the system Fontconfig library. `libfontconfig1` is a mandatory DEB runtime dependency, alongside the existing window/GPU dependencies. It reads available fonts through a private native configuration and does not register fonts, invoke `fc-cache`, set global configuration or modify default editors. Missing native support is reported as unavailable; there is no directory-scan substitute or claimed automatic Linux update channel.


## Real 0.26 portable compatibility (unreleased, acceptance pending)

The separate `test_deb_version_upgrade.py` fixture uses the actual published `v0.26.0` Linux portable from source `a00cf47accde87400fad3f0fcfc5fe96fc4f5be1`. Before running it, the check reads back official tag/assets/checksums, verifies the independently pinned ZIP digest and compares its bundled and separate corresponding source with a clean exact historical checkout. Current package, executable and source selections use the existing independent DEB contract. A second fresh Ubuntu container owns the historical Git checkout; no host Git-trust exception or token mount is needed.

The old executable runs headlessly as a synthetic non-root user and creates real tool preferences plus editable Shape PSD/PSB projects at both native depths. The 16-bit fixture includes non-8-bit-replicated channel values. After the old process group stops, APT installs the actual current DEB. Current explicit-state launches must preserve preferences, native PNG samples, document depth, layer kind/bounds and editable source definitions. The fixture then explicitly invokes `settings migrate --from`, checks original/profile/backup bytes, and verifies default-profile launch and restart under isolated HOME/XDG/TMPDIR. Original projects, settings, old executable and selected DEB must remain unchanged.

This is portable-to-installed compatibility and opt-in settings migration. No earlier DEB existed, and an explicit state directory does not establish automatic discovery/migration of an old default profile. Desktop input/Open With, interrupted updates, unsaved-work protection, package signatures, repositories and public availability remain separate. Both disposable opt-ins are required; the CLI refuses host execution. Ordinary Python discovery runs only the ten offline source/input/PNG/protocol/lifetime checks, without downloading or executing an old binary. Native execution happens only in the explicit CI/release Docker phase after the ordinary suite passes.

Fixed phase labels identify failures without RPC output, connection data or local paths. Disposable child logs are removed with their temporary container work. A successful private `PeerBrushPortableToDebCompatibilityV1` receipt stays outside public package inventory and cannot authenticate its own producer or enable a download. `candidate_verified`, `ready` and `production_ready` remain false. The new fixture and callers require the next combined/native pass; preparation alone is not successful migration evidence.

## Prepared installed GUI check in the disposable environment

The separate `scripts/test_deb_installed_gui.py` check runs after successful portable-to-DEB compatibility in that same disposable Ubuntu 24.04 container. It requires the existing synthetic non-root migration account, actual original native8/16 PSD/PSB files, current installed executable/source hashes and clean BuildInfo, and the migrated per-user profile. It does not install packages or change a real desktop. Missing migration inputs, display tools, a usable renderer or a successful window capture fail the check; they do not produce readiness.

CI-only Xvfb, Xauth, DBus, GLib, Openbox, wmctrl and Mesa packages provide a private display/session. `HOME` in the child environment remains the synthetic account's actual passwd home; the runner's home and user profile are never repurposed. Display authentication lives under that account's private runtime directory, TCP X11 is disabled, and no host display/session/socket is mounted. Software rendering is requested; hardware behavior is not qualified.

The check copies only four known synthetic original projects into private filenames containing spaces, CJK text and a combining Unicode character. It verifies their real Photoshop MIME classification and launches the installed `/usr/share/applications/com.peerbrush.PeerBrush.desktop` through `gio launch`, exercising its literal `/usr/bin/peerbrush -- %F` Exec route. This tests an explicit app-info/Open With delivery path; it does not simulate a file manager menu or establish physical desktop/default-app consent. The synthetic user's default-editor preference bytes, including prior absence, must remain unchanged.

The actual GUI must load four independent projects at their original precision, retaining the editable Shape source and non-8-bit-replicated native16 samples. Real shared engine edits create distinct per-document quadrant images and real nonempty undo/redo histories. Undo must restore original native samples and source definitions; redo and a subsequent undo retain both history directions. The app's `capture_ui` route must wait for its current painted canvas and produce an actual 1360-by-900 window PNG. A bounded pixel oracle requires four large correctly arranged color regions, ignores small layer thumbnails and rejects a different project's palette. Duplicate GIO delivery from another selected tab must activate the original dirty project without replacing its state/history. Actual native Save/export and GUI restart must retain the current native raster and editable source.

The child uses a process-local Linux subreaper, the private connection's PID, exact installed executable hash/argv/user/adopted-parent/start identity and an owned pidfd. Graceful close goes through the exact window's EWMH request and the real shared close/on_exit path; the exact adopted app must exit successfully and be reaped. A timeout, wrong/missing/ambiguous window or changed owner fails without a forced app kill or fake restart. An explicit synthetic brush-preference change must survive normal on_exit and a real default-profile GUI restart, with unchanged persisted bytes after the second normal close. Original artwork, legacy preferences and migration backups remain byte-exact.

The private `PeerBrushInstalledGuiV1` record and thirteen synthetic window captures stay outside package inventory and public artifacts. Connection files, profile contents and app/window-manager logs are not copied into that record. Existing installation and migration receipt schemas, protected workflow gates, signatures, update/repository policy and public readiness remain unchanged. `candidate_verified`, `ready` and `production_ready` are false. This prepared source and its eight offline contract methods have not run; exact-source combined checks and the actual isolated native CI execution remain required. Physical input, user-desktop consent, unsaved close-modal choices, real GPU/tablet/clipboard behavior, interrupted installed updates and receiving-application compatibility remain separate acceptance work.
