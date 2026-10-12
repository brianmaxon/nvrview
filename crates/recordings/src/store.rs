//! Small file helpers: crash-safe writes and bounded JSON reads.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Largest JSON file read back (index day files and job states are far
/// smaller); a larger file is treated as corrupt.
pub const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;

/// Writes `bytes` to `path` so that a crash leaves either the old or the
/// new content: write a sibling temporary file, flush it to disk, rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Serializes `value` as JSON and writes it atomically.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_atomic(path, &bytes)
}

/// Reads JSON written by [`write_json`]; `Ok(None)` if the file is missing.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if meta.len() > MAX_JSON_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_json_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("a.json");
        assert_eq!(read_json::<Vec<u32>>(&p).unwrap(), None);
        write_json(&p, &vec![1u32, 2, 3]).unwrap();
        write_json(&p, &vec![4u32]).unwrap();
        assert_eq!(read_json::<Vec<u32>>(&p).unwrap(), Some(vec![4]));
        assert!(!dir.path().join("sub").join("a.json.tmp").exists());
        fs::write(&p, b"{not json").unwrap();
        assert!(read_json::<Vec<u32>>(&p).is_err());
    }
}
