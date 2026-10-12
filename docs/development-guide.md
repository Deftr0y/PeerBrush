# PeerBrush development guide

PeerBrush is a GPL v3 native image editor built with Rust, egui/eframe and wgpu. Document editing, codecs and transports share one engine. Read [AGENTS.md](../AGENTS.md), the [backlog](../FOLLOWUPS.MD) and the relevant version queue before changing behavior. Version 0.26 is unreleased; published downloads remain 0.25.

## Branches and pull requests

| Branch | Role | Pull request destination |
| --- | --- | --- |
| `main` | Production source | Receives `dev` |
| `dev` | Final integration and release acceptance | `main` |
| `version/0.26` | Changes and acceptance work intended for 0.26 | `dev` |
| `feature/0.26/<topic>` | One focused feature or documentation change | `version/0.26` |
| `fix/0.26/<topic>` | One focused bug fix | `version/0.26` |
| `site-content` | Reviewed public website output | Website deployment workflow |
| `maintenance/archive-<checkpoint>` | Retained historical packaging corrections | No normal development promotion |
| `maintenance/site-<topic>` | A reviewed website content change | `site-content` |

The normal promotion order is **feature or fix → matching version → dev → main**. Use lowercase names and a descriptive topic, for example `fix/0.26/selection-coverage`. Never create a `codex/` branch, regardless of capitalization. `origin/` is the remote-tracking prefix, not part of a branch name. Git cannot hold both `version/0.26` and `version/0.26/selection-coverage`; feature and fix names use their own namespace.

Create each version branch from the current `dev` after synchronizing required production fixes. Keep a version's scope explicit so another version's unfinished work cannot enter its production promotion. Create focused branches from their matching version branch:

```sh
git fetch origin --prune
git switch -c feature/0.26/selection-coverage origin/version/0.26
# Implement and review the focused change.
git push -u origin feature/0.26/selection-coverage
```

Open a PR against `version/0.26`, describe the resulting behavior, and include verification evidence and remaining limits. Prefer one backlog item or coherent slice per PR. A source implementation alone does not close native or receiving-application acceptance work.

Feature and fix PRs may be squash merged. Use merge commits for version → dev and dev → main, retaining shared ancestry for subsequent promotions. Do not rebase or force-push shared promotion branches. After production fixes, merge the production history back into `dev` and each affected active version before new work. Delete a focused branch only after its work is merged or explicitly superseded and unique work is preserved. Preserve published tags, release provenance and unique historical corrections during cleanup.

The CI branch-policy check rejects forbidden names and incorrect PR destinations. Configure required checks on `main`, `dev` and `version/*`: branch policy, contributor attribution, all native platform jobs and package inspection. Disable force pushes and deletion on these active promotion branches. A CI check is not a server-side branch creation restriction; repository protection settings must enforce required checks.

## Build and verification

Install stable Rust and the native platform prerequisites. Linux CI installs `libxkbcommon-dev`, `libwayland-dev`, `libxcb-shape0-dev` and `libxcb-xfixes0-dev`. Windows development can use the ignored toolchain prepared by `scripts/dev.ps1`; it does not modify persistent PATH.

```sh
cargo run --locked
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --locked
python scripts/test_package.py
```

```powershell
.\scripts\dev.ps1 run
.\scripts\dev.ps1 test
.\scripts\dev.ps1 build
```

GitHub Actions runs the engine, codec, protocol, UI and packaging checks on Windows, macOS and Linux. Push focused work while those checks run; inspect failures and fix them before merging. Passing a previous commit does not validate a later revision. Package inspection checks the matching version, source commit, source archive and platform packages. Actions artifacts are development or candidate downloads until an actual release is published.

Behavior changes also require native visual verification: inspect actual rendered strokes, transforms, masks, effects and layer movement, plus gesture completion/cancellation and undo. Preserve human work and use an isolated workspace for synthetic checks. Verify native16 edits and saved PSD/PNG channels at their original precision. Record real Photoshop/application versions for receiving-application checks; protected unsupported documents must remain read-only. Windows native evidence does not establish macOS/Linux desktop acceptance.

## Architecture and contribution rules

Editing logic belongs in the engine; native UI, MCP and CLI must share commands, document/revision guards and reservations. Render gestures through the shared preview engine, committing history when the gesture finishes. AI blue feedback and animations belong to presentation and derived caches. Save current standard PSD raster/mask channels and the merged composite alongside retained editable sources. Keep model selection outside PeerBrush.

Use the Ember palette, compact unboxed controls and the connected Color/Mask selector. Preserve native16 samples, unsupported Photoshop protection and unrelated human edits. See [implementation notes](development.md), [Photoshop fidelity](photoshop-fidelity.md), [MCP](mcp.md), [packaging](packaging.md) and the [0.26 queue](development-0.26.md).

Use only an approved human Git identity from `.github/human-contributors.json`. Never add an AI author, committer, co-author or contributor badge. Keep the README's AI-development disclosure. Do not commit credentials, runtime state, recovery files, local tools, QA captures or build output. Required bundled artwork, fonts and licenses belong in source and packages. Review the actual diff and package contents through the [privacy/security gate](release-maintenance.md#privacy-and-security-publication-gate).

## Release promotion and website

Before version → dev, complete the intended scope and report unresolved acceptance work. Before dev → main, confirm the release version and notes, all required checks, native acceptance evidence and a clean reviewed release source. Production publication must use Windows, macOS and Linux packages built and verified from the exact same production commit and version; never relabel an older platform binary.

The intended automated production sequence is **main → exact-source tests/builds → verified packages → versioned GitHub release → matching website release information and deployment**. The existing CI builds and inspects all three platforms. Currently `release.yml` prepares candidates through manual dispatch. The website output branch carries `pages.yml`: a push to `site-content` deploys that exact reviewed commit, subject to GitHub Pages environment gates; manual dispatch also requires an expected site commit. Merging application source into `main` does not currently publish a release or synchronize website release data. Do not describe the complete production sequence as active until it is configured and verified.

Follow the [release-maintenance playbook](release-maintenance.md) for actual publication, checksums, version history and read-back verification. Keep the site's development-guide link pointed at a published branch or a verified source commit containing this guide. Keep unreleased version queues separate from published downloads. Website content changes use `maintenance/site-<topic>` based on `site-content`; review the public diff before promoting the result to `site-content`, since its push starts deployment. Confirm the deployed content, not just workflow completion. Do not merge the website output branch into application source or delete it while the deployment workflow uses it.
