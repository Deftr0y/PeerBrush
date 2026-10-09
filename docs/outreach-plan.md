# PeerBrush outreach plan

Last updated and source rules checked: 2026-10-09. Recheck every destination's current rules and submission flow before taking action.

This file preserves the prepared outreach drafts and future distribution options for artist feedback and developer contributions. It is a planning document. All sends, posts, issue creation, contributor-guide publication, directory submissions and upstream PRs remain pending review and approval.

**Next intended action: first-wave item 4 only, later on 2026-10-09 after an explicit go-ahead.** No time is scheduled, and saving this plan does not publish that issue. The other actions stay on hold until the software is ready and the maintainer approves the relevant action. A tentative return in a few days is not a deadline or automatic publishing instruction.

## First-wave numbering

Keep these numbers stable when referring to the plan.

| Number | Action | Audience | Current state |
| --- | --- | --- | --- |
| 1 | Libre Arts email | Artists using free creative software | Draft saved; pending approval |
| 2 | CG Channel email | Digital artists and CG-tool users | Draft saved; pending approval |
| 3 | CGPress news form | CG artists | Draft saved; pending approval |
| 4 | PeerBrush artist-feedback issue | Artists already trying the project | Intended first action later today; not yet authorized for publication |
| 5 | Contributor guide, scoped issue and This Week in Rust CFP chain | Rust/open-source contributors | All three drafts saved; dependencies and approval pending |

Social posts, directories and MCP distribution routes are in the later sections. None is approved by inclusion here. Editorial coverage, listing acceptance and contributor response are never guaranteed.

## Readiness and approval gate

Before item 4:

- [ ] Obtain the explicit go-ahead to create the exact artist-feedback issue in `Deftr0y/PeerBrush`.
- [ ] Recheck open and closed issues to avoid creating a duplicate.
- [ ] Verify the downloadable release, release-specific setup links and the small test workflow in the draft.
- [ ] If the version, copy or test workflow needs changing, review the revised draft before publishing.
- [ ] After creation, record the returned issue URL and verify that its title and body match the approved draft.

Before broader outreach:

- [ ] Maintainer confirms that the chosen build is ready for the intended audience.
- [ ] Check a clean download/install and launch on every platform claimed in the copy.
- [ ] Test one small manual editing workflow, undo/redo, save/reopen and PNG export using disposable artwork.
- [ ] If promoting MCP, test one compatible client connection, a layer edit, inspection and undo. Record the actual client/version, build and OS without publishing private configuration.
- [ ] Verify the release asset names, release notes, license, setup docs and known limitations.
- [ ] Ensure screenshots or recordings genuinely show the advertised build and use artwork that may be shared. Use only assets explicitly approved for that destination.
- [ ] Provide a working feedback destination. Developer recruitment additionally needs a current contribution guide and a concrete open task.
- [ ] Search each directory/community for an existing PeerBrush entry or recent post to avoid duplicates.
- [ ] Recheck self-promotion, AI-authorship, automation, flair/media and repeat-post rules. Unstated policy is not affirmative permission for automation.
- [ ] Confirm the intended account and publishing access. New accounts, subscriptions, paid listings, new access grants and package publication are separate decisions, not approved by this plan.
- [ ] Review the exact recipient/destination, wording, assets and any contact fields. Fill contact placeholders only for the approved submission; do not add personal contact information to this repository.
- [ ] Record the submission result or blocker. Do not treat an unverified click, queued form or editorial review as successful publication.

No paid spend is planned. Do not buy priority reviews, advertisements or paid listings. Do not send unsolicited member DMs, request upvotes, repeat-post automatically, or migrate project hosting/issue tracking.

## Factual and copy boundaries

Snapshot verified on 2026-10-09:

- [PeerBrush v0.1.4](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4) is an experimental prerelease published on 2026-10-08. Its public assets are `PeerBrush-Windows.zip` and `PeerBrush-source.zip`.
- `main` is ahead of that downloadable release. A feature seen in current source or a local build is not automatically available in the public download.
- PeerBrush is free and open source under GPL-3.0, built in Rust with egui/eframe and wgpu, and designed from the ground up for human and compatible AI-agent editing in one project.
- Release-grounded descriptions may mention painting, layers, masks, selections, editable effects and native 16-bit editing for supported raster documents. Recheck before changing the release anchor.
- The Windows portable prerelease is the distribution claim. CI configuration for macOS/Linux is not evidence of complete native validation or downloadable packages for those systems.
- PSD support is limited. Do not claim Photoshop feature parity, full PSD fidelity, production readiness, Adobe affiliation or endorsement. An honestly qualified early alternative to Photoshop is an acceptable audience description.
- A compatible AI client/model is separate and may incur provider costs. PeerBrush does not include hosted inference. Do not imply a hosted remote MCP service when describing the local editor/adapter.
- Use plain language and concrete feedback requests, without slogans or hype. Keep artist workflow feedback and developer contribution requests suited to each audience.

