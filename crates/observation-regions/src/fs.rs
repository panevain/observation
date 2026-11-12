//! Small file-system helpers.

use std::path::Path;

/// Writes `bytes` to a temporary file next to `path`, then renames it over.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let path = path.to_owned();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || write_atomic_blocking(&path, &bytes))
        .await
        .map_err(std::io::Error::other)?
}

/// Blocking form of [`write_atomic`].
pub fn write_atomic_blocking(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    // A unique name per write, so concurrent writers never share a temp file.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".tmp-{}-{n}", std::process::id()));
    // Synced before the rename, so a power loss leaves the old file or the new one.
    let result = (|| {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
