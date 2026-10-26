//! The portable tree model.
//!
//! A [`Tree`] is a session root with nested [`Node`]s. The well-known fields
//! (title, url, custom title, …) are typed. Everything else the source format
//! carried is kept in `data` / `marks` / `extra`, so nothing is lost and the
//! tree can be written back to the Tabs Outliner format.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Name written into the `format` field of portable files.
pub const FORMAT_NAME: &str = "treetabs";
/// Current portable format version.
pub const FORMAT_VERSION: u32 = 1;

/// Node id, stable across saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl NodeId {
    pub fn fresh() -> Self {
        NodeId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Make sure later `fresh()` ids are greater than `id`.
    pub fn bump_above(id: NodeId) {
        NEXT_ID.fetch_max(id.0 + 1, Ordering::Relaxed);
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::fresh()
    }
}

/// What a node represents.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Session,
    /// An open browser window.
    Window,
    /// A closed (saved) window.
    SavedWindow,
    /// A window waiting to be restored.
    WaitingWindow,
    /// An open tab.
    Tab,
    /// A closed (saved) tab.
    SavedTab,
    /// A tab waiting to be restored.
    WaitingTab,
    AttachWaitingTab,
    Group,
    Note,
    Separator,
    /// Unknown node type, kept verbatim.
    Other(String),
}

impl NodeKind {
    /// Name used in the portable format.
    pub fn as_str(&self) -> &str {
        match self {
            NodeKind::Session => "session",
            NodeKind::Window => "window",
            NodeKind::SavedWindow => "saved_window",
            NodeKind::WaitingWindow => "waiting_window",
            NodeKind::Tab => "tab",
            NodeKind::SavedTab => "saved_tab",
            NodeKind::WaitingTab => "waiting_tab",
            NodeKind::AttachWaitingTab => "attach_waiting_tab",
            NodeKind::Group => "group",
            NodeKind::Note => "note",
            NodeKind::Separator => "separator",
            NodeKind::Other(s) => s,
        }
    }

    pub fn from_portable(s: &str) -> Self {
        match s {
            "session" => NodeKind::Session,
            "window" => NodeKind::Window,
            "saved_window" => NodeKind::SavedWindow,
            "waiting_window" => NodeKind::WaitingWindow,
            "tab" => NodeKind::Tab,
            "saved_tab" => NodeKind::SavedTab,
            "waiting_tab" => NodeKind::WaitingTab,
            "attach_waiting_tab" => NodeKind::AttachWaitingTab,
            "group" => NodeKind::Group,
            "note" => NodeKind::Note,
            "separator" => NodeKind::Separator,
            other => NodeKind::Other(other.to_owned()),
        }
    }

    /// Type string used by Tabs Outliner. Saved tabs have no `type` field
    /// there, which is why this returns `None` for them.
    pub fn to_tabs_outliner(&self) -> Option<&str> {
        Some(match self {
            NodeKind::Session => "session",
            NodeKind::Window => "win",
            NodeKind::SavedWindow => "savedwin",
            NodeKind::WaitingWindow => "waitingwin",
            NodeKind::Tab => "tab",
            NodeKind::SavedTab => return None,
            NodeKind::WaitingTab => "waitingtab",
            NodeKind::AttachWaitingTab => "attachwaitingtab",
            NodeKind::Group => "group",
            NodeKind::Note => "textnote",
            NodeKind::Separator => "separatorline",
            NodeKind::Other(s) => s,
        })
    }

    pub fn from_tabs_outliner(t: Option<&str>) -> Self {
        match t {
            None | Some("savedtab") => NodeKind::SavedTab,
            Some("session") => NodeKind::Session,
            Some("win") => NodeKind::Window,
            Some("savedwin") => NodeKind::SavedWindow,
            Some("waitingwin") => NodeKind::WaitingWindow,
            Some("tab") => NodeKind::Tab,
            Some("waitingtab") => NodeKind::WaitingTab,
            Some("attachwaitingtab") => NodeKind::AttachWaitingTab,
            Some("group") => NodeKind::Group,
            Some("textnote") => NodeKind::Note,
            Some("separatorline") => NodeKind::Separator,
            Some(other) => NodeKind::Other(other.to_owned()),
        }
    }

