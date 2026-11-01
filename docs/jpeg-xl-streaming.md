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

`PBJXLS03` uses fixed binary framing: an 80-byte header contains message kind,
nonce, per-direction sequence, a SHA-256 configuration binding and payload length.
Reserved bytes must be zero. The 240-byte start configuration binds build digest,
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
deadline. The separate streaming entry selects a validated CPU ceiling of at
most 1200 seconds before any legacy guard runs, preserving inherited lower hard
limits and all memory/parent-death/no-core protections. Traffic does not reset
either deadline.
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

After Done, the parent closes the child input before checking child-output EOF
and successful exit. A failed or partial exchange is never resumed or retried on
the same pipes. A fresh attempt requires a new nonce and fresh admitted owners.

Pinned enc_frame.cc extends 2048-group patches by eight border pixels on each
side, requesting native callback edges up to 2064 despite public 2048 wording.
The adapter uses RGB3 plus separate alpha1 native channels. Each complete owner
is at most 32 MiB and is filled directly from authenticated 2048-bounded RGBA wire
windows, with no 2064 RGBA intermediate. Every subwindow ID remains held until its
native buffer is released; eight-ID/128MiB aggregate bounds remain. Fixed 64 KiB
input and 64 KiB output callback buffers count inside the 128 MiB callback envelope.
Larger unsupported requests refuse. Small thin-image assembly is a bounded
compatibility path and does not qualify genuine group streaming or dense 16000.
VarDCT RGB with distance-zero alpha can stream at ordinary distances. Remaining
option restrictions need separate qualification; no algorithm/quality/alpha
default has been changed to avoid them.

All child messages, including zero-payload controls, share the derived hard event
ceiling `4*max_records + 4*ceil(raw_bytes/65536) + 64`, at most 1048576 events.
Storage must admit its control transcript and interval metadata from this bound
in addition to DATA bytes/record overhead. The absolute deadline alone does not
bound hostile control-record growth.

Protocol V3 additionally binds `max_input_bytes`. It must be nonzero and no more
than checked `8*raw_bytes +32 MiB`. Every authenticated rectangle response counts
against this declared budget; NeedRectangle preflight refuses excess before the
parent reads/stages that window. This bounds repeated source work as well as
resident/output metadata. Legitimate effort/halo demand must be qualified before
choosing a production policy. Earlier unpublished V1/V2 peers fail the new magic.

V3 also binds an explicit `max_source_read_bytes`, at most checked
`64*raw_bytes +256 MiB`. The parent accounts successful original sealed readback
(header/payload/index/seal), every actual uncached covered-chunk range read and
fresh whole-hash pass before IO. The byte factory's frozen original chunk index
supplies the integrity proof; no second mutable index/cache is substituted.
Returned ROI bytes and physical authenticated readwork are separate counters.
This initial bounded policy still needs legitimate effort/halo qualification.

The closed parent input is a `StagedNativeRows` owner minted from an exact captured
scene/output and the session's straight-native byte factory. Its original frozen
chunk hashes authenticate each full covered chunk before ROI bytes reach the
worker. Raw/source/profile/output/encoder options, build and nonce are bound
separately; arbitrary byte spools are not accepted as native row authority.
Parent callback buffers stay within 32 MiB, with the child's 96 MiB callback envelope
admitted simultaneously. Native allocator/profile reservations are additional.

The broker records every child event in the registered append-only segment log,
requires terminal EOF and known successful owned-child exit, then rechecks the
raw source before and after canonical forward replay. The resulting owner keeps
both source and log/canonical namespaces through explicit retirement. It proves
encoding transport and source ownership; final decoder/fidelity checks and
destination publication remain separate mandatory gates.

If stopping the worker is uncertain, its actual owned child, kernel-limit owner,
source/profile operation and resource tickets remain parked. An internal reaper
starts before launch and polls only that owned child; after known exit it drops
these owners outside the retained-state mutex, even during profile closing with
no future codec request. Future codec work remains disabled after uncertain stop
until restart. No filesystem retirement is inferred merely from worker exit.
The new operation is not activated by these producer/transport components.

The pinned `ModularPartIsLossless` test applies to the modular channels, not the
VarDCT RGB distance. With the existing alpha distance 0 setting, a tiny 33×2050
native 8/16-bit fixture at RGB distance 1/effort 3 reached the second DC-group halo,
kept exact alpha/hidden RGB behavior, and matched separately isolated unchanged
one-shot encoder/decoder pixels and decoded ICC byte-for-byte. This qualifies
those small fixtures/options only. XYB output may return a canonical sRGB profile
rather than the original ICC byte encoding; comparison must follow the unchanged
decoder's actual output-profile contract, without relabeling its samples.

Distances 10–15 still enable the pinned automatic 2× resampling and adjusted RGB
distance. The streaming selector currently refuses this resampling. Its sharper
filter now uses an exact regional fallible row kernel with 13 bounded workspace
rows, including image-edge clamps and the original scalar accumulation order;
the ordinary sharper path calls that kernel. This removes its full-plane copy,
box and mask intermediates without changing filters or encoder options. Tiny
bitwise old-reference tests cover odd edges, interior ROI boundaries, negative
and signed-zero samples, wide/tall one-dimensional offsets and explicit failures.
The full native high-distance group-coordinate and iterative-filter integration
remains open; this refactor does not qualify high-distance 16000 exports.

An iterative regional kernel supplies the existing scalar ImageF helper, with the exact pinned
three passes and an independent whole-image reference oracle. AntiUpsample needs
raw support `[2x-4,2x+5]`; its upsample reads `floor(raw/2)±2`, giving a coarse
radius 4 per pass, hence 12 for three passes. Final ringing's initial±1 is inside
that extent; sharper initialization separately fetches its raw±5/+6 support.

Its workspace processes one channel sequentially: immutable initial plus two
alternating scalar fields. Logical 2072² fields require 16.38 MiB each/49.13 MiB total.
Actual stride is validated at most 2200 floats and 2072 rows, so all three capacities
plus bounded correction/source/sharper scratch stay below 53 MiB. No replacement
allocation occurs during a pass. Allocator overhead and all other native state
still count under the unchanged 512 MiB allocator admission; this is not total app
RAM. Buffers must remain admitted and disjoint. Source and sink failures refuse
completion. The existing helper still receives its resident input plane; its three-channel
caller and actual resampling2 group/XYB-row bridge remain separate work. This
introduces no new spool, wire format, option override or publication authority.
