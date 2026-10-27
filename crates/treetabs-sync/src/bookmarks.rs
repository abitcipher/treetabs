//! Two-way mirror between the tree and a `treetabs` bookmark folder.
//!
//! Mapping:
//! * windows, saved windows and groups → folders;
//! * a tab without children → a bookmark;
//! * a tab with children → a folder named like the tab, holding the tab's
//!   own bookmark first, then its children;
//! * notes → folders (bookmarks need a URL); separators are not mirrored.
//!
//! Tree → bookmarks works by diffing: the desired bookmark tree is computed
//! from the tree and compared with a local model of Chrome's folder; the
//! difference becomes `chrome.bookmarks.*` calls. The model is updated when
//! a call is sent, so the echo events of our own calls match the model and
//! are ignored. Any other event in the folder is a user edit and becomes
//! tree ops.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use treetabs_core::{Node, NodeId, NodeKind, Op, Tree};

const ROOT_TITLE: &str = "treetabs";

/// Which bookmark item a node owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "k", content = "n", rename_all = "snake_case")]
pub enum Key {
    Folder(NodeId),
    Item(NodeId),
}

impl Key {
    pub fn node(self) -> NodeId {
        match self {
            Key::Folder(n) | Key::Item(n) => n,
        }
    }
}

/// One item of the desired bookmark tree.
#[derive(Debug, Clone, PartialEq)]
struct Want {
    key: Key,
    /// `None` = directly in the root folder.
    parent: Option<Key>,
    title: String,
    url: Option<String>,
}

