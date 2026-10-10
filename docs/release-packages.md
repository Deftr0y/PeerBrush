# Release packages

The [published PeerBrush 0.2 prerelease](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.2) uses application version **0.2.0**. Its Windows x64, Linux x64 and macOS arm64 packages are unsigned; native publisher signing, notarization and archive signatures are deferred. Public archive checksums are included in the release. Release candidates in Actions are not published downloads.

Packages target Windows x64, Linux x64 and macOS Apple silicon (arm64). Each archive includes `release.json` with the source commit, version, architecture and executable/source checksums, the matching GPL source archive, bundled artwork/fonts, documentation and license notices. macOS has a `PeerBrush.app` bundle; Windows includes `Start PeerBrush.cmd`. Linux binaries are built on Ubuntu 24.04 and require compatible system graphics/windowing libraries. Other Linux distributions and Intel Macs are not separately verified targets.

The release workflow checks out one exact commit, runs engine/codec/protocol tests and builds all three platforms. It verifies archive integrity, architecture, licenses, identical source archives and every committed source blob, then produces `SHA256SUMS` and `release-inventory.json`. Local Windows native rendering is checked separately; successful macOS/Linux builds do not imply manual desktop verification on those systems.

## Checksums and signatures

Compare the downloaded archive's SHA-256 to its entry in `SHA256SUMS`. A checksum detects corruption; it needs a trusted origin. When Sigstore signing is enabled, each package, source archive, checksum file and inventory has a corresponding `.sigstore.json` bundle. Verify the signature against the **exact workflow identity and issuer documented in that release**, using the [Sigstore verifier](https://docs.sigstore.dev/quickstart/quickstart-ci/). Download the signature bundle alongside its artifact. A successful verification checks artifact integrity, the signing certificate identity and transparency-log evidence.

Sigstore signs through the GitHub Actions identity without a long-lived signing key. It **does not provide Windows Authenticode, Apple Developer ID signing or notarization**. Windows SmartScreen and macOS Gatekeeper publisher warnings may remain. Native publisher certificates are a separate requirement; the package manifest reports their actual configuration. Never describe a Sigstore archive signature as trusted native publisher signing.

## Preparing candidates

Commit all tracked changes before packaging. Run `python scripts/build_release.py`, then `python scripts/package.py`. The release build remaps checkout and dependency paths so personal local directories are not embedded in the executable. The packager checks the executable's `--version` output and reads source/documentation/artwork from committed Git blobs. It cannot include untracked connection files, recovery work or development outputs. Candidate filenames include version, platform and architecture.

The manual **PeerBrush release candidates** workflow requires the full commit SHA matching the selected workflow ref. It prepares and inspects all platforms. The optional Sigstore step signs and verifies candidates; it does not create a release, upload public assets or update the website. Publish only after inspecting the actual candidates and completing the [release-maintenance gates](release-maintenance.md). Never replace an existing published tag or package in place.
