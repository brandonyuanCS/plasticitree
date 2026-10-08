use jev_storage::{Engine, Error};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
};
use tempfile::tempdir;

#[test]
fn every_truncation_of_final_wal_frame_is_repaired_before_append() {
    let source = tempdir().unwrap();
    let mut db = Engine::open(source.path()).unwrap();
    db.put(b"first", b"durable").unwrap();
    let boundary = fs::metadata(source.path().join("WAL")).unwrap().len() as usize;
    db.put(b"last", b"interrupted").unwrap();
    drop(db);
    let wal = fs::read(source.path().join("WAL")).unwrap();
    let manifest = fs::read(source.path().join("MANIFEST")).unwrap();
    for cut in boundary..wal.len() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("WAL"), &wal[..cut]).unwrap();
        fs::write(dir.path().join("MANIFEST"), &manifest).unwrap();
        let mut db = Engine::open(dir.path()).unwrap();
        assert_eq!(
            db.get(b"first").unwrap(),
            Some(b"durable".to_vec()),
            "cut {cut}"
        );
        assert_eq!(db.get(b"last").unwrap(), None, "cut {cut}");
        assert_eq!(
            fs::metadata(dir.path().join("WAL")).unwrap().len(),
            boundary as u64
        );
        db.put(b"later", b"safe").unwrap();
        drop(db);
        let mut db = Engine::open(dir.path()).unwrap();
        assert_eq!(db.get(b"later").unwrap(), Some(b"safe".to_vec()));
        assert_eq!(db.stats().sequence, 2);
    }
}

#[test]
fn every_truncation_of_final_manifest_frame_replays_wal() {
    let source = tempdir().unwrap();
    let mut db = Engine::open(source.path()).unwrap();
    db.put(b"key", b"old").unwrap();
    db.flush().unwrap();
    let boundary = fs::metadata(source.path().join("MANIFEST")).unwrap().len() as usize;
    db.put(b"key", b"new").unwrap();
    db.flush().unwrap();
    drop(db);
    let manifest = fs::read(source.path().join("MANIFEST")).unwrap();
    for cut in boundary..manifest.len() {
        let dir = tempdir().unwrap();
        for name in [
            "WAL",
            "sst-0000000000000001.sst",
            "sst-0000000000000002.sst",
        ] {
            fs::copy(source.path().join(name), dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("MANIFEST"), &manifest[..cut]).unwrap();
        let mut db = Engine::open(dir.path()).unwrap();
        assert_eq!(db.get(b"key").unwrap(), Some(b"new".to_vec()));
        assert_eq!(db.stats().sstable_count, 1);
        assert_eq!(db.stats().memtable_entries, 1);
        db.flush().unwrap();
        drop(db);
        assert!(dir.path().join("sst-0000000000000003.sst").exists());
        let mut db = Engine::open(dir.path()).unwrap();
        assert_eq!(db.get(b"key").unwrap(), Some(b"new".to_vec()));
        assert_eq!(db.stats().memtable_entries, 0);
    }
}

#[test]
fn orphans_are_ignored_and_ids_are_not_reused() {
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    db.put(b"k", b"v").unwrap();
    drop(db);
    fs::write(dir.path().join("sst-0000000000000100.sst"), b"bad orphan").unwrap();
    fs::write(
        dir.path().join("sst-0000000000000200.tmp"),
        b"partial orphan",
    )
    .unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    assert_eq!(db.stats().sstable_count, 0);
    db.flush().unwrap();
    assert!(dir.path().join("sst-0000000000000201.sst").exists());
    assert_eq!(
        fs::read(dir.path().join("sst-0000000000000100.sst")).unwrap(),
        b"bad orphan"
    );
}

#[test]
fn incomplete_initialization_fails_clearly() {
    let dir = tempdir().unwrap();
    let db = Engine::open(dir.path()).unwrap();
    drop(db);
    fs::remove_file(dir.path().join("MANIFEST")).unwrap();
    assert!(matches!(
        Engine::open(dir.path()),
        Err(Error::Corruption(_))
    ));
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("WAL"), b"JEV").unwrap();
    fs::write(dir.path().join("MANIFEST"), b"JEV").unwrap();
    assert!(matches!(
        Engine::open(dir.path()),
        Err(Error::Corruption(_))
    ));
}

#[test]
fn corrupted_files_are_not_repaired_during_failed_open() {
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    drop(db);
    let manifest_path = dir.path().join("MANIFEST");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&manifest_path)
        .unwrap();
    file.write_all(&[25, 0]).unwrap();
    drop(file);
    let wal_path = dir.path().join("WAL");
    let mut file = fs::OpenOptions::new().write(true).open(&wal_path).unwrap();
    file.seek(SeekFrom::Start(20)).unwrap();
    file.write_all(&[255]).unwrap();
    drop(file);
    let before = fs::read(&manifest_path).unwrap();
    assert!(matches!(
        Engine::open(dir.path()),
        Err(Error::Corruption(_))
    ));
    assert_eq!(fs::read(manifest_path).unwrap(), before);
}
