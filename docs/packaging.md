# Release archive contents

`scripts/package-files.json` is the reviewed file list for corresponding source and portable packages. Adding a tracked file under `docs/`, `assets/` or `scripts/` does not put it into a release. The packager reads the policy and selected blobs from one exact Git commit; concurrent working-tree changes cannot change that snapshot.

## Selection and source completeness

The `source` list includes application code, shaders, tests, build metadata/scripts, the patched input adapter, required assets, public guides and optional segmentation integration. Every committed file under `src`, `tests`, `examples`, `integrations` and `vendor`, plus root build/Cargo configuration, must be reviewed into the list; otherwise packaging fails. This prevents newly required code or embedded assets being silently omitted. The `portable` list selects user guides, artwork/fonts and integrations from those source inputs. `readme` generates a focused archive README. The repository README, backlog, contributor coordination, checkpoint evidence and provider deployment mapping stay out of release archives. Application documentation screenshots and bundled app artwork remain source assets. Website code, trailers, showcase screenshots and demo PSD/PNG files are maintained separately and are not application release inputs.

Manifest paths must be portable relative names with no traversal, duplicate/case aliases, reserved Windows names or generated-file collisions. Committed source entries must be regular files. Binary and license inputs reject links, junctions and reparse points. A policy is a selection boundary, not a content review: inspect approved text and media before publication.

## Build and package

Use Python 3.11 or newer and stable Rust. From a clean committed checkout:

```sh
cargo test --locked
python -B scripts/test_package.py
python scripts/build_release.py
python scripts/package.py
```

`--binary`, `--registry` and `--output` select the native executable, build's Cargo registry and output directory. Packaging rejects tracked modifications. There is no dirty-source release override. Extracted source builds do not need repository metadata; packaging a new release requires committing the reviewed inputs first.

Versioned Windows x64, Linux x64 and macOS arm64 archives contain their matching source ZIP. macOS retains its `.app` bundle. Source entries use fixed commit timestamps and stored ZIP entries so all platforms produce identical corresponding source bytes. `release.json` records version, full source commit, executable/source checksums, notice checksums and actual signing configuration. Metadata does not prove an arbitrary local binary was compiled from that commit; the exact-source release workflow provides that separate check.

Executable checks reject recognized credential markers and personal build paths, including common UTF-16 forms. `scripts/build_release.py` remaps checkout, home and tool-cache paths before compiling. The version is checked by executing the staged copy that will actually be packaged. Synthetic tests can skip execution; they still enforce architecture and binary privacy checks. Targeted patterns cannot detect every private datum, so actual artifacts still need review.

License collection uses exact notice filenames and explicitly declared license files. Embedded font notices must appear in Cargo's checksum inventory. Named compiler copyright notices may be included when present. Registry crates missing for another target are omitted from the downloaded-dependency inventory; linked dependencies and original license obligations still require review.

Each run stages source, notices and the native ZIP in a fresh temporary directory. Final files are replaced only after validation and collection succeed. Existing extracted folders and unrelated output files are neither copied nor deleted. Failed input or notice checks preserve previous archives.

## Verification and publication

`scripts/test_package.py` exercises tracked and untracked private canaries, stale output, exact nested source, all platform layouts, notice selection, invalid manifests, new compiler inputs, linked inputs and failed builds. `scripts/verify_release.py` requires the three-platform matrix, checks every selected source byte, portable files, recorded notices, executable/source checksums, architecture, macOS bundle identity and archive paths. Unexpected package entries fail inspection.

Run a clean contributor build from the extracted source before publication. Follow [release maintenance](https://github.com/Deftr0y/PeerBrush/blob/main/docs/release-maintenance.md) for media/privacy review, signatures, native verification and authorized publication. Filtering new archives does not remove historical Git blobs or previously published downloads. Archive signatures do not replace Windows Authenticode or Apple signing/notarization.

For an explicitly authorized packaging-only correction, `--binary-source-commit` can retain an existing executable while recording its original full commit. Every application/build input must match that original commit; changed or missing engine inputs reject the correction. The source archive and packaging revision identify the new reviewed policy separately. Preserve executable hashes, publish the correction explicitly and verify replacement assets before removing superseded uploads.
