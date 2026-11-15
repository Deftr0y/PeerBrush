# PeerBrush input patch

Upstream: egui-winit 0.31.1, https://github.com/emilk/egui/tree/0.31.1/crates/egui-winit

Changes in `src/lib.rs`: emit `Event::Paste("")` for an image-only clipboard when the user presses a paste shortcut. Upstream consumes that shortcut without delivering an event unless text is available. The native canvas then reads the OS image clipboard through arboard. Fresh TextEdit paste remains unchanged; inserting an empty string has no effect.

Keep the upstream MIT and Apache licenses. Retire the image-only paste change when upstream exposes image paste requests directly; retain the repeat guard until upstream provides equivalent behavior. The small adapter is included in source packages so all platforms build the same behavior.

Native clipboard commands now suppress OS key repeats before generating Copy/Cut/Paste events. Fresh presses retain text and image-only paste behavior; releases still reach ordinary key handling. Suppressed repeats never request clipboard text. Pure adapter regression coverage uses synthetic text providers, without reading or changing the OS clipboard.

Alt-modified clipboard chords stay on the ordinary keyboard path. This prevents Ctrl+Alt representations of AltGr from generating clipboard events or reading clipboard text. On Windows and Linux, Ctrl+Alt can deliver printable text supplied by the native key event. Dedicated Copy, Cut and Paste keys still deliver clipboard events.

Copy Merged delivers its resolved keypress with its own Shift modifier, followed by the standard Copy event for text fields. The editor uses the first copy intent in the frame, so that pair requests one copy. Dedicated Copy keys still deliver the standard event.

Focus loss clears the adapter's physical-key press records. An OS repeat requires a fresh press in the current focus session before key, text or clipboard translation; this prevents egui's cleared key state from interpreting a returning repeat as a fresh command. Ordinary repeats after a fresh press retain upstream handling. Retain this focus guard until upstream provides equivalent behavior; synthetic coverage includes independent keys, unidentified physical keys, releases and refocusing.

Logical keys normally win and layouts without a recognized logical key fall back once to their physical key. When Shift changes a digit or bracket into punctuation, the desktop adapter uses that layout's unmodified digit or bracket for the key event. The native text event still carries the typed symbol. Application shortcuts consume the single resolved key. Physical identities still protect focus and repeat handling. Adapter tests cover multilingual fallback, Latin remapping and clipboard translation without OS clipboard access. CI runs them explicitly in addition to application regressions.

The pointer-driving touch contact releases at its actual end position. The release can differ from the last move packet. Other contacts and cancellation retain their existing event routing.
