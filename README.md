# Jev storage engine

A Rust storage engine built to study persistence, recovery, and eventually
workload-adaptive LSM policies. **Phase 0 is implemented.** Jev is not connected;
all current behavior is deterministic.

This is an educational storage engine, not a production database. It implements
byte-oriented `put`, `get`, `delete`, and half-open range `scan`, a synchronous
write-ahead log, an ordered memtable, immutable sorted tables, manifest-based
flush publication, and restart recovery.

## Run it

Requires Rust 1.89 or newer. From the project directory:

```sh
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo run --example basic
```

The example uses `demo-data/`, writes and flushes records, reopens the engine, and
checks recovered updates and deletes. Pass a directory as its first argument to
use another location. No server or background process is needed.

If an already-open terminal does not recognize `cargo` after installing Rust,
restart the terminal so it picks up the installer’s PATH changes.

## Library usage

```rust
use jev_storage::{Engine, Result};

fn main() -> Result<()> {
    let mut db = Engine::open("demo-data")?;
    db.put(b"alice", b"engineer")?;
    db.put(b"bob", b"")?;          // An empty value is still a live value.
    db.flush()?;                   // Publish a sorted file and clear the memtable.
    db.delete(b"bob")?;            // A tombstone hides older on-disk values.
    assert_eq!(db.get(b"alice")?, Some(b"engineer".to_vec()));
    let rows = db.scan(b"a", b"c")?; // [a, c), ordered by raw key bytes.
    assert_eq!(rows.len(), 1);
    drop(db);                      // No implicit SSTable flush is necessary.

    let mut db = Engine::open("demo-data")?;
    assert_eq!(db.get(b"bob")?, None); // WAL replay restores the unflushed delete.
    Ok(())
}
```

Keys and values are arbitrary bytes; key ordering is lexicographic, not numeric
or locale-aware. Keys are limited to 64 KiB and values to 16 MiB. Empty keys are
valid. Deleting an absent key succeeds. Reversed scan bounds return an error;
equal bounds return an empty result. Scans return owned vectors.

Only one engine can open a directory at a time. An OS-backed lock is held until
the engine and its file handles are dropped; a crashed process releases it.

## How persistence works

```text
put/delete: append WAL -> sync WAL -> update memtable -> return success
get:        memtable -> newest SSTable -> older SSTables
flush:      write/sync SSTable -> rename -> publish/sync manifest -> clear memtable
open:       validate manifest/tables/WAL -> repair incomplete tails -> replay newer writes
```

Each mutation receives a sequence number. A tombstone is distinct from an empty
value and prevents an older file from bringing a deleted value back. SSTable
values remain on disk; an in-memory index stores keys, offsets, and sequences.
Records are checksummed again when read, not just at startup.

The manifest names published SSTables and records a contiguous flush watermark.
Recovery applies WAL mutations newer than that watermark. It still validates
all retained WAL records. Temporary files and unpublished final files are
ignored, and file IDs account for those orphans so they are not overwritten.

A complete record with a bad checksum causes an error. Only an incomplete final
journal frame is truncated, and that repair is synced before new appends.
Record lengths have their own checksum so damaged complete headers are not
mistaken for interrupted payloads. Missing published tables also cause errors.

An append, sync, or publication failure may leave an uncertain outcome. The
engine then returns `RecoveryRequired` for operations until closed and reopened.
A write whose caller never saw success may nevertheless be recovered. Successful
acknowledged writes must be preserved under the tested failure model.

## Durability scope

Validated on Windows with Rust 1.99: normal restart, externally killed processes,
abrupt termination at WAL/flush boundaries, incomplete journal tails, corrupted
records/files, and injected IO failures. Tests confirm locking across processes.

**Power-loss durability is not claimed.** Windows currently uses standard file
sync and rename, without a directory-metadata durability implementation. The
Unix path syncs directory handles, but has not been runtime-tested here. A killed
process leaves the OS page cache alive; these tests do not emulate a machine
losing power, arbitrary disk faults, or every filesystem’s persistence behavior.

Partially initialized directories fail with a diagnostic. The engine never
silently creates an empty replacement for a directory containing incomplete
journals or table files without journals. Do not modify database files while an
engine is open; tests deliberately do so only to check corruption detection.

## Validation and current limits

Tests cover binary and empty data, size limits, overwritten keys, tombstones,
multiple flushes, disk reads, scans, exclusive opening, every truncation position
in a final WAL/manifest frame, repair followed by further writes, publication
crash boundaries, short writes, injected append/sync failures, and seeded
reference-model comparisons across flushes and restarts.

The ignored unit test `engine::tests::crash_worker` is a subprocess fixture. Parent
tests invoke it explicitly; it is not an omitted correctness test.

Phase 0 deliberately retains all WAL history, SSTables, and tombstones. There is
no compaction, WAL deletion, automatic flushing, block cache, Bloom filter, or
background work. Table indexes contain every key, startup validates whole files,
and scans materialize an ordered merge before returning results. Memtable byte
statistics exclude tree and allocator overhead. These costs grow with the data;
no throughput or latency improvements are claimed.

Phase 1 will add actual LSM level/compaction mechanics and safe reclamation before
introducing background concurrency. Benchmarking precedes adaptive policies.
Jev will remain an optional control-plane experiment.

## Read the implementation

- `src/engine.rs`: ownership and operation ordering; start here.
- `src/wal.rs` and `src/journal.rs`: WAL replay, framing, sync, and tail repair.
- `src/sstable.rs`: immutable format, validation, offsets, and disk reads.
- `src/manifest.rs`: file publication and flush watermarks.
- `docs/file-format.md`: exact version-1 binary layouts and recovery rules.
- `docs/phase-0-plan.md`: design tradeoffs and implementation milestones.
