# PeerBrush decoder patch

Source: image-webp 0.2.4, from the crates.io package already used by PeerBrush.
The upstream MIT and Apache-2.0 licenses are retained in this directory.

PeerBrush adds an opt-in `WebPDecodeOptions::lossless_huffman_limit`. The limit
admits the complete group vector and each tree/table before allocation, with a
fixed allowance for temporary code-length decoding. It applies to static VP8L,
animated VP8L and compressed ALPH data. The default keeps upstream behavior.

`WebPDecodeOptions::lossy_partition_limit` separately admits encoded VP8
partitions, including the first partition, declared coefficient partitions,
unknown-length final-partition growth and the temporary packed copy. Arithmetic
decoding retains the admitted Vec capacity instead of shrinking to a boxed slice.
The default keeps the limit disabled. A fixed 4 KiB stack read buffer is separate.

All WebP VP8 entry points verify the embedded dimensions before allocating YUV
planes. Animated ALPH data must be followed by a VP8 chunk of the declared frame
size, avoiding an unchecked write into the animation frame buffer.

Animation traversal honors the RIFF and padded ANMF boundaries and skips
intervening metadata. First-frame subchunk inspection seeks to each header.
Disposal defaults to transparent when no background override was selected,
and always clears the previous rectangle in the RGBA canvas, independent of
the incoming frame's dimensions or channel layout. PeerBrush source regressions
cover opaque and full-canvas alpha frames as well as metadata between frames.

Native callers read admitted VP8, VP8L and ALPH payload ranges and decode their
rows without allocating the buffered animation canvas or full frame pixels.
The buffered and tiled paths share the frame header parser and RGBA blend
arithmetic.

These limits cover Huffman and encoded partition allocations only. Frame,
canvas, transform, macroblock and metadata memory still require separate
admission. They must not be described as a complete WebP decoder memory limit.
PeerBrush's integration regression source is in `tests/webp_admission.rs`.
Native producer admission and command regressions are in
`src/image_import/webp/` and `src/engine/native/worker/webp_tests.rs`.

The opt-in `read_static_lossless_rgba` entry point decodes VP8L into the
caller's existing RGBA buffer. It supplies opaque alpha for RGB images and
shares the normal lossless decoder and Huffman guard. `read_image` retains
its existing channel layout.

`read_static_lossy_color_rows` sends VP8 color rows to a fallible sink. It
retains three macroblock bands for deblocking and chroma interpolation and
one row of filter metadata. It shares the regular entropy decoder, prediction,
filter kernels and color conversion, with the same encoded partition guard.
Callers own staged rows until decode and final source validation succeed.

`RawAlphaRows` reads an uncompressed ALPH chunk from a separate bounded
reader. Its two caller-owned scratch rows use the same predictors and
wrapping addition as the buffered alpha decoder.

`read_static_lossless_rgba_rows` decodes VP8L into a bounded pixel history and
three scratch rows. It shares Huffman and LZ77 parsing with buffered decoding.
Inverse transforms operate in reverse order on completed rows, retaining the
previous predictor row separately from final RGBA samples. Entropy, transform
and Huffman allocations still need separate admission.

`decode_compressed_alpha_residual_rows` uses the VP8L row decoder with
implicit dimensions and yields the green channel of each inverse-transformed
row. PeerBrush spools these residuals and applies the ALPH predictors while
combining them with the VP8 color rows.