    pub fn is_window(&self) -> bool {
        matches!(
            self,
            NodeKind::Window | NodeKind::SavedWindow | NodeKind::WaitingWindow
        )
    }

    pub fn is_tab(&self) -> bool {
        matches!(
            self,
            NodeKind::Tab | NodeKind::SavedTab | NodeKind::WaitingTab | NodeKind::AttachWaitingTab
        )
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for NodeKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NodeKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(NodeKind::from_portable(&s))
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One node of the tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Stable id, unique within the tree.
    #[serde(default)]
    pub id: NodeId,
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    /// User-assigned title (Tabs Outliner `marks.customTitle`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_favicon: Option<String>,
    /// Text of a note node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub collapsed: bool,
    /// Remaining browser data (window rect, tab state, …).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub data: Map<String, Value>,
    /// Remaining Tabs Outliner marks.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub marks: Map<String, Value>,
    /// Unknown top-level node fields.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

impl Node {
    /// Turn open tabs and windows in this subtree into saved ones, without
    /// browser ids: for copies (imports, backups) whose tabs aren't live.
    pub fn make_saved(&mut self) {
        match self.kind {
            NodeKind::Tab => self.kind = NodeKind::SavedTab,
            NodeKind::Window => self.kind = NodeKind::SavedWindow,
            _ => {}
        }
        if matches!(self.kind, NodeKind::SavedTab | NodeKind::SavedWindow) {
            self.data.remove("id");
            self.data.remove("windowId");
        }
        for c in &mut self.children {
            c.make_saved();
        }
    }

    pub fn new(kind: NodeKind) -> Self {
        Node {
            id: NodeId::fresh(),
            kind,
            title: None,
            url: None,
            favicon: None,
            custom_title: None,
            custom_favicon: None,
            note: None,
            collapsed: false,
            data: Map::new(),
            marks: Map::new(),
            extra: Map::new(),
            children: Vec::new(),
        }
    }

    /// Text to show for this node.
    pub fn display_title(&self) -> String {
        if let Some(t) = self.custom_title.as_deref().filter(|t| !t.is_empty()) {
            return t.to_owned();
        }
        match &self.kind {
            NodeKind::Note => self.note.clone().unwrap_or_default(),
            NodeKind::Separator => "────────".to_owned(),
            NodeKind::Session => "Session".to_owned(),
            k if k.is_window() => "Window".to_owned(),
            _ => self
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .or_else(|| self.url.clone())
                .unwrap_or_else(|| self.kind.to_string()),
        }
    }

    /// Number of nodes in this subtree, including self.
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }

    /// Depth-first, pre-order walk.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Node, usize)) {
        fn go<'a>(n: &'a Node, depth: usize, f: &mut impl FnMut(&'a Node, usize)) {
            f(n, depth);
            for c in &n.children {
                go(c, depth + 1, f);
            }
        }
        go(self, 0, f)
    }

    pub fn find(&self, id: NodeId) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    pub fn find_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(id))
    }

    /// Returns true if `id` is this node or one of its descendants.
    pub fn contains(&self, id: NodeId) -> bool {
        self.find(id).is_some()
    }

    /// Parent id and child index of `id`.
    pub fn locate(&self, id: NodeId) -> Option<(NodeId, usize)> {
        for (i, c) in self.children.iter().enumerate() {
            if c.id == id {
                return Some((self.id, i));
            }
            if let Some(r) = c.locate(id) {
                return Some(r);
            }
        }
        None
    }

    /// Detach the node with `id` from this subtree.
    pub fn remove(&mut self, id: NodeId) -> Option<Node> {
        if let Some(i) = self.children.iter().position(|c| c.id == id) {
            return Some(self.children.remove(i));
        }
        self.children.iter_mut().find_map(|c| c.remove(id))
    }

    /// Give every node in the subtree a fresh id (for example after cloning).
    pub fn reassign_ids(&mut self) {
        self.id = NodeId::fresh();
        for c in &mut self.children {
            c.reassign_ids();
        }
    }
}

