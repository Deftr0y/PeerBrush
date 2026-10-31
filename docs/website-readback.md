# Deployed website readback - unreleased

`scripts/readback_site_release.py` is a read-only comparison tool for a separately reviewed, deployed release overlay. It performs unauthenticated public HTTPS GETs and local source inspection. It does not publish a release, deploy Pages, change Git or settings, submit to a store, contact signing providers, or start an updater. Downloaded packages/source use temporary staging that is removed after inspection; no downloaded code executes. The result is bounded JSON on stdout, with exit status 0 for the verified comparison or 2 for mismatch/unavailable. Invalid local inputs produce a fixed unavailable result without exception text, paths or a traceback.

Use the exact external seven-file overlay and `website-output.json` from [website output preparation](website-release-output.md), plus an exact checkout of that released source SHA:

```sh
python scripts/readback_site_release.py \
  --expected /private/reviewed-website-candidate \
  --source-root /checkout/of/exact-released-source
```

This is a future post-publication check. There is no published 0.30 release/inventory to verify, and this implementation has not probed an unpublished version or performed deployed acceptance. The command is not wired to automatic deployment or a background schedule.

## Expected comparison target

The input report must identify one canonical stable tag, exact source/site commits, all seven expected file hashes and `release_inventory_sha256`. The selected history entry must retain that same digest in `releaseInventorySha256`. The tool reopens each file, rejects links/reparse points within the input, checks its bounded length/hash, validates the selected platform rows before rendering, and requires an idempotent renderer result. This checks that the expected static home, fallback, cache tokens, documentation and schema2 latest/history values describe one prepared overlay. Historical targets are rejected until both their source and the retained latest downloads can be independently verified; the tool does not claim to reverify every historical package. A blocked report without a public-site overlay is not acceptable. Malformed platform data, duplicate/missing latest entries, altered expected files, extra overlay files, old reports without the inventory binding and a mismatched source checkout all stop before network requests.

`release_inventory_sha256` is SHA-256 of the public inventory's canonical JSON representation (sorted keys, compact separators). It binds only the reviewed public version/source/asset metadata; it does not authenticate private signing reports, Store submission state or an operator's approval. The expected report and files are comparison targets, never proof of production readiness. Earlier prepared reports that lack this field require a fresh reviewed preparation; the reader does not invent a binding.

## Website and cache URLs

The fixed website origin is `https://peerbrush.com/`. The reader checks each canonical file below and the homepage `/`:

- `downloads.json`
- `index.html`
- `main.js`
- `platform.js`
- `releases.js`
- `release-page.js`
- `releases.html`

It also enumerates the actual module/feed query URLs from the expected HTML and JS, including versioned `main.js`, `release-page.js`, shared imports and the `downloads.json` query copied by those modules. Canonical files and query variants must independently match the expected bytes. A fresh unversioned file cannot hide a stale versioned cache response, or vice versa. There are at most 16 distinct website requests, each at most 2 MiB. Allowed queries contain one canonical stable `release=vA.B.C` and, optionally, a numeric `layout`; unknown parameters, credentials, ports, fragments, foreign hosts and every website redirect are rejected. The www subdomain and a redirect from an HTML path to the homepage are deliberately not silently accepted.

Requests send `Cache-Control: no-cache` and `Pragma: no-cache`, with no Authorization header or account credentials. A cached 304 cannot establish equality without bytes and is unavailable. Missing/failed requests are unavailable; returned bytes that differ from the expected file are a mismatch. The report records only bounded official paths and expected/actual SHA-256 values, never response bodies, presigned redirect URLs or remote exception text.

The shared public reader transfers in at most 64 KiB `read1` chunks. Large release assets go to temporary files, while JSON/checksum bodies are capped at 2 MiB. Its 120-second monotonic budget is checked between blocking operations, with a 15-second socket timeout. HTTP open/header and chunk-header parsing remain blocking library operations; this is not a hard wall-clock cancellation guarantee for a peer that dribbles headers. The existing archive inspector can buffer archive contents during structural checks. No automatic retries or mutating requests occur.

## Published download verification

The future release publisher must attach **public** `release-inventory.json` to the same published release as its source, packages and `SHA256SUMS`. That asset is not an available 0.30 artifact today. It must contain only the strict version/source/asset fields accepted by the existing inventory validator, not private notary work directories, provider responses, approval notes, keys or Store receipts. Its returned canonical GitHub asset URL, uploaded state, size, optional API digest and canonical inventory hash must match the expected comparison target. A missing required public inventory yields `public-release-inventory-unavailable`; there is no private receipt or guessed-filename fallback.

The reader then reuses the real published-release verifier against the explicit source checkout: public tag/source resolution, non-draft stable publication, actual package bytes and sizes, final inventory hashes, published checksum text, full corresponding source and portable archive inspection must agree. A selected platform that disappears is unavailable; an older platform package is never substituted. A changed selection is a mismatch requiring review. The committed release distribution policy still applies. DMG/DEB candidates remain excluded until qualified native final-installer validation exists, and Store service certification/Published does not create a public download. Website labels cannot override these gates.

## Independent results and remaining acceptance

The top-level comparison is `verified`, `mismatch` or `unavailable`. The report separately retains website-file and public-download results. One can pass while the other fails. `verified_platforms` reports only portable bytes/source/checksum validation; its accompanying status is authoritative for whether the complete expected selection agrees.

Native signatures remain `not-verified-by-readback`, installed acceptance and production readiness remain `not-assessed`, Store public availability remains `unverified`, and deployment is always false. An exact deployed website match does not establish trusted native signatures, installer acceptance, provider approval or Microsoft Store availability. No test fixture or report string can promote those states.

The comparison covers the seven prepared release-related files, actual cache URLs and selected public release downloads. It does not render a browser, inspect every image/font/video or prove every CDN region is consistent. Normal visual/accessibility checks, native installed-platform acceptance and separately authorized live release/website readback remain required.

The regression tests are entirely offline: synthetic committed package repositories, the existing public website layout fixture and fake HTTP responses cover correct/stale/missing/cache/redirect behavior, package/source/checksum/inventory tampering, wrong/unpublished sources, historical availability, malformed local inputs and bounded diagnostics. Earlier readback checks passed in the combined Python checkpoint; the new reconciliation cases require the next combined verification. No live unpublished release probe is part of this work.

## Typed Mac qualification binding (unreleased)

The shared published-download reader recognizes the explicit signed-candidate
handoff's FINAL_DMG.qualification.json without confusing it with a signer
verification receipt. Actual public signed ZIP/image/source/checksum bytes and
the strict qualification must agree. Missing companions and ambiguous legacy
plus typed receipts fail closed. This is asset binding only: the reader does
not authenticate native evidence or grant installed-package eligibility, and
the existing expected/selectable platform schema remains portable-only. It
therefore cannot make a DMG selectable or change production/installation status.
Native installed/public acceptance and an explicitly reviewed eligible-platform
schema remain separate work before any future DMG download can be shown ready.
