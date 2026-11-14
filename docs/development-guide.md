# PeerBrush development guide

PeerBrush is a GPL v3 native image editor built with Rust, egui/eframe and wgpu. Document editing, codecs and transports share one engine. Read [AGENTS.md](../AGENTS.md), the [backlog](../FOLLOWUPS.MD) and the [development queue](development-0.3.md) before changing behavior. The latest published version is [0.30.1](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.30.1). The next version carries forward the remaining backlog, with PB-108 first.

## Direct version development and release promotion

Develop directly on the appropriate version branch. The active branch is
`version/0.4`, with 0.4.0 as the working version. Development builds remain
unreleased. Production source is `main`. `dev` and existing feature/fix branches
retain historical and explicitly unqualified WIP checkpoints. Do not delete,
rewrite or automatically integrate those checkpoints.

See the [0.4.0 draft release notes](release-0.4.md) for the source changes and remaining release work.

The development path is **version branch → main**. Do not create routine feature
PRs or run push/feature-to-version CI. A version-to-`main` promotion is a release
candidate, not authorization to publish. Never force-push shared history. Website
maintenance remains separate: `maintenance/site-*` → `site-content`.

```sh
git switch version/0.4
```

Once the branch has an upstream, synchronize it with `git pull --ff-only` before editing. Commit the requested change directly on the version branch.

The 0.4 series follows the published 0.30 series in PeerBrush release ordering. The app and release assets use 0.4.0. The 0.4.0 MSIX identity version is 2.4.0.0, the Debian package version is `1:0.4.0-1`, and the macOS bundle build version is 1.4.0. Each native package version sorts above its 0.30.1 predecessor. Once 0.4.0 is published, users of older portable builds can use the official download for a one-time manual upgrade. Those builds compare release tags numerically. Native package acceptance remains required.

## Essential release verification

Ordinary development commits do not automatically execute tests. Preserve useful
test source, but do not add broad suites or repeatedly run exhaustive checks by
default. Build or investigate a specific failure only when the work requires it.

Open a version-to-`main` release candidate after its intended features are complete.
The release gate runs once for that candidate: approved attribution and branch
policy, ten existing critical editing/selection/undo/save-reopen/conflict/export
cases, native builds, and real package/source integrity on Windows, macOS and
Linux. The selected code cases are defined in `scripts/release_essentials.py`.
Large resource experiments, broad unit suites, benchmarks and SVG oracles are
not automatic development or release gates. The separate SVG oracle is manual.

If a release check fails, stop promotion. Repair the actual failure on the version
branch, use the relevant targeted recheck when requested, and identify the repaired
candidate precisely. An old pass or a failed run cannot qualify a later source.
The selector accepts `--case TARGET:NAME` for that targeted diagnosis; `--check`
validates the selection without executing application tests.

Actual artifact identity, signatures, Store/notarization requirements, package
installation, native feature acceptance and publication protections still apply.
Release/signing producers build and validate their exact artifacts without
repeating the broad code suite. The 16000 × 16000 editor (PB-068) is in scope
for the next version and requires acceptance at those limits before release.

Use Rust 1.92.0 and the [native JPEG XL prerequisites](jpeg-xl-build.md). For an
explicit build use `python scripts/jxl_build.py -- build --locked --bin peerbrush`.
Do not automatically invoke `cargo test`, Clippy, benchmark suites or Python test
discovery during routine work.

Repository administrators must reconcile branch rules with this policy: permit
authorized direct development on `version/*`; retain protected production `main`
with its release gate, attribution and package inspection. Changing protection,
required-check or credential settings requires separate authorization. Existing
branches and PRs must not be deleted or forcibly closed to enforce this workflow.

## Architecture and contribution rules