/// Local copy of a Chrome bookmark item inside our folder.
#[derive(Debug, Clone)]
struct Item {
    parent: String,
    title: String,
    url: Option<String>,
    children: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum PendingCall {
    GetTree,
    CreateRoot,
    Create {
        key: Key,
        parent: String,
        title: String,
        url: Option<String>,
    },
    Other,
}

pub struct BookmarkCall {
    pub api: String,
    pub args: Vec<Value>,
    pub pending: PendingCall,
}

/// A create sent to Chrome, until its result arrives.
struct Creating {
    key: Key,
    parent: String,
    title: String,
    url: Option<String>,
    /// Id from the `onCreated` echo, when it came before the result.
    echo_id: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    root: Option<String>,
    map: Vec<(Key, String)>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
enum Phase {
    #[default]
    Off,
    Fetching,
    Ready,
}

#[derive(Default)]
pub struct Mirror {
    pub enabled: bool,
    path: Option<PathBuf>,
    phase: Phase,
    root: Option<String>,
    other: Option<String>,
    items: HashMap<String, Item>,
    map: HashMap<Key, String>,
    rev: HashMap<String, Key>,
    inflight: usize,
    /// Creates sent but not answered; their echo events are recognised here.
    creating: Vec<Creating>,
    /// Keys Chrome refused to create (bad URL, …); not retried.
    failed: HashSet<Key>,
    dirty: bool,
    map_changed: bool,
    importing: bool,
    /// A full diff walks the whole tree (~0.1 s for 50k nodes): rate-limit it.
    last_diff: Option<std::time::Instant>,
    followup: bool,
    /// Override of `MIN_DIFF_INTERVAL` (tests use zero).
    pub min_diff_interval: Option<std::time::Duration>,
}

const MIN_DIFF_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// A string field of an event's info object.
fn text(info: &Value, key: &str) -> Option<String> {
    info.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The `index` field of an event's info object.
fn index_of(info: &Value) -> Option<usize> {
    info.get("index")
        .and_then(Value::as_u64)
        .and_then(|i| usize::try_from(i).ok())
}

impl Mirror {
    pub fn configure(&mut self, enabled: bool, path: PathBuf) {
        self.enabled = enabled;
        self.path = Some(path);
        if let Some(saved) = self
            .path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
        {
            self.root = saved.root;
            for (k, id) in saved.map {
                self.rev.insert(id.clone(), k);
                self.map.insert(k, id);
            }
        }
    }

    fn call(&mut self, api: &str, args: Vec<Value>, pending: PendingCall) -> BookmarkCall {
        self.inflight += 1;
        BookmarkCall {
            api: api.to_owned(),
            args,
            pending,
        }
    }

    /// Browser (re)connected: fetch the bookmark tree.
    pub fn start(&mut self, _tree: &Tree) -> Vec<BookmarkCall> {
        if !self.enabled {
            return vec![];
        }
        self.phase = Phase::Fetching;
        self.inflight = 0;
        self.creating.clear();
        vec![self.call("bookmarks.getTree", vec![], PendingCall::GetTree)]
    }

    pub fn disconnected(&mut self) {
        self.phase = Phase::Off;
        self.inflight = 0;
        self.creating.clear();
        self.save();
    }

    pub fn tree_changed(&mut self, _tree: &Tree) -> Vec<BookmarkCall> {
        self.dirty = true;
        vec![]
    }

    /// Called periodically: diff when the tree changed and no call is in flight.
    pub fn tick(&mut self, tree: &Tree) -> Vec<BookmarkCall> {
        if self.map_changed {
            self.save();
        }
        if self.phase != Phase::Ready || !self.dirty || self.inflight > 0 || self.importing {
            return vec![];
        }
        // While a sync is in progress (calls just answered) go on at once;
        // otherwise wait so bursts of tab updates share one diff.
        if self.creating.is_empty()
            && self
                .last_diff
                .is_some_and(|t| t.elapsed() < self.min_diff_interval.unwrap_or(MIN_DIFF_INTERVAL))
            && !self.followup
        {
            return vec![];
        }
        self.dirty = false;
        self.last_diff = Some(std::time::Instant::now());
        let calls = self.diff(tree);
        // More rounds are needed while folders are still being created.
        self.followup = !calls.is_empty();
        calls
    }

    fn save(&mut self) {
        let Some(p) = &self.path else { return };
        let saved = Saved {
            root: self.root.clone(),
            map: self.map.iter().map(|(k, v)| (*k, v.clone())).collect(),
        };
        if let Ok(b) = serde_json::to_vec(&saved) {
            let _ = treetabs_core::store::write_atomic(p, &b);
        }
        self.map_changed = false;
    }

    fn bind(&mut self, key: Key, id: String) {
        if let Some(old) = self.map.insert(key, id.clone()) {
            self.rev.remove(&old);
        }
        self.rev.insert(id, key);
        self.map_changed = true;
    }

    fn unbind_id(&mut self, id: &str) {
        if let Some(k) = self.rev.remove(id) {
            self.map.remove(&k);
            self.map_changed = true;
        }
    }

    pub fn call_result(
        &mut self,
        p: PendingCall,
        ok: Option<Value>,
        err: Option<String>,
        tree: &Tree,
    ) -> (Vec<Op>, Vec<BookmarkCall>) {
        self.inflight = self.inflight.saturating_sub(1);
        if let Some(e) = &err {
            log::warn!("bookmark call {p:?} failed: {e}");
        }
        match p {
            PendingCall::GetTree => match ok {
                Some(v) => return self.loaded(&v, tree),
                None => self.phase = Phase::Off,
            },
            PendingCall::CreateRoot => match ok
                .as_ref()
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str)
            {
                Some(id) => {
                    self.root = Some(id.to_owned());
                    self.items.insert(
                        id.to_owned(),
                        Item {
                            parent: self.other.clone().unwrap_or_default(),
                            title: ROOT_TITLE.into(),
                            url: None,
                            children: vec![],
                        },
                    );
                    self.map_changed = true;
                    self.phase = Phase::Ready;
                    self.dirty = true;
                }
                None => self.phase = Phase::Off,
            },
            PendingCall::Create {
                key,
                parent,
                title,
                url,
            } => {
                let pos = self.creating.iter().position(|c| c.key == key);
                let early = pos.map(|i| self.creating.remove(i)).and_then(|c| c.echo_id);
                match ok
                    .as_ref()
                    .and_then(|v| v.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    Some(id) => {
                        if early.is_none() {
                            let index = ok
                                .as_ref()
                                .and_then(|v| v.get("index"))
                                .and_then(Value::as_u64)
                                .and_then(|i| usize::try_from(i).ok());
                            self.insert_item(&id, &parent, index, &title, url);
                        }
                        self.bind(key, id);
                    }
                    None => {
                        self.failed.insert(key);
                    }
                }
                self.dirty = true;
            }
            PendingCall::Other => {
                if err.is_some() {
                    // Our model may be off now: fetch again.
                    return (vec![], self.start(tree));
                }
            }
        }
        (vec![], vec![])
    }

    fn insert_item(
        &mut self,
        id: &str,
        parent: &str,
        index: Option<usize>,
        title: &str,
        url: Option<String>,
    ) {
        self.items.insert(
            id.to_owned(),
            Item {
                parent: parent.to_owned(),
                title: title.to_owned(),
                url,
                children: vec![],
            },
        );
        if let Some(p) = self.items.get_mut(parent)
            && !p.children.iter().any(|c| c == id)
        {
            let i = index.unwrap_or(p.children.len()).min(p.children.len());
            p.children.insert(i, id.to_owned());
        }
    }

    fn remove_item(&mut self, id: &str) {
        let Some(item) = self.items.remove(id) else {
            return;
        };
        if let Some(p) = self.items.get_mut(&item.parent) {
            p.children.retain(|c| c != id);
        }
        for c in item.children {
            self.remove_item_no_parent(&c);
        }
        self.unbind_id(id);
    }

    fn remove_item_no_parent(&mut self, id: &str) {
        if let Some(item) = self.items.remove(id) {
            for c in item.children {
                self.remove_item_no_parent(&c);
            }
        }
        self.unbind_id(id);
    }

    /// `bookmarks.getTree` answered: find or create our folder and load it.
    fn loaded(&mut self, v: &Value, tree: &Tree) -> (Vec<Op>, Vec<BookmarkCall>) {
        let top = v
            .get(0)
            .and_then(|r| r.get("children"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let other = top
            .iter()
            .find(|c| c.get("folderType").and_then(Value::as_str) == Some("other"))
            .or_else(|| {
                top.iter()
                    .find(|c| c.get("id").and_then(Value::as_str) == Some("2"))
            })
            // Firefox: "Other Bookmarks".
            .or_else(|| {
                top.iter()
                    .find(|c| c.get("id").and_then(Value::as_str) == Some("unfiled_____"))
            })
            .or_else(|| top.get(1));
        let Some(other) = other else {
            log::warn!("no 'Other bookmarks' folder");
            self.phase = Phase::Off;
            return (vec![], vec![]);
        };
        self.other = other.get("id").and_then(Value::as_str).map(str::to_owned);
        let kids = other
            .get("children")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let found = kids
            .iter()
            .find(|c| {
                c.get("id").and_then(Value::as_str) == self.root.as_deref()
                    && c.get("url").is_none()
            })
            .or_else(|| {
                kids.iter().find(|c| {
                    c.get("title").and_then(Value::as_str) == Some(ROOT_TITLE)
                        && c.get("url").is_none()
                })
            });
        self.items.clear();
        match found {
            None => {
                // Fresh start: nothing is mapped any more.
                self.map.clear();
                self.rev.clear();
                self.root = None;
                let other = self.other.clone().unwrap_or_default();
                let call = self.call(
                    "bookmarks.create",
                    vec![json!({"parentId": other, "title": ROOT_TITLE})],
                    PendingCall::CreateRoot,
                );
                (vec![], vec![call])
            }
            Some(folder) => {
                let root = folder["id"].as_str().unwrap_or_default().to_owned();
                self.root = Some(root.clone());
                self.load_subtree(folder, &self.other.clone().unwrap_or_default());
                // Drop mappings to items that are gone.
                let stale: Vec<String> = self
                    .rev
                    .keys()
                    .filter(|id| !self.items.contains_key(*id))
                    .cloned()
                    .collect();
                for id in stale {
                    self.unbind_id(&id);
                }
                let ops = self.recover_mapping(tree);
                self.phase = Phase::Ready;
                self.dirty = true;
                (ops, vec![])
            }
        }
    }

    fn load_subtree(&mut self, v: &Value, parent: &str) {
        let id = v["id"].as_str().unwrap_or_default().to_owned();
        let children: Vec<Value> = v
            .get("children")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.items.insert(
            id.clone(),
            Item {
                parent: parent.to_owned(),
                title: v
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                url: v.get("url").and_then(Value::as_str).map(str::to_owned),
                children: children
                    .iter()
                    .filter_map(|c| c["id"].as_str().map(str::to_owned))
                    .collect(),
            },
        );
        for c in &children {
            self.load_subtree(c, &id);
        }
    }

    /// Match unmapped bookmarks to unmapped wanted items with the same title
    /// and url under the same parent (mapping file lost or new store).
    /// Bookmarks still unmatched were added by the user while we were away:
    /// import them.
    fn recover_mapping(&mut self, tree: &Tree) -> Vec<Op> {
        let want = desired(tree);
        let mut by_parent: HashMap<Option<Key>, Vec<&Want>> = HashMap::new();
        for w in &want {
            by_parent.entry(w.parent).or_default().push(w);
        }
        // Breadth-first so parents are matched before their children.
        let mut queue: Vec<Option<Key>> = vec![None];
        while let Some(pk) = queue.pop() {
            let Some(pid) = (match pk {
                None => self.root.clone(),
                Some(k) => self.map.get(&k).cloned(),
            }) else {
                continue;
            };
            let kids = self
                .items
                .get(&pid)
                .map(|i| i.children.clone())
                .unwrap_or_default();
            for w in by_parent.get(&pk).cloned().unwrap_or_default() {
                if !self.map.contains_key(&w.key)
                    && let Some(id) = kids.iter().find(|id| {
                        !self.rev.contains_key(*id)
                            && self
                                .items
                                .get(*id)
                                .is_some_and(|i| i.title == w.title && i.url == w.url)
                    })
                {
                    self.bind(w.key, id.clone());
                }
                queue.push(Some(w.key));
            }
        }
        // Import what's left.
        let mut ops = Vec::new();
        let root = self.root.clone().unwrap_or_default();
        let mut stack = vec![root];
        let mut unmapped = Vec::new();
        while let Some(id) = stack.pop() {
            let kids = self
                .items
                .get(&id)
                .map(|i| i.children.clone())
                .unwrap_or_default();
            for c in kids {
                if self.rev.contains_key(&c) {
                    stack.push(c);
                } else {
                    unmapped.push(c);
                }
            }
        }
        let mut sim = tree.clone();
        for id in unmapped {
            if let Some(more) = self.import_item(&id, &mut sim) {
                for op in &more {
                    let _ = sim.apply(op);
                }
                ops.extend(more);
            }
        }
        ops
    }

    /// Ops creating tree nodes for a user-made bookmark item (and its
    /// children), placed where it sits in the folder.
    fn import_item(&mut self, id: &str, tree: &mut Tree) -> Option<Vec<Op>> {
        let item = self.items.get(id)?.clone();
        let (parent, index) = self.tree_position(id, &item.parent, tree)?;
        let mut node = match &item.url {
            Some(u) => {
                let mut n = Node::new(NodeKind::SavedTab);
                n.title = Some(item.title.clone());
                n.url = Some(u.clone());
                n
            }
            None => {
                let mut n = Node::new(NodeKind::Group);
                n.custom_title = Some(item.title.clone());
                n
            }
        };
        let key = if item.url.is_some() {
            Key::Item(node.id)
        } else {
            Key::Folder(node.id)
        };
        self.bind(key, id.to_owned());
        // Children of an imported folder come along.
        let mut child_ops = Vec::new();
        for c in &item.children {
            if let Some(sub) = self.build_import(c) {
                node.children.push(sub);
            }
        }
        child_ops.insert(
            0,
            Op::Insert {
                parent,
                index,
                node: Box::new(node),
            },
        );
        Some(child_ops)
    }

    fn build_import(&mut self, id: &str) -> Option<Node> {
        let item = self.items.get(id)?.clone();
        let mut n = match &item.url {
            Some(u) => {
                let mut n = Node::new(NodeKind::SavedTab);
                n.title = Some(item.title.clone());
                n.url = Some(u.clone());
                n
            }
            None => {
                let mut n = Node::new(NodeKind::Group);
                n.custom_title = Some(item.title.clone());
                n
            }
        };
        let key = if item.url.is_some() {
            Key::Item(n.id)
        } else {
            Key::Folder(n.id)
        };
        self.bind(key, id.to_owned());
        for c in &item.children {
            if let Some(sub) = self.build_import(c) {
                n.children.push(sub);
            }
        }
        Some(n)
    }

    /// Tree parent and index for the bookmark `id` in folder `parent`.
    fn tree_position(&self, id: &str, parent: &str, tree: &Tree) -> Option<(NodeId, usize)> {
        let target = if Some(parent) == self.root.as_deref() {
            tree.root.id
        } else {
            match self.rev.get(parent)? {
                Key::Folder(n) => *n,
                Key::Item(_) => return None,
            }
        };
        let tnode = tree.root.find(target)?;
        let siblings = &self.items.get(parent)?.children;
        let pos = siblings
            .iter()
            .position(|s| s == id)
            .unwrap_or(siblings.len());
        // After the nearest earlier sibling that maps to a child of `target`.
        for s in siblings[..pos].iter().rev() {
            if let Some(k) = self.rev.get(s)
                && let Some(i) = tnode.children.iter().position(|c| c.id == k.node())
            {
                return Some((target, i + 1));
            }
        }
        Some((target, 0))
    }

    /// A bookmark event from the browser: ops for the tree, and calls when
    /// the folder must be read again.
    pub fn browser_event(
        &mut self,
        name: &str,
        args: &[Value],
        tree: &Tree,
    ) -> (Vec<Op>, Vec<BookmarkCall>) {
        if !self.enabled || self.phase != Phase::Ready {
            return (vec![], vec![]);
        }
        let id = args
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let info = args.get(1).cloned().unwrap_or(Value::Null);
        match name {
            "bookmarks.onImportBegan" => {
                self.importing = true;
                (vec![], vec![])
            }
            "bookmarks.onImportEnded" => {
                self.importing = false;
                (vec![], self.start(tree))
            }
            _ if self.importing => (vec![], vec![]),
            "bookmarks.onCreated" => (self.on_created(&id, &info, tree), vec![]),
            "bookmarks.onChanged" => (self.on_changed(&id, &info, tree), vec![]),
            "bookmarks.onRemoved" => (self.on_removed(&id, tree), vec![]),
            "bookmarks.onMoved" => self.on_moved(&id, &info, tree),
            "bookmarks.onChildrenReordered" => (vec![], self.start(tree)),
            _ => (vec![], vec![]),
        }
    }

    /// A bookmark made in our folder: by us (an echo), or by the user.
    fn on_created(&mut self, id: &str, info: &Value, tree: &Tree) -> Vec<Op> {
        let parent = text(info, "parentId").unwrap_or_default();
        if self.items.contains_key(id) || !self.items.contains_key(&parent) {
            return vec![]; // ours (already known) or outside our folder
        }
        let title = text(info, "title").unwrap_or_default();
        let url = text(info, "url");
        let index = index_of(info);
        // Echo of a create still waiting for its result?
        if let Some(c) = self
            .creating
            .iter_mut()
            .find(|c| c.parent == parent && c.title == title && c.url == url && c.echo_id.is_none())
        {
            c.echo_id = Some(id.to_owned());
            self.insert_item(id, &parent, index, &title, url);
            return vec![];
        }
        self.insert_item(id, &parent, index, &title, url);
        let mut sim = tree.clone();
        self.import_item(id, &mut sim).unwrap_or_default()
    }

    /// A title or URL changed: carried over to the node.
    fn on_changed(&mut self, id: &str, info: &Value, tree: &Tree) -> Vec<Op> {
        let Some(item) = self.items.get_mut(id) else {
            return vec![];
        };
        let title = text(info, "title").unwrap_or_else(|| item.title.clone());
        let url = text(info, "url").or(item.url.clone());
        if item.title == title && item.url == url {
            return vec![]; // echo
        }
        item.title.clone_from(&title);
        item.url.clone_from(&url);
        let Some(&key) = self.rev.get(id) else {
            return vec![];
        };
        let Some(node) = tree.root.find(key.node()) else {
            return vec![];
        };
        let mut f = node.fields();
        match (key, &node.kind) {
            (_, NodeKind::Note) => f.note = Some(title),
            (Key::Item(_), _) => {
                f.custom_title = (node.title.as_deref() != Some(title.as_str())).then_some(title);
                if url.is_some() {
                    f.url = url;
                }
            }
            (Key::Folder(_), _) => f.custom_title = Some(title),
        }
        vec![Op::Update {
            id: node.id,
            fields: Box::new(f),
        }]
    }

    /// A bookmark removed by the user: its node goes to the trash. When only
    /// a tab's own bookmark went, the tab's children stay.
    fn on_removed(&mut self, id: &str, tree: &Tree) -> Vec<Op> {
        if !self.items.contains_key(id) {
            return vec![]; // echo of our remove
        }
        let key = self.rev.get(id).copied();
        self.remove_item(id);
        let Some(key) = key else { return vec![] };
        let Some(node) = tree.root.find(key.node()) else {
            return vec![];
        };
        let mut ops = Vec::new();
        if matches!(key, Key::Item(_))
            && !node.children.is_empty()
            && let Some((p, i)) = tree.root.locate(node.id)
        {
            for (k, c) in node.children.iter().enumerate() {
                ops.push(Op::Move {
                    id: c.id,
                    parent: p,
                    index: i + 1 + k,
                });
            }
        }
        ops.push(Op::Trash {
            id: node.id,
            at: treetabs_core::ops::now_ms(),
        });
        self.dirty = true;
        ops
    }

    /// A bookmark moved: inside our folder the node moves too; moved out it
    /// counts as removed; moved in, the folder is read again.
    fn on_moved(&mut self, id: &str, info: &Value, tree: &Tree) -> (Vec<Op>, Vec<BookmarkCall>) {
        let parent = text(info, "parentId").unwrap_or_default();
        let index = index_of(info).unwrap_or(0);
        let known = self.items.contains_key(id);
        let into_ours = self.items.contains_key(&parent);
        match (known, into_ours) {
            (false, false) => (vec![], vec![]),
            (false, true) => (vec![], self.start(tree)),
            (true, false) => (self.on_removed(id, tree), vec![]),
            (true, true) => (self.move_within(id, &parent, index, tree), vec![]),
        }
    }

    /// A known bookmark moved to `parent` at `index`, both in our folder.
    fn move_within(&mut self, id: &str, parent: &str, index: usize, tree: &Tree) -> Vec<Op> {
        let old_parent = self
            .items
            .get(id)
            .expect("caller checked it is known")
            .parent
            .clone();
        let cur = self
            .items
            .get(&old_parent)
            .and_then(|p| p.children.iter().position(|c| c == id));
        if old_parent == parent && cur == Some(index) {
            return vec![]; // echo
        }
        if let Some(p) = self.items.get_mut(&old_parent) {
            p.children.retain(|c| c != id);
        }
        if let Some(p) = self.items.get_mut(parent) {
            let i = index.min(p.children.len());
            p.children.insert(i, id.to_owned());
        }
        if let Some(it) = self.items.get_mut(id) {
            it.parent = parent.to_owned();
        }
        self.dirty = true;
        let Some(&key) = self.rev.get(id) else {
            return vec![];
        };
        let Some((tp, ti)) = self.tree_position(id, parent, tree) else {
            return vec![];
        };
        let node = key.node();
        if tree.root.find(node).is_none_or(|n| n.contains(tp)) {
            return vec![];
        }
        // Index is counted after the node is taken out.
        let ti = match tree.root.locate(node) {
            Some((p, old)) if p == tp && old < ti => ti - 1,
            _ => ti,
        };
        vec![Op::Move {
            id: node,
            parent: tp,
            index: ti,
        }]
    }

    /// Calls that make Chrome's folder match the tree.
    fn diff(&mut self, tree: &Tree) -> Vec<BookmarkCall> {
        let Some(root) = self.root.clone() else {
            return vec![];
        };
        let want = desired(tree);
        let wanted: HashSet<Key> = want.iter().map(|w| w.key).collect();
        let mut calls = Vec::new();
        // 1) Remove items whose node is gone (or that changed role).
        self.remove_stale(&root, &wanted, &mut calls);
        // 2) Update titles and URLs, then create and place wanted items,
        //    parent by parent.
        self.update_items(&want, &mut calls);
        let mut children_of: HashMap<Option<Key>, Vec<&Want>> = HashMap::new();
        for w in &want {
            children_of.entry(w.parent).or_default().push(w);
        }
        let creating: HashSet<Key> = self.creating.iter().map(|c| c.key).collect();
        let mut parents: Vec<Option<Key>> = vec![None];
        parents.extend(want.iter().filter(|w| w.url.is_none()).map(|w| Some(w.key)));
        for pk in parents {
            let Some(pid) = (match pk {
                None => Some(root.clone()),
                Some(k) => self.map.get(&k).cloned(),
            }) else {
                continue; // parent not created yet: next round
            };
            if !self.items.contains_key(&pid) {
                continue;
            }
            let kids = children_of.get(&pk).cloned().unwrap_or_default();
            self.place_children(&pid, &kids, &creating, &mut calls);
        }
        // Placeholders only served index bookkeeping within this round.
        let placeholders: Vec<String> = self
            .items
            .keys()
            .filter(|k| k.starts_with("pending:"))
            .cloned()
            .collect();
        for p in placeholders {
            self.remove_item(&p);
        }
        calls
    }

    /// Remove items whose node is gone; children still wanted are parked in
    /// the root first, and placed again by the later passes.
    fn remove_stale(&mut self, root: &str, wanted: &HashSet<Key>, calls: &mut Vec<BookmarkCall>) {
        let stale: Vec<String> = self
            .map
            .iter()
            .filter(|(k, _)| !wanted.contains(k))
            .map(|(_, id)| id.clone())
            .collect();
        for id in stale {
            if self.items.contains_key(&id) {
                let folder = self.items[&id].url.is_none();
                // Keep children that are still wanted: move them out first by
                // letting the later passes re-home them; removeTree takes the rest.
                let keep: Vec<String> = self.items[&id]
                    .children
                    .iter()
                    .filter(|c| self.rev.get(*c).is_some_and(|k| wanted.contains(k)))
                    .cloned()
                    .collect();
                for c in keep {
                    // Park in the root; pass 2 moves it to its real place.
                    let len = self.items.get(root).map_or(0, |r| r.children.len());
                    calls.push(self.call(
                        "bookmarks.move",
                        vec![json!(c), json!({"parentId": root, "index": len})],
                        PendingCall::Other,
                    ));
                    self.local_move(&c, root, len);
                }
                let api = if folder {
                    "bookmarks.removeTree"
                } else {
                    "bookmarks.remove"
                };
                calls.push(self.call(api, vec![json!(id)], PendingCall::Other));
                self.remove_item(&id);
            } else {
                self.unbind_id(&id);
            }
        }
    }

    /// New titles and URLs for items that already exist.
    fn update_items(&mut self, want: &[Want], calls: &mut Vec<BookmarkCall>) {
        for w in want {
            if let Some(id) = self.map.get(&w.key).cloned()
                && let Some(item) = self.items.get(&id)
                && (item.title != w.title || (w.url.is_some() && item.url != w.url))
            {
                let mut change = json!({"title": w.title});
                if let Some(u) = &w.url {
                    change["url"] = json!(u);
                }
                calls.push(self.call(
                    "bookmarks.update",
                    vec![json!(id), change],
                    PendingCall::Other,
                ));
                let it = self.items.get_mut(&id).expect("item");
                it.title = w.title.clone();
                if w.url.is_some() {
                    it.url = w.url.clone();
                }
            }
        }
    }

    /// Create or move the wanted children of folder `pid` into their order.
    fn place_children(
        &mut self,
        pid: &str,
        kids: &[&Want],
        creating: &HashSet<Key>,
        calls: &mut Vec<BookmarkCall>,
    ) {
        let mut slot = 0;
        for w in kids {
            if self.failed.contains(&w.key) || creating.contains(&w.key) {
                continue;
            }
            match self
                .map
                .get(&w.key)
                .cloned()
                .filter(|id| self.items.contains_key(id))
            {
                Some(id) => {
                    let at = self.items[pid].children.get(slot).cloned();
                    if at.as_deref() != Some(id.as_str()) {
                        calls.push(self.call(
                            "bookmarks.move",
                            vec![json!(id), json!({"parentId": pid, "index": slot})],
                            PendingCall::Other,
                        ));
                        self.local_move(&id, pid, slot);
                    }
                    slot += 1;
                }
                None => {
                    let mut spec = json!({"parentId": pid, "index": slot, "title": w.title});
                    if let Some(u) = &w.url {
                        spec["url"] = json!(u);
                    }
                    self.creating.push(Creating {
                        key: w.key,
                        parent: pid.to_owned(),
                        title: w.title.clone(),
                        url: w.url.clone(),
                        echo_id: None,
                    });
                    calls.push(self.call(
                        "bookmarks.create",
                        vec![spec],
                        PendingCall::Create {
                            key: w.key,
                            parent: pid.to_owned(),
                            title: w.title.clone(),
                            url: w.url.clone(),
                        },
                    ));
                    // The created item will sit at `slot`.
                    slot += 1;
                    // Keep the model's indexes right for later siblings.
                    let placeholder = format!("pending:{:?}", w.key);
                    self.insert_item(&placeholder, pid, Some(slot - 1), &w.title, w.url.clone());
                }
            }
        }
    }

    /// Mirror Chrome's move semantics in the local model.
    fn local_move(&mut self, id: &str, parent: &str, index: usize) {
        let old_parent = self
            .items
            .get(id)
            .map(|i| i.parent.clone())
            .unwrap_or_default();
        let old_index = self
            .items
            .get(&old_parent)
            .and_then(|p| p.children.iter().position(|c| c == id));
        if let Some(p) = self.items.get_mut(&old_parent) {
            p.children.retain(|c| c != id);
        }
        // Chrome: within the same parent, an index past the old position
        // counts the item itself, so it shifts by one.
        let index = match old_index {
            Some(o) if old_parent == parent && index > o => index - 1,
            _ => index,
        };
        if let Some(p) = self.items.get_mut(parent) {
            let i = index.min(p.children.len());
            p.children.insert(i, id.to_owned());
        }
        if let Some(it) = self.items.get_mut(id) {
            it.parent = parent.to_owned();
        }
    }
}

/// Folder title for a node.
fn folder_title(n: &Node) -> String {
    if let Some(t) = n.custom_title.as_deref().filter(|t| !t.is_empty()) {
        return t.to_owned();
    }
    match n.kind {
        NodeKind::Window => "Window".into(),
        NodeKind::SavedWindow | NodeKind::WaitingWindow => "Saved window".into(),
        NodeKind::Group => "Group".into(),
        _ => n.display_title(),
    }
}

/// The bookmark tree the current tree should produce, in pre-order.
fn desired(tree: &Tree) -> Vec<Want> {
    fn go(n: &Node, parent: Option<Key>, out: &mut Vec<Want>) {
        for c in &n.children {
            match c.kind {
                NodeKind::Separator | NodeKind::Session => {
                    // Not mirrored; children go to the parent.
                    go(c, parent, out);
                }
                ref k if k.is_tab() && c.url.is_some() => {
                    let item = Want {
                        key: Key::Item(c.id),
                        parent,
                        title: c.display_title(),
                        url: c.url.clone(),
                    };
                    if c.children.is_empty() {
                        out.push(item);
                    } else {
                        let folder = Key::Folder(c.id);
                        out.push(Want {
                            key: folder,
                            parent,
                            title: c.display_title(),
                            url: None,
                        });
                        out.push(Want {
                            parent: Some(folder),
                            ..item
                        });
                        go(c, Some(folder), out);
                    }
                }
                _ => {
                    let folder = Key::Folder(c.id);
                    out.push(Want {
                        key: folder,
                        parent,
                        title: folder_title(c),
                        url: None,
                    });
                    go(c, Some(folder), out);
                }
            }
        }
    }
    let mut out = Vec::new();
    go(&tree.root, None, &mut out);
    out
}

#[cfg(test)]
mod tests;
