//! Durable tree storage: a snapshot plus an append-only journal of ops.
//!
//! ```text
//! <dir>/tree.json       snapshot (portable format) with `journal_seq`
//! <dir>/journal.jsonl   one {"seq", "at", "op"} per line, appended + fsynced
//! <dir>/backups/        previous snapshots (newest kept)
//! <dir>/lock            held while a process owns the store
//! ```
//!
//! Every op is on disk before `apply` returns. On open, the journal is
//! replayed on top of the snapshot. Entries with `seq <= journal_seq` are
//! already in the snapshot and skipped, so a crash between writing a new
//! snapshot and truncating the journal is harmless. A damaged journal tail
//! (for example a half-written last line) is kept aside, never deleted.

use crate::model::Tree;
use crate::ops::{Op, now_ms};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Snapshot after this many journal entries.
const COMPACT_EVERY: u64 = 2000;
/// Number of old snapshots kept in `backups/`.
const KEEP_BACKUPS: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("store {0} is in use by another process")]
    Locked(PathBuf),
    #[error("store {0} already exists")]
    Exists(PathBuf),
    #[error(transparent)]
    Edit(#[from] crate::model::EditError),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: u64,
    pub at: i64,
    pub op: Op,
}

pub struct Store {
    dir: PathBuf,
    tree: Tree,
    journal: File,
    seq: u64,
    since_snapshot: u64,
    _lock: File,
    /// Problems found while opening (damaged journal, skipped ops).
    pub warnings: Vec<String>,
}

impl Store {
    /// Default store location: `$XDG_DATA_HOME/treetabs` or `~/.local/share/treetabs`.
    pub fn default_dir() -> PathBuf {
        if let Some(d) = std::env::var_os("TREETABS_DIR") {
            return PathBuf::from(d);
        }
        if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(d).join("treetabs");
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| ".".into());
        home.join(".local/share/treetabs")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Create a store from `tree`. Fails if the store already has a tree.
    pub fn create(dir: &Path, mut tree: Tree) -> Result<Store, StoreError> {
        fs::create_dir_all(dir)?;
        let lock = lock(dir)?;
        if dir.join("tree.json").exists() {
            return Err(StoreError::Exists(dir.to_owned()));
        }
        tree.fix_ids();
        tree.journal_seq = Some(0);
        write_atomic(&dir.join("tree.json"), &serde_json::to_vec(&tree)?)?;
        let _ = fs::remove_file(dir.join("journal.jsonl"));
        drop(lock);
        Store::open(dir)
    }

    /// Open (or create empty) the store in `dir`.
    pub fn open(dir: &Path) -> Result<Store, StoreError> {
        fs::create_dir_all(dir)?;
        let lock = lock(dir)?;
        let mut warnings = Vec::new();
        let snap = dir.join("tree.json");
        let mut tree: Tree = if snap.exists() {
            serde_json::from_slice(&fs::read(&snap)?)?
        } else {
            // Write the empty tree right away: journal ops refer to its root id.
            let t = Tree { journal_seq: Some(0), ..Default::default() };
            write_atomic(&snap, &serde_json::to_vec(&t)?)?;
            t
        };
        let fixed = tree.fix_ids();
        if fixed > 0 {
            warnings.push(format!("{fixed} duplicate node id(s) in snapshot were renumbered"));
        }
        let base = tree.journal_seq.unwrap_or(0);
        let mut seq = base;
        let mut since = 0;

        let jpath = dir.join("journal.jsonl");
        if jpath.exists() {
            let reader = BufReader::new(File::open(&jpath)?);
            let mut good_bytes = 0u64;
            let mut damaged = false;
            for line in reader.split(b'\n') {
                let line = line?;
                let len = line.len() as u64 + 1;
                if line.iter().all(u8::is_ascii_whitespace) {
                    good_bytes += len;
                    continue;
                }
                let entry: JournalEntry = match serde_json::from_slice(&line) {
                    Ok(e) => e,
                    Err(e) => {
                        warnings.push(format!("journal damaged after seq {seq}: {e}"));
                        damaged = true;
                        break;
                    }
                };
                good_bytes += len;
                if entry.seq <= base {
                    continue;
                }
                if let Err(e) = tree.apply(&entry.op) {
                    // Keep going: one bad op must not hide all later changes.
                    warnings.push(format!("journal seq {}: {} skipped: {e}", entry.seq, entry.op.name()));
                }
                seq = seq.max(entry.seq);
                since += 1;
            }
            if damaged {
                let keep = dir.join(format!("journal.damaged-{}.jsonl", now_ms()));
                fs::copy(&jpath, &keep)?;
                warnings.push(format!("damaged journal copied to {}", keep.display()));
                let f = OpenOptions::new().write(true).open(&jpath)?;
                f.set_len(good_bytes)?;
                f.sync_all()?;
            }
        }
        let journal = OpenOptions::new().create(true).append(true).open(&jpath)?;
        let mut store = Store {
            dir: dir.to_owned(),
            tree,
            journal,
            seq,
            since_snapshot: since,
            _lock: lock,
            warnings,
        };
        if store.since_snapshot >= COMPACT_EVERY {
            store.compact()?;
        }
        Ok(store)
    }

    /// Apply one op and write it to disk. Returns the inverse op.
    pub fn apply(&mut self, op: Op) -> Result<Option<Op>, StoreError> {
        let mut inv = self.apply_batch(vec![op])?;
        Ok(inv.pop().flatten())
    }

