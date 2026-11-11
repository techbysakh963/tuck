//! Content-addressed files for data too large to keep in the history index.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct BlobStore {
    dir: PathBuf,
}

/// 64-bit FNV-1a. Not cryptographic; it only has to tell clipboard payloads apart.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The name `put` would give these bytes.
pub fn blob_name(bytes: &[u8], ext: &str) -> String {
    format!("{:016x}.{}", hash_bytes(bytes), ext)
}

impl BlobStore {
    pub fn open(dir: &Path) -> io::Result<Self> {
        crate::private::create_dir(dir)?;
        Ok(Self::at(dir))
    }

    /// Does not touch the disk; later writes report any error.
    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    pub fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Stores `bytes` and returns the blob name. Storing the same bytes twice is a no-op.
    pub fn put(&self, bytes: &[u8], ext: &str) -> io::Result<String> {
        let name = blob_name(bytes, ext);
        let path = self.dir.join(&name);
        if path.exists() {
            return Ok(name);
        }
        crate::private::create_dir(&self.dir)?;
        let tmp = self.dir.join(format!("{name}.tmp"));
        crate::private::write_file(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(name)
    }

    pub fn get(&self, name: &str) -> io::Result<Vec<u8>> {
        fs::read(self.safe_path(name)?)
    }

    pub fn remove(&self, name: &str) {
        if let Ok(path) = self.safe_path(name) {
            let _ = fs::remove_file(path);
        }
    }

    /// Deletes every blob not in `keep`; returns how many were removed.
    pub fn prune(&self, keep: &HashSet<String>) -> usize {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// Names come from the index file; refuse anything that is not a plain file name.
    fn safe_path(&self, name: &str) -> io::Result<PathBuf> {
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid blob name",
            ));
        }
        Ok(self.dir.join(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_is_idempotent_and_prune_removes_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BlobStore::open(tmp.path()).unwrap();
        let a = store.put(b"hello", "txt").unwrap();
        assert_eq!(a, store.put(b"hello", "txt").unwrap());
        let b = store.put(b"other", "png").unwrap();
        assert_eq!(store.get(&a).unwrap(), b"hello");
        let keep: HashSet<String> = [a.clone()].into();
        assert_eq!(store.prune(&keep), 1);
        assert!(store.get(&b).is_err());
        assert!(store.get(&a).is_ok());
    }

    #[test]
    fn rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BlobStore::open(tmp.path()).unwrap();
        assert!(store.get("../x").is_err());
    }
}
