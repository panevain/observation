//! Walking a cache directory: what is in it, and evicting the oldest entries.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::Kind;

/// A cached entry found by [`scan`].
struct Entry {
    /// The `.obsb` or `.png`.
    path: PathBuf,
    len: u64,
    /// 0 if the entry has no sidecar or it is missing.
    sidecar_len: u64,
    modified: SystemTime,
}

/// What is in a store's directory: entries, sidecars without their main file,
/// and the size of both, and temp files left by interrupted writes (not
/// counted). Other files are neither listed nor counted.
#[derive(Default)]
pub(super) struct Scan {
    entries: Vec<Entry>,
    /// `(path, length)` of the sidecars that have no main file.
    orphans: Vec<(PathBuf, u64)>,
    /// `.tmp-*` files older than [`STALE_TEMP_AGE`].
    stale_temps: Vec<PathBuf>,
    pub(super) total: u64,
}

/// A temp file this old belongs to a write that was interrupted (a write
/// renames its temp file as soon as it is written).
pub(super) const STALE_TEMP_AGE: Duration = Duration::from_secs(3600);

/// Notes `path` in `scan` if it is a stale temp file at `now`.
fn note_temp(path: &Path, m: &std::fs::Metadata, scan: &mut Scan, now: SystemTime) {
    let is_temp = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(".tmp-"));
    let age = m.modified().ok().and_then(|t| now.duration_since(t).ok());
    if is_temp && age.is_some_and(|a| a > STALE_TEMP_AGE) {
        scan.stale_temps.push(path.to_path_buf());
    }
}

/// Walks `dir` (nothing is found if it does not exist): `bundles/` is flat,
/// `tiles/` is walked recursively. Symlinks are skipped. Temp files are
/// stale by their age at `now`.
pub(super) fn scan(dir: &Path, kind: Kind, now: SystemTime) -> Scan {
    let mut scan = Scan::default();
    match kind {
        Kind::Bundles => scan_bundles(dir, &mut scan, now),
        Kind::Tiles => scan_tiles(dir, &mut scan, now),
    }
    let entries: u64 = scan.entries.iter().map(|e| e.len + e.sidecar_len).sum();
    scan.total = entries + scan.orphans.iter().map(|o| o.1).sum::<u64>();
    scan
}

fn entry(path: PathBuf, m: &std::fs::Metadata, sidecar_len: u64) -> Entry {
    Entry {
        path,
        len: m.len(),
        sidecar_len,
        modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    }
}

/// `bundles/` holds only `<key>.obsb` and temp files; nothing below it is read.
fn scan_bundles(dir: &Path, scan: &mut Scan, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        // `DirEntry::metadata` does not follow symlinks.
        let (path, Ok(m)) = (e.path(), e.metadata()) else {
            continue;
        };
        if !m.is_file() {
            continue;
        }
        if path.extension().and_then(|x| x.to_str()) == Some("obsb") {
            scan.entries.push(entry(path, &m, 0));
        } else {
            note_temp(&path, &m, scan, now);
        }
    }
}

fn scan_tiles(dir: &Path, scan: &mut Scan, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut pngs = Vec::new();
    // Keyed by the `.png` path they belong to.
    let mut sidecars = HashMap::new();
    for e in entries.flatten() {
        let (path, Ok(m)) = (e.path(), e.metadata()) else {
            continue;
        };
        if m.is_dir() {
            scan_tiles(&path, scan, now);
        } else if m.is_file() {
            match path.extension().and_then(|x| x.to_str()) {
                Some("png") => pngs.push((path, m)),
                Some("json") => {
                    sidecars.insert(path.with_extension("png"), (path, m.len()));
                }
                _ => note_temp(&path, &m, scan, now),
            }
        }
    }
    for (png, m) in pngs {
        let json_len = sidecars.remove(&png).map_or(0, |s| s.1);
        scan.entries.push(entry(png, &m, json_len));
    }
    scan.orphans.extend(sidecars.into_values());
}

/// Result of [`evict_oldest`].
pub(super) struct Evicted {
    /// Size of what is left, as scanned.
    pub(super) total: u64,
    pub(super) entries: u64,
    pub(super) bytes: u64,
    /// Stale temp files removed.
    pub(super) temps: u64,
}

/// Scans `dir` at `now`, deletes stale temp files and sidecars without a main file,
/// then the oldest entries until at most `target` bytes are left. Never
/// deletes `keep` (a main file that is being written) or its sidecar, nor an
/// entry whose main file was rewritten after the scan.
pub(super) fn evict_oldest(
    dir: &Path,
    kind: Kind,
    keep: &Path,
    target: u64,
    now: SystemTime,
) -> Evicted {
    let scan = scan(dir, kind, now);
    let mut total = scan.total;
    let temps = scan.stale_temps.iter().filter(|t| remove_file(t)).count() as u64;
    let keep_sidecar = kind.sidecar(keep);
    for (path, len) in scan
        .orphans
        .iter()
        .filter(|o| Some(&o.0) != keep_sidecar.as_ref())
    {
        if remove_file(path) {
            total = total.saturating_sub(*len);
        }
    }
    let mut entries = scan.entries;
    entries.sort_unstable_by(|a, b| (a.modified, &a.path).cmp(&(b.modified, &b.path)));
    let mut count = 0;
    for e in entries.iter().filter(|e| e.path != keep) {
        if total <= target {
            break;
        }
        // A write may have replaced the file since the scan; that entry is new.
        let unchanged = std::fs::symlink_metadata(&e.path)
            .is_ok_and(|m| m.len() == e.len && m.modified().ok() == Some(e.modified));
        if !unchanged {
            continue;
        }
        // Sidecar first: an entry left without one is only an orphan main
        // file, which the next eviction still finds.
        if let Some(sidecar) = kind.sidecar(&e.path)
            && remove_file(&sidecar)
        {
            total = total.saturating_sub(e.sidecar_len);
        }
        if remove_file(&e.path) {
            total = total.saturating_sub(e.len);
            count += 1;
        }
    }
    Evicted {
        total,
        entries: count,
        bytes: scan.total.saturating_sub(total),
        temps,
    }
}

/// Removes a file; true if it is gone afterwards.
fn remove_file(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!(error = %e, "could not delete a cached file");
            false
        }
    }
}
