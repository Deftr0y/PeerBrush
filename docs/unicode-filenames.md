# Unicode filenames

Unreleased development behavior. Tabs, the open-project picker, tab filename tooltips and the status-bar filename preserve the original logical UTF-8 name and display shaped text. Arabic joins, Hebrew and Arabic use their detected paragraph direction, numbers and extensions retain Unicode bidirectional ordering, and Hindi combining forms are shaped. Bundled fonts cover Latin, Greek, Cyrillic, Hebrew, Arabic, Devanagari, Han, Japanese kana and Korean Hangul. This is not a promise of coverage for every Unicode character or emoji.

Ubuntu Sans remains the primary interface font. Noto provides fallback glyphs; Han shapes use the Simplified Chinese font variant. Long tab names are clipped after shaping, with an ellipsis on the corresponding reading edge. The full logical name remains available to accessibility clients. Filename tooltips show names up to the available width, without editing or renaming files.

Only presentation textures are cached: at most 64 labels and 8 MiB per UI context, with bounded raster dimensions and density. Colors, including blue AI activity, tint the same glyph texture. Paths, artwork, history, PSD sources and saved pixel precision are unaffected. Native file dialogs use the operating system's rendering. Editable document Text sources continue to use the three documented Ubuntu Sans fonts; these UI fallbacks do not extend that source format.

## Bundled fonts and licenses

The unmodified Noto fonts and their accompanying licenses use the SIL Open Font License 1.1. Both source and portable packages include every font and both license files. The project retains GPL v3. Sources below are pinned to the downloaded upstream commits; SHA-256 values describe the distributed files.

| File | Upstream source | SHA-256 |
| --- | --- | --- |
| `NotoSansHebrew-Regular.ttf` | [Pinned source](https://raw.githubusercontent.com/notofonts/noto-fonts/ffebf8c1ee449e544955a7e813c54f9b73848eac/hinted/ttf/NotoSansHebrew/NotoSansHebrew-Regular.ttf) | `a7fa16fffb27bedb060a0866267c29e9859aeb9c21cc33f5b3aaf6eb062eca85` |
| `NotoSansArabic-Regular.ttf` | [Pinned source](https://raw.githubusercontent.com/notofonts/noto-fonts/ffebf8c1ee449e544955a7e813c54f9b73848eac/hinted/ttf/NotoSansArabic/NotoSansArabic-Regular.ttf) | `ceea25b464a656dc3b26849bab9356740401af62aedf1bfa8b7f0d9b75925b1b` |
| `NotoSansDevanagari-Regular.ttf` | [Pinned source](https://raw.githubusercontent.com/notofonts/noto-fonts/ffebf8c1ee449e544955a7e813c54f9b73848eac/hinted/ttf/NotoSansDevanagari/NotoSansDevanagari-Regular.ttf) | `385e78e6359a9d88a0f243d53b1209d7548361ba2194e2b9ec779bcaa7e8949d` |
| `NotoSansCJKsc-Regular.otf` | [Pinned source](https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf) | `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b` |
| `Noto-LICENSE.txt` | [Pinned source](https://raw.githubusercontent.com/notofonts/noto-fonts/ffebf8c1ee449e544955a7e813c54f9b73848eac/LICENSE) | `0dab92d0544f7b233403f14b84a663bdbfa746982eda629e7f4f9ffe1b036feb` |
| `NotoCJK-LICENSE.txt` | [Pinned source](https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/LICENSE) | `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2` |
