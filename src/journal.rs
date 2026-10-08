use crate::{Result, error::corrupt, faults};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub(crate) const HEADER_SIZE: u64 = 12;
const VERSION: u32 = 1;

pub(crate) struct Journal {
    file: File,
    label: &'static str,
}

impl Journal {
    pub fn create(path: &Path, magic: &[u8; 8], label: &'static str) -> Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(magic)?;
        file.write_all(&VERSION.to_le_bytes())?;
        file.sync_all()?;
        let _ = label;
        Ok(())
    }

    pub fn open(path: &Path, magic: &[u8; 8], label: &'static str) -> Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut header = [0u8; HEADER_SIZE as usize];
        match file.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(corrupt(format!("{label}: incomplete header")));
            }
            Err(e) => return Err(e.into()),
        }
        if &header[..8] != magic || u32::from_le_bytes(header[8..].try_into().unwrap()) != VERSION {
            return Err(corrupt(format!(
                "{label}: missing/invalid header or unsupported version"
            )));
        }
        Ok(Self { file, label })
    }

    // Validates complete frames before handing payloads to the typed decoder.
    // Returns a repair boundary, without modifying disk during validation.
    pub fn replay(
        &mut self,
        max: usize,
        mut visit: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<u64> {
        let end = self.file.metadata()?.len();
        self.file.seek(SeekFrom::Start(HEADER_SIZE))?;
        let mut offset = HEADER_SIZE;
        while offset < end {
            if end - offset < 8 {
                break;
            }
            let mut length = [0; 4];
            self.file.read_exact(&mut length)?;
            let mut length_checksum = [0; 4];
            self.file.read_exact(&mut length_checksum)?;
            if crc32fast::hash(&length) != u32::from_le_bytes(length_checksum) {
                return Err(corrupt(format!(
                    "{}: frame length checksum mismatch at {offset}",
                    self.label
                )));
            }
            let size = u32::from_le_bytes(length) as usize;
            if size == 0 || size > max {
                return Err(corrupt(format!(
                    "{}: invalid frame length at {offset}",
                    self.label
                )));
            }
            let frame_size = size as u64 + 12;
            if end - offset < frame_size {
                break;
            }
            let mut payload = vec![0; size];
            self.file.read_exact(&mut payload)?;
            let mut checksum = [0; 4];
            self.file.read_exact(&mut checksum)?;
            let mut hash = crc32fast::Hasher::new();
            hash.update(&length);
            hash.update(&payload);
            if hash.finalize() != u32::from_le_bytes(checksum) {
                return Err(corrupt(format!(
                    "{}: checksum mismatch at {offset}",
                    self.label
                )));
            }
            visit(&payload)?;
            offset += frame_size;
        }
        Ok(offset)
    }

    pub fn repair(&mut self, boundary: u64) -> Result<()> {
        if self.file.metadata()?.len() != boundary {
            self.file.set_len(boundary)?;
            self.file.sync_all()?;
        }
        self.file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    pub fn append(&mut self, payload: &[u8]) -> Result<()> {
        self.file.seek(SeekFrom::End(0))?;
        write_frame(&mut self.file, payload, self.label)?;
        faults::hit(if self.label == "WAL" {
            "wal_before_sync"
        } else {
            "manifest_before_sync"
        })?;
        self.file.sync_all()?;
        faults::hit(if self.label == "WAL" {
            "wal_after_sync"
        } else {
            "manifest_after_sync"
        })?;
        Ok(())
    }
}

fn write_frame(writer: &mut impl Write, payload: &[u8], label: &str) -> Result<()> {
    let length = (payload.len() as u32).to_le_bytes();
    writer.write_all(&length)?;
    faults::hit(if label == "WAL" {
        "wal_after_length"
    } else {
        "manifest_after_length"
    })?;
    writer.write_all(&crc32fast::hash(&length).to_le_bytes())?;
    let mut hash = crc32fast::Hasher::new();
    hash.update(&length);
    hash.update(payload);
    writer.write_all(payload)?;
    writer.write_all(&hash.finalize().to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    struct ShortWriter {
        bytes: Vec<u8>,
        fail_after: usize,
    }
    impl Write for ShortWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if self.bytes.len() >= self.fail_after {
                return Err(io::Error::other("disk full"));
            }
            let count = data.len().min(2).min(self.fail_after - self.bytes.len());
            self.bytes.extend_from_slice(&data[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn frame_writer_completes_short_writes_and_propagates_partial_failures() {
        let payload = b"test payload";
        let mut writer = ShortWriter {
            bytes: Vec::new(),
            fail_after: usize::MAX,
        };
        write_frame(&mut writer, payload, "WAL").unwrap();
        assert_eq!(&writer.bytes[8..8 + payload.len()], payload);
        let end = writer.bytes.len();
        let mut hash = crc32fast::Hasher::new();
        hash.update(&writer.bytes[..4]);
        hash.update(payload);
        assert_eq!(
            u32::from_le_bytes(writer.bytes[end - 4..].try_into().unwrap()),
            hash.finalize()
        );
        for cut in 0..end {
            let mut writer = ShortWriter {
                bytes: Vec::new(),
                fail_after: cut,
            };
            assert!(matches!(
                write_frame(&mut writer, payload, "WAL"),
                Err(crate::Error::Io(_))
            ));
            assert_eq!(writer.bytes.len(), cut);
        }
    }
}
