//! Copies of the tree to come back to: one plain JSON export per day in
//! `<store>/exports/`, plus the snapshots the store keeps in `backups/`
//! when it compacts.

use crate::Tree;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    /// Daily export (`exports/treetabs-YYYY-MM-DD.json`).
    Daily,
    /// Store snapshot from before a compaction (`backups/tree-<ms>.json`).
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupInfo {
    pub kind: BackupKind,
    /// File name, which is also its id for [`read`].
    pub name: String,
    /// When it was written (ms since the epoch).
    pub at: i64,
    pub bytes: u64,
}

fn dir_of(store: &Path, kind: BackupKind) -> PathBuf {
    store.join(match kind {
        BackupKind::Daily => "exports",
        BackupKind::Snapshot => "backups",
    })
}

/// `YYYY-MM-DD` (UTC) of a time in ms since the epoch.
pub fn date(ms: i64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Local date and time of a time in ms since the epoch: `YYYY-MM-DD HH:MM`.
pub fn local_time(ms: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
            t.format("%Y-%m-%d %H:%M").to_string()
        }
        chrono::LocalResult::None => date(ms),
    }
}

fn daily_path(store: &Path, now_ms: i64) -> PathBuf {
    // One file per local day.
    let day = local_time(now_ms)[..10].to_owned();
    dir_of(store, BackupKind::Daily).join(format!("treetabs-{day}.json"))
}

/// Write today's export unless it exists. Keeps the newest `keep` exports
/// (0 turns exports off). Returns the path written, if any.
///
/// # Errors
///
/// When the export folder or file cannot be written, or old copies cannot be removed.
pub fn export_daily(
    store: &Path,
    tree: &Tree,
    keep: usize,
    now_ms: i64,
) -> io::Result<Option<PathBuf>> {
    if keep == 0 || daily_path(store, now_ms).exists() {
        return Ok(None);
    }
    export_now(store, tree, keep, now_ms).map(Some)
}

/// Write (or overwrite) today's export now.
///
/// # Errors
///
/// When the export folder or file cannot be written, or old copies cannot be removed.
pub fn export_now(store: &Path, tree: &Tree, keep: usize, now_ms: i64) -> io::Result<PathBuf> {
    let path = daily_path(store, now_ms);
    fs::create_dir_all(path.parent().expect("has a parent"))?;
    crate::save_portable(tree, &path, crate::JsonStyle::Compact)?;
    prune(store, keep.max(1))?;
    Ok(path)
}

/// Save `tree` among the snapshots (`backups/<name>`), where [`list`] and
/// [`read`] find it. Pruning leaves it alone.
///
/// # Errors
///
/// When the name is not a plain `.json` file name, or the file cannot be written.
pub fn save_copy(store: &Path, tree: &Tree, name: &str) -> io::Result<PathBuf> {
    if name.contains(['/', '\\']) || name.starts_with('.') || !name.ends_with(".json") {
        return Err(io::Error::other(format!("bad backup name: {name}")));
    }
    let dir = dir_of(store, BackupKind::Snapshot);
    fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    crate::save_portable(tree, &path, crate::JsonStyle::Compact)?;
    Ok(path)
}

fn prune(store: &Path, keep: usize) -> io::Result<()> {
    let mut names: Vec<String> = list(store)
        .into_iter()
        .filter(|b| b.kind == BackupKind::Daily)
        .map(|b| b.name)
        .collect();
    // Names sort by date; newest first.
    names.sort_unstable_by(|a, b| b.cmp(a));
    for old in names.iter().skip(keep) {
        fs::remove_file(dir_of(store, BackupKind::Daily).join(old))?;
    }
    Ok(())
}

/// All copies, newest first.
pub fn list(store: &Path) -> Vec<BackupInfo> {
    let mut out = Vec::new();
    for kind in [BackupKind::Daily, BackupKind::Snapshot] {
        let Ok(entries) = fs::read_dir(dir_of(store, kind)) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let at = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            out.push(BackupInfo {
                kind,
                name,
                at,
                bytes: meta.len(),
            });
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.at));
    out
}

/// Why a backup could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadBackupError {
    /// Not a plain file name (has a path separator or starts with a dot).
    #[error("bad backup name: {0}")]
    BadName(String),
    /// No daily copy or snapshot has this name.
    #[error("no backup named {0}")]
    NotFound(String),
    /// The file exists but does not load.
    #[error("backup {name}: {source}")]
    Load {
        name: String,
        #[source]
        source: crate::LoadError,
    },
}

/// Load one copy by its name from [`list`].
///
/// # Errors
///
/// [`ReadBackupError::BadName`] for names that could leave the backup
/// folders, [`ReadBackupError::NotFound`] when there is no such copy, and
/// [`ReadBackupError::Load`] when the file does not load.
pub fn read(store: &Path, name: &str) -> Result<Tree, ReadBackupError> {
    if name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(ReadBackupError::BadName(name.to_owned()));
    }
    let path = [BackupKind::Daily, BackupKind::Snapshot]
        .into_iter()
        .map(|k| dir_of(store, k).join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| ReadBackupError::NotFound(name.to_owned()))?;
    crate::load(&path)
        .map(|l| l.tree)
        .map_err(|source| ReadBackupError::Load {
            name: name.to_owned(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400_000), "2000-02-29");
        assert_eq!(date(1_792_890_000_000), "2026-10-25");
    }

    #[test]
    fn one_export_per_day_and_old_ones_go() {
        let dir = std::env::temp_dir().join(format!("treetabs-backups-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tree = Tree::default();
        let day = 86_400_000;
        let t0 = 1_792_890_000_000;
        assert!(export_daily(&dir, &tree, 3, t0).unwrap().is_some());
        assert!(
            export_daily(&dir, &tree, 3, t0 + 1000).unwrap().is_none(),
            "same day"
        );
        for i in 1..5 {
            export_daily(&dir, &tree, 3, t0 + i * day).unwrap();
        }
        let mut names: Vec<_> = list(&dir).into_iter().map(|b| b.name).collect();
        names.sort();
        let name = |i: i64| format!("treetabs-{}.json", &local_time(t0 + i * day)[..10]);
        assert_eq!(names, [name(2), name(3), name(4)]);
        assert!(read(&dir, &name(3)).is_ok());
        assert!(matches!(
            read(&dir, "../tree.json"),
            Err(ReadBackupError::BadName(_))
        ));
        assert!(matches!(
            read(&dir, "treetabs-1999-01-01.json"),
            Err(ReadBackupError::NotFound(_))
        ));
        assert_eq!(
            export_daily(&dir, &tree, 0, t0 + 9 * day).unwrap(),
            None,
            "0 = off"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
