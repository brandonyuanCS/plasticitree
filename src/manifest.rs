use crate::{Result, error::corrupt, journal::Journal};
use std::path::Path;

const MAGIC: &[u8; 8] = b"JEVMAN01";
#[derive(Clone, Debug)]
pub(crate) struct TableMeta {
    pub id: u64,
    pub size: u64,
    pub seq: u64,
}
pub(crate) struct Manifest(pub Journal);

impl Manifest {
    pub fn create(path: &Path) -> Result<()> {
        Journal::create(path, MAGIC, "MANIFEST")
    }
    pub fn recover(path: &Path) -> Result<(Self, Vec<TableMeta>, u64)> {
        let mut journal = Journal::open(path, MAGIC, "MANIFEST")?;
        let mut tables: Vec<TableMeta> = Vec::new();
        let boundary = journal.replay(25, |bytes| {
            if bytes.len() != 25 || bytes[0] != 1 {
                return Err(corrupt("invalid manifest operation"));
            }
            let meta = TableMeta {
                id: u64::from_le_bytes(bytes[1..9].try_into().unwrap()),
                size: u64::from_le_bytes(bytes[9..17].try_into().unwrap()),
                seq: u64::from_le_bytes(bytes[17..25].try_into().unwrap()),
            };
            if meta.id == 0
                || meta.seq == 0
                || meta.size < 32
                || tables
                    .last()
                    .is_some_and(|old| meta.id <= old.id || meta.seq <= old.seq)
            {
                return Err(corrupt("invalid manifest table order/metadata"));
            }
            tables.push(meta);
            Ok(())
        })?;
        Ok((Self(journal), tables, boundary))
    }
    pub fn publish(&mut self, meta: &TableMeta) -> Result<()> {
        let mut bytes = Vec::with_capacity(25);
        bytes.push(1);
        bytes.extend_from_slice(&meta.id.to_le_bytes());
        bytes.extend_from_slice(&meta.size.to_le_bytes());
        bytes.extend_from_slice(&meta.seq.to_le_bytes());
        self.0.append(&bytes)
    }
}
