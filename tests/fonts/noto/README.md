# Noto text-shaping acceptance fixtures

These two unmodified static outline fonts are test inputs, never application fallback fonts. They are from the official [notofonts/noto-fonts repository](https://github.com/notofonts/noto-fonts/tree/ffebf8c1ee449e544955a7e813c54f9b73848eac), commit `ffebf8c1ee449e544955a7e813c54f9b73848eac`. The full upstream license is retained as [OFL.txt](OFL.txt), alongside [the embedded notices](SOURCE-NOTICES.md). No font is subsetted, renamed internally or modified.

| Fixture | Official pinned file | Bytes | SHA256 |
| --- | --- | --- | --- |
| NotoSansArabic-Regular.ttf | [Source](https://raw.githubusercontent.com/notofonts/noto-fonts/ffebf8c1ee449e544955a7e813c54f9b73848eac/hinted/ttf/NotoSansArabic/NotoSansArabic-Regular.ttf) | 240456 | `ceea25b464a656dc3b26849bab9356740401af62aedf1bfa8b7f0d9b75925b1b` |
| NotoSansDevanagari-Regular.ttf | [Source](https://raw.githubusercontent.com/notofonts/noto-fonts/ffebf8c1ee449e544955a7e813c54f9b73848eac/hinted/ttf/NotoSansDevanagari/NotoSansDevanagari-Regular.ttf) | 219212 | `385e78e6359a9d88a0f243d53b1209d7548361ba2194e2b9ec779bcaa7e8949d` |
| OFL.txt | [Source LICENSE](https://raw.githubusercontent.com/notofonts/noto-fonts/ffebf8c1ee449e544955a7e813c54f9b73848eac/LICENSE) | 4377 | `0dab92d0544f7b233403f14b84a663bdbfa746982eda629e7f4f9ffe1b036feb` |

Source unit tests bind these exact owned bytes/index through the test-only font resolver, then use ordinary shared source/engine/protocol/codec commands. They exercise joining/lam-alef, combining and pre-base ordering at native8/16, rather than implying installed OS font coverage or full typography conformance. The normal application still requires the selected exact installed font; no fixture font becomes a bundled picker option.
