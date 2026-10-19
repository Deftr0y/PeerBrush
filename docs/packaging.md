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

`--binary`, `--registry` and `--output` select the native executable, build's Cargo registry and output directory. Packaging rejects tracked modifications. There is no dirty-source release override. Extracted source builds do not need repository metadata; packaging a new release requires committing the reviewed inputs first.

Versioned Windows x64, Linux x64 and macOS arm64 archives contain their matching source ZIP. macOS retains its `.app` bundle. Source entries use fixed commit timestamps and stored ZIP entries so all platforms produce identical corresponding source bytes. `release.json` records version, full source commit, executable/source checksums, notice checksums and actual signing configuration. Metadata does not prove an arbitrary local binary was compiled from that commit; the exact-source release workflow provides that separate check.

Executable checks reject recognized credential markers and personal build paths, including common UTF-16 forms. `scripts/build_release.py` remaps checkout, home and tool-cache paths before compiling. The version is checked by executing the staged copy that will actually be packaged. Synthetic tests can skip execution; they still enforce architecture and binary privacy checks. Targeted patterns cannot detect every private datum, so actual artifacts still need review.

License collection uses exact notice filenames and explicitly declared license files. Embedded font notices must appear in Cargo's checksum inventory. Policy-listed compiler copyright notices are required from the active build compiler's `rustc --print sysroot` under `share/doc/rust`; install that toolchain's `rust-docs` component before packaging. Missing, empty or linked notice files fail packaging, and archive inspection independently requires their inventory and hashes. Registry crates missing for another target are omitted from the downloaded-dependency inventory; linked dependencies and original license obligations still require review.

Each run stages source, notices and the native ZIP in a fresh temporary directory. Final files are replaced only after validation and collection succeed. Existing extracted folders and unrelated output files are neither copied nor deleted. Failed input or notice checks preserve previous archives.

## macOS DMG candidates

`scripts/package_macos_dmg.py` prepares a compressed, read-only HFS+ DMG from an existing Developer ID signed, notarized and stapled ZIP. It requires the independently verified input SHA-256 and exact source checkout/version, reuses the ZIP/source inspector, and performs native app signature, ticket and Gatekeeper checks. It preserves every original app byte and file mode. The volume contains `PeerBrush.app`, an `Applications` shortcut and a `Documentation and source` folder containing the original GPL source, notices and support material. No installed files or application state are changed.

The script verifies the image with `hdiutil`, mounts it read-only without opening Finder, compares the complete mounted file/directory/link inventory and original bytes/modes, verifies the mounted app again, then detaches it. Final candidate, SHA-256 and provenance receipt are exclusively created and never overwritten; choose a new revision after an existing or incomplete output. Failed work remains in a private temporary staging folder beneath the external output directory for inspection, especially if detachment fails. Do not remove that folder while its image remains mounted.

These are explicitly named `-dmg-candidate-rN.dmg`, with `release_ready: false` and image signing/notarization reported as `not-performed`. `sign_macos.py --format dmg` provides the separately gated Developer ID/timestamp/notarization/stapling path, reusing existing provider preflight and resumable journal logic. It verifies the final signed image and mounted original app/source before writing new final checksums and receipts. Signed DMGs still need installed/quarantined-download acceptance and public release integration; neither candidate nor signed image is automatically accepted by the ZIP-only release/website inventory. See [DMG signing and installation gates](macos-signing.md#dmg-preparation-and-remaining-release-gates). Credential-free macOS CI mounts synthetic unsigned data to check image creation and payload preservation; that test does not establish real publisher signing, notarization or installation acceptance.

## Verification and publication

`scripts/test_package.py` exercises tracked and untracked private canaries, stale output, exact nested source, all platform layouts, notice selection, invalid manifests, new compiler inputs, linked inputs and failed builds. `scripts/verify_release.py` requires the three-platform matrix, checks every selected source byte, portable files, recorded notices, executable/source checksums, architecture, macOS bundle identity and archive paths. Unexpected package entries fail inspection.

Run a clean contributor build from the extracted source before publication. Follow [release maintenance](https://github.com/Deftr0y/PeerBrush/blob/main/docs/release-maintenance.md) for media/privacy review, signatures, native verification and authorized publication. Filtering new archives does not remove historical Git blobs or previously published downloads. Archive signatures do not replace Windows Authenticode or Apple signing/notarization.

For an explicitly authorized packaging-only correction, `--binary-source-commit` can retain an existing executable while recording its original full commit. Every application/build input must match that original commit; changed or missing engine inputs reject the correction. The source archive and packaging revision identify the new reviewed policy separately. Preserve executable hashes, publish the correction explicitly and verify replacement assets before removing superseded uploads.
