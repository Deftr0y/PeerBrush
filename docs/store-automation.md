# Microsoft Store release submission (inactive draft)

This infrastructure is **not an active end-to-end release integration**. Keep
`MSSTORE_AUTOMATION_ENABLED` unset until all gates below are satisfied. It does
not build MSIX packages or publish GitHub releases. No Store credentials are
included, and no real submission has been tested.

## Missing integration gates

1. Integrate and review the Windows Store packaging/runtime changes into the
   normal version branch, then promote through `version/0.25` → `dev` → `main`.
   Do not blindly merge the historical Store candidate branch: it also contains
   superseded signing/release changes. Preserve newer fixes and attribution.
2. The actual release publisher must build the Windows MSIX from the same
   reviewed commit as the other platforms and attach exactly one x64 `.msix`,
   its matching `.msix.json` provenance report, and `.msix.sha256` checksum to
   the GitHub release **before publishing it**. Use the reviewed MSIX packager;
   do not fabricate these reports for arbitrary binaries. The current candidate
   workflow does not publish releases or provide this end-to-end contract.
3. Wire the real GitHub release publisher to call this reusable workflow after
   publication. A `release: published` listener alone is insufficient: releases
   created using `GITHUB_TOKEN` do not start downstream event workflows. A
   reusable job is explicit and does not need a new personal access token:

   ```yaml
   store:
     needs: publish
     uses: ./.github/workflows/store-submit.yml
     with:
       tag: ${{ needs.publish.outputs.tag }}
       submit: true
     secrets: inherit
   ```

   `publish` must expose its exact published tag; adapt the job name to the
   actual publisher. Never add this after candidate building and imply that
   the candidate is already published. A person publishing via GitHub's UI
   triggers the release listener. If both paths trigger, product-wide
   concurrency plus Store pending/version checks prevent duplicate submissions.
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
6. Review the complete pipeline, run the offline tests, and only then enable
   the repository variable `MSSTORE_AUTOMATION_ENABLED=true`. First manually
   dispatch with `submit=false` and a new, reviewed, fully packaged stable
   release. This authenticates and checks Store state without changing it.
   An actual next-version submission and subsequent certification/publication
   verification remain required to establish end-to-end success.

## Behavior and boundaries

- Only published, non-prerelease numeric tags are accepted. `v0.25`, `v0.25.0`
  and older releases are rejected. There is no backfill or scheduled scanner.
- Store version mapping is `Cargo major + 1 . minor . patch . 0`. The package
  must be newer than both `1.25.0.0` and the current published Store packages.
  An intentional future mapping change needs code review.
- Verify release checksum, report hash, resolved tag commit, app version,
  package version, x64 architecture and exact manifest/product identity before
  requesting any Store mutation. This is not a replacement for SDK validation,
  installed-runtime QA or source/license review by the package builder.
- The automation code is checked out from the repository's default branch,
  never executed from downloaded artifacts or a release tag with secrets.
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
```

## Official references

- [Microsoft Store submission API prerequisites](https://learn.microsoft.com/en-us/windows/uwp/monetize/create-and-manage-submissions-using-windows-store-services)
- [MSIX app identity lookup](https://learn.microsoft.com/en-us/windows/uwp/monetize/get-an-app)
- [Create a submission](https://learn.microsoft.com/en-us/windows/uwp/monetize/create-an-app-submission)
- [Manage app submissions and upload sequence](https://learn.microsoft.com/en-us/windows/uwp/monetize/manage-app-submissions)
- [Microsoft's CLI/GitHub Actions setup](https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/github-actions)
- [GitHub workflow trigger restrictions](https://docs.github.com/en/actions/how-tos/writing-workflows/choosing-when-your-workflow-runs/triggering-a-workflow)
