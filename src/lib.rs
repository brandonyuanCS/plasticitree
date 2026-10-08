//! A synchronous, byte-oriented storage engine.
//!
//! Mutations are synced to the WAL before acknowledgement. Flushes publish
//! immutable sorted tables through a manifest. See the README for the tested
//! failure model and the deliberately bounded scope of this implementation.
mod engine;
mod error;
mod faults;
mod fs;
mod journal;
mod manifest;
mod memtable;
mod record;
mod sstable;
mod wal;

pub use engine::{Engine, ScanResult, Stats};
pub use error::{Error, Result};
pub use record::{MAX_KEY_SIZE, MAX_VALUE_SIZE};
