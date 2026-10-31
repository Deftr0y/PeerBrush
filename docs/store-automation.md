# Microsoft Store release submission (inactive draft)

This infrastructure is **not an active end-to-end release integration**. Keep
`MSSTORE_AUTOMATION_ENABLED` unset until all gates below are satisfied. It does
not build MSIX packages or publish GitHub releases. No Store credentials are
included, and no real submission has been tested.

Windows distribution for 0.30.0 is Store only; standalone installer certificates
and the dormant direct Windows updater are not requirements of this channel.
Store-only final composition binds the unsigned SDK submission package and its
reports without an App Installer or direct signing dependency. Store-managed
installation and upgrades still require genuine native acceptance.

The final-input acquisition selector supports schema 2 for this channel. Its
five roles are candidate, macos_signed, macos_native, linux_native and
store_package. The Store package is the existing unsigned-store-msix-candidate
artifact from the same exact successful first main release.yml run as the
ordinary candidate, with a distinct artifact ID. Select independently pinned
outer ZIP and exact MSIX/report/checksum member digests. Direct Windows signing,
runtime, descriptor and native-direct artifacts are excluded from this variant.
Existing bounded extraction, source/content reinspection, Mac/Linux native
authentication and late original/metadata checks still run. Schema 1 remains
the historical direct-Windows input contract. Downloading a successful SDK
artifact does not itself grant installed acceptance or Store public readiness.

The protected publisher has an explicit same-invocation Store handoff. It can
run only after that invocation freshly publishes the exact final release and
reads back its public bytes. An already-public release, rerun, lost publish
response or missing/expired receipt cannot start another automatic Store write.
First-attempt, exact-main, native readiness and protected approval checks remain.
`PEERBRUSH_STORE_HANDOFF_ENABLED` is literally false; no live writer is qualified.

Before create, draft update, package upload and commit, the adapter rechecks
the actual Partner Center product, unchanged published baseline and exact pending
submission owner. A sanitized write-ahead receipt is retained when possible;
the complete draft metadata is also compared, stopping when concurrent owner
edits to listings or pricing are observed. This is a read-before-write check,
not a service-side atomic lock; a qualified run requires exclusive operator
ownership of the product's submission throughout the handoff. Each in-process create/update/upload/commit permission
is consumed before its request, including when a response is lost. Unknown
completion cannot be turned into a second upload or commit.
The receipt contains no credential or SAS URL. Artifacts are diagnostic evidence, never
service truth or replay authority. A crash before receipt upload leaves a
manual/service reconciliation hold. Certification is not public availability.

The publisher and read-only Store workflow share product concurrency with
`cancel-in-progress: false` and `queue: max` (at most 100 pending runs). Overflow,
cancellation and an existing Store draft remain visible holds. This bounded
route needs no separate hosting purchase. The standalone `--submit --state` CLI
retains its durable private filesystem contract for explicit operator recovery;
hosted artifacts do not replace that contract.

## Missing integration gates

1. Integrate and review the Windows Store packaging/runtime changes into the
   active version branch, then promote through `version/0.3` → `dev` → `main`.
   Do not blindly merge the historical Store candidate branch: it also contains
   superseded signing/release changes. Preserve newer fixes and attribution.
2. The actual release publisher must build the Windows MSIX from the same
   reviewed commit as the other platforms and attach exactly one x64 `.msix`,
   its matching `.msix.json` provenance report, and `.msix.sha256` checksum to
   the GitHub release **before publishing it**. Use the reviewed MSIX packager;
   do not fabricate these reports for arbitrary binaries. The current candidate
   workflow does not publish releases or provide this end-to-end contract.
