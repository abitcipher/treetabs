//! A store kept in memory, for a caller that saves it somewhere else
//! itself: the extension without the companion app keeps it in the
//! browser's extension storage.
//!
//! Same design as the file [`Store`](crate::store::Store): a snapshot plus
//! journal entries. Every applied batch adds entries to write, and a new
//! snapshot is due every `COMPACT_EVERY` entries. The caller takes them
//! with [`MemStore::take_writes`], writes them in order, and on load gives
//! them back with [`MemStore::open`] and [`MemStore::replay`].

use crate::model::{EditError, Tree};
use crate::ops::{Op, now_ms};
use crate::store::{COMPACT_EVERY, JournalEntry, Replay, StoreError, TreeStore};

pub struct MemStore {
    tree: Tree,
    seq: u64,
    since_snapshot: u64,
    /// `journal_seq` of the last snapshot handed out for writing.
    base: u64,
    entries: Vec<JournalEntry>,
    snapshot_due: bool,
    /// Problems found while loading.
    pub warnings: Vec<String>,
}

/// What a [`MemStore`] needs saved, in this order.
#[derive(Debug, Default)]
pub struct Writes {
    /// New journal entries, oldest first.
    pub entries: Vec<JournalEntry>,
    /// A new snapshot (portable JSON with `journal_seq`), which covers
    /// every entry so far.
    pub snapshot: Option<String>,
    /// Once the snapshot is written, the journal entries with these
    /// numbers are no longer needed (first, last; both included).
    pub drop_journal: Option<(u64, u64)>,
}

impl Writes {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.snapshot.is_none()
    }
}

impl MemStore {
    /// Start from a saved snapshot, or from an empty tree when there is
    /// none. Then give it the journal entries after
    /// [`MemStore::next_seq`] with [`MemStore::replay`].
    ///
    /// # Errors
    ///
    /// When the snapshot is not a tree.
    pub fn open(snapshot: Option<&str>) -> Result<MemStore, StoreError> {
        let (mut tree, snapshot_due) = match snapshot {
            Some(json) => (serde_json::from_str::<Tree>(json)?, false),
            // Saved right away: journal entries refer to its root id.
            None => (
                Tree {
                    journal_seq: Some(0),
                    ..Default::default()
                },
                true,
            ),
        };
        let mut warnings = Vec::new();
        let fixed = tree.fix_ids();
        if fixed > 0 {
            warnings.push(format!(
                "{fixed} duplicate node id(s) in snapshot were renumbered"
            ));
        }
        let base = tree.journal_seq.unwrap_or(0);
        Ok(MemStore {
            tree,
            seq: base,
            since_snapshot: 0,
            base,
            entries: Vec::new(),
            snapshot_due,
            warnings,
        })
    }

    /// Number of the journal entry to load next.
    pub fn next_seq(&self) -> u64 {
        self.seq + 1
    }

    /// Apply one saved journal entry while loading. Returns false when the
    /// entry is damaged: loading stops there.
    pub fn replay(&mut self, entry_json: &str) -> bool {
        let mut replay = Replay::new(&self.tree, std::mem::take(&mut self.warnings));
        replay.seq = self.seq;
        replay.since = self.since_snapshot;
        let ok = match serde_json::from_str::<JournalEntry>(entry_json) {
            Ok(entry) => {
                replay.entry(&mut self.tree, &entry);
                true
            }
            Err(e) => {
                replay.damaged(&e);
                false
            }
        };
        self.seq = replay.seq;
        self.since_snapshot = replay.since;
        self.warnings = replay.warnings;
        if self.since_snapshot >= COMPACT_EVERY {
            self.snapshot_due = true;
        }
        ok
    }

    /// Everything to save since the last call.
    pub fn take_writes(&mut self) -> Writes {
        let entries = std::mem::take(&mut self.entries);
        if !self.snapshot_due {
            return Writes {
                entries,
                ..Writes::default()
            };
        }
        self.snapshot_due = false;
        self.tree.journal_seq = Some(self.seq);
        self.tree.saved_at = Some(now_ms());
        let snapshot = match serde_json::to_string(&self.tree) {
            Ok(s) => s,
            Err(e) => {
                // Not expected for trees; keep the journal then.
                log::error!("cannot write a snapshot: {e}");
                return Writes {
                    entries,
                    ..Writes::default()
                };
            }
        };
        let drop_journal = (self.seq > self.base).then_some((self.base + 1, self.seq));
        self.base = self.seq;
        self.since_snapshot = 0;
        // The snapshot covers the entries not yet written.
        Writes {
            entries: Vec::new(),
            snapshot: Some(snapshot),
            drop_journal,
        }
    }

    fn rollback(&mut self, inverses: &[Option<Op>]) {
        for inv in inverses.iter().rev().flatten() {
            let _ = self.tree.apply(inv);
        }
    }
}

