//! Small filesystem helpers for the on-disk stores (settings, address book).

use std::path::{Path, PathBuf};

/// Write `bytes` to `path` atomically: write a sibling temp file, flush it,
/// then rename over the target. A crash or power loss mid-save leaves the old
/// file intact instead of a truncated one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Don't leave the temp file behind on failure.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Move an unreadable store file aside to `<name>.bak` so the next save cannot
/// destroy it, returning the backup path if the rename succeeded.
pub fn quarantine(path: &Path) -> Option<PathBuf> {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    let bak = PathBuf::from(bak);
    std::fs::rename(path, &bak).ok().map(|_| bak)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("emberviewer-fsutil-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.json");
        write_atomic(&path, b"one").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quarantine_moves_file_aside() {
        let dir = std::env::temp_dir().join(format!("emberviewer-fsutil-q-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.json");
        std::fs::write(&path, b"corrupt").unwrap();
        let bak = quarantine(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&bak).unwrap(), b"corrupt");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
