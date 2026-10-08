use jev_storage::{Engine, Error};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
};
use tempfile::tempdir;

fn checksum_frame(bytes: &mut [u8], offset: usize) {
    let len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    let mut hash = crc32fast::Hasher::new();
    hash.update(&bytes[offset..offset + 4]);
    hash.update(&bytes[offset + 8..offset + 8 + len]);
    bytes[offset + 8 + len..offset + 12 + len].copy_from_slice(&hash.finalize().to_le_bytes());
}

#[test]
fn complete_corrupt_wal_frames_fail_including_at_eof() {
    for offset in [12, 16, 20, 28, 29, 33, 37, 38, 42] {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();
        db.put(b"k", b"value").unwrap();
        drop(db);
        let path = dir.path().join("WAL");
        let mut bytes = fs::read(&path).unwrap();
        bytes[offset] ^= 128;
        fs::write(path, bytes).unwrap();
        assert!(
            matches!(Engine::open(dir.path()), Err(Error::Corruption(_))),
            "offset {offset}"
        );
    }
}

#[test]
fn checksum_valid_malformed_records_fail_typed_validation() {
    for mode in 0..5 {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();
        db.put(b"k", b"v").unwrap();
        drop(db);
        let path = dir.path().join("WAL");
        let mut bytes = fs::read(&path).unwrap();
        match mode {
            0 => bytes[20..28].copy_from_slice(&0u64.to_le_bytes()),
            1 => bytes[20..28].copy_from_slice(&2u64.to_le_bytes()),
            2 => bytes[28] = 2,
            3 => bytes[29..33].copy_from_slice(&u32::MAX.to_le_bytes()),
            4 => bytes[28] = 0, // tombstone with nonempty value
            _ => unreachable!(),
        }
        checksum_frame(&mut bytes, 12);
        fs::write(path, bytes).unwrap();
        assert!(
            matches!(Engine::open(dir.path()), Err(Error::Corruption(_))),
            "mode {mode}"
        );
    }
}

#[test]
fn corrupt_retained_wal_history_is_not_skipped() {
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    drop(db);
    let path = dir.path().join("WAL");
    let mut bytes = fs::read(&path).unwrap();
    bytes[33] ^= 1;
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        Engine::open(dir.path()),
        Err(Error::Corruption(_))
    ));
}

#[test]
fn missing_or_corrupt_published_tables_fail_open() {
    for mode in 0..5 {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();
        db.put(b"k", b"v").unwrap();
        db.flush().unwrap();
        drop(db);
        let path = dir.path().join("sst-0000000000000001.sst");
        if mode == 0 {
            fs::remove_file(path).unwrap();
        } else {
            let mut bytes = fs::read(&path).unwrap();
            match mode {
                1 => {
                    bytes.pop();
                }
                2 => bytes[33] ^= 1,
                3 => {
                    let n = bytes.len();
                    bytes[n - 1] ^= 1;
                }
                4 => bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes()),
                _ => unreachable!(),
            }
            fs::write(path, bytes).unwrap();
        }
        assert!(
            matches!(Engine::open(dir.path()), Err(Error::Corruption(_))),
            "mode {mode}"
        );
    }
}

#[test]
fn corruption_after_open_is_detected_on_disk_read_and_poisons_engine() {
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("sst-0000000000000001.sst"))
        .unwrap();
    file.seek(SeekFrom::Start(34)).unwrap();
    file.write_all(b"x").unwrap();
    drop(file);
    assert!(matches!(db.get(b"k"), Err(Error::Corruption(_))));
    assert!(matches!(db.put(b"a", b"b"), Err(Error::RecoveryRequired)));
}

#[test]
fn corrupt_manifest_and_unsupported_versions_fail() {
    for filename in ["WAL", "MANIFEST"] {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();
        db.put(b"k", b"v").unwrap();
        db.flush().unwrap();
        drop(db);
        let path = dir.path().join(filename);
        let mut bytes = fs::read(&path).unwrap();
        bytes[8] = 2;
        fs::write(path, bytes).unwrap();
        assert!(matches!(
            Engine::open(dir.path()),
            Err(Error::Corruption(_))
        ));
    }
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    drop(db);
    let path = dir.path().join("MANIFEST");
    let mut bytes = fs::read(&path).unwrap();
    bytes[20] ^= 1;
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        Engine::open(dir.path()),
        Err(Error::Corruption(_))
    ));
}

#[test]
fn checksum_valid_invalid_frame_lengths_are_rejected_before_allocation() {
    for length in [0u32, u32::MAX] {
        let dir = tempdir().unwrap();
        let db = Engine::open(dir.path()).unwrap();
        drop(db);
        let path = dir.path().join("WAL");
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&crc32fast::hash(&length.to_le_bytes()).to_le_bytes());
        fs::write(path, bytes).unwrap();
        assert!(matches!(
            Engine::open(dir.path()),
            Err(Error::Corruption(_))
        ));
    }
}

#[test]
fn checksum_valid_sstable_order_and_sequence_damage_is_rejected() {
    for mode in 0..4 {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();
        db.put(b"old", b"v").unwrap();
        db.flush().unwrap();
        db.put(b"a", b"v").unwrap();
        db.put(b"b", b"v").unwrap();
        db.flush().unwrap();
        drop(db);
        let path = dir.path().join("sst-0000000000000002.sst");
        let mut bytes = fs::read(&path).unwrap();
        let first = 12;
        let first_len = u32::from_le_bytes(bytes[first..first + 4].try_into().unwrap()) as usize;
        let second = first + first_len + 12;
        match mode {
            0 => bytes[second + 8 + 17] = b'a', // duplicate key, valid checksums
            1 => bytes[first + 8..first + 16].copy_from_slice(&1u64.to_le_bytes()),
            2 => bytes[first + 8..first + 16].copy_from_slice(&4u64.to_le_bytes()),
            3 => bytes[second + 8..second + 16].copy_from_slice(&2u64.to_le_bytes()),
            _ => unreachable!(),
        }
        checksum_frame(&mut bytes, first);
        checksum_frame(&mut bytes, second);
        let end = bytes.len();
        let hash = crc32fast::hash(&bytes[..end - 20]);
        bytes[end - 4..].copy_from_slice(&hash.to_le_bytes());
        fs::write(path, bytes).unwrap();
        assert!(
            matches!(Engine::open(dir.path()), Err(Error::Corruption(_))),
            "mode {mode}"
        );
    }
}

#[test]
fn wal_sequence_gap_and_watermark_without_wal_history_fail() {
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    db.put(b"a", b"v").unwrap();
    let boundary = fs::metadata(dir.path().join("WAL")).unwrap().len() as usize;
    db.put(b"b", b"v").unwrap();
    drop(db);
    let path = dir.path().join("WAL");
    let mut bytes = fs::read(&path).unwrap();
    bytes[boundary + 8..boundary + 16].copy_from_slice(&3u64.to_le_bytes());
    checksum_frame(&mut bytes, boundary);
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        Engine::open(dir.path()),
        Err(Error::Corruption(_))
    ));

    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    drop(db);
    let path = dir.path().join("WAL");
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(12)
        .unwrap();
    assert!(matches!(
        Engine::open(dir.path()),
        Err(Error::Corruption(_))
    ));
}
