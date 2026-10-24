# cmake

[Documentation](https://docs.rs/cmake)

A build dependency for running the `cmake` build tool to compile a native
library.

```toml
# Cargo.toml
[build-dependencies]
cmake = "0.1"
```

The CMake executable is assumed to be `cmake` unless the `CMAKE`
environmental variable is set.

# License

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   https://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   https://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in cmake by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

PeerBrush carries a same-version CMake 0.1.58 source patch. Upstream registry archive SHA-256: `c0f78a02292a74a88ac736019ab962ece0bc380e3f977bf72e376c5d78ff0678`. Both native compiler-argument joins quote each complete OsStr for the current native host (POSIX bytes or Windows CRT UTF-16) before storing the CMake flags command fragment. This preserves default and user arguments containing spaces, quotes, apostrophes, backslashes and Unicode. The existing Ninja generator selection also escapes each literal dollar in these discovered arguments once for Ninja variable syntax, including Ninja Multi-Config. Explicit Config flag fragments and other generators retain their previous handling. This patch targets the native Ninja workflow; other cross-host/MSYS generators are not covered. The MIT and Apache-2.0 licenses remain included.
