# PeerBrush development

- Follow the [development guide](docs/development-guide.md) for branch names, pull request targets, verification and promotion. Never create branches under `codex/` (in any capitalization). Use `feature/<version>/<topic>` or `fix/<version>/<topic>`, based on the matching `version/<version>` branch. Promote through `version/<version>` → `dev` → `main`; preserve shared history with merge commits between promotion branches. Historical maintenance and website content have separate documented roles.

- Git attribution belongs to the human contributor. Never register an LLM or AI tool as a commit author, committer, co-author or contributor, or add automated contributor badges. Use an approved human identity from `.github/human-contributors.json`; adding another identity requires the maintainer to verify that it belongs to a human. GitHub's signing service is allowed only as a committer. Keep the README's AI-development disclosure.

- Preserve the GPL v3 license and Ember palette from MayaMCP.
- Use compact, unboxed range values. Avoid decorative containers and borders that add clutter or waste space.
- Show the actual rendered result while the user works: strokes, transforms, blend choices, effect changes and layer reordering. Preview through the shared engine, without committing history until the gesture finishes. Outline-only feedback is insufficient.
- Show AI work in blue. Animate meaningful AI changes in presentation only: numeric controls, canvas results and hierarchy movement. Commit the actual shared state immediately, keep animations out of history/PSD sources, isolate derived caches, and let human input interrupt them.
- Editing logic belongs in the document engine. UI, MCP, and CLI must share commands and enforce the same reservations.
- Never silently drop unsupported Photoshop features or claim full PSD compatibility. Keep unsupported documents read-only.
- Every save must contain a current merged composite. PeerBrush-only source data must not replace standard PSD raster/mask data.
- Preserve human work when resolving conflicts or undoing agent tasks.
- Keep model choice outside the application; return actual image content and explicit document coordinates to clients.
- Run engine/codec/protocol tests and visually verify the native workspace after behavior changes.
- Do not commit generated QA artifacts, credentials, connection tokens, development tools, recovery files, or build outputs. Requested bundled app artwork, fonts and curated documentation screenshots are source assets and must accompany the application.

- Layer controls use a connected Color/Mask selector, with blend and opacity beneath the effects. Top effects run last. Rename inline, without a dialog. Create regular layers, folders and explicitly requested adjustment layers; new folders collect selected roots.

- Preserve native 16-bit channels through edits, internal clipboard, effects, transforms and PSD/PNG saves. Screen/agent previews may project to 8 bit. Never silently make an 8-bit editable copy of a 16-bit original. Unsupported Photoshop structures can be explicitly flattened at their original precision.

## Release and public-information maintenance

- For authorized releases, patches and material project developments, follow [the release-maintenance playbook](docs/release-maintenance.md). Keep GitHub release artifacts/notes, the official website's home and Downloads information, confirmed official Discord announcements and social accounts consistent with the actual published version and verified changes. Keep Windows, macOS and Linux packages on the same version and commit; verify real assets and checksums and report any missing platform. Maintain website version history with dated patch notes and versioned downloads, alongside consistent home/latest information. Respect current holds, destination-specific permissions and platform rules; unreleased work must be labelled as such.
- Read back published state, avoid duplicate posts, and keep necessary receipts/blockers privately outside this public repository. Never claim every destination is updated when any result is failed, inaccessible or unverified. Missing access is a blocker, not permission to guess a destination.
- Apply the playbook's privacy/security gate before commits, packages or public updates. Keep credentials, private configuration, personal working practices and internal coordination out of public files, history, artifacts, logs and screenshots. Use redacted examples and report suspected exposure privately.

## Ponytail development approach

Use Ponytail's compact project rules for development. These rules complement the PeerBrush requirements above; they never replace the required verification, precision, preservation or release gates. Adapted from [Ponytail's project rules at e39ce9b](https://github.com/DietrichGebert/ponytail/blob/e39ce9bfbd1cc008e3233c065ef70839e604d559/AGENTS.md), under the [MIT license](docs/ponytail-license.txt).

Solve the whole problem with the least new code. End the final reply with one or two lines stating what was skipped or not checked and any material remaining risk.

### Before writing

Read the task and the code it touches. Identify every place the change must reach: callers, tests, fixtures, configuration and exports. Check what it could break for users, including data loss, exposure and incompatible callers. That is the scope; extra features are not.

### The smallest complete change

Take the first option that fully works:

1. Skip unrequested features, options and flexibility. For a broad request, deliver the smallest version that completes the core job.
2. Reuse an existing helper, component, service or pattern in the way the surrounding code uses it.
3. Use the standard library or a platform feature unless the project already has its own equivalent.
4. Use an installed dependency. Do not add a dependency for a few lines.
5. Use a clear one-line solution when it is sufficient.
6. Otherwise, write the minimum readable code that completes the task.

- Finish every required caller, test and fixture change. Keep the project's layers, interfaces and conventions.
- Avoid speculative abstractions, wrappers, conversions, configuration and boilerplate. Keep values in the form the platform already provides.
- Prefer the shortest readable working diff, after understanding its full scope. Comment only reasons the code cannot express.
- For a bug fix, search every caller before editing and fix the root cause in shared code. Preserve existing validation and error handling when moving or merging code.
- Between equally small options, choose the one that handles edge cases correctly.
- New non-trivial logic needs a focused test or assert-based self-check. Trivial changes need none. Follow PeerBrush's required suites and native verification for behavior changes.
- Mark a deliberate bounded shortcut with `shortcut: <the limit>, <when to upgrade>`; do not use shortcuts to bypass project requirements.

Never cut trust-boundary validation, protection against data loss, security, accessibility, hardware calibration or anything the user requested.
