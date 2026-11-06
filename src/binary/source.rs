use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::AppResult;

/// A byte backing store for the hex viewer.
///
/// `File` only `seek`s and `read`s windows on demand — large files are never
/// fully loaded into memory. `Memory` is used for stdin and tests.
pub enum ByteSource {
    File { file: File, len: u64 },
    Memory { data: Vec<u8> },
}

impl ByteSource {
    /// Open a file on disk by path. The file is kept open for windowed reads.
    pub fn open(path: &Path) -> AppResult<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self::File { file, len })
    }

    /// Wrap an in-memory buffer (stdin, test fixture, …).
    pub fn from_bytes(data: Vec<u8>) -> Self {
        Self::Memory { data }
    }

    /// Total length in bytes.
    pub fn len(&self) -> u64 {
        match self {
            ByteSource::File { len, .. } => *len,
            ByteSource::Memory { data } => data.len() as u64,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read `len` bytes starting at `start`.
    ///
    /// Reads past EOF are silently truncated — the hex viewer's last row
    /// legitimately extends beyond the file end and must not error.
    pub fn read_range(&mut self, start: u64, len: usize) -> AppResult<Vec<u8>> {
        match self {
            ByteSource::File {
                file,
                len: file_len,
            } => {
                let total = *file_len;
                if start >= total {
                    return Ok(Vec::new());
                }
                let available = (total - start) as usize;
                let to_read = len.min(available);
                file.seek(SeekFrom::Start(start))?;
                let mut buf = vec![0u8; to_read];
                let mut filled = 0;
                while filled < to_read {
                    let n = file.read(&mut buf[filled..])?;
                    if n == 0 {
                        // Unexpected short read; truncate to what we got.
                        buf.truncate(filled);
                        break;
                    }
                    filled += n;
                }
                Ok(buf)
            }
            ByteSource::Memory { data } => {
                let total = data.len() as u64;
                if start >= total {
                    return Ok(Vec::new());
                }
                let start_u = start as usize;
                let end = (start_u + len).min(data.len());
                Ok(data[start_u..end].to_vec())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_len_and_read() {
        let mut s = ByteSource::from_bytes(vec![0, 1, 2, 3, 4]);
        assert_eq!(s.len(), 5);
        assert_eq!(s.read_range(0, 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(s.read_range(2, 10).unwrap(), vec![2, 3, 4]); // truncate at EOF
        assert_eq!(s.read_range(10, 5).unwrap(), Vec::<u8>::new()); // past EOF
        assert!(s.read_range(0, 0).unwrap().is_empty());
    }

    #[test]
    fn memory_is_empty() {
        assert!(ByteSource::from_bytes(Vec::new()).is_empty());
        assert!(!ByteSource::from_bytes(vec![1]).is_empty());
    }
}
