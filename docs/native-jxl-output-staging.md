# Compatible JPEG XL output staging (unreleased)

The mode 1 encoder may seek within unfinished output. Its output must therefore
be captured in a distinct append-only segment log, then replayed in logical order
into an exact-length owned byte source. Existing source spools and destinations
never receive random writes. Mode 2 and its newer container are excluded. This
is a storage design for the separate streaming operation; it does not enable a
runtime route, raise editor limits, or qualify a dense 16000-pixel export.

## Admission and transport

Creation requires the actual SourceSession and its retained profile, instance
and WorkspaceFileOperation admission. A fresh `segments-UUID` namespace is
registered before any event is accepted. Its held directory and exclusive
regular, single-link file identities remain checked through append, seal, replay
and explicit retirement. A supplied path, nonce or digest is not admission.

The operation binds the existing 16-byte transport nonce and the SHA-256 binding
of the exact 240-byte private transport version 3 configuration. It binds the
explicit cumulative input-work and authenticated source-read ceilings, native
source, output, raw input, ICC profile, encoder build and options. The parent
checks framed transport and exact source/row provenance independently; a valid
log cannot replace those checks or authorize destination publication.

`CompletedBytes::authenticated_read_cost` reports the full payload chunks read
by one uncached range invocation, including partial edge chunks. The broker
precharges that exact cost before I/O and meters each cursor Read separately;
adjacent reads may authenticate the same chunk again. Owner metadata checks read
no header/index/seal payload. A successful exact-spool finish additionally reads
its complete `encoded_bytes` (header, payload, frozen index and seal). These
ceilings bound requested native file reads, without credit for filesystem cache.

The log independently counts every child event, including Ready, rectangle
requests/releases and zero-byte controls. Its event ceiling is the protocol's
checked `4*max_records + 4*ceil(raw_bytes/65536) + 64`, at most 1048576. DATA is
also bounded to 131072 records, at most 65536 bytes each, and the operation's
explicit protocol-log ceiling counts DATA payload plus 48 bytes per DATA record.
All event sequence numbers are consecutive from zero; every event must match the
operation binding. Only fully verified events may advance the log state or be
acknowledged. A failure permanently latches the log.

The 64 GiB logical ceiling does not promise a 64 GiB file under every other
bound. For example, 131072 records of 65536 bytes allow at most 8 GiB of written
payload, and overlapping replacements consume that allowance too. The caller
chooses smaller explicit limits and admits actual retained source/history,
input, log, canonical replay, destination and backups together.

## Forward-only physical representation

The log has a fixed 128-byte header, event records and a fixed 128-byte
seal. The header binds a distinct log magic/version, fresh file identity, nonce,
configuration binding and explicit logical, DATA, protocol-log and event limits.
All reserved bytes are zero and all integer arithmetic is checked.

Each event has a 64-byte header: event sequence (u64), kind (u32), payload length
(u32), logical position (u64), event value (u64), and payload SHA-256 (32 bytes).
It is followed by its bounded DATA payload and a 32-byte domain-separated record
SHA-256. Control events have no payload and use their exact declared position or
value; observational input events carry no output authority. DATA's extra 48
bytes of physical overhead beyond the protocol's counter, every control record,
the file header and seal are admitted separately. The maximum physical length
is computed before creation as
`256 + max_log_bytes + 48*max_records + 96*max_events`; this intentionally
conservatively counts DATA twice in the event-overhead term.

The shared persistent DiskBudget reserves the initial header/seal and grows
before each append. It never releases unused or failed-write debt on Drop.
The physical file grows only at its known current end through a private held
handle; there is no exposed seek, write-at, mutable file or truncate interface.
Partial writes, changed stamps or unexpected lengths latch an uncertain result.

