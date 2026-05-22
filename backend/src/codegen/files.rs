//! Atomic per-file write for generated Rust source.
//!
//! Reuses the `tmp.<uuid> + sync_all + rename` pattern from S2's
//! `ProjectStore::write_json_atomic`. Same correctness guarantees:
//! no half-written file ever appears at the target path, and on rename
//! failure the tmp is cleaned up best-effort.
//!
//! Differs from the store's helper in two ways:
//! 1. The payload here is a `&str` of pretty-printed Rust source, not
//!    JSON. We write UTF-8 bytes directly.
//! 2. We `create_dir_all` for the parent — the generator creates new
//!    module subdirectories (`src/dto/`, `src/handlers/`, etc.) as
//!    templates emit into them.

use std::io;
use std::path::Path;

use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const TMP_SUFFIX: &str = ".tmp";

/// Write `contents` to `target` atomically. Creates parent directories as
/// needed (`create_dir_all`). Returns `Ok(true)` if the file changed,
/// `Ok(false)` if the contents are byte-identical to what was already
/// there (skipped — preserves mtime, avoids spurious rebuild triggers).
///
/// The "did anything change" return value lets the generator report
/// only files that actually moved on disk in this regen.
pub async fn write_atomic_if_changed(target: &Path, contents: &str) -> io::Result<bool> {
    // Skip the write if existing contents already match. Cheap-on-disk
    // optimisation that matters for two reasons:
    //   - Avoids retriggering `cargo`'s mtime-based incremental rebuild
    //     when nothing in the emission changed.
    //   - Makes "regen twice in a row produces no diff" trivially true,
    //     which is what users expect.
    if let Ok(existing) = fs::read(target).await {
        if existing == contents.as_bytes() {
            return Ok(false);
        }
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).await?;
    }

    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
    let tmp_name = format!(
        "{}{}{}",
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("emit"),
        TMP_SUFFIX,
        Uuid::new_v4()
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path).await?;
        f.write_all(contents.as_bytes()).await?;
        f.flush().await?;
        f.sync_all().await?;
    }

    if let Err(err) = fs::rename(&tmp_path, target).await {
        let _ = fs::remove_file(&tmp_path).await;
        return Err(err);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_write_creates_file_and_parent_dirs() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("a/b/c/file.rs");
        let changed = write_atomic_if_changed(&target, "fn main() {}").await.unwrap();
        assert!(changed);
        assert!(target.is_file());
        let read = fs::read_to_string(&target).await.unwrap();
        assert_eq!(read, "fn main() {}");
    }

    #[tokio::test]
    async fn test_write_idempotent_when_contents_match() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("file.rs");
        assert!(write_atomic_if_changed(&target, "fn a() {}").await.unwrap());
        // Second write with identical contents reports unchanged.
        assert!(!write_atomic_if_changed(&target, "fn a() {}").await.unwrap());
        // Third write with different contents reports changed.
        assert!(write_atomic_if_changed(&target, "fn b() {}").await.unwrap());
    }

    #[tokio::test]
    async fn test_write_overwrites_existing() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("file.rs");
        write_atomic_if_changed(&target, "fn old() {}").await.unwrap();
        write_atomic_if_changed(&target, "fn new() {}").await.unwrap();
        let read = fs::read_to_string(&target).await.unwrap();
        assert_eq!(read, "fn new() {}");
    }
}
