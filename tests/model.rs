use jev_storage::Engine;
use std::collections::BTreeMap;
use tempfile::tempdir;

fn next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 16
}

#[test]
fn seeded_operations_match_reference_model_across_flushes_and_restarts() {
    for seed in 1..=12 {
        let dir = tempdir().unwrap();
        let mut db = Engine::open(dir.path()).unwrap();
        let mut model = BTreeMap::<Vec<u8>, Vec<u8>>::new();
        let mut rng = seed;
        let mut sequence = 0;
        for step in 0..300 {
            let key = vec![(next(&mut rng) % 32) as u8];
            match next(&mut rng) % 8 {
                0..=2 => {
                    let value = next(&mut rng).to_le_bytes().to_vec();
                    db.put(&key, &value).unwrap();
                    model.insert(key, value);
                    sequence += 1;
                }
                3 => {
                    db.delete(&key).unwrap();
                    model.remove(&key);
                    sequence += 1;
                }
                4 => assert_eq!(
                    db.get(&key).unwrap(),
                    model.get(&key).cloned(),
                    "seed {seed}, step {step}"
                ),
                5 => db.flush().unwrap(),
                6 => {
                    drop(db);
                    db = Engine::open(dir.path()).unwrap();
                }
                _ => {
                    let other = vec![(next(&mut rng) % 33) as u8];
                    let (start, end) = if key <= other {
                        (key, other)
                    } else {
                        (other, key)
                    };
                    let expected: Vec<_> = model
                        .range(start.clone()..end.clone())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    assert_eq!(
                        db.scan(&start, &end).unwrap(),
                        expected,
                        "seed {seed}, step {step}"
                    );
                }
            }
            assert_eq!(db.stats().sequence, sequence);
        }
        db.flush().unwrap();
        drop(db);
        let mut db = Engine::open(dir.path()).unwrap();
        assert_eq!(
            db.scan(&[], &[255]).unwrap(),
            model.into_iter().collect::<Vec<_>>()
        );
        assert_eq!(db.stats().memtable_entries, 0);
    }
}
