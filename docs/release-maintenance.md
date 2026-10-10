# Release and public-information maintenance

Contributor instructions for keeping PeerBrush's published information accurate. This is a maintenance procedure, not a running integration or permission to publish a release, deploy a site, start a campaign, or obtain new account access.

## Scope and gates

For each authorized release or patch, maintain GitHub release artifacts and notes, the official website, confirmed official Discord announcement channels, and confirmed official social accounts as one release task. For material project developments, maintain applicable documentation and publish an explicitly labelled development update only within the authorized communication scope. Do not require a new feature to wait for a release to be documented; do not represent unreleased work as downloadable.

Before any external change, confirm the current publishing scope, destination ownership, appropriate approval, access and platform rules. Follow current maintainer holds and release-readiness gates. A maintenance instruction does not lift a hold. Do not interpret third-party community lists as official destinations. Third-party promotion, unsolicited messages, directory submissions, new accounts, paid services and broader campaigns need their own authorization. Repository instructions never override the acting agent's safety or permission requirements.

## Public destination registry

Keep this registry limited to verified, public project URLs and public content source locations. Add or change an entry only after verifying that it is an official project destination. An entry alone does not establish publishing permission.

| Surface | Verified public destination | Content source |
| --- | --- | --- |
| GitHub | https://github.com/Deftr0y/PeerBrush | This repository |
| Releases | https://github.com/Deftr0y/PeerBrush/releases | GitHub releases, tag-specific checkpoint notes |
| Website | https://peerbrush.com/ | Reviewed public `site-content` build output, deployed by `.github/workflows/pages.yml`; source and publication records are maintained privately |
| Discord announcements | Not configured in this registry | Confirm official public destination before posting |
| Social accounts | Not configured in this registry | Confirm each official public profile before posting |

Do not guess domains, hosting URLs, social handles, server/channel identifiers or account ownership. A requested domain, prepared site or chosen hosting provider is not evidence of a live website. Never add credentials, private invitation/admin URLs, personal account identifiers, access details, internal task links or private operating records to this registry. Keep necessary access and approval information in an approved private location outside the repository.

## Establish the release facts

1. Read the current branch instructions and relevant checkpoint, development and setup documentation. Fetch fresh remote state; preserve concurrent changes. Check existing publication records before doing any work.
2. Query the releases collection, not only the `releases/latest` endpoint: the latter can omit prereleases or return 404 when only prereleases exist. Exclude drafts. Identify the exact tag, resolved commit, publication time, prerelease flag, actual uploaded assets and their download URLs.
3. Keep stable versions, versioned prerelease checkpoints and development snapshots distinct. Respect the established selection policy for each website section. If no policy is confirmed, show separately labelled published choices or request a decision; do not silently promote a development snapshot to the default stable download. Do not rely on tag names or collection order alone.
4. Build patch notes from the actual changes since the relevant previous release, merged work, reproducible checks and known limitations at the selected commit. Link evidence. Current `main`, roadmap entries, open issues and planned features are not proof of released behavior.
5. Require Windows, macOS and Linux builds for each new release, all from the same version/tag and resolved source commit. Verify a per-platform matrix of architecture, build/test result, package identity, checksum and actual public asset URL. If any build fails or is unavailable, report that platform as blocked and the release synchronization as incomplete; never substitute an older binary under the new version or label it current. Do not confuse successful builds with native desktop verification.
6. Use one factual release summary across destinations: version/tag, release type, shipped changes, fixes, breaking changes or migration steps, known limitations, verified platform/architecture assets, testing limits and canonical release link. Adapt length, not facts. Use concrete language without slogans or filler.

## Complete the publication sequence

Perform only steps covered by the current authorization. When blocked, prepare permissible work and record the blocker instead of silently skipping it.

