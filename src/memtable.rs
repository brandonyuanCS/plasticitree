use crate::record::Entry;
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Memtable {
    pub entries: BTreeMap<Vec<u8>, Entry>,
    pub bytes: usize,
}

impl Memtable {
    pub fn insert(&mut self, key: Vec<u8>, entry: Entry) {
        if let Some(old) = self.entries.get(&key) {
            self.bytes -= key.len() + old.value.as_ref().map_or(0, Vec::len) + 8;
        }
        self.bytes += key.len() + entry.value.as_ref().map_or(0, Vec::len) + 8;
        self.entries.insert(key, entry);
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}