Public references: [release README](https://github.com/Deftr0y/PeerBrush/blob/v0.1.4/README.md), [release getting started](https://github.com/Deftr0y/PeerBrush/blob/v0.1.4/docs/getting-started.md), [release MCP docs](https://github.com/Deftr0y/PeerBrush/blob/v0.1.4/docs/mcp.md), [current README](https://github.com/Deftr0y/PeerBrush/blob/main/README.md), [development notes](https://github.com/Deftr0y/PeerBrush/blob/main/docs/development.md), [CI](https://github.com/Deftr0y/PeerBrush/blob/main/.github/workflows/ci.yml).

## Prepared first-wave drafts

The wording below is preserved for review. Personal name/contact fields are placeholders for this public file. Dates, version links and destination rules must be refreshed if they become stale; no automatic substitutions beyond an explicitly approved workflow are assumed.

### 1 Libre Arts email

Destination: Aleksandr Prokudin at `alexandre.prokoudine@gmail.com`, as published on the [official contact page](https://librearts.org/about/). The publication welcomes tips about free creative applications. A sender account and approval of the exact email are required. No specific AI-writing policy was stated on the intake page.


````text
From: [maintainer name] <[maintainer contact email]>
To: Aleksandr Prokudin <alexandre.prokoudine@gmail.com>
Cc/Bcc: none
Attachments: none

Subject: PeerBrush: free, open-source image editor seeking artist feedback

Hi Aleksandr,

I'm building PeerBrush, a free, open-source image editor designed from the ground up for people and AI agents to edit the same project together. It has manual painting and layer-based editing; compatible agents can inspect and change the live document through MCP.

It's early and experimental. The Windows v0.1.4 prerelease includes layers, masks, selections, editable effects and native 16-bit editing for supported raster documents. PSD compatibility is limited, and AI use needs a separate compatible client/model.

I'm iterating quickly, and feedback will help decide what I build next. I'm looking for artists to try real workflows and suggest what needs improving, alongside developers interested in bug reports, forks and focused PRs. Would this fit a Libre Arts news item or showcase?

Windows prerelease: https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4
GPL-3.0 Rust source and project information: https://github.com/Deftr0y/PeerBrush

Thanks,
[maintainer name]
````

### 2 CG Channel email

Destination: `news@cgchannel.com`, the [official news intake](https://www.cgchannel.com/contact/), separate from paid advertising. Fit: digital artists and creative-tool workflows. A sender account and approval of the exact email are required. No specific AI-writing policy was stated on the intake page.


````text
From: [maintainer name] <[maintainer contact email]>
To: CG Channel news team <news@cgchannel.com>
Cc/Bcc: none
Attachments: none

Subject: News tip: PeerBrush, a free image editor with live MCP editing

Hi CG Channel team,

I'm developing PeerBrush, a free, open-source image editor built from the ground up for manual and AI-assisted work in the same layered project.

The public Windows v0.1.4 prerelease has painting, masks, selections, editable effects and native 16-bit editing for supported raster documents. A compatible agent connects through MCP to inspect the canvas and edit through the same commands as the interface.

I'm looking for artists to test small editing workflows and identify missing features, and developers who want to contribute. I'm iterating quickly, and feedback will help decide what I build next. It's an early project for people exploring free alternatives to Photoshop, with limited PSD compatibility and more work needed before production use. AI clients/models are separate and can carry their own costs.

Would it be useful to cover for CG Channel's readers?

Windows prerelease: https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4
GPL-3.0 Rust source, screenshot and setup: https://github.com/Deftr0y/PeerBrush

Thanks,
[maintainer name]
````

### 3 CGPress news form

Destination: [Contribute News](https://cgpress.org/contribute). The public form solicits CG news and requires first name, last name, contact email and credit preference. JavaScript is required; inspect the live submission flow before use. No specific AI-writing policy was stated on the intake page. The proposed credit choice is “No.” Approval must cover the exact form and contact information provided to CGPress.


````text
First name: [maintainer first name]
Last name: [maintainer last name]
Email: [maintainer contact email]
Your Web Site: https://github.com/Deftr0y/PeerBrush
Link for news: https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4
Would you like to be credited as the source?: No
Nickname: leave empty

Summary of the news:

PeerBrush is a free, open-source image editor designed from the ground up for people and AI agents to edit the same project. Its Windows v0.1.4 prerelease includes painting, layers, masks, selections, editable effects and native 16-bit editing for supported raster documents. Compatible agents can inspect the canvas and edit through MCP using the shared editing engine.

Developer [maintainer name] is seeking hands-on artist feedback and feature requests, plus developers interested in bug reports, forks and focused PRs. The GPL-3.0 Rust source and Windows download are public on GitHub.

The project is experimental. PSD compatibility is limited, broader macOS/Linux native validation remains incomplete, and AI use requires a separate compatible client/model with any provider costs separate. The current main branch is ahead of the downloadable release.

Source and setup: https://github.com/Deftr0y/PeerBrush
````

### 4 Artist feedback issue

Destination: [PeerBrush new issue](https://github.com/Deftr0y/PeerBrush/issues/new). Check [existing issues](https://github.com/Deftr0y/PeerBrush/issues) first. Intended first action later on 2026-10-09 only after the go-ahead. This gives incoming artists a place to report observations; it does not reach an external audience by itself.

Use the first heading as the issue title and the rest as the body:


````markdown
# PeerBrush v0.1.4: first-use feedback and feature requests

I’m looking for feedback from artists trying the Windows v0.1.4 prerelease. PeerBrush is free, open source and experimental, with manual and compatible AI-agent editing in the same document.

## Try one small task

Use a copy of your artwork:

1. Open an image and make a layer, mask or brush edit.
2. Undo and redo the change, then save a new PSD and export PNG.
3. If you already use a compatible AI client, optionally connect it and try one small layer edit. Keep the original file separate.

Download: https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4

Setup for that version: https://github.com/Deftr0y/PeerBrush/blob/v0.1.4/docs/getting-started.md

Optional AI setup: https://github.com/Deftr0y/PeerBrush/blob/v0.1.4/docs/mcp.md

## What to share

- Version or commit and operating system
- The task you tried and the steps you took
- What you expected and what happened
- The missing feature or awkward step that matters most to your work
- AI client/version, only if the report involves an agent

A small screenshot or sample can help if you have permission to publish it. Don’t upload private artwork, tokens, connection files or full client configuration. A successful build or download alone does not tell us whether the editing workflow worked.

For a distinct reproducible bug, open a separate issue and link it here. General first-use observations and feature requests can stay in this thread.

The public release and current main branch differ. Limited PSD support and the experimental status matter when testing; preserve your original files. AI use requires a separate compatible client/model and may involve provider costs.
````

### 5 Contributor guide and scoped issue with TWiR submission

All parts remain pending review. Execute only an approved part or an explicitly approved chain, in this order:

1. Publish the approved guide at repository-root `CONTRIBUTING.md` using the approved repository workflow. Saving the guide inside this plan does not make that public contribution-guidelines destination exist.
2. Once the guide link works, create the approved scoped issue in PeerBrush after checking for duplicates and whether the task is already implemented.
3. Once that concrete issue is open and its prerequisites work, submit the approved CFP entry to the current upcoming [This Week in Rust draft](https://github.com/rust-lang/this-week-in-rust/tree/main/draft). Verify publisher access and the current contribution rules before opening a PR.

The initial source check found no public `CONTRIBUTING.md` and no open PeerBrush issues. This is a dated observation, not a guarantee about the state when the work resumes.

#### Proposed contributor guide

Intended destination: repository-root `CONTRIBUTING.md`. Relative links in this copy are written for that root destination, not for this plan's `docs/` location.


````markdown
# Contributing to PeerBrush

PeerBrush is a free, GPL-3.0 image editor built in Rust. Useful contributions include reproducible bug reports, feature proposals, documentation, platform testing and focused code changes.

Start with [README.md](README.md), [AGENTS.md](AGENTS.md) and the [development notes](docs/development.md). Check [open issues](https://github.com/Deftr0y/PeerBrush/issues) and [FOLLOWUPS.MD](FOLLOWUPS.MD) before starting. For a larger change, open an issue describing the problem and proposed scope first.

## Report a problem

Include your release or commit, operating system, reproduction steps, expected result and actual result. For graphics issues include the GPU/backend if known; for AI connection issues include the client/version. Say whether you used a portable release or built from source.

Only share artwork and logs you have permission to publish. Remove tokens, personal information and private paths. Do not upload connection files, full client configurations, runtime/recovery directories or credentials.

## Build and test

Use stable Rust and your platform's native compiler/linker prerequisites. The [CI workflow](.github/workflows/ci.yml) lists its Linux packages and Windows/macOS/Linux build steps. A successful CI build alone does not establish native desktop behavior.

```sh
git clone https://github.com/Deftr0y/PeerBrush.git
cd PeerBrush
cargo run --locked --target-dir target/contributor -- --state-dir .runtime/contributor
```

Use a separate runtime workspace for testing and the same state directory for CLI/MCP calls targeting that instance. Do not test against your working artwork.

Before submitting code, run the relevant tests and these CI checks where your environment supports them:

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --locked
cargo build --release --locked
```

State exactly what passed, failed or was not run, with your environment. Follow `AGENTS.md`: behavior changes require native workspace checks as well as engine/codec/protocol tests. Check undo/redo and saved-file behavior when affected. Do not claim GPU or native-platform validation from headless tests.

For documentation-only changes, verify paths, commands, links and the target release/commit. Clearly say if your review was source-only.

## Submit a focused change

Link the issue or explain the problem, keep unrelated changes separate, and describe the checks and remaining limits. Contributors are responsible for understanding and verifying submitted changes, including any generated code.

Preserve the existing GPL license, third-party notices and artwork provenance. UI, MCP and CLI must use the shared editing engine and reservations. Protect native precision, human work and unsupported document structures. Keep generated QA artifacts, build outputs and secrets out of commits.

Review timing and acceptance depend on the change; opening a PR does not guarantee either.
````

#### Proposed scoped issue

Intended destination: [PeerBrush new issue](https://github.com/Deftr0y/PeerBrush/issues/new), after the guide is published. Use the first heading as the issue title and the remainder as the body. The contribution-guide URL in the draft is a prerequisite, not a claim that it currently exists.


````markdown
# Add a first-connection troubleshooting checklist

Difficulty: easy documentation task; requires checking existing setup behavior against Rust source and docs. No new integration is requested.

## Why

`docs/mcp.md` describes registration, client presence and workspace matching in several places. Add a short symptom-based checklist so someone trying PeerBrush can distinguish a configured client from a working connection.

Contribution guidelines: https://github.com/Deftr0y/PeerBrush/blob/main/CONTRIBUTING.md

## Scope

Add a “Troubleshooting a first connection” section to `docs/mcp.md`, linked from the Connect AI section in `docs/getting-started.md`.

Cover:

- Registration succeeded but the top bar is red: reload/restart the client, check its approval step, and distinguish configuration from connection.
- Tools appear but editing fails: the editor must be running, with the intended executable and `--state-dir` workspace.
- A packaged build and development build coexist: identify the intended workspace without changing another instance's document.
- Automatic setup is unavailable or partially fails: use the documented settings panel and manual setup for the intended client.
- Safe reporting: OS, client/version, build/commit and sanitized error text. Never request tokens, connection files or full private configuration.

Starting points: `docs/mcp.md`, `docs/getting-started.md`, `src/main.rs`, `src/discovery.rs`, `src/server.rs` and `tests/discovery.rs`.

## Acceptance criteria

- Clearly distinguishes registration, editor availability and an attached client.
- Uses only commands/settings already supported in current source or documentation.
- Explains how to target the intended workspace and preserve existing settings.
- Includes safe report details and redaction guidance.
- Keeps the existing setup instructions intact and all links working.

No new transport, client integration, config cleanup, credential collection or permission bypass is in scope.

## Validation

Check the proposed instructions against the named files and current docs. If you can try a native client in a disposable workspace, report the exact OS/client/build and result. Otherwise mark the contribution source-reviewed only. Do not claim a successful live connection that you did not test.

The project uses GPL-3.0; preserve its license and existing third-party notices. Please comment before starting so work does not overlap.
````

#### TWiR submission draft and checks

The preserved draft below targets the 2026-10-14 edition. That date is not a publishing commitment. Before use, choose the actual next open draft and review any date/copy change. The statement that the maintainer reviewed the submission must be true before sending. Current [CFP guidelines](https://github.com/rust-lang/this-week-in-rust/blob/main/README.md#call-for-participation-guidelines) require a concrete open issue, difficulty, contribution guidelines and relevant requirements. Generic Projects/Tooling Updates are not accepted by the current README.


````markdown
# This Week in Rust CFP submission

Status: Conditional, not submitted. The prerequisite guide and scoped issue do not exist publicly yet. Do not submit a generic repository promotion issue as a substitute.

Verified current destination file:
https://github.com/rust-lang/this-week-in-rust/blob/main/draft/2026-10-14-this-week-in-rust.md

Submission workflow: PR to `rust-lang/this-week-in-rust`, base `main`, adding one entry under `CFP - Projects`. An authorized fork/branch publishing route is required. The README's `drafts/` wording is stale; the actual directory is `draft/`.

## Approval chain

1. Publish the exact approved contributor-guide draft above as `CONTRIBUTING.md` in `Deftr0y/PeerBrush` using a docs-only PR or the maintainer-approved branch workflow.
2. After that public link works, create the exact scoped issue from the draft above in `Deftr0y/PeerBrush`.
3. Substitute that newly returned issue URL in the entry below, then submit the approved CFP PR. This is a deterministic URL replacement, not new promotional wording.

Optional independent first action: publish the item 4 artist-feedback draft in PeerBrush as the landing point for artist reports. This is useful infrastructure, not audience distribution on its own.

## Exact PR title

Add PeerBrush connection-documentation task to CFP

## Exact PR body

Please consider this PeerBrush documentation task for Call for Participation in the October 14 issue.

The task is to add a symptom-based first-connection troubleshooting checklist. It has a difficulty level, source/doc starting points, acceptance criteria and public contribution guidelines. PeerBrush is a free, GPL-3.0 image editor built in Rust with a shared UI/MCP/CLI editing engine.

The issue link in this PR points to the open task. This submission and task description were drafted with AI assistance and reviewed by the project maintainer.

## Exact entry

* [PeerBrush - Add a first-connection troubleshooting checklist](CREATED_PEERBRUSH_ISSUE_URL)

The approval process must actually include maintainer review before the disclosure above is true. Recheck that the task is still open and that October 14 is still the next draft immediately before submission.

## Current rules

https://github.com/rust-lang/this-week-in-rust/blob/main/README.md#call-for-participation-guidelines

Requires an OSI-approved license, public issue tracker, a concrete issue, stated difficulty, a contribution-guidelines link and any special contributor requirements. A completed/closed issue is omitted. Selection is editorial, not guaranteed.

The current default-branch README requests disclosure for LLM-written articles and no longer accepts Projects/Tooling Updates PR submissions. CFP remains available. Do not use the stale `master` README for current policy.
````

## Deferred social routes and exact drafts

Every route below is on hold until readiness and approval. Use the maintainer's intended account after verifying identity and publishing access. No account creation is part of this plan.

### LinkedIn

Audience: artists and developers in the maintainer's professional network. Destination: the maintainer's own feed at [LinkedIn](https://www.linkedin.com/feed/). Verify the actual account before use; no profile URL is assumed. Follow [professional community policies](https://www.linkedin.com/legal/professional-community-policies), use accurate claims and disclose the maker relationship as in the draft.


````text
I'm building PeerBrush, a free, open-source image editor designed from the ground up for you and an AI agent to work on the same project.

If you're looking for an alternative to Photoshop and are willing to test an early tool, I'd like your feedback. PeerBrush already has layers, masks, selections, painting and editable effects. A compatible AI agent can inspect the document and edit through the same commands.

I'm iterating quickly, and feedback will help decide what I build next. Try a small editing task on a copy of your artwork and tell me what gets in your way, what's missing, or what you'd want next.

Developers: it's built in Rust with egui/eframe and wgpu, under GPL-3.0. Bug reports, feature suggestions, forks and focused PRs are welcome.

Windows v0.1.4 prerelease: https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4
Source and issues: https://github.com/Deftr0y/PeerBrush

It's experimental, with limited PSD compatibility. AI use requires a separate compatible client/model; any provider costs are separate.
````

### X

Audience: the maintainer's own network and interested artists/developers. Destination: an original post from the intended account at [X](https://x.com/), not a promotional reply to an unrelated thread. Verify account, composer limits and current [automation rules](https://help.x.com/en/rules-and-policies/x-automation) before posting. Those rules prohibit non-API website scripting. Plan human posting, or a separately authorized compliant API route; do not automate the website.


````text
PeerBrush is a free, open-source image editor I'm building from the ground up for human + AI editing. Early Windows build available. Looking for artist feedback, feature requests and Rust contributors.
https://github.com/Deftr0y/PeerBrush
````

### r/mcp

Audience: MCP users and developers testing creative tools. Destination and rules: [r/mcp](https://www.reddit.com/r/mcp/). The checked rules allow disclosed self-promotion with showcase flair and a real usable release; no waitlist or AI-generated slop. Recheck the current rules and composer before use. A signed-in account and exact-copy approval are required.


````text
Title: PeerBrush: a free Rust image editor with MCP access to the live document

I'm building PeerBrush, a free, open-source image editor designed from the ground up for human and AI editing in the same project. It has layers, masks, selections and painting. MCP tools can inspect the canvas and use the shared editing commands.

I'd like people to test the connection and editing workflow: connect your client, make one layer edit, inspect the result, then undo it. If it breaks, please include your OS, client/version, PeerBrush build and steps to reproduce. Remove tokens and private artwork from reports.

Windows v0.1.4 prerelease: https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4
MCP setup for that build: https://github.com/Deftr0y/PeerBrush/blob/v0.1.4/docs/mcp.md
Source and issues: https://github.com/Deftr0y/PeerBrush

A compatible AI client/model is separate; PeerBrush doesn't include hosted inference. I'm iterating quickly, and feedback will help decide what I build next. Feature requests, bug reports, forks and focused PRs are welcome.
````

### r/madeinrust

Audience: Rust contributors. Destination and rules: [r/madeinrust](https://www.reddit.com/r/madeinrust/). An image or video is required; repository-only posts are removed. Review a genuine app screenshot, such as the [release workspace screenshot](https://github.com/Deftr0y/PeerBrush/blob/v0.1.4/docs/images/workspace.png), before posting. If the image-post composer has no body field, links/body may need a first comment; approve that comment too. Recheck current rules and do not substitute an unapproved video.


````text
Title: PeerBrush: a free, open-source image editor built in Rust, looking for contributors

I'm building PeerBrush with Rust, egui/eframe and wgpu. It's an early image editor designed from the ground up for people and compatible AI agents to work on the same document through MCP.

UI, MCP and CLI edits use a shared engine. The public v0.1.4 prerelease includes layers, masks, painting and native 16-bit editing for supported raster documents.

I'd particularly value clean-checkout setup reports, first-connection documentation improvements and focused regression tests. If you want to help, tell me what area you'd like to work on in a GitHub issue. Forks and PRs are welcome.

Source: https://github.com/Deftr0y/PeerBrush
Development notes: https://github.com/Deftr0y/PeerBrush/blob/main/docs/development.md

GPL-3.0. Windows has a portable prerelease; macOS/Linux builds are configured in CI, with broader native validation still incomplete. Main is ahead of the downloadable release, so please include your commit or release in reports.
````

### r/aiArt permission request then comment

Audience: artists exploring AI-assisted editing. [r/aiArt rules](https://www.reddit.com/r/aiArt/) require moderator permission for AI projects. First approve and send the permission request to the [moderation team](https://www.reddit.com/message/compose/?to=/r/aiArt). Approval to ask moderators does not approve the later comment. Wait for moderator permission, verify the current designated self-promotion thread and separately approve the comment before posting. The checked thread rules allow no selling and require at least seven days between reposts; no repost is scheduled.

Permission request:


````text
Subject: Permission to request PeerBrush feedback in the self-promotion thread

Hi mods, I'm developing PeerBrush, a free, GPL-3.0 image editor built from the ground up for manual and AI-assisted editing in the same layered document. A compatible AI agent connects through MCP; it isn't a hosted image generator.

Could I post a short invitation in your designated self-promotion thread asking artists to try the Windows prerelease and suggest improvements to layers, masks and the human/agent workflow? I'd link the public GitHub release and source, clearly identify myself as the developer, and keep it to one comment with no sales or private messages to members.

Project: https://github.com/Deftr0y/PeerBrush
Release: https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4
````

Proposed comment for the [previously verified self-promotion thread](https://www.reddit.com/r/aiArt/comments/1o4s6st/10122025_ongoing_selfpromotion_thread_promote/). Confirm that this is still the current, open and permitted destination; the thread's existence alone is not permission.


````text
I'm building PeerBrush, a free, open-source image editor designed from the ground up for you and an AI agent to edit the same project. You can paint and work with layers and masks manually; a compatible agent can inspect the canvas and make editable changes through MCP.

I'm looking for artist feedback on one small workflow: open a copy of an image, make a layer or mask edit, then try asking your agent for a color adjustment on another layer. What felt awkward, and what editing feature would make this useful for you?

Windows v0.1.4 prerelease: https://github.com/Deftr0y/PeerBrush/releases/tag/v0.1.4
AI setup: https://github.com/Deftr0y/PeerBrush/blob/v0.1.4/docs/mcp.md

It's experimental. The app is free; the AI client/model is separate and may have its own costs. I'm iterating quickly, and feedback will help decide what I build next. Concrete bug reports and feature requests are welcome here or in GitHub issues.
````

## Deferred editorial and directory routes

These additional routes are researched options, not ready-to-submit drafts. Prepare destination-specific wording and obtain approval after the prerequisites below are satisfied. Policies were checked on 2026-10-09 where public; rules behind sign-in or unspecified AI/automation policies still need review.

### Changelog News

- Audience: software developers and open-source contributors.
- Submission and rules: [Changelog news form](https://changelog.com/news/submit).
- The form encourages submitting one's own work, asks for a URL, title and explanation of what is interesting, and requires an account. Commercial-product pitches, tutorials and reader-hostile sites are excluded from the news route.
- Lead with a real release and the Rust shared-document/MCP engineering, then invite scoped contributions. Do not use the paid sponsorship route.
- Prerequisites: relevant release story, truthful limitations, exact-copy approval and authorized publishing access. AI-writing/automation permission is not established by the intake page.

### itch.io

- Audience: game artists, indie developers and people testing creative tools.
- Destination: a real tool project page through [itch.io creator publishing](https://itch.io/docs/creators/getting-started), followed by [Get Feedback](https://itch.io/board/255031/get-feedback) only when its rules are satisfied.
- Rules: [creator FAQ](https://itch.io/docs/creators/faq) and [quality guidelines](https://itch.io/docs/creators/quality-guidelines). Tools are supported. The feedback board requires a genuine itch.io project/profile page; a GitHub-only promotion is insufficient.
- Prerequisites: approved account/publishing route, Windows package, accurate development status, real screenshots and correct AI-created-asset disclosures. Repetitive promotion or posting advertisements on others' projects is excluded.
- Creating a page, uploading a build and posting for feedback are separate actions to approve. Review any ongoing release-maintenance burden; do not invent an account or project URL.

### AlternativeTo

- Audience: people comparing image editors and alternatives to established applications.
- Submission: follow [Add a new application](https://alternativeto.net/faq/#add-a-new-application) using an authorized verified-email account.
- The [FAQ](https://alternativeto.net/faq/) distinguishes potentially eligible public/open beta from ineligible Early Access, private/invite-only or announced-only products. Clarify PeerBrush's eligibility honestly; do not relabel its stage to get accepted.
- Prerequisites: an eligible public release, accurate platform/license metadata, icon/screenshots and a neutral factual description. State that this is an early image-editing alternative with limited PSD support, without parity claims.
- Use only the free review queue; no completion or acceptance is promised. Optional priority review is not approved. Recheck current AI-copy and submission rules.

### OpenAlternative

- Audience: people seeking open-source alternatives to proprietary software.
- Submission: [OpenAlternative submit](https://openalternative.co/submit). Scope: [about the directory](https://openalternative.co/about).
- The site supports GitHub-backed open-source tools; the submission route requires sign-in. Full form requirements, costs and AI/automation rules need review inside the authorized flow.
- Prerequisites: GPL-3.0 metadata, project URL, real screenshots and candid experimental status. Use a free route only if available; stop if payment is required.
- Mention Photoshop only as an honestly qualified comparison category, with no equivalence or affiliation claim.

### SourceForge

- Audience: users browsing open-source desktop software and downloads.
- Submission: [create project](https://sourceforge.net/p/add_project); [official hosting and distribution options](https://sourceforge.net/create/).
- Existing-release distribution and an open-source directory presence are supported. An account and explicit terms acceptance are required; uploads and future release upkeep add work.
- Prerequisites: approval for the precise project/listing setup, terms, files and publishing route. Keep GitHub as the canonical contribution/issue destination unless a separate migration is requested.
- Do not create duplicate issue tracking, mirror releases automatically or commit to future hosting maintenance. AI-copy/automated-creation policy needs review.

### Glama

- Audience: MCP developers and users discovering creative-tool servers.
- Submission: “Add Server” at the [Glama server directory](https://glama.ai/mcp/servers). Rules and requirements: [Glama MCP FAQ](https://glama.ai/mcp/faq).
- A GitHub-source submission accepts repository URL, display name and short description. License, security and server-health checks affect indexing. Optional `glama.json` can configure metadata/build information; adding it is a separate engineering change.
- Prerequisites: clear local-editor and adapter setup, accurate transport/platform description, a safe working installation and exact submission approval. Check for an existing listing first.
- Do not describe PeerBrush as a deployed remote connector or include local endpoint credentials. Initial account requirements and AI-written-submission policy require confirmation in the current flow.

### Smithery

- Audience: users who want an installable local MCP integration.
- Submission: [Smithery publishing destination](https://smithery.ai/new); [official publishing guide](https://smithery.ai/docs/build/publish).
- Local stdio distribution uses a prebuilt `.mcpb` bundle. A public HTTPS Streamable HTTP server is a different route and does not describe the current local PeerBrush setup.
- Future engineering prerequisites: design/package the adapter and its editor dependency, supply configuration metadata, test clean installation and workspace selection, obtain publisher authentication and approve the artifact/release publication.
- MCPB packaging/publication is not implemented by this plan. CLI/API publication is documented, but no command is scheduled or authorized here. Do not expose the local editor to the public internet merely to fit a listing flow.

### Official MCP Registry

- Audience: MCP clients, registries and users discovering installable servers.
- Destination: [official registry](https://registry.modelcontextprotocol.io/), using the [publishing quickstart](https://modelcontextprotocol.io/registry/quickstart).
- [Supported package types](https://modelcontextprotocol.io/registry/package-types) include Cargo/crates.io and MCPB binaries hosted on GitHub/GitLab Releases. A rewrite into npm is not required just for registry eligibility.
- Future engineering prerequisites: choose a supported distribution route, produce and test the artifact, provide `server.json`, verify the namespace and satisfy package-ownership validation. MCPB metadata needs a SHA-256 checksum. Cargo installation requires Rust; a properly packaged binary route can avoid that requirement for users.
- Registry packaging/publication is not implemented by this plan. Treat package releases, namespace authentication and registry publication as separately authorized work. Never include credentials or local connection configuration in public metadata.

### Product Hunt

- Audience: makers and early adopters after there is a credible demo and a usable download.
- Destination and preparation: [Product Hunt launch workflow](https://www.producthunt.com/launch/preparing-for-launch); [community guidelines](https://help.producthunt.com/en/articles/3615694-community-guidelines).
- Human-managed only. Use a real person's account. Prepare a thumbnail, at least two real gallery images and a description within the current 500-character limit; verify requirements again at launch. A direct GitHub product link can be used when eligible.
- Prerequisites: maker review, approved assets, release readiness and availability to answer questions personally. Self-launch and genuine feedback requests are allowed; bots, mass messages and upvote solicitation are excluded.
- No automated launch, comments, votes, account creation or scheduled submission is planned.

## Excluded or permission-dependent venues

Keep these constraints when revisiting distribution; do not recycle the saved AI-assisted drafts into destinations that prohibit them.

- Hacker News: [guidelines](https://news.ycombinator.com/newsguidelines.html) prohibit generated/AI-edited comments and automated posting. No automated or AI-written campaign is queued.
- r/opensource: [current rules](https://www.reddit.com/r/opensource/) prohibit AI-generated content. No generated launch post is queued.
- DEV: [AI-assisted article guidelines](https://dev.to/guidelines-for-ai-assisted-articles-on-dev) prohibit AI-assisted/generated program promotion and AI-written comments. No promotional draft is queued there.
- Tech-Artists.org: [terms](https://www.tech-artists.org/tos) prohibit machine-generated content. Excluded from automated/generated outreach.
- Polycount: [community rules](https://polycount.com/discussion/63361/information-about-polycount-new-member-introductions) prohibit AI-written replies and expect participation before promotion. Excluded from automated/generated outreach.
- PIXLS.US: [FAQ](https://discuss.pixls.us/faq) rejects chatbot answers and discourages generative-LLM solutions. Hold rather than assume this project invitation fits.
- r/DigitalArt: [community rules/wiki](https://www.reddit.com/r/DigitalArt/wiki/index/) limit AI discussion. A promotion thread does not establish permission for this invitation; moderator clarification would be needed.
- MCP.so: [server submission](https://mcp.so/submit?type=server) showed a $39 charge during research. Skip; no paid budget is approved. Rechecking a price does not authorize purchase.

## Resumption order

1. Reopen item 4 and check its release links and current issue tracker when the maintainer gives the go-ahead. Publish only that approved issue and verify the result.
2. Keep items 1–3, item 5 and every social/directory route on hold. Resume them only after software readiness and destination-specific approval.
3. Prefer a small, supportable batch over publishing everywhere at once. Use incoming feedback to decide whether the build or setup documentation needs work before the next batch.
4. Recheck submission rules, copies, actual account access and duplicate listings on the day each route is used. Choose the current TWiR edition only when its chain is ready.
5. Treat Smithery/official-registry packaging and any hosting or account setup as future implementation work, not promotion-only form filling.

## Results ledger template

No outreach result is asserted by this plan. Add one row only when an action is actually attempted or completed, and distinguish submitted, published, rejected, blocked and pending review.

| Date | Item or route | Approved destination and copy revision | Release or commit | Result | Verified public URL or non-sensitive receipt | Feedback or next step | Follow-up explicitly agreed |
| --- | --- | --- | --- | --- | --- | --- | --- |
| YYYY-MM-DD | e.g. first-wave 4 | Record approved target and plan commit | vX.Y.Z or SHA | Not started / submitted / published / blocked | URL when verified | Brief factual note | None, or agreed scope/date |

Keep this public ledger free of personal contact details, private messages, credentials, tokens, connection files and account-session information. Record meaningful testing reports, reproducible issues and contributor interest rather than treating views, votes or listing count as proof of product readiness.
