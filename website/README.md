# PeerBrush website

Static product website using PeerBrush’s Ember palette, bundled Ubuntu Sans fonts, logo and curated native workspace screenshots. Content is grounded in the application documentation; no generated product mockups or slogans.

From this directory, run `npm run dev` and open http://127.0.0.1:4173. The server binds to loopback only. Run `npm run build` to produce the static `dist/` directory. Node.js is the only development dependency.

`index.html` contains the hero, a real application window, the trailer, the shared-document layer breakdown, downloads, screenshot navigation, setup, AI connection instructions, supported formats, shortcuts and contribution links. `styles.css` defines responsive Ember layouts and motion that respects reduced-motion preferences. `main.js` handles screenshot tabs, enlargement, command copying, downloads and scroll reveals. `showcase.js` adds the hero parallax, the tilting application window, the trailer play button and the scroll-driven layer breakdown (layer toggles, work-area markers); below 900 px or with reduced motion it renders the same content statically. `platform.js` detects desktop operating systems without recommending desktop builds to Android, iOS or ChromeOS devices.

`public/assets/showcase/` holds the golden-hour example from the trailer. Sources: "Large mountain lake" by Robert Magnusson (Unsplash, CC0) and "Pink clouds panorama" by W.carter (CC0), edited in PeerBrush 0.1.4 by an AI agent over MCP and scripted manual input. The layer images are solo exports of that project's PSD, and `workspace-golden-hour.webp`, `mcp-status.webp` and `trailer-poster.webp` are frames from the real screen capture and the trailer. The logo parts are colour separations of `assets/peerbrush-logo.png`. The work-area markers in the layer section are annotations drawn by the page, not application screenshots.

`public/downloads.json` is the release data source for the home suggestion, Downloads and permanent version history. Its `version` selects an explicitly published entry from `history`; each entry records its date, type, patch notes, resolved source commit and real platform URLs/checksums. Incomplete historical checkpoints remain visible with missing platforms identified. The public GitHub feed does not silently promote a different release. Update this manifest only after verifying published assets under the [release-maintenance procedure](../docs/release-maintenance.md). Linux/macOS manual desktop verification remains pending; release notes state signing limits.

The mountain-lake artwork was created with ImageGen and imported into the real PeerBrush document engine. Screenshot UI is captured from the native application. The original image, editable PSD and generation prompt are in `assets/examples/` in the parent repository. The website bundles the PNG and curated native screenshots.

The official website is deployed through `.github/workflows/pages.yml`. Build output, QA captures and publication archives are excluded from source control.

GPL v3 applies to the website. Ubuntu Sans retains its bundled Ubuntu Font Licence in `public/assets/fonts/LICENCE.txt`.
