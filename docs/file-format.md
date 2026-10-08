# Phase 0 file format, version 1

All integers are unsigned and little-endian. Checksums use CRC-32/ISO-HDLC
(IEEE), implemented by `crc32fast`, not CRC32C. The standard check vector
`123456789` produces `0xcbf43926`. Checksums detect accidental damage; they are
not authentication or a repair mechanism.

No Rust structs are written by copying their memory layout. Every field is
encoded explicitly. The decoder checks sizes before allocating payload buffers.

## Common journal layout

WAL and MANIFEST have a 12-byte header:

| Offset | Size | Meaning |
| --- | --- | --- |
| 0 | 8 | Magic: `JEVWAL01` or `JEVMAN01` |
| 8 | 4 | Format version: `1` |

Each subsequent frame is:

| Relative offset | Size | Meaning |
| --- | --- | --- |
| 0 | 4 | Payload length `N` |
| 4 | 4 | CRC32 of the four encoded length bytes |
| 8 | N | Payload |
| 8 + N | 4 | CRC32 of length bytes followed by payload; excludes length checksum |

Total frame size is `N + 12`. A separate length checksum lets recovery reject
damaged complete frame headers before deciding whether a payload is truncated.

An incomplete last header (fewer than eight bytes), or a checksum-valid header
without its complete payload and final checksum, is an interrupted tail. Recovery
truncates to the last complete valid frame boundary and syncs the truncation.

A complete invalid header, zero/excessive frame length, complete invalid payload,
or bad final checksum is corruption. Recovery never searches forward past damage
for another plausible record. A record truncated to a shorter but complete valid
prefix of the journal cannot be distinguished from that older journal alone;
published flush watermarks catch some, but not all, externally removed history.

## Mutation payload

Used by WAL records and SSTable record frames:

| Offset | Size | Meaning |
| --- | --- | --- |
| 0 | 8 | Sequence number, starting at 1 |
| 8 | 1 | Tag: `0` delete, `1` put |
| 9 | 4 | Key length `K` |
| 13 | 4 | Value length `V` |
| 17 | K | Key bytes |
| 17 + K | V | Value bytes |

Payload length must equal `17 + K + V`. `K <= 65536`, `V <= 16777216`;
maximum payload length is `16842769`. Delete requires `V == 0`. Put allows zero
value bytes. Unknown tags and sequence zero are rejected.

The WAL contains every mutation in strict contiguous sequence order, including
historical mutations already covered by tables. It is never rotated or shortened
except for incomplete-tail repair in Phase 0. Replay validates all records but
only inserts sequences above the published watermark into the memtable.

## Manifest payload

Every payload is exactly 25 bytes. Frame size is 37 bytes:

| Offset | Size | Meaning |
| --- | --- | --- |
| 0 | 1 | Operation tag `1`: AddTable |
| 1 | 8 | Table ID, positive and strictly increasing |
| 9 | 8 | Expected SSTable file size |
| 17 | 8 | Persisted sequence after this flush, positive and strictly increasing |

The manifest is append-only. No removal records exist in version 1. A new table
covers mutations in `(previous persisted sequence, new persisted sequence]`.
Each key stores only its newest mutation in that interval. The table’s maximum
entry sequence must equal its published watermark. The final watermark cannot
exceed the last retained WAL sequence.

## SSTable layout

Filename: `sst-{id:016}.sst` with the decimal ID padded to at least 16 digits.
Temporary output uses the same ID and `.tmp` extension. File IDs are never
intentionally reused, including IDs from abandoned output.

Header: eight magic bytes `JEVSST01`, followed by version `1` as `u32`.

Body: the same checksummed frames described above, with mutation payloads sorted
strictly by key. Duplicate or out-of-order keys are invalid. Sequences must be
inside the table’s published interval. All tombstones are retained.

Footer: exactly 20 bytes:

| Relative offset | Size | Meaning |
| --- | --- | --- |
| 0 | 8 | Magic `JEVEND01` |
| 8 | 8 | Record count |
| 16 | 4 | CRC32 of complete header and body, including frame checksums |

The body must end exactly at the footer boundary. The actual file size must
match the manifest. Open validates all records, count, key order, sequence
interval, footer, and whole-file checksum, then builds a full key-to-offset index.
It does not retain values in memory. Later disk reads revalidate the record frame
and confirm its key and sequence against the index.

## Publication order

1. Exclusively create temporary table and write all records/footer.
2. Sync and close the writer.
3. Rename to its unique final name on the same filesystem.
4. Sync the containing directory on Unix; Windows currently provides no directory sync.
5. Open and validate the new reader.
6. Append and sync its manifest AddTable frame.
7. Install the reader and clear the memtable.

Unpublished files never contribute data to reads or recovery. A missing or invalid
published table causes an error even though historical WAL data is retained.
There is no automatic salvage or garbage collection. A crash before publication
is recovered through the WAL; after publication, replay starts above its watermark.

Version changes require an explicit migration or compatibility implementation;
unsupported versions currently fail to open.