    /// Apply ops in order, all or nothing, with one fsync. Returns inverses
    /// in the same order.
    pub fn apply_batch(&mut self, ops: Vec<Op>) -> Result<Vec<Option<Op>>, StoreError> {
        let mut inverses = Vec::with_capacity(ops.len());
        for (i, op) in ops.iter().enumerate() {
            match self.tree.apply(op) {
                Ok(inv) => inverses.push(inv),
                Err(e) => {
                    self.rollback(&inverses);
                    log::debug!("op {i} ({}) rejected: {e}", op.name());
                    return Err(e.into());
                }
            }
        }
        let at = now_ms();
        let mut buf = Vec::new();
        for (k, op) in ops.into_iter().enumerate() {
            let entry = JournalEntry { seq: self.seq + 1 + k as u64, at, op };
            serde_json::to_writer(&mut buf, &entry)?;
            buf.push(b'\n');
        }
        let n = inverses.len() as u64;
        if let Err(e) = self.journal.write_all(&buf).and_then(|_| self.journal.sync_data()) {
            self.rollback(&inverses);
            return Err(e.into());
        }
        self.seq += n;
        self.since_snapshot += n;
        if self.since_snapshot >= COMPACT_EVERY {
            self.compact()?;
        }
        Ok(inverses)
    }

    fn rollback(&mut self, inverses: &[Option<Op>]) {
        for inv in inverses.iter().rev().flatten() {
            let _ = self.tree.apply(inv);
        }
    }

    /// Write a fresh snapshot, keep the old one as a backup, and empty the
    /// journal.
    pub fn compact(&mut self) -> Result<(), StoreError> {
        self.tree.journal_seq = Some(self.seq);
        self.tree.saved_at = Some(now_ms());
        let snap = self.dir.join("tree.json");
        if snap.exists() {
            let backups = self.dir.join("backups");
            fs::create_dir_all(&backups)?;
            fs::copy(&snap, backups.join(format!("tree-{}.json", now_ms())))?;
            prune_backups(&backups)?;
        }
        write_atomic(&snap, &serde_json::to_vec(&self.tree)?)?;
        // The snapshot now covers every journal entry.
        self.journal.set_len(0)?;
        self.journal.sync_all()?;
        self.since_snapshot = 0;
        Ok(())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.since_snapshot > 0 {
            let _ = self.compact();
        }
    }
}

fn lock(dir: &Path) -> Result<File, StoreError> {
    let f = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("lock"))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(StoreError::Locked(dir.to_owned())),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Write via a temp file + fsync + rename, so readers see old or new, never half.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent()
        && let Ok(d) = File::open(parent)
    {
        let _ = d.sync_all();
    }
    Ok(())
}

fn prune_backups(dir: &Path) -> std::io::Result<()> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("tree-")))
        .collect();
    files.sort();
    while files.len() > KEEP_BACKUPS {
        fs::remove_file(files.remove(0))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, NodeKind};

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("treetabs-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn tab(t: &str) -> Node {
        let mut n = Node::new(NodeKind::SavedTab);
        n.title = Some(t.into());
        n
    }

    /// Copy the store files while the store is still open: what a crash
    /// at this moment would leave on disk.
    fn crash_copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for f in ["tree.json", "journal.jsonl"] {
            if from.join(f).exists() {
                fs::copy(from.join(f), to.join(f)).unwrap();
            }
        }
    }

    #[test]
    fn journal_survives_crash_and_damage() {
        let live = tmpdir("live");
        let dir = tmpdir("crashed");
        let (a_id, root) = {
            let mut s = Store::open(&live).unwrap();
            let root = s.tree().root.id;
            let a = tab("a");
            let a_id = a.id;
            s.apply(Op::Insert { parent: root, index: 0, node: Box::new(a) }).unwrap();
            s.apply(Op::Insert { parent: root, index: 1, node: Box::new(tab("b")) }).unwrap();
            crash_copy(&live, &dir);
            (a_id, root)
        };
        // A half-written last line, as left by a crash mid-append.
        let mut j = OpenOptions::new().append(true).open(dir.join("journal.jsonl")).unwrap();
        j.write_all(b"{\"seq\":3,\"at\":1,\"op\":{\"op\":\"tra").unwrap();
        drop(j);

        let mut s = Store::open(&dir).unwrap();
        assert_eq!(s.tree().root.children.len(), 2);
        assert!(s.warnings.iter().any(|w| w.contains("damaged")), "{:?}", s.warnings);
        // New ops append cleanly after the cut.
        s.apply(Op::Trash { id: a_id, at: 1 }).unwrap();
        crash_copy(&dir, &dir.with_extension("again"));
        drop(s);
        let s = Store::open(&dir.with_extension("again")).unwrap();
        assert_eq!(s.tree().root.children.len(), 1);
        assert_eq!(s.tree().trash.len(), 1);
        assert_eq!(s.tree().root.id, root);
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        drop(s);
        for d in [live, dir.clone(), dir.with_extension("again")] {
            fs::remove_dir_all(&d).unwrap();
        }
    }

    #[test]
    fn second_open_is_locked() {
        let dir = tmpdir("lock");
        let _s = Store::open(&dir).unwrap();
        assert!(matches!(Store::open(&dir), Err(StoreError::Locked(_))));
        drop(_s);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_batch_is_not_written() {
        let dir = tmpdir("batch");
        let mut s = Store::open(&dir).unwrap();
        let root = s.tree().root.id;
        let a = tab("a");
        let bad = Op::Move { id: a.id, parent: root, index: 9 };
        let r = s.apply_batch(vec![Op::Insert { parent: root, index: 0, node: Box::new(a) }, bad]);
        assert!(r.is_err());
        assert!(s.tree().root.children.is_empty());
        assert_eq!(s.seq(), 0);
        drop(s);
        let s = Store::open(&dir).unwrap();
        assert!(s.tree().root.children.is_empty());
        drop(s);
        fs::remove_dir_all(&dir).unwrap();
    }
}
