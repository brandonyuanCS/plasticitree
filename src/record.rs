use crate::{Error, Result, error::corrupt};

pub const MAX_KEY_SIZE: usize = 64 * 1024;
pub const MAX_VALUE_SIZE: usize = 16 * 1024 * 1024;
pub(crate) const RECORD_HEADER: usize = 17;
pub(crate) const MAX_RECORD_SIZE: usize = RECORD_HEADER + MAX_KEY_SIZE + MAX_VALUE_SIZE;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub seq: u64,
    // None is a tombstone; Some(empty) is a live empty value.
    pub value: Option<Vec<u8>>,
}

pub(crate) fn validate(key: &[u8], value: Option<&[u8]>) -> Result<()> {
    if key.len() > MAX_KEY_SIZE || value.is_some_and(|v| v.len() > MAX_VALUE_SIZE) {
        return Err(Error::InvalidInput("key/value exceeds size limit".into()));
    }
    Ok(())
}

pub(crate) fn encode(key: &[u8], entry: &Entry) -> Vec<u8> {
    let value = entry.value.as_deref().unwrap_or_default();
    let mut bytes = Vec::with_capacity(RECORD_HEADER + key.len() + value.len());
    bytes.extend_from_slice(&entry.seq.to_le_bytes());
    bytes.push(u8::from(entry.value.is_some()));
    bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(value);
    bytes
}

pub(crate) fn decode(bytes: &[u8]) -> Result<(Vec<u8>, Entry)> {
    if bytes.len() < RECORD_HEADER {
        return Err(corrupt("short mutation record"));
    }
    let seq = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let tag = bytes[8];
    let k = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let v = u32::from_le_bytes(bytes[13..17].try_into().unwrap()) as usize;
    if seq == 0
        || tag > 1
        || k > MAX_KEY_SIZE
        || v > MAX_VALUE_SIZE
        || (tag == 0 && v != 0)
        || bytes.len() != RECORD_HEADER + k + v
    {
        return Err(corrupt("invalid mutation sequence, tag, or lengths"));
    }
    let key = bytes[RECORD_HEADER..RECORD_HEADER + k].to_vec();
    let value = (tag == 1).then(|| bytes[RECORD_HEADER + k..].to_vec());
    Ok((key, Entry { seq, value }))
}
