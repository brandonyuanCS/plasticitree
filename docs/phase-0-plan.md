# Phase 0: a persistent Rust storage engine

Status: Phase 0 implemented. See `../README.md` for usage, validation, and actual durability scope. No performance claims exist yet. This document retains the milestone rationale; exact implemented layouts are in `file-format.md`.

## Objective and scope

Build a small, deterministic, single-process storage engine whose writes survive process crashes and whose reads actually reach immutable files on disk. Implement byte-oriented put/get/delete/range-scan operations, a WAL, an ordered memtable, SSTables, publication metadata, and recovery.

No Jev, controller interfaces, SQL, server, async runtime, concurrent engine access, levels, compaction, Bloom filters, cache, transactions, or MVCC in this phase. Sequence numbers establish operation order; they do not provide snapshot isolation.

Use a flat list of SSTables in Phase 0. This brings the minimum multiple-file handling forward from Phase 1: repeated flushes need somewhere to go. Phase 1 adds actual level organization, compaction, reclamation, and concurrency.

## Decisions and alternatives

| Decision | Phase 0 choice | Tradeoff / later alternative |
| --- | --- | --- |
| Execution | One owner, synchronous operations; mutable receiver for engine operations | Easy ordering and recovery reasoning; concurrency comes later |
| Keys and values | Owned byte vectors; keys use bytewise lexicographic order | Predictable storage format; callers encode strings/numbers themselves |
| Memtable | `BTreeMap<Vec<u8>, Entry>` | Ordered scans without a custom skip list; custom allocation/layout work can follow |
| Durability | Sync WAL on each mutation before returning success | Clear contract, expensive write path; batching is a later measured optimization |
| Flush | Blocking, initially explicit via `flush()` | Removes background coordination; simple size-triggered flush can follow |
| SSTable lookup | In-memory full key-to-record-offset index; values stay on disk | Simple real disk reads, but memory grows with key count; sparse/block indexes replace it later |
| Published files | Framed append-only manifest | Small amount of essential metadata; avoids relying on directory discovery or replacing a metadata file |
| WAL lifecycle | One append-only WAL, retained in full | Avoids premature log rotation/deletion protocols; disk and recovery cost grow |
| File format | Explicit versioned binary encoding | Teaches layout and bounds checks; general object serialization hides these decisions |
| IO | Standard blocking file IO; seek/read through owned handles | No mmap or async complexity; positioned reads become useful with concurrent readers |

Do not introduce a trait for every module. Extract an IO seam when fault injection needs it. A small library crate with concrete types is enough.

## API contract

Illustrative signatures, not implemented code:

```rust
impl Engine {
    pub fn open(path: impl AsRef<Path>) -> Result<Self>;
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()>;
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    pub fn delete(&mut self, key: &[u8]) -> Result<()>;
    pub fn scan(&mut self, start: &[u8], end: &[u8])
        -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
    pub fn flush(&mut self) -> Result<()>;
}
```

- `scan` returns live keys in ascending order over `[start, end)`. Equal bounds return empty; reversed bounds return an error. Collecting results is acceptable initially; the public streaming API is deferred.
- Empty keys and empty values are valid. An empty value is distinct from a deletion.
- Deleting an absent key succeeds and records a tombstone.
- A successful mutation is visible to subsequent operations and has been synced to the WAL.
- An interrupted or failed mutation may appear after recovery even if its caller never received success. This is an uncertain outcome, not a lost acknowledged write.
- Hold an OS-backed exclusive directory lock for the engine lifetime. A second opener fails; a crash releases the lock. A lock file's existence alone is not a lock.
- Reject excessive lengths before allocation. Begin with documented constants, for example a 64 KiB key limit and 16 MiB value limit. They are limits, not tuning claims.
- Any uncertain append/sync/publication failure puts the engine into a recovery-required state. Return errors until it is closed and reopened. Do not continue after a partial write.

`&mut self` deliberately keeps access serialized, including file seeks. Relax this only when Phase 1 introduces an explicit concurrency model.

## Module layout

```text
Cargo.toml
src/
  lib.rs          public exports
  engine.rs       ownership, read/write/flush/recovery orchestration
  error.rs        IO, corruption, invalid input, locked, recovery-required errors
  record.rs       sequence number, value/tombstone, byte encoding helpers
  wal.rs          append, sync, validated replay, torn-tail repair
  memtable.rs     ordered latest mutation per key and byte accounting
  sstable.rs      sorted immutable writer, validated reader, offset index
  manifest.rs     published table list and durable flush watermark
tests/
  api.rs
  recovery.rs
  corruption.rs
  model.rs
```

Start with the standard library. Use small dependencies for checksums, test temporary directories, property testing, or a portable OS lock if the selected Rust toolchain requires one. Implement the engine mechanics yourself; do not build it on another database.

## State and files

```text
database/
  LOCK
  WAL
  MANIFEST
  sst-0000000000000001.sst
  sst-0000000000000002.sst
  *.tmp                         unpublished output, never a read source
```

An `Entry` contains a monotonically increasing `u64` sequence number and either a value or a tombstone. Guard against counter overflow. The memtable holds only the newest entry per key.