Development follows [Ponytail's compact project rules](../AGENTS.md#ponytail-development-approach): read the affected flow, reuse existing code and deliver the smallest complete change while preserving validation and verification. The rules are adapted from [upstream revision e39ce9b](https://github.com/DietrichGebert/ponytail/tree/e39ce9bfbd1cc008e3233c065ef70839e604d559) with its [MIT notice](ponytail-license.txt). They are loaded through the repository's `AGENTS.md`; this project setup does not install lifecycle hooks or mode-switching commands. PeerBrush's specific contribution and release requirements remain authoritative.

Editing logic belongs in the engine; native UI, MCP and CLI must share commands, document/revision guards and reservations. Render gestures through the shared preview engine, committing history when the gesture finishes. AI blue feedback and animations belong to presentation and derived caches. Save current standard PSD raster/mask channels and the merged composite alongside retained editable sources. Keep model selection outside PeerBrush.

For Rust embedding, `server::RecoveryVersions` is now opaque worker state that retains admitted native checkpoint bookkeeping as well as PSD deduplication state. Replace the former raw `HashMap` alias with `RecoveryVersions::new()` or `Default::default()`, retain that state across ticks and pass it to `server::checkpoint_projects`. Native source I/O and retirement still run outside Engine and workspace locks.

Use the Ember palette, compact unboxed controls and the connected Color/Mask selector. Preserve native16 samples, unsupported Photoshop protection and unrelated human edits. See [implementation notes](development.md), [Photoshop fidelity](photoshop-fidelity.md), [MCP](mcp.md), [packaging](packaging.md) and the [0.26 queue](development-0.26.md).

Use controls suited to each setting in new and existing menus. Use an interactive color picker for colors and recognizable icons for visual choices such as text alignment. Give icon controls accessible labels and tooltips. Keep precise entry available where the setting needs it.

Use only an approved human Git identity from `.github/human-contributors.json`. Never add an AI author, committer, co-author or contributor badge. Keep the README's AI-development disclosure. Do not commit credentials, runtime state, recovery files, local tools, QA captures or build output. Required bundled artwork, fonts and licenses belong in source and packages. Review the actual diff and package contents through the [privacy/security gate](release-maintenance.md#privacy-and-security-publication-gate).

Default launches use the versioned [per-user preferences profile](settings.md). Explicit `--state-dir` development workspaces retain instance-local settings and cache; keep their MCP launch arguments unchanged. Never copy entire runtime directories into settings migration or package inputs: recovery files, discovery tokens, artwork and unknown private files have separate ownership.

## Release promotion and website

Before version → `main`, complete the intended scope and report all unresolved
acceptance work. Confirm the release version, notes, essential gate and actual
native/package evidence for the exact candidate. Publication requires packages
from the same reviewed production commit and version; never relabel an older
binary. Production build/signing/publication workflows keep their separate
authority and artifact checks. A version push or release-gate pass does not grant
release, deployment, signing-credential or Store-submission authority.

Follow the [release-maintenance playbook](release-maintenance.md) for actual publication, checksums, version history and read-back verification. Keep the site's development-guide link pointed at a published branch or a verified source commit containing this guide. Keep unreleased version queues separate from published downloads. Website content changes use `maintenance/site-<topic>` based on `site-content`; review the public diff before promoting the result to `site-content`, then dispatch the website workflow from an eligible branch with its exact site commit. Confirm the deployed content, not just workflow completion. Do not merge the website output branch into application source or delete it while the deployment workflow uses it.

JPEG XL builds require the committed Rust 1.92 toolchain and native CMake/Ninja/Clang prerequisites. See [JPEG XL build inputs](jpeg-xl-build.md); do not install a global toolchain or workload to bypass an unqualified compiler.

Integration codec tests launch the application worker in the matching Cargo profile directory. Build that source-matched main before running tests; the development test helper and all current native CI callers do so. Installed application launches use their exact current executable, including renamed installations; no PATH-selected worker or configurable executable is accepted.

macOS native workflows qualify the already installed `python` as Python 3.11 or newer before native tool setup and pin its existing directory through `GITHUB_PATH`. Both `python` and `python3` in that directory must resolve to the qualified executable; no interpreter is installed. This preserves the qualified runtime when codec tool selection exports a PATH that could otherwise shadow it. A fixed numeric Python version is recorded; filesystem paths are not printed.

Release native flags retain the inherited `cc` mode: absent/empty/`0`/`false`/`no` means ASCII whitespace splitting; other values opt into shell quoting. The release wrapper reserializes standard C/C++/archiver flag families and target variants before enabling shell parsing and adding path remaps. Malformed shell syntax and control values are rejected. The same-version CMake 0.1.58 patch preserves complete discovered compiler arguments when forwarding them as a CMake command fragment; default flags and explicit CMake fragments remain intact. Its upstream archive hash, licenses and change scope are recorded in `vendor/cmake/README.md`. A later package test runs a small offline, locked cc/CMake C-and-C++ fixture only with qualified native tools; it proves actual argv, preprocessing and object file-prefix mapping for legacy/shell flag modes, including spaces, quotes, backslashes and Unicode. This does not qualify other host/generator combinations.
