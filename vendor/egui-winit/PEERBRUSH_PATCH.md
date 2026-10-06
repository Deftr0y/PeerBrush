# PeerBrush input patch

Upstream: egui-winit 0.31.1, https://github.com/emilk/egui/tree/0.31.1/crates/egui-winit

One change in `src/lib.rs`: emit `Event::Paste("")` for an image-only clipboard when the user presses a paste shortcut. Upstream consumes that shortcut without delivering an event unless text is available. The native canvas then reads the OS image clipboard through arboard. Normal TextEdit paste remains unchanged; inserting an empty string has no effect.

Keep the upstream MIT and Apache licenses. Remove this patch when an upstream version exposes image paste requests directly. The small adapter is included in source packages so all platforms build the same behavior.