1. **GitHub and packages:** update release/checkpoint notes and relevant README/setup/development documentation. For an authorized release, build and package the exact selected commit using the current supported workflow. Run applicable tests and inspect packages for matching source, licenses, expected executable/assets, archive integrity and privacy. Upload only approved artifacts and verify the release tag, type, notes and actual asset inventory after publication. Never rewrite an existing tag or silently replace a published binary. Source changes after a release belong to a separately identified build.
2. **Website:** synchronize the home version/status, Downloads entries, platform labels, release notes and links with the selected published release(s). Use returned release-asset URLs; do not invent missing platform packages. A CI configuration or successful Actions artifact upload is not a public release download. Maintain a permanent website version history with each published version/tag, release date, patch notes, release type, source commit and real per-platform versioned asset links/checksums. Drive the home/latest display, Downloads and full history from one consistent release data source. Add new entries without overwriting older notes or removing historical download links; do not delete published artifacts as routine cleanup. Label stable, prerelease and development entries distinctly. Historical releases may honestly lack some platform builds; do not invent retroactive binaries. Explain experimental/unsigned/untested limitations where applicable. Verify the deployed page after an authorized deployment, not merely local source or a successful build.
3. **Discord and social accounts:** after release/download verification, publish the concise factual update in each confirmed official destination permitted by the current scope. Include the canonical release link and relevant limitations. Check platform automation and posting rules. For a development update, link its source/commit and explicitly state whether the change is unreleased. Do not announce planned work as shipped.
4. **Reconcile:** read back each changed destination. Confirm visible text, release label, version and working links match the intended revision; confirm assets are publicly obtainable. A queued request, accepted API call or local edit alone is not verified publication. Report inaccessible or failed destinations individually.

The existing [CI workflow](../.github/workflows/ci.yml) builds and uploads Actions artifacts; do not assume it publishes GitHub releases or updates any external channel. Changes to automation, deployment settings or credentials require the applicable separate permissions.

## Idempotency, receipts and recovery

Use a stable publication key: repository + exact release tag (or development commit) + update kind + destination. Before posting, inspect the existing destination and an approved private publication record for that key. Prefer correcting an existing post within the authorized scope over creating another. Do not retry an ambiguous send until its outcome has been reconciled. Do not repost an unchanged release just because a job reruns.

Record only necessary operational data in the approved private record outside the public repository: publication key, source commit and release URL, content revision/hash, destination, status, timestamp, returned message/post/deployment receipt, read-back verification and any blocker/next step. Suggested statuses: prepared, pending approval, held, sent-unverified, verified, failed, blocked, not applicable. Never store credentials in the record. No public ledger of personal workflow or private channel/account details belongs here.

A release-published event is a useful future trigger only after an integration is explicitly configured and authorized. It must follow the same gates and idempotency checks. At the start/end of subsequent release work, reconcile published releases against the site and authorized announcement receipts to catch missed events or partial failures. Check after a material release correction as well. Retry only missing/failed destinations after resolving the cause; leave verified destinations alone. Documentation does not create a background scheduler.

Close the task only when every in-scope destination is verified or explicitly reported as held, blocked, failed or not applicable. Report what actually shipped, what was checked and what remains, without claiming all channels are updated when any result is unknown.

## Privacy and security publication gate

Before committing or publishing, inspect the proposed diff and tracked files, relevant history, packaged source/release archives, site output, logs and screenshots for accidental disclosure. Inspect the actual artifact that will be shared, not only an ignore file. Never claim a repository or artifact is clean without performing the relevant checks; state coverage and limits.

Exclude secrets and private working material: credentials, tokens, API keys, passwords, webhook URLs containing secrets, `.env` files, runtime connection manifests, private client/config files, recovery data, local paths/device identifiers, personal correspondence, private assistant coordination and personal working practices. Use synthetic or redacted examples. Only include public contributor-facing build/test and release instructions. Existing tracking or historical disclosure is not authorization to republish.

If sensitive material is found, stop the affected publication and report privately. Do not reproduce the value in an issue, commit message, log or public report. Coordinate credential rotation or history cleanup through the appropriate authorized process; deleting a working-tree file does not remove earlier commits or already-published archives.
