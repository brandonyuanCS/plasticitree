use crate::{
    Error, Result,
    error::corrupt,
    faults,
    manifest::{Manifest, TableMeta},
    memtable::Memtable,
    record::{self, Entry},
    sstable::SsTable,
    wal::Wal,
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
};

/// Owned live key/value pairs in lexicographic key order.
pub type ScanResult = Vec<(Vec<u8>, Vec<u8>)>;

/// Structural counters, not benchmark measurements. Memtable bytes count key,
/// value, and sequence bytes, excluding allocator and tree overhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub sequence: u64,
    pub persisted_sequence: u64,
    pub sstable_count: usize,
    pub memtable_entries: usize,
    pub memtable_bytes: usize,
}

/// One exclusively locked database. No implicit flush happens on drop.
pub struct Engine {
    dir: PathBuf,
    wal: Wal,
    manifest: Manifest,
    mem: Memtable,
    tables: Vec<SsTable>,
    sequence: u64,
    persisted: u64,
    next_table_id: u64,
    poisoned: bool,
    // Declared last so it remains held until all data-file handles are dropped.
    _lock: File,
}

impl Engine {
    /// Opens or creates a database. Incomplete initialization and corruption
    /// return errors; incomplete final journal frames are repaired on success.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(path.as_ref())?;
        let dir = fs::canonicalize(path.as_ref())?;
        let lock = crate::fs::lock(&dir.join("LOCK"))?;
        let wal_path = dir.join("WAL");
        let manifest_path = dir.join("MANIFEST");
        match (wal_path.try_exists()?, manifest_path.try_exists()?) {
            (false, false) => {
                for item in fs::read_dir(&dir)? {
                    if item?.file_name().to_string_lossy().starts_with("sst-") {
                        return Err(corrupt("table files exist without database journals"));
                    }
                }
                Wal::create(&wal_path)?;
                Manifest::create(&manifest_path)?;
                crate::fs::sync_directory(&dir)?;
            }
            (true, true) => {}
            _ => {
                return Err(corrupt(
                    "incomplete initialization: WAL or MANIFEST is missing",
                ));
            }
        }
        let (mut manifest, metadata, manifest_boundary) = Manifest::recover(&manifest_path)?;
        let mut tables = Vec::new();
        let mut persisted = 0;
        for meta in &metadata {
            tables.push(SsTable::open(
                &dir.join(table_name(meta.id)),
                meta,
                persisted,
            )?);
            persisted = meta.seq;
        }
        let mut mem = Memtable::default();
        let (mut wal, sequence, wal_boundary) = Wal::recover(&wal_path, persisted, &mut mem)?;
        // Repair only after every referenced object and journal validates.
        wal.0.repair(wal_boundary)?;
        manifest.0.repair(manifest_boundary)?;
        let mut highest_id = metadata.last().map_or(0, |meta| meta.id);
        for item in fs::read_dir(&dir)? {
            let name = item?.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = name
                .strip_prefix("sst-")
                .and_then(|s| s.strip_suffix(".sst").or_else(|| s.strip_suffix(".tmp")))
                && let Ok(id) = id.parse::<u64>()
            {
                highest_id = highest_id.max(id);
            }
        }
        let next_table_id = highest_id
            .checked_add(1)
            .ok_or_else(|| corrupt("SSTable file ID space exhausted"))?;
        Ok(Self {
            dir,
            _lock: lock,
            wal,
            manifest,
            mem,
            tables,
            sequence,
            persisted,
            next_table_id,
            poisoned: false,
        })
    }

    /// Synchronously logs a mutation before returning success.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.mutate(key, Some(value))
    }

    /// Writes a tombstone, including when the key does not currently exist.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.mutate(key, None)
    }

    fn mutate(&mut self, key: &[u8], value: Option<&[u8]>) -> Result<()> {
        self.healthy()?;
        record::validate(key, value)?;
        let seq = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("sequence space exhausted".into()))?;
        let key = key.to_vec();
        let entry = Entry {
            seq,
            value: value.map(<[u8]>::to_vec),
        };
        let result = self.wal.append(&key, &entry);
        self.check_io(result)?;
        self.mem.insert(key, entry);
        self.sequence = seq;
        Ok(())
    }

    /// Returns the newest value, with a tombstone represented as absence.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.healthy()?;
        record::validate(key, None)?;
        if let Some(entry) = self.mem.entries.get(key) {
            return Ok(entry.value.clone());
        }
        for table in self.tables.iter_mut().rev() {
            match table.get(key) {
                Ok(Some(entry)) => return Ok(entry.value),
                Ok(None) => {}
                Err(e) => {
                    self.poisoned = true;
                    return Err(e);
                }
            }
        }
        Ok(None)
    }

    /// Returns live entries in `[start, end)`. Equal bounds return empty;
    /// reversed bounds are invalid. Results and intermediate entries are owned.
    pub fn scan(&mut self, start: &[u8], end: &[u8]) -> Result<ScanResult> {
        self.healthy()?;
        record::validate(start, None)?;
        record::validate(end, None)?;
        if start > end {
            return Err(Error::InvalidInput("scan start exceeds end".into()));
        }
        if start == end {
            return Ok(Vec::new());
        }
        // A materialized ordered merge is deliberately simple. Later replace
        // this with streaming merge iterators to bound range-scan memory.
        let mut merged: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();
        for table in &mut self.tables {
            let result = table.visit_range(start, end, |key, entry| {
                merged.insert(key, entry);
            });
            if let Err(e) = result {
                self.poisoned = true;
                return Err(e);
            }
        }
        use std::ops::Bound::{Excluded, Included};
        for (key, entry) in self
            .mem
            .entries
            .range::<[u8], _>((Included(start), Excluded(end)))
        {
            merged.insert(key.clone(), entry.clone());
        }
        Ok(merged
            .into_iter()
            .filter_map(|(key, entry)| entry.value.map(|v| (key, v)))
            .collect())
    }

    /// Blocks while publishing the entire memtable. Retains all WAL records,
    /// old tables, and tombstones. Empty flushes do not create files.
    pub fn flush(&mut self) -> Result<()> {
        self.healthy()?;
        if self.mem.entries.is_empty() {
            return Ok(());
        }
        let result = self.flush_inner();
        self.check_io(result)
    }

    fn flush_inner(&mut self) -> Result<()> {
        let id = self.next_table_id;
        self.next_table_id = id
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("SSTable ID space exhausted".into()))?;
        let temp = self.dir.join(format!("sst-{id:016}.tmp"));
        let final_path = self.dir.join(table_name(id));
        faults::hit("flush_before_write")?;
        let size = SsTable::write(&temp, &self.mem.entries)?;
        faults::hit("flush_before_rename")?;
        fs::rename(&temp, &final_path)?;
        faults::hit("flush_after_rename")?;
        crate::fs::sync_directory(&self.dir)?;
        faults::hit("flush_after_directory_sync")?;
        let meta = TableMeta {
            id,
            size,
            seq: self.sequence,
        };
        // Validate and open the new reader before its irreversible publication.
        let table = SsTable::open(&final_path, &meta, self.persisted)?;
        self.manifest.publish(&meta)?;
        faults::hit("flush_before_install")?;
        self.tables.push(table);
        self.persisted = self.sequence;
        self.mem.clear();
        faults::hit("flush_after_clear")?;
        Ok(())
    }

    pub fn stats(&self) -> Stats {
        Stats {
            sequence: self.sequence,
            persisted_sequence: self.persisted,
            sstable_count: self.tables.len(),
            memtable_entries: self.mem.entries.len(),
            memtable_bytes: self.mem.bytes,
        }
    }

    fn healthy(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::RecoveryRequired)
        } else {
            Ok(())
        }
    }
    fn check_io<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

