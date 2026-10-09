# PeerBrush website

Static product website using PeerBrush’s Ember palette, bundled Ubuntu Sans fonts, logo and curated native workspace screenshots. Content is grounded in the application documentation; no generated product mockups or slogans.

From this directory, run `npm run dev` and open http://127.0.0.1:4173. The server binds to loopback only. Run `npm run build` to produce the static `dist/` directory. Node.js is the only development dependency.

`index.html` contains downloads, screenshot navigation, setup, AI connection instructions, supported formats, shortcuts and contribution links. `styles.css` defines responsive Ember layouts and motion that respects reduced-motion preferences. `main.js` handles screenshot tabs, enlargement, command copying and downloads. `platform.js` detects desktop operating systems without recommending desktop builds to Android, iOS or ChromeOS devices.

`public/downloads.json` pins verified Windows x64, Linux x64 and macOS Apple silicon development packages from the same successful CI source revision. The public GitHub release feed can update the suggestion only when a release includes all three packages. The bundled manifest supplies a fallback if the feed is unavailable. Linux/macOS desktop workflow verification is pending; the macOS build is unsigned.

The mountain-lake artwork was created with ImageGen and imported into the real PeerBrush document engine. Screenshot UI is captured from the native application. The original image, editable PSD and generation prompt are in `assets/examples/` in the parent repository. The website bundles the PNG and curated native screenshots.

`.openai/hosting.json` stores the Sites project identity. Publication uses the Sites workflow and an isolated ignored checkout; it does not commit application work in the parent repository. Build output, QA captures and publication archives are excluded from source control.

GPL v3 applies to the website. Ubuntu Sans retains its bundled Ubuntu Font Licence in `public/assets/fonts/LICENCE.txt`.