3. Review and qualify the publisher's same-invocation handoff described above.
   This separate workflow's retained
   `release` and reusable declarations are explicitly blocked by the job and
   pre-checkout guards; do not wire an assumed publisher or inherit credentials
   into an unreviewed caller. A `release: published` listener alone is also
   insufficient: releases created using `GITHUB_TOKEN` do not start downstream
   event workflows. The future publisher must supply the exact published tag,
   source SHA, independently selected Windows portable hash and final
   SDK-validated MSIX hash. Candidates are not published releases.

   The existing local CLI already requires durable private `--state` before
   submission and journals every uncertain mutation. A runner temporary file,
   cache or Actions artifact is not a persistence adapter. Hosted submission is
   refused before checkout even when `submit=true` and the enable variable is
   set. See [the state and host boundary](production-automation.md#private-store-submission-state).
4. Verify API access for the existing Partner Center account. An Entra tenant
   must be associated and an application authorized for Store submissions.
   Account-type eligibility must be confirmed in Partner Center; do not assume
   an individual account has the necessary application-management controls.
5. Have the maintainer securely configure repository Actions secrets
   `MSSTORE_TENANT_ID`, `MSSTORE_CLIENT_ID`, `MSSTORE_CLIENT_SECRET` using the
   application credentials. This direct MSIX API uses the Store ID
   `9PNR6T1Q2ZL9` as `applicationId`; it is **not** the Entra client ID and does
   not require the CLI's seller ID. Credential creation and access grants are
   separate security operations. Do not paste values into issues, code or logs.
6. Keep `MSSTORE_AUTOMATION_ENABLED` unset pending explicit approval, offline
   verification and actual provider access. A separately approved manual
   `submit=false` preflight must select `automation_commit` as the independently
   reviewed full main automation/workflow SHA, plus `tag`, `source_commit`,
   `portable_sha256` and `msix_sha256` for the real published stable release.
   It authenticates and checks Store state without creating a submission.
   Genuine next-version submission, crash/reconciliation qualification,
   certification and public readback remain required for end-to-end
   success. No account, credential, permission or enable setting is configured
   by preparing this code.

## Behavior and boundaries

- Only published, non-prerelease numeric tags are accepted. `v0.25`, `v0.25.0`
  and older releases are rejected. There is no backfill or scheduled scanner.
- Store version mapping is `Cargo major + 1 . minor . patch . 0`. The package
  must be newer than both `1.25.0.0` and the current published Store packages.
  An intentional future mapping change needs code review.
- Verify the final package against the independently selected Windows portable
  and clean exact-source checkout before Store authentication. The shared payload
  verifier checks the executable, embedded build identity, corresponding source,
  notices, launcher, complete manifest, logos and block map, alongside report/hash,
  app/Store version and approved product identity. A manifest-only ZIP is invalid.
  This is not a replacement for SDK validation,
  installed-runtime QA or source/license review by the package builder.
- A fixed isolated inline guard runs before any checkout or repository script.
  Only a first-attempt manual invocation of the official main Store workflow on
  hosted Ubuntu x64 is accepted. The independent `automation_commit` must equal
  both `GITHUB_SHA` and `GITHUB_WORKFLOW_SHA`; checkout uses that `github.sha`,
  never a moving default branch or the release input. The existing release gate
  verifies the actual automation checkout afterward. The separate
  `source_commit` is release data and may differ from the automation SHA; none
  of its scripts executes. Its full SHA and both independently selected hashes
  must be well formed before checkout. None is taken from the MSIX report.
  Select the final MSIX hash only after the native SDK validation job succeeds;
  custom footprint checks cannot establish SDK acceptance.
- Only the final authenticated read-only preflight step receives the existing
  Partner Center secrets and read-only GitHub token. No hosted step forwards
  `--submit` or invents `--state`. The preflight still requires authorized Store
  access, because it checks actual product identity, pending submissions and
  published version. The unchanged local CLI's explicitly authorized write
  route has the persistence rules above; this workflow supplies no write host.
- Refuse any existing pending submission. Never delete, resume, replace or
  overwrite it automatically. A successful creation followed by failure leaves
  its draft for deliberate inspection. A retry cannot create a second draft.
- Clone the API-created draft and modify only its package collection. Preserve
  listings, price, availability, certification notes and target publish mode.
  Unexpected non-x64 inherited packages stop the operation for review.
- The runner checks commit status for up to ten minutes and fails visibly on
  failure, unknown status or timeout. Reaching preprocessing means the package
  entered processing, **not** that certification
  passed or the update is live. Existing manual/scheduled publication settings
  remain effective. Verify Partner Center status and the public Store after
  certification. A rejected update does not remove the existing live release.
- No blind network retries. If an upload/commit response is uncertain, inspect
  the pending submission first. Logs never print credentials, upload SAS URLs,
  authentication responses or full submission metadata.
- GitHub concurrency does not preserve an unlimited FIFO queue. If several
  releases arrive while another job is pending, inspect Actions and manually
  dispatch the missing intended release after the Store becomes available.
  This draft deliberately fails closed rather than discarding another draft.

Run offline tests:

```sh
python scripts/test_submit_store_release.py
python scripts/test_store_release_state.py
python -B scripts/test_store_workflow.py
```

## Official references

- [Microsoft Store submission API prerequisites](https://learn.microsoft.com/en-us/windows/uwp/monetize/create-and-manage-submissions-using-windows-store-services)
- [MSIX app identity lookup](https://learn.microsoft.com/en-us/windows/uwp/monetize/get-an-app)
- [Create a submission](https://learn.microsoft.com/en-us/windows/uwp/monetize/create-an-app-submission)
- [Manage app submissions and upload sequence](https://learn.microsoft.com/en-us/windows/uwp/monetize/manage-app-submissions)
- [Microsoft's CLI/GitHub Actions setup](https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/github-actions)
- [GitHub workflow trigger restrictions](https://docs.github.com/en/actions/how-tos/writing-workflows/choosing-when-your-workflow-runs/triggering-a-workflow)
