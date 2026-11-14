# Arboard 3.6.1

The source and licenses come from the published 3.6.1 crate, upstream revision `a3750c79a5d63f3987317e03c412b7d9dffdc2af`. The local manifest omits the upstream example targets and pins the Windows dependency version already selected by PeerBrush's lockfile. [Upstream repository](https://github.com/1Password/arboard/tree/a3750c79a5d63f3987317e03c412b7d9dffdc2af)

The Linux extension traits can send and receive the exact `image/png` payload and read bounded `text/uri-list` offers through the existing X11 and Wayland backends. The bounded X11 path checks properties and incremental transfers before extending the buffer. Wayland limits the bytes read from the offer. Existing image, text and file-list APIs retain their upstream behavior.