The manifest records the ordered live SSTable list and the largest sequence number covered by a completed flush. Call this `persisted_seq`. A flush covers every operation since the previous completed flush through this watermark, even if repeated mutations to one key collapse into one table entry.

Allocate file IDs above every existing candidate filename, including orphans. Create new files exclusively. Never overwrite an existing SSTable or reuse an orphan's name during a live session.

## On-disk encoding

Specify exact field widths, little-endian integer encoding, magic bytes, versions, checksum coverage, and maximum lengths in the format documentation before implementation.

### WAL

Versioned header followed by framed records:

```text
payload_length:u32 | length_crc32:u32 | payload | record_crc32:u32
payload = sequence:u64 | kind:u8 | key_length:u32 | value_length:u32 | key | value
```

The length has a separate checksum; the record checksum covers the frame length and payload. The implementation uses IEEE CRC32, not CRC32C. Put and delete have distinct tags; delete has no value bytes. Validate tags, lengths, checksums, and strict sequence order on replay. This format is intentionally simpler than a block-fragmented production WAL.

### SSTable

Versioned header, variable-length records sorted strictly by key, and a fixed-size footer containing record count and a checksum covering the header and records. Each record contains key, sequence number, and value/tombstone using explicit bounded lengths.

On open, sequentially validate the table and build an in-memory key/offset index. Do not load all values into RAM. Point reads seek to the selected record. Range reads walk consecutive records. This has expensive startup and an unbounded full index across tables; those are explicit Phase 0 limitations.

### Manifest

Versioned header and checksummed framed `AddTable` records containing a table ID, expected file size, and the resulting `persisted_seq`. Only successful complete flushes advance that sequence. No removal records are needed before compaction.

The manifest is the source of truth for file membership. A valid-looking orphan is still unpublished. A manifest-referenced missing or corrupt table is an error, not permission to return partial data.

## Write path

1. Validate and own the input; reserve the next sequence number.
2. Encode and append the WAL record with complete-write handling.
3. Call `File::sync_all()` and check the result.
4. Apply the entry to the memtable and advance the in-memory sequence.
5. Return success.

Invariant: a visible acknowledged mutation has a durable WAL record. WAL writes precede memtable changes. An IO error stops further operations until recovery.

Do not equate a buffered writer's `flush()` with durable storage. Explicitly propagate sync errors; dropping a file is not a durability protocol.

## Read and scan paths

For `get`, check the memtable first, then SSTables from newest flush to oldest. The first match wins, including a tombstone, which returns absence immediately. With serialized writes and whole-memtable flushes, newer tables cover strictly newer operation intervals.

For `scan`, merge ordered sources by key, keep the highest-sequence entry for duplicates, suppress tombstones, and enforce the range bounds. The implementation materializes an ordered map, applying tables oldest to newest and then the memtable; validated nonoverlapping sequence intervals make that equivalent to selecting the highest sequence. Streaming merge iterators are deferred. Tests include duplicate keys and deletes across several flushes.

Invariant: the highest-sequence mutation wins everywhere. A deletion must not reveal an older value.

## Flush publication protocol

The engine blocks all other operations while flushing. Empty memtables are a no-op.

1. Capture the memtable and its maximum sequence `S` without clearing it.
2. Write sorted entries, including tombstones, to a new temporary SSTable.
3. Complete its footer, sync the file, and close the writer.
4. Rename it to its unique final filename on the same filesystem. Persist the directory entry using the supported platform protocol.
5. Append `AddTable(id, size, S)` to the manifest and sync the manifest.
6. Install the table in the in-memory published list, set `persisted_seq = S`, and clear the memtable.

Keep the WAL unchanged. Do not delete old SSTables or tombstones.

Invariant: recovery cannot see a publication record before its complete table is durable. If publication never happened, WAL replay supplies the mutations. If publication happened, recovery uses the table and replays only later mutations.

An error after a manifest write has an uncertain publication outcome. Reopen and recover; do not append another publication from the uncertain in-memory state.

## Recovery protocol

1. Acquire the exclusive lock and validate database headers/versions.
2. Read the manifest to its last complete valid record; rebuild the published table list and watermark.
3. Validate every referenced SSTable and construct its index. Validate manifest ordering and file metadata.
4. Replay the WAL in order. Validate all retained records, including records at or below `persisted_seq`; apply only records with sequence greater than that watermark.
5. Set the next sequence above the maximum recovered sequence. Check that manifest watermarks are consistent with the retained WAL and table metadata.
6. Truncate incomplete WAL/manifest tails to the last valid frame boundary and sync those repairs before allowing new appends.
7. Ignore unpublished temporary/final files for reads. Leave garbage cleanup for a separate, conservative operation.

Only an incomplete final frame is automatically repairable. A complete frame with a bad checksum, impossible lengths, invalid tag, or out-of-order sequence returns corruption, including at EOF. Do not silently skip damaged records. This conservative policy trades availability for avoiding hidden data loss.

The failure model allows append interruption to leave a partial final frame. Arbitrary bit rot or a full-sized torn record may produce an error; checksums detect damage and do not repair it.