fn table_name(id: u64) -> String {
    format!("sst-{id:016}.sst")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::faults::testing;
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    const FLUSH_POINTS: &[&str] = &[
        "flush_before_write",
        "sst_after_record",
        "sst_before_sync",
        "sst_after_sync",
        "flush_before_rename",
        "flush_after_rename",
        "flush_after_directory_sync",
        "manifest_after_length",
        "manifest_before_sync",
        "manifest_after_sync",
        "flush_before_install",
        "flush_after_clear",
    ];
    const WAL_POINTS: &[&str] = &["wal_after_length", "wal_before_sync", "wal_after_sync"];

    fn prepare(path: &Path) {
        let mut db = Engine::open(path).unwrap();
        db.put(b"key", b"old").unwrap();
        db.put(b"deleted", b"old").unwrap();
        db.flush().unwrap();
    }
    fn check_recovered(path: &Path) {
        let mut db = Engine::open(path).unwrap();
        assert_eq!(db.get(b"key").unwrap(), Some(b"new".to_vec()));
        assert_eq!(db.get(b"deleted").unwrap(), None);
        assert_eq!(db.get(b"last").unwrap(), Some(b"acknowledged".to_vec()));
        let extra = db.get(b"uncertain").unwrap();
        assert!(extra.is_none() || extra == Some(b"possible".to_vec()));
        assert_eq!(db.stats().sequence, if extra.is_some() { 6 } else { 5 });
        db.put(b"after-restart", b"safe").unwrap();
        db.flush().unwrap();
        drop(db);
        let mut db = Engine::open(path).unwrap();
        assert_eq!(db.get(b"after-restart").unwrap(), Some(b"safe".to_vec()));
    }

    #[test]
    fn injected_flush_errors_require_recovery_and_preserve_data() {
        for &point in FLUSH_POINTS {
            let dir = tempdir().unwrap();
            prepare(dir.path());
            let mut db = Engine::open(dir.path()).unwrap();
            db.put(b"key", b"new").unwrap();
            db.delete(b"deleted").unwrap();
            db.put(b"last", b"acknowledged").unwrap();
            testing::set(point, false);
            let result = db.flush();
            testing::clear();
            assert!(matches!(result, Err(Error::Io(_))), "{point}");
            assert!(matches!(db.get(b"key"), Err(Error::RecoveryRequired)));
            assert!(matches!(db.scan(b"", b"z"), Err(Error::RecoveryRequired)));
            assert!(matches!(db.put(b"no", b"no"), Err(Error::RecoveryRequired)));
            assert!(matches!(db.flush(), Err(Error::RecoveryRequired)));
            drop(db);
            check_recovered(dir.path());
        }
    }

    #[test]
    fn injected_wal_errors_require_recovery_without_inventing_mutations() {
        for &point in WAL_POINTS {
            let dir = tempdir().unwrap();
            let mut db = Engine::open(dir.path()).unwrap();
            db.put(b"ack", b"yes").unwrap();
            testing::set(point, false);
            let result = db.put(b"uncertain", b"possible");
            testing::clear();
            assert!(matches!(result, Err(Error::Io(_))));
            assert!(matches!(db.delete(b"ack"), Err(Error::RecoveryRequired)));
            drop(db);
            let mut db = Engine::open(dir.path()).unwrap();
            assert_eq!(db.get(b"ack").unwrap(), Some(b"yes".to_vec()));
            assert_eq!(
                db.get(b"uncertain").unwrap(),
                if point == "wal_after_length" {
                    None
                } else {
                    Some(b"possible".to_vec())
                }
            );
            db.put(b"later", b"safe").unwrap();
            drop(db);
            let mut db = Engine::open(dir.path()).unwrap();
            assert_eq!(db.get(b"later").unwrap(), Some(b"safe".to_vec()));
        }
    }

    // Run only by the parent crash tests, in a separate process. Its acknowledgement
    // line is independent of the engine and captured by the parent before checks.
    #[test]
    #[ignore = "subprocess worker; invoked by recovery tests"]
    fn crash_worker() {
        let Some(path) = std::env::var_os("JEV_TEST_DB") else {
            return;
        };
        let point = std::env::var("JEV_TEST_POINT").unwrap();
        let mut db = Engine::open(path).unwrap();
        db.put(b"key", b"new").unwrap();
        db.delete(b"deleted").unwrap();
        db.put(b"last", b"acknowledged").unwrap();
        println!("JEV_ACK:5");
        std::io::stdout().flush().unwrap();
        if point == "external-kill" {
            loop {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let point = FLUSH_POINTS
            .iter()
            .chain(WAL_POINTS)
            .copied()
            .find(|&p| p == point)
            .unwrap();
        testing::set(point, true);
        if WAL_POINTS.contains(&point) {
            db.put(b"uncertain", b"possible").unwrap();
        } else {
            db.flush().unwrap();
        }
        panic!("crash point was not reached");
    }

    fn worker(path: &Path, point: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "engine::tests::crash_worker",
            "--ignored",
            "--nocapture",
        ])
        .env("JEV_TEST_DB", path)
        .env("JEV_TEST_POINT", point);
        cmd
    }

    #[test]
    fn abrupt_exit_at_every_publication_and_wal_boundary_recovers() {
        for &point in FLUSH_POINTS.iter().chain(WAL_POINTS) {
            let dir = tempdir().unwrap();
            prepare(dir.path());
            let mut child = worker(dir.path(), point)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("worker timed out at {point}");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(!status.success(), "{point}");
            let mut output = String::new();
            use std::io::Read;
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut output)
                .unwrap();
            assert!(
                output.contains("JEV_ACK:5"),
                "worker did not acknowledge at {point}: {output}"
            );
            check_recovered(dir.path());
        }
    }

    #[test]
    fn externally_killed_process_preserves_acknowledged_writes_and_releases_lock() {
        let dir = tempdir().unwrap();
        prepare(dir.path());
        let mut child = worker(dir.path(), "external-kill")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("JEV_ACK:5") {
            line.clear();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "worker exited before acknowledgement"
            );
        }
        assert!(matches!(Engine::open(dir.path()), Err(Error::Locked)));
        child.kill().unwrap();
        child.wait().unwrap();
        check_recovered(dir.path());
    }
}
