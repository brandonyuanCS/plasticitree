use crate::{
    Result,
    error::corrupt,
    journal::Journal,
    memtable::Memtable,
    record::{self, Entry},
};
use std::path::Path;

const MAGIC: &[u8; 8] = b"JEVWAL01";
pub(crate) struct Wal(pub Journal);

impl Wal {
    pub fn create(path: &Path) -> Result<()> {
        Journal::create(path, MAGIC, "WAL")
    }
    pub fn recover(path: &Path, persisted: u64, mem: &mut Memtable) -> Result<(Self, u64, u64)> {
        let mut journal = Journal::open(path, MAGIC, "WAL")?;
        let mut last = 0u64;
        let boundary = journal.replay(record::MAX_RECORD_SIZE, |payload| {
            let (key, entry) = record::decode(payload)?;
            if last.checked_add(1) != Some(entry.seq) {
                return Err(corrupt("WAL sequences are not contiguous"));
            }
            last = entry.seq;
            if last > persisted {
                mem.insert(key, entry);
            }
            Ok(())
        })?;
        if persisted > last {
            return Err(corrupt("manifest watermark exceeds retained WAL"));
        }
        Ok((Self(journal), last, boundary))
    }
    pub fn append(&mut self, key: &[u8], entry: &Entry) -> Result<()> {
        self.0.append(&record::encode(key, entry))
    }
}