/// Where to insert a node relative to a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    FirstChild,
    LastChild,
    Before(NodeId),
    After(NodeId),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditError {
    #[error("node {0:?} not found")]
    NotFound(NodeId),
    #[error("cannot move a node into itself or its descendants")]
    IntoSelf,
    #[error("the root node cannot be moved or deleted")]
    Root,
    #[error("node id {0:?} already exists")]
    DuplicateId(NodeId),
    #[error("index {index} out of range (parent has {len} children)")]
    BadIndex { index: usize, len: usize },
    #[error("storage error: {0}")]
    Io(String),
}

/// A node removed by the user or by a browser event, kept for recovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrashItem {
    /// Removal time, ms since epoch.
    pub removed_at: i64,
    /// Where the node was, so it can be put back.
    pub parent: NodeId,
    pub index: usize,
    pub node: Node,
}

/// A whole tree with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    pub format: String,
    pub version: u32,
    /// Tabs Outliner `treeId`, if the tree came from there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_id: Option<String>,
    /// Save time from the source, ms since epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<i64>,
    /// Last journal entry included in this snapshot (see `store`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_seq: Option<u64>,
    /// Extra session data from the source.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub session: Map<String, Value>,
    pub root: Node,
    /// Removed nodes, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trash: Vec<TrashItem>,
}

impl Default for Tree {
    fn default() -> Self {
        Tree {
            format: FORMAT_NAME.to_owned(),
            version: FORMAT_VERSION,
            tree_id: None,
            saved_at: None,
            journal_seq: None,
            session: Map::new(),
            root: Node::new(NodeKind::Session),
            trash: Vec::new(),
        }
    }
}

impl Tree {
    pub fn count(&self) -> usize {
        self.root.count()
    }

