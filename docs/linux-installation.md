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

## Installation, consent and retention

After separate authorization to install a reviewed candidate on a test system, APT can resolve dependencies for a local DEB. A direct file must be verified against a trusted checksum/signature source first; this unsigned preparation workflow does not provide such a public trust source. Do not install it over the user's working environment as a substitute for isolated acceptance.

The desktop entry offers PeerBrush in **Open With** for Photoshop documents. The user's desktop environment chooses its default editor. The package never writes `mimeapps.list`, invokes a default-setting command, or alters another application's handlers. Its literal `/usr/bin/peerbrush -- %F` launch uses the existing guarded multi-file/project activation, keeping filenames with spaces/Unicode as OS arguments. Actual native file launching and existing-instance behavior on Linux remain separate UI acceptance work.

APT/dpkg removal or purge removes package-owned binaries and desktop/MIME integration. User documents, per-user settings, recovery data and external AI configuration remain outside the installation and are retained. Reinstall must preserve them. Removing retained user state requires a separate explicit user choice; the package does not recursively delete home directories or configure AI clients. PB-155 owns validated preference migration and PB-147 owns compatible update recovery.

## Native CI acceptance and limits

CI builds the real candidate, then invokes `scripts/test_deb_installation.py` inside a fresh Ubuntu 24.04 Docker container. The script refuses host execution: it requires Linux, root inside Docker and `PEERBRUSH_DISPOSABLE_PACKAGE_TEST=1`. Only this disposable container is installed into. It runs the installed executable as a non-root fixture user, verifies PSD/PSB MIME caches/Open With registration, preserves an existing default editor, and exercises upgrade, remove, purge and reinstall while checking user-owned settings/document/default bytes.

The upgrade fixture increases the Debian package revision while retaining the same executable. It is private test data, never uploaded. It establishes package lifecycle behavior; it does not establish a real previous-application-version migration, native desktop launch, GPU rendering, dialogs, clipboard, unsaved-project/update interruption protection or a signed update feed. These checks remain explicit gates before installed Linux release readiness. Windows portable tests verify source/staging/tar inspection and skip no native acceptance into a success claim.
