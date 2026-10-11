# PeerBrush input patch

Upstream: egui-winit 0.31.1, https://github.com/emilk/egui/tree/0.31.1/crates/egui-winit

Changes in `src/lib.rs`: emit `Event::Paste("")` for an image-only clipboard when the user presses a paste shortcut. Upstream consumes that shortcut without delivering an event unless text is available. The native canvas then reads the OS image clipboard through arboard. Fresh TextEdit paste remains unchanged; inserting an empty string has no effect.

Keep the upstream MIT and Apache licenses. Retire the image-only paste change when upstream exposes image paste requests directly; retain the repeat guard until upstream provides equivalent behavior. The small adapter is included in source packages so all platforms build the same behavior.

Native clipboard commands now suppress OS key repeats before generating Copy/Cut/Paste events. Fresh presses retain text and image-only paste behavior; releases still reach ordinary key handling. Suppressed repeats never request clipboard text. Pure adapter regression coverage uses synthetic text providers, without reading or changing the OS clipboard.

Focus loss clears the adapter's physical-key press records. An OS repeat requires a fresh press in the current focus session before key, text or clipboard translation; this prevents egui's cleared key state from interpreting a returning repeat as a fresh command. Ordinary repeats after a fresh press retain upstream handling. Retain this focus guard until upstream provides equivalent behavior; synthetic coverage includes independent keys, unidentified physical keys, releases and refocusing.