The seal binds the complete physical transcript SHA-256, exact event/DATA counts,
protocol-log bytes, final logical length, nonce and configuration binding. Seal
completion requires sync, fresh full readback, exact framing/EOF and every record
hash. Hashes detect corruption; filesystem/profile ownership remains separate.
No fsync-per-event promise is needed for this derived temporary: after a crash an
unsealed or partial log is held inventory, never an editable source or resumed
encoder session. Windows directory-metadata power-loss durability is unqualified.

## Latest-write coverage

The in-memory output index contains ordered, disjoint logical intervals. Each
interval identifies the exact DATA record and subrange that supplies its current
bytes. DATA must begin at the tracked logical cursor and at or after the
finalized prefix. A seek occurs only after the encoder releases its output
buffer, remains at or after that prefix, and stays within the logical ceiling.
A later DATA record replaces only the overlapping unfinished intervals; left
and right survivors retain their original record identity and subrange.

The index has at most `2*max_records + 1` intervals. Every growth reserves a
fallible resident capacity charge before allocating, including old and new
buffers during replacement. Frozen interval owners keep that charge until their
payload drops. Record descriptors and transfer buffers have separate charges in
the same pool. No uncharged BTreeMap, source-payload copy or zero-filled gap is
permitted. The first implementation retains the complete bounded index through
replay; finalization does not silently discard interval ownership.

Finalization is monotonic and cannot exceed the highest write. Before accepting
a new finalized prefix, the index must prove gap-free coverage of that prefix.
Done requires a nonempty length equal to the finalized prefix and highest write,
exact counts, no outstanding native rectangles/buffers, and gap-free coverage of
`[0, length)`. Protocol state and successful child EOF/exit are checked separately
before the parent accepts a completed operation. A log-completion object alone
is neither an encoder-success nor a source/publication receipt.

## Canonical replay and lifetime

Replay walks the frozen interval index forward. It reads a complete referenced
DATA record through a charged buffer of at most 64 KiB, rechecks its full record
and payload hashes, and appends the selected subrange to a newly created
exact-length SourceSession byte temporary. Canonical output uses a distinct
`CompatibleJxlMode1` byte role; the segment log has its own domain and never
masquerades as a Photoshop container. The canonical complete SHA-256 is computed
in forward byte order and checked against the opaque replay completion.

`SessionSegments::canonical(self)` returns a `SessionJxlOutput` that owns both
log and canonical namespaces. Its byte access, checks and explicit retirement
retain the same actual borrowed source session. Raw row input uses the separate
`StraightNativeRgba` role, preserving straight-alpha native BE samples rather
than borrowing Photoshop white-matte composite semantics.

The log, canonical bytes, source/session guards and any worker admission coexist
until the caller finishes validation/publication or explicit retirement. The
child's 128 MiB held-rectangle envelope and 512 MiB native allocator require their
own enforced process/coexistence admission; they do not fit implicitly inside a
parent resident-pool claim. Neither charged payload nor a Job commit limit is a
measurement of total application RAM or filesystem cache.

Cancellation stops acknowledgement and terminates/reaps only the owned worker.
Verified retirement may ignore gesture cancellation/closing only through the
existing retirement-only session check, after readers and candidate owners are
dropped. It proves the exclusive file identity and every known prefix byte
before removing the log and then the exact empty directory. Unknown partial
writes, foreign replacements, missing owners or uncertain worker exit keep
bytes, ledger debt and inventory. There is no Drop cleanup or automatic orphan
selection. Startup inventories recognized logs for accounting only; it never
resumes an encoder or grants source authority from a log.

## Required verification

Small tests must cover overlapping writes, split interval survivors, out-of-order
physical versus logical writes, gaps, finalized rewrites, every counter/overflow,
control spam, allocation/I/O refusal, cancellation before/after append, partial
seal, changed or same-byte foreign files, interrupted replay and exact debt/
owner lifetimes. Reference replay must match a simple bounded oracle for varied
small transcripts. Actual compatible native8/16 decode, ICC/hidden RGB fidelity,
full dense 16000 resource/performance acceptance, installed behavior and platform
qualification remain separate release gates. This design supplies no such test
claim and changes no current release gate.
