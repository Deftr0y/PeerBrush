# PeerBrush JPEG row output patch

Source is the crates.io archive `zune-jpeg-0.5.15.crate`, SHA-256
`27bc9d5b815bc103f142aa054f561d9187d191692ec7c2d1e2b4737f8dbd7296`,
upstream commit `31d81fed7551c8ccea456d9d8e2b1fd8bebb6995`. The upstream MIT,
Apache-2.0 and Zlib license texts are retained.

`decode_rows_into` sends the existing color-converted rows to a caller-provided
sink using one caller-provided output row. Baseline, separate-scan and
progressive JPEGs share the existing color conversion and upsampling code.
The row sink reports failures immediately and requires all rows in order.
`decode` and `decode_into` retain the original contiguous output and recovery
behavior. Coefficient allocation and scan limits are unchanged.