    /// Make ids unique (duplicates get fresh ids) and make sure future
    /// `NodeId::fresh()` values don't collide with ids in this tree.
    /// Call after deserializing.
    pub fn fix_ids(&mut self) -> usize {
        fn go(n: &mut Node, seen: &mut std::collections::HashSet<NodeId>, fixed: &mut usize) {
            if !seen.insert(n.id) {
                n.id = NodeId::fresh();
                seen.insert(n.id);
                *fixed += 1;
            }
            NodeId::bump_above(n.id);
            for c in &mut n.children {
                go(c, seen, fixed);
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut fixed = 0;
        go(&mut self.root, &mut seen, &mut fixed);
        for t in &mut self.trash {
            go(&mut t.node, &mut seen, &mut fixed);
        }
        fixed
    }

    /// Make later `NodeId::fresh()` values in this process larger than every
    /// id in the tree (for replicas received from a server, which must not
    /// be renumbered).
    pub fn bump_ids(&self) {
        self.root.walk(&mut |n, _| NodeId::bump_above(n.id));
        for t in &self.trash {
            t.node.walk(&mut |n, _| NodeId::bump_above(n.id));
        }
    }

    /// Resolve a [`Position`] inside `parent` to a child index, as seen after
    /// `moving` (if any) has been taken out of the tree.
    pub fn resolve(
        &self,
        parent: NodeId,
        pos: Position,
        moving: Option<NodeId>,
    ) -> Result<usize, EditError> {
        let p = self.root.find(parent).ok_or(EditError::NotFound(parent))?;
        let kids: Vec<NodeId> = p
            .children
            .iter()
            .map(|c| c.id)
            .filter(|&id| Some(id) != moving)
            .collect();
        Ok(match pos {
            Position::FirstChild => 0,
            Position::LastChild => kids.len(),
            Position::Before(s) => kids.iter().position(|&c| c == s).unwrap_or(kids.len()),
            Position::After(s) => kids.iter().position(|&c| c == s).map_or(kids.len(), |i| i + 1),
        })
    }

    /// Ops that move `ids` (in order) to `pos` in `target`. Nodes inside
    /// another moved node travel with it and are skipped. The ops must be
    /// applied in order; each one is computed against the tree as it will
    /// be after the previous ones.
    pub fn plan_move(
        &self,
        ids: &[NodeId],
        target: NodeId,
        pos: Position,
    ) -> Result<Vec<crate::ops::Op>, EditError> {
        if ids.contains(&self.root.id) {
            return Err(EditError::Root);
        }
        for &id in ids {
            let n = self.root.find(id).ok_or(EditError::NotFound(id))?;
            if n.contains(target) {
                return Err(EditError::IntoSelf);
            }
        }
        let top: Vec<NodeId> = ids
            .iter()
            .copied()
            .filter(|&id| {
                !ids.iter().any(|&o| o != id && self.root.find(o).is_some_and(|n| n.contains(id)))
            })
            .collect();
        // Simulate on a light copy of the target's child list.
        let mut sim = self.clone_shallow(target)?;
        let mut ops = Vec::new();
        let mut pos = pos;
        for id in top {
            let index = sim.resolve(target, pos, Some(id))?;
            let op = crate::ops::Op::Move { id, parent: target, index };
            sim.apply_shallow_move(id, target, index);
            ops.push(op);
            pos = Position::After(id);
        }
        Ok(ops)
    }

    /// A tree holding only `target` with stub children, for `plan_move`.
    fn clone_shallow(&self, target: NodeId) -> Result<Tree, EditError> {
        let t = self.root.find(target).ok_or(EditError::NotFound(target))?;
        let mut root = Node::new(NodeKind::Session);
        root.id = target;
        root.children = t
            .children
            .iter()
            .map(|c| {
                let mut n = Node::new(NodeKind::Group);
                n.id = c.id;
                n
            })
            .collect();
        Ok(Tree { root, ..Default::default() })
    }

    fn apply_shallow_move(&mut self, id: NodeId, parent: NodeId, index: usize) {
        let mut n = self.root.remove(id).unwrap_or_else(|| {
            let mut n = Node::new(NodeKind::Group);
            n.id = id;
            n
        });
        n.children.clear();
        if let Some(p) = self.root.find_mut(parent) {
            let i = index.min(p.children.len());
            p.children.insert(i, n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(t: &str) -> Node {
        let mut n = Node::new(NodeKind::SavedTab);
        n.title = Some(t.into());
        n
    }

    fn titles(n: &Node) -> Vec<String> {
        n.children.iter().map(|c| c.display_title()).collect()
    }

    #[test]
    fn plan_move_between_parents() {
        let mut t = Tree::default();
        let mut w1 = Node::new(NodeKind::SavedWindow);
        let a = tab("a");
        let b = tab("b");
        let c = tab("c");
        let (aid, bid, cid) = (a.id, b.id, c.id);
        w1.children = vec![a, b, c];
        let w2 = Node::new(NodeKind::SavedWindow);
        let (w1id, w2id) = (w1.id, w2.id);
        t.root.children = vec![w1, w2];

        for op in t.plan_move(&[aid], w2id, Position::LastChild).unwrap() {
            t.apply(&op).unwrap();
        }
        assert_eq!(titles(t.root.find(w2id).unwrap()), ["a"]);
        // move c and a after b, in that order
        for op in t.plan_move(&[cid, aid], w1id, Position::After(bid)).unwrap() {
            t.apply(&op).unwrap();
        }
        assert_eq!(titles(t.root.find(w1id).unwrap()), ["b", "c", "a"]);
        // moving b before itself keeps the order
        for op in t.plan_move(&[bid], w1id, Position::Before(cid)).unwrap() {
            t.apply(&op).unwrap();
        }
        assert_eq!(titles(t.root.find(w1id).unwrap()), ["b", "c", "a"]);
        assert_eq!(t.plan_move(&[w1id], aid, Position::FirstChild).unwrap_err(), EditError::IntoSelf);
    }

    #[test]
    fn ids_roundtrip_and_fix() {
        let mut t = Tree::default();
        let a = tab("a");
        let mut dup = tab("dup");
        dup.id = a.id;
        t.root.children = vec![a, dup];
        assert_eq!(t.fix_ids(), 1);
        let json = serde_json::to_string(&t).unwrap();
        let mut back: Tree = serde_json::from_str(&json).unwrap();
        assert_eq!(back.fix_ids(), 0);
        assert_eq!(back.root.children[1].id, t.root.children[1].id);
        assert!(NodeId::fresh().0 > t.root.children[1].id.0);
    }
}
