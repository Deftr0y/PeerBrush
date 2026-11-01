# Native JPEG XL streaming operation (unreleased)

The separate streaming operation uses the pinned libjxl 0.12.0 chunked input
and output processor APIs, with output mode **1**. Mode 1 seeks logically while
producing compatible files. Mode 2 requires a newer container version and is
not accepted. Existing resident operations and their limits remain unchanged.
No runtime route is enabled by the protocol component alone.

The parent first captures the exact current native scene/output, writes straight
interleaved RGBA into a SourceSession-owned exact-length byte source and verifies
its complete SHA-256. Native16 samples are big endian, including low bits and
hidden RGB under zero alpha. An immutable captured ICC profile is supplied
unchanged. The input source, metadata/profile, callback allocations, output log,
canonical replay and destination temporary must coexist under explicit shared
resident/disk admissions. These admissions are not a total process RAM claim.

`PBJXLS01` uses fixed binary framing: an 80-byte header contains message kind,
nonce, per-direction sequence, a SHA-256 configuration binding and payload length.
Reserved bytes must be zero. The 224-byte start configuration binds build digest,
source/output bindings, exact raw/profile lengths and SHA-256, native dimensions,
options and operation ceilings. No filesystem path or credential is transported.
Control prefixes have fixed sizes; large bodies pass through an already charged
buffer of at most 64 KiB. Bodies are provisional until their declared full hash
is verified. State advances only after complete framed transport; malformed,
truncated, canceled, late or uncertain transport permanently fails that operation.

After Start (including the profile body), the child sends Ready. The parent
acknowledges Ready and each output/release/seek/finalization event using its exact
request sequence. A rectangle request instead receives its matching rectangle
body and digest. There is one outstanding protocol exchange, while several
previously returned native rectangles may remain held by libjxl. Every rectangle
has a monotonically allocated nonzero ID and receives exactly one release.

Hard protocol envelopes are 16000 per dimension; native8/16 RGBA;
2048-square rectangles (at most 32 MiB at native16); eight simultaneously held
buffers totaling at most 128 MiB; 64 KiB output records; 131072 records; and
64 GiB each for logical output and protocol log bytes. Every invocation supplies
smaller or equal explicit ceilings. The log counter includes payload plus its
48-byte segment prefix; storage must additionally admit its actual file/index/
seal overhead. The new operation carries an explicit absolute deadline of at most 20 minutes,
including launch, callbacks, pipes and final exit. A production policy selecting
that deadline still requires resource/performance qualification; there is no
automatic extension. Existing resident/decode operations retain their 120-second
deadline. Traffic does not reset either deadline.
Cancellation must stop parent pumps and terminate/reap the isolated child using
the existing process protections; a surviving or uncertain child blocks reuse.

Child output mode 1 callbacks append `(logical offset, length, SHA-256, bytes)`
records to a distinct session-owned segment log. Existing byte sources and the
destination never receive random writes. Seek is legal only outside a buffer
and at or after the monotonically finalized position. A new segment must start
at the current logical position. Finalization cannot exceed the highest write.
Done requires zero held rectangles, a nonempty final length equal to the highest
write and finalized prefix, and an exact record count. These checks alone do
**not** prove gap-free logical coverage or final file integrity.

Before publication, storage must independently verify its owned segment inventory,
latest-write interval map, all hashes, exact gap-free `[0, length)` coverage and
the finalized-prefix promise. It then replays canonical forward bytes into the
existing exact-length owned byte factory, verifies complete output SHA-256 and
source/session bindings, and retains both ownership guards through destination
publication. All retained source/history/backup/log/replay/destination bytes count
together. Failure retires only verified owned unpublished bytes; unknown or
foreign replacements and uncertain debt remain untouched and charged.

The child custom allocator and held-pointer registry need independent enforced
limits, with parent admission retained for their coexistence. The existing
process limits remain required. No full-frame Vec/mmap, fallback 8-bit copy,
unbounded outstanding callback allocation or in-process file-data codec route
is allowed. Source/revision/lease checks, native pixel/profile fidelity, real
installed acceptance, interruption tests and actual dense 16000 qualification are
separate gates. Small protocol tests do not establish those results.
