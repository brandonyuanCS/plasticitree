use jev_storage::{Engine, Error, MAX_KEY_SIZE, MAX_VALUE_SIZE};
use tempfile::tempdir;

#[test]
fn byte_semantics_and_scan_bounds() {
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    assert_eq!(db.get(b"absent").unwrap(), None);
    db.put(b"", b"").unwrap();
    db.put(b"a", b"one").unwrap();
    db.put(b"a\0", b"two").unwrap();
    db.put(&[255], &[0, 255]).unwrap();
    db.put(b"a", b"updated").unwrap();
    db.delete(b"missing").unwrap();
    assert_eq!(db.get(b"").unwrap(), Some(Vec::new()));
    assert_eq!(
        db.scan(b"", b"a\0").unwrap(),
        vec![(vec![], vec![]), (b"a".to_vec(), b"updated".to_vec())]
    );
    assert_eq!(db.scan(b"a", b"a").unwrap(), vec![]);
    assert!(matches!(db.scan(b"z", b"a"), Err(Error::InvalidInput(_))));
    assert_eq!(db.get(&[255]).unwrap(), Some(vec![0, 255]));
}

#[test]
fn tombstones_and_overwrites_across_multiple_runs() {
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    db.flush().unwrap();
    assert_eq!(db.stats().sstable_count, 0);
    db.put(b"a", b"old").unwrap();
    db.put(b"b", b"old").unwrap();
    db.flush().unwrap();
    assert_eq!(db.stats().memtable_entries, 0);
    assert_eq!(db.get(b"a").unwrap(), Some(b"old".to_vec()));
    db.delete(b"a").unwrap();
    db.put(b"b", b"new").unwrap();
    db.flush().unwrap();
    db.put(b"c", b"live").unwrap();
    db.flush().unwrap();
    db.put(b"a", b"reborn").unwrap();
    db.delete(b"b").unwrap();
    let expected = vec![
        (b"a".to_vec(), b"reborn".to_vec()),
        (b"c".to_vec(), b"live".to_vec()),
    ];
    assert_eq!(db.scan(b"", b"z").unwrap(), expected);
    drop(db);
    let mut db = Engine::open(dir.path()).unwrap();
    assert_eq!(db.stats().sstable_count, 3);
    assert_eq!(db.stats().memtable_entries, 2);
    assert_eq!(db.get(b"b").unwrap(), None);
    assert_eq!(db.scan(b"", b"z").unwrap(), expected);
    db.flush().unwrap();
    drop(db);
    let mut db = Engine::open(dir.path()).unwrap();
    assert_eq!(db.stats().memtable_entries, 0);
    assert_eq!(db.scan(b"", b"z").unwrap(), expected);
}

#[test]
fn exclusive_lock_is_released_on_drop() {
    let dir = tempdir().unwrap();
    let db = Engine::open(dir.path()).unwrap();
    assert!(matches!(Engine::open(dir.path()), Err(Error::Locked)));
    drop(db);
    Engine::open(dir.path()).unwrap();
}

#[test]
fn limits_are_checked_without_poisoning_engine() {
    let dir = tempdir().unwrap();
    let mut db = Engine::open(dir.path()).unwrap();
    assert!(matches!(
        db.put(&vec![1; MAX_KEY_SIZE + 1], b""),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        db.put(b"k", &vec![1; MAX_VALUE_SIZE + 1]),
        Err(Error::InvalidInput(_))
    ));
    db.put(&vec![1; MAX_KEY_SIZE], b"").unwrap();
    db.put(b"large", &vec![2; MAX_VALUE_SIZE]).unwrap();
    db.flush().unwrap();
    assert_eq!(db.get(b"large").unwrap().unwrap().len(), MAX_VALUE_SIZE);
    assert_eq!(db.stats().sequence, 2);
}