impl crate::sync::Apply for MemStore {
    fn tree(&self) -> &Tree {
        &self.tree
    }
    fn apply_op(&mut self, op: Op) -> Result<(), EditError> {
        TreeStore::apply(self, op).map(|_| ()).map_err(|e| match e {
            StoreError::Edit(e) => e,
            other => EditError::Io(other.to_string()),
        })
    }
}

impl TreeStore for MemStore {
    fn seq(&self) -> u64 {
        self.seq
    }

    fn apply_batch(&mut self, ops: Vec<Op>) -> Result<Vec<Option<Op>>, StoreError> {
        let mut inverses = Vec::with_capacity(ops.len());
        for op in &ops {
            match self.tree.apply(op) {
                Ok(inv) => inverses.push(inv),
                Err(e) => {
                    self.rollback(&inverses);
                    return Err(e.into());
                }
            }
        }
        let at = now_ms();
        for op in ops {
            self.seq += 1;
            self.entries.push(JournalEntry {
                seq: self.seq,
                at,
                op,
            });
        }
        self.since_snapshot += inverses.len() as u64;
        if self.since_snapshot >= COMPACT_EVERY {
            self.snapshot_due = true;
        }
        Ok(inverses)
    }

    fn adopt(&mut self, mut tree: Tree) -> Result<(), StoreError> {
        tree.fix_ids();
        self.tree = tree;
        self.snapshot_due = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, NodeKind};

    fn tab(t: &str) -> Node {
        let mut n = Node::new(NodeKind::SavedTab);
        n.title = Some(t.into());
        n
    }

    /// Writes kept the way the extension keeps them: a snapshot and
    /// journal entries by number.
    #[derive(Default)]
    struct Saved {
        snapshot: Option<String>,
        journal: std::collections::BTreeMap<u64, String>,
    }

    impl Saved {
        fn write(&mut self, w: Writes) {
            for e in w.entries {
                self.journal
                    .insert(e.seq, serde_json::to_string(&e).unwrap());
            }
            if let Some(s) = w.snapshot {
                self.snapshot = Some(s);
            }
            if let Some((a, b)) = w.drop_journal {
                for k in a..=b {
                    self.journal.remove(&k);
                }
            }
        }

        fn load(&self) -> MemStore {
            let mut s = MemStore::open(self.snapshot.as_deref()).unwrap();
            while let Some(e) = self.journal.get(&s.next_seq()) {
                assert!(s.replay(e));
            }
            s
        }
    }

    #[test]
    fn saved_writes_load_back() {
        let mut saved = Saved::default();
        let mut s = MemStore::open(None).unwrap();
        let root = s.tree.root.id;
        saved.write(s.take_writes());
        assert!(saved.snapshot.is_some(), "an empty tree is saved first");
        s.apply_batch(vec![
            Op::Insert {
                parent: root,
                index: 0,
                node: Box::new(tab("a")),
            },
            Op::Insert {
                parent: root,
                index: 1,
                node: Box::new(tab("b")),
            },
        ])
        .unwrap();
        saved.write(s.take_writes());
        assert_eq!(saved.journal.len(), 2);
        let back = saved.load();
        assert_eq!(back.seq(), 2);
        assert_eq!(back.tree.root.children.len(), 2);
        assert_eq!(back.tree.root.id, root);
    }

    #[test]
    fn snapshot_replaces_the_journal() {
        let mut saved = Saved::default();
        let mut s = MemStore::open(None).unwrap();
        let root = s.tree.root.id;
        for i in 0..COMPACT_EVERY + 5 {
            let n = usize::try_from(i).unwrap();
            s.apply(Op::Insert {
                parent: root,
                index: n,
                node: Box::new(tab("x")),
            })
            .unwrap();
            saved.write(s.take_writes());
        }
        assert!(saved.journal.len() < 10, "{}", saved.journal.len());
        let back = saved.load();
        assert_eq!(back.seq(), COMPACT_EVERY + 5);
        assert_eq!(back.tree.root.children.len(), 2005);
    }

    #[test]
    fn failed_batch_changes_nothing() {
        let mut s = MemStore::open(None).unwrap();
        let _ = s.take_writes();
        let root = s.tree.root.id;
        let a = tab("a");
        let bad = Op::Move {
            id: a.id,
            parent: root,
            index: 9,
        };
        let r = s.apply_batch(vec![
            Op::Insert {
                parent: root,
                index: 0,
                node: Box::new(a),
            },
            bad,
        ]);
        assert!(r.is_err());
        assert!(s.tree.root.children.is_empty());
        assert!(s.take_writes().is_empty());
        assert!(s.is_new());
    }

    #[test]
    fn adopt_saves_a_snapshot() {
        let mut s = MemStore::open(None).unwrap();
        let _ = s.take_writes();
        let mut t = Tree::default();
        t.root.children.push(tab("kept"));
        s.adopt(t).unwrap();
        let w = s.take_writes();
        let snap: Tree = serde_json::from_str(&w.snapshot.unwrap()).unwrap();
        assert_eq!(snap.root.children.len(), 1);
        assert!(!s.is_new());
    }
}
