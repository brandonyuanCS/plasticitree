use crate::{
    Result,
    error::corrupt,
    faults,
    manifest::TableMeta,
    record::{self, Entry},
};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    ops::Bound::{Excluded, Included},
    path::Path,
};

const HEADER: &[u8; 12] = b"JEVSST01\x01\x00\x00\x00";
const FOOTER_MAGIC: &[u8; 8] = b"JEVEND01";
const FOOTER_SIZE: u64 = 20;

struct Location {
    offset: u64,
    seq: u64,
}

pub(crate) struct SsTable {
    file: File,
    index: BTreeMap<Vec<u8>, Location>,
    body_end: u64,
}

impl SsTable {
    pub fn write(path: &Path, entries: &BTreeMap<Vec<u8>, Entry>) -> Result<u64> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut hash = crc32fast::Hasher::new();
        file.write_all(HEADER)?;
        hash.update(HEADER);
        for (key, entry) in entries {
            let payload = record::encode(key, entry);
            let length = (payload.len() as u32).to_le_bytes();
            let length_checksum = crc32fast::hash(&length).to_le_bytes();
            let mut frame_hash = crc32fast::Hasher::new();
            frame_hash.update(&length);
            frame_hash.update(&payload);
            let checksum = frame_hash.finalize().to_le_bytes();
            for bytes in [&length[..], &length_checksum, &payload, &checksum] {
                file.write_all(bytes)?;
                hash.update(bytes);
            }
            faults::hit("sst_after_record")?;
        }
        file.write_all(FOOTER_MAGIC)?;
        file.write_all(&(entries.len() as u64).to_le_bytes())?;
        file.write_all(&hash.finalize().to_le_bytes())?;
        faults::hit("sst_before_sync")?;
        file.sync_all()?;
        faults::hit("sst_after_sync")?;
        Ok(file.metadata()?.len())
    }

    pub fn open(path: &Path, meta: &TableMeta, previous_seq: u64) -> Result<Self> {
        let mut file = File::open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                corrupt(format!("published table {} is missing", meta.id))
            } else {
                e.into()
            }
        })?;
        let size = file.metadata()?.len();
        if size != meta.size || size < HEADER.len() as u64 + FOOTER_SIZE {
            return Err(corrupt(format!("table {} has incorrect size", meta.id)));
        }
        let body_end = size - FOOTER_SIZE;
        file.seek(SeekFrom::Start(body_end))?;
        let mut footer = [0; FOOTER_SIZE as usize];
        file.read_exact(&mut footer)?;
        if &footer[..8] != FOOTER_MAGIC {
            return Err(corrupt("invalid SSTable footer"));
        }
        let count = u64::from_le_bytes(footer[8..16].try_into().unwrap());
        let checksum = u32::from_le_bytes(footer[16..20].try_into().unwrap());
        file.seek(SeekFrom::Start(0))?;
        let mut header = [0; 12];
        file.read_exact(&mut header)?;
        if &header != HEADER {
            return Err(corrupt("invalid SSTable header/version"));
        }
        let mut hash = crc32fast::Hasher::new();
        hash.update(&header);
        let mut index = BTreeMap::new();
        let mut last_key: Option<Vec<u8>> = None;
        let mut max_seq = 0;
        while file.stream_position()? < body_end {
            let offset = file.stream_position()?;
            let (key, entry) = read_record(&mut file, body_end, Some(&mut hash))?;
            if last_key.as_ref().is_some_and(|last| last >= &key)
                || entry.seq <= previous_seq
                || entry.seq > meta.seq
            {
                return Err(corrupt(
                    "SSTable keys/order or sequence interval is invalid",
                ));
            }
            max_seq = max_seq.max(entry.seq);
            last_key = Some(key.clone());
            index.insert(
                key,
                Location {
                    offset,
                    seq: entry.seq,
                },
            );
        }
        if index.len() as u64 != count || max_seq != meta.seq || hash.finalize() != checksum {
            return Err(corrupt("SSTable count, watermark, or checksum mismatch"));
        }
        Ok(Self {
            file,
            index,
            body_end,
        })
    }

    pub fn get(&mut self, key: &[u8]) -> Result<Option<Entry>> {
        let Some(location) = self.index.get(key) else {
            return Ok(None);
        };
        Ok(Some(read_at(&mut self.file, self.body_end, key, location)?))
    }

    pub fn visit_range(
        &mut self,
        start: &[u8],
        end: &[u8],
        mut visit: impl FnMut(Vec<u8>, Entry),
    ) -> Result<()> {
        for (key, location) in self
            .index
            .range::<[u8], _>((Included(start), Excluded(end)))
        {
            let entry = read_at(&mut self.file, self.body_end, key, location)?;
            visit(key.clone(), entry);
        }
        Ok(())
    }
}

fn read_at(file: &mut File, end: u64, expected_key: &[u8], location: &Location) -> Result<Entry> {
    file.seek(SeekFrom::Start(location.offset))?;
    let (key, entry) = read_record(file, end, None)?;
    if key != expected_key || entry.seq != location.seq {
        return Err(corrupt("SSTable changed after opening"));
    }
    Ok(entry)
}

fn read_record(
    file: &mut File,
    end: u64,
    hash: Option<&mut crc32fast::Hasher>,
) -> Result<(Vec<u8>, Entry)> {
    let offset = file.stream_position()?;
    if end.saturating_sub(offset) < 8 {
        return Err(corrupt("truncated SSTable frame"));
    }
    let mut length = [0; 4];
    file.read_exact(&mut length)?;
    let mut length_checksum = [0; 4];
    file.read_exact(&mut length_checksum)?;
    if crc32fast::hash(&length) != u32::from_le_bytes(length_checksum) {
        return Err(corrupt("SSTable frame length checksum mismatch"));
    }
    let size = u32::from_le_bytes(length) as usize;
    if !(record::RECORD_HEADER..=record::MAX_RECORD_SIZE).contains(&size)
        || end - offset < size as u64 + 12
    {
        return Err(corrupt("invalid SSTable frame length"));
    }
    let mut payload = vec![0; size];
    file.read_exact(&mut payload)?;
    let mut checksum = [0; 4];
    file.read_exact(&mut checksum)?;
    let mut frame_hash = crc32fast::Hasher::new();
    frame_hash.update(&length);
    frame_hash.update(&payload);
    if frame_hash.finalize() != u32::from_le_bytes(checksum) {
        return Err(corrupt("SSTable record checksum mismatch"));
    }
    if let Some(hash) = hash {
        hash.update(&length);
        hash.update(&length_checksum);
        hash.update(&payload);
        hash.update(&checksum);
    }
    record::decode(&payload)
}