## Durability target and platform scope

First validate abrupt process termination and restart on the development platform. A child-process kill test exercises recovery but does not simulate power loss or a lost OS page cache.

The protocol must sync file contents and required filesystem metadata in dependency order. File sync and rename alone are not a cross-platform proof of power-loss safety. Implement the platform filesystem helper explicitly and document what Windows/Linux support has actually been validated. Do not advertise machine-power-loss durability before the file-publication protocol and initialization are tested on the target filesystem.

Database creation must also persist WAL/manifest headers and their directory entries before acknowledging any mutation. A partially initialized directory must produce a clear diagnostic or follow a documented recovery rule; it must not be mistaken for a healthy empty database.

## Implementation milestones and acceptance gates

### M0 — contract and buildable library

Create the Cargo project, error types, byte semantics, records, and in-memory API. Add API tests for overwrite, absent/present delete, empty bytes, lexical ordering, and half-open scans. Document toolchain and limits. Implementation was validated with the installed Rust 1.99 toolchain; the crate requires Rust 1.89 or newer.

Gate: a small example and tests run locally; you can explain why tombstones differ from empty values.

### M1 — durable WAL and recovery

Implement bounded framed encoding, checksum validation, synchronous append, exclusive locking, replay, torn-tail truncation, and recovery-required error state.

Gate: reopen recovers overwrites/deletes; a process killed without normal shutdown preserves acknowledged mutations. Every byte-position truncation of the final test frame produces the specified outcome. Complete-frame corruption fails clearly. Repaired logs accept and recover later writes.

### M2 — immutable SSTable

Implement table writer/reader, format validation, disk point lookup, ordered range traversal, and the full offset index. Test independently before connecting flush.

Gate: round trips cover binary keys/values and tombstones; malformed lengths, sorting violations, missing footer, and checksum mismatches fail without uncontrolled allocation or panic.

### M3 — manifest, flush, and multiple flat runs

Implement publication and restart behavior, then merged reads/scans across several flushes. Keep full WAL retention. Test every publication boundary: before/after file sync, rename, metadata persistence, manifest append, manifest sync, and memtable clear.

Gate: each restart selects a consistent published state and recovers all acknowledged mutations. Orphans do not affect reads; referenced missing/corrupt tables fail. Deletes survive repeated flushes and restart.

### M4 — correctness evidence and explanation

Run seeded operation sequences against a `BTreeMap` reference model, interleaving put/delete/get/scan/flush/reopen. Use subprocess crash tests with an external record of acknowledgements. If extra unacknowledged operations recover, they must correspond to a valid operation prefix, not invented state.

Add focused append/sync failure injection and confirm the engine rejects further work until reopened. Inject short writes through the IO seam where feasible. Keep failure injection private to testing.

Gate: checked-in reproducible tests and an architecture/format walkthrough. A tiny timed smoke run may catch regressions, but it is not a benchmark claim.

## Phase 0 completion checklist

- All four KV operations work on arbitrary bounded bytes.
- Reads use on-disk SSTable values after flush and restart.
- Latest-write-wins semantics and deletion remain correct across sources.
- Acknowledged WAL writes survive the tested crash model.
- Incomplete tail repair cannot leave garbage before future appends.
- Publication/recovery tests cover crash boundaries.
- Missing files and corruption produce explicit errors.
- No cleanup can discard the only durable representation of data.
- Documentation names the actual platform durability guarantees and limitations.

## Deliberate limitations and Phase 1 handoff

WAL and SSTable count grow indefinitely; reads degrade as runs accumulate; the full index consumes memory; flushes block; WAL sync occurs per mutation; scans collect results. These are educationally useful limitations to measure and replace, not performance claims.

Phase 1 should introduce compaction with manifest add/remove edits, a safe WAL rotation/reclamation protocol, sparse/block indexes, filters, cache, then background/concurrent access with explicit ownership and publication rules. Tombstones may be dropped only when no older reachable value can be resurrected. Benchmark instrumentation follows actual engine behavior; adaptive controllers remain out of the implementation until their prerequisites exist.

## Interview questions to answer before moving on

1. Why must the WAL become durable before a mutation is acknowledged?
2. What can a caller infer from a failed write, and what remains uncertain?
3. Why is discovering `.sst` filenames insufficient for recovery?
4. Why must a table become durable before its manifest publication?
5. How does a tombstone prevent resurrection across files?
6. Why can a flush watermark skip old WAL replay without losing overwritten operations?
7. How is process termination different from power failure?
8. Which costs grow without compaction, sparse indexes, and log reclamation?

## Primary references

- Rust `File` documentation, including sync behavior: https://doc.rust-lang.org/std/fs/struct.File.html
- Rust filesystem rename behavior: https://doc.rust-lang.org/std/fs/fn.rename.html
- RocksDB WAL role: https://github.com/facebook/rocksdb/wiki/Write-Ahead-Log-(WAL)
- RocksDB manifest role: https://github.com/facebook/rocksdb/wiki/MANIFEST

These references support the underlying IO and recovery concepts. This plan intentionally uses a smaller format and protocol than RocksDB.
