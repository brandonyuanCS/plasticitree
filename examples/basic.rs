use jev_storage::{Engine, Result};

fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "demo-data".into());
    let mut db = Engine::open(&path)?;
    db.put(b"apple", b"red")?;
    db.put(b"banana", b"yellow")?;
    db.flush()?;
    db.put(b"apple", b"green")?;
    db.delete(b"banana")?;
    drop(db);

    let mut db = Engine::open(path)?;
    assert_eq!(db.get(b"apple")?, Some(b"green".to_vec()));
    assert_eq!(db.get(b"banana")?, None);
    println!("Recovered scan: {:?}", db.scan(b"a", b"z")?);
    println!("State: {:?}", db.stats());
    Ok(())
}
