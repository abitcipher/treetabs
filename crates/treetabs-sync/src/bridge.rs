//! Tree → browser direction: open/focus/close requests from viewers, and
//! matching the resulting browser events back to the nodes that asked for
//! them. Bookmark mirroring lives in [`crate::bookmarks`].

use crate::bookmarks::Mirror;
use crate::protocol::ServerMsg;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use treetabs_core::sync::{BrowserEvent, BrowserTab};
use treetabs_core::{Node, NodeId, NodeKind, Op, Tree};

/// How long an open request waits for its browser events.
const EXPECT_FOR: Duration = Duration::from_secs(20);

/// Bookmark calls sent to the browser and not yet answered, at most. A big
/// sync makes thousands; sent all at once they hold up everything else on
/// the same channel (the tree window waited many seconds for its tree).
const BOOKMARK_WINDOW: usize = 64;

/// A saved node waiting for the browser to open it.
struct Expect {
    node: NodeId,
    /// Browser window it will appear in, if known.
    window: Option<i64>,
    url: Option<String>,
    is_window: bool,
    since: Instant,
}

impl Expect {
    /// A tab opening `url` for `node`, in `window` when known.
    fn tab(node: NodeId, window: Option<i64>, url: String, since: Instant) -> Self {
        Expect {
            node,
            window,
            url: Some(url),
            is_window: false,
            since,
        }
    }

    /// A window opening for `node`.
    fn window(node: NodeId, since: Instant) -> Self {
        Expect {
            node,
            window: None,
            url: None,
            is_window: true,
            since,
        }
    }
}

#[derive(Default)]
pub struct Browser {
    clients: Vec<u64>,
    next_req: u64,
    expects: Vec<Expect>,
    /// Tabs we close on the user's request: keep them as saved.
    closing: HashSet<i64>,
    pending: std::collections::HashMap<u64, Pending>,
    pub mirror: Mirror,
    /// Bookmark calls waiting for room in [`BOOKMARK_WINDOW`].
    queued: std::collections::VecDeque<crate::bookmarks::BookmarkCall>,
    bookmark_inflight: usize,
}

enum Pending {
    Plain(String),
    Bookmark(crate::bookmarks::PendingCall),
}

pub type Calls = Vec<(u64, ServerMsg)>;

impl Browser {
    pub fn connected(&mut self, client: u64) {
        self.clients.push(client);
    }

    pub fn disconnected(&mut self, client: u64) {
        self.clients.retain(|&c| c != client);
        if self.clients.is_empty() {
            self.pending.clear();
            self.queued.clear();
            self.bookmark_inflight = 0;
            self.mirror.disconnected();
        }
    }

    fn call(&mut self, api: &str, args: Vec<Value>, out: &mut Calls) {
        self.call_with(api, args, Pending::Plain(api.to_owned()), out);
    }

    fn call_with(&mut self, api: &str, args: Vec<Value>, p: Pending, out: &mut Calls) {
        let Some(&client) = self.clients.first() else {
            return;
        };
        self.next_req += 1;
        let req = self.next_req;
        self.pending.insert(req, p);
        out.push((
            client,
            ServerMsg::Call {
                req,
                api: api.to_owned(),
                args,
            },
        ));
    }

    /// Bookmark calls produced by the mirror, wrapped for sending.
    fn send_mirror_calls(&mut self, calls: Vec<crate::bookmarks::BookmarkCall>, out: &mut Calls) {
        self.queued.extend(calls);
        while self.bookmark_inflight < BOOKMARK_WINDOW
            && !self.clients.is_empty()
            && let Some(c) = self.queued.pop_front()
        {
            self.bookmark_inflight += 1;
            self.call_with(&c.api, c.args, Pending::Bookmark(c.pending), out);
        }
    }

    /// After a browser snapshot was reconciled.
    pub fn resynced(&mut self, _client: u64, tree: &Tree) -> Calls {
        let mut out = Vec::new();
        // The bookmark tree is fetched fresh after every resync.
        let calls = self.mirror.start(tree);
        self.send_mirror_calls(calls, &mut out);
        out
    }

    pub fn tick(&mut self, tree: &Tree) -> Calls {
        let mut out = Vec::new();
        if self.clients.is_empty() {
            return out;
        }
        let calls = self.mirror.tick(tree);
        self.send_mirror_calls(calls, &mut out);
        out
    }

    pub fn tree_changed(&mut self, tree: &Tree) -> Calls {
        let mut out = Vec::new();
        let calls = self.mirror.tree_changed(tree);
        self.send_mirror_calls(calls, &mut out);
        out
    }

    pub fn bookmark_event(&mut self, name: &str, args: &[Value], tree: &Tree) -> (Vec<Op>, Calls) {
        let mut out = Vec::new();
        let (ops, calls) = self.mirror.browser_event(name, args, tree);
        self.send_mirror_calls(calls, &mut out);
        (ops, out)
    }

    pub fn call_result(
        &mut self,
        req: u64,
        ok: Option<Value>,
        err: Option<String>,
        tree: &Tree,
    ) -> (Vec<Op>, Calls) {
        let mut out = Vec::new();
        match self.pending.remove(&req) {
            Some(Pending::Plain(api)) => {
                if let Some(e) = err {
                    log::warn!("{api} failed: {e}");
                }
                (vec![], out)
            }
            Some(Pending::Bookmark(p)) => {
                self.bookmark_inflight = self.bookmark_inflight.saturating_sub(1);
                let (ops, calls) = self.mirror.call_result(p, ok, err, tree);
                self.send_mirror_calls(calls, &mut out);
                (ops, out)
            }
            None => (vec![], out),
        }
    }

    pub fn before_event(&mut self, _ev: &BrowserEvent, _tree: &Tree) -> Calls {
        Vec::new()
    }

    /// If `ev` is the browser carrying out one of our requests, the ops that
    /// bind it to the requesting node (instead of normal event handling).
    pub fn take_expectation(&mut self, ev: &BrowserEvent, tree: &Tree) -> Option<Vec<Op>> {
        self.expects
            .retain(|e| e.since.elapsed() < EXPECT_FOR && tree.root.find(e.node).is_some());
        match ev {
            BrowserEvent::WindowCreated(w) => {
                let i = self
                    .expects
                    .iter()
                    .position(|e| e.is_window && e.window.is_none())?;
                let e = self.expects.remove(i);
                let node = tree.root.find(e.node)?;
                // Later tab expectations of this request belong to this window.
                for x in &mut self.expects {
                    if !x.is_window && x.window.is_none() && x.since == e.since {
                        x.window = Some(w.id);
                    }
                }
                let mut f = node.fields();
                f.kind = NodeKind::Window;
                f.data.insert("id".into(), json!(w.id));
                f.data.remove("closeDate");
                f.data.remove("crashDetectedDate");
                // Move a restored window to the top level if it was nested.
                let mut ops = vec![Op::Update {
                    id: e.node,
                    fields: Box::new(f),
                }];
                if tree
                    .root
                    .locate(e.node)
                    .is_some_and(|(p, _)| p != tree.root.id)
                {
                    ops.push(Op::Move {
                        id: e.node,
                        parent: tree.root.id,
                        index: tree.root.children.len() - 1,
                    });
                }
                Some(ops)
            }
            BrowserEvent::TabCreated(t) | BrowserEvent::TabUpdated(t) => {
                let url = tab_url(t);
                let i = self.expects.iter().position(|e| {
                    !e.is_window
                        && e.window.is_none_or(|w| w == t.window_id)
                        && url.is_some()
                        && e.url.as_deref() == url
                })?;
                // A TabUpdated for a tab we already track is not ours to bind.
                if matches!(ev, BrowserEvent::TabUpdated(_)) && tracked_tab(tree, t.id) {
                    return None;
                }
                let e = self.expects.remove(i);
                let node = tree.root.find(e.node)?;
                let mut f = node.fields();
                f.kind = NodeKind::Tab;
                f.data.insert("id".into(), json!(t.id));
                f.data.insert("windowId".into(), json!(t.window_id));
                f.data.remove("crashDetectedDate");
                Some(vec![Op::Update {
                    id: e.node,
                    fields: Box::new(f),
                }])
            }
            BrowserEvent::TabRemoved { tab_id, .. } if self.closing.remove(tab_id) => {
                let nid = find_open_tab(tree, *tab_id)?;
                let node = tree.root.find(nid)?;
                let mut f = node.fields();
                f.kind = NodeKind::SavedTab;
                f.data.remove("id");
                f.data.remove("windowId");
                Some(vec![Op::Update {
                    id: nid,
                    fields: Box::new(f),
                }])
            }
            _ => None,
        }
    }

    /// Open or focus `id` in the browser.
    pub fn activate(&mut self, id: NodeId, tree: &Tree) -> Calls {
        let mut out = Vec::new();
        if self.clients.is_empty() {
            return out;
        }
        let Some(node) = tree.root.find(id) else {
            return out;
        };
        match node.kind {
            NodeKind::Tab => {
                if let (Some(tab), Some(win)) = (int(node, "id"), int(node, "windowId")) {
                    self.call(
                        "tabs.update",
                        vec![json!(tab), json!({"active": true})],
                        &mut out,
                    );
                    self.focus_window(win, &mut out);
                }
            }
            NodeKind::Window => {
                if let Some(win) = int(node, "id") {
                    self.focus_window(win, &mut out);
                }
            }
            NodeKind::SavedTab | NodeKind::WaitingTab | NodeKind::AttachWaitingTab => {
                if let Some(url) = node.url.clone() {
                    self.open_saved_tab(id, url, tree, &mut out);
                }
            }
            NodeKind::SavedWindow | NodeKind::WaitingWindow | NodeKind::Group => {
                self.open_saved_window(node, &mut out)
            }
            _ => {}
        }
        out
    }

    fn focus_window(&mut self, win: i64, out: &mut Calls) {
        self.call(
            "windows.update",
            vec![json!(win), json!({"focused": true})],
            out,
        );
    }

    /// A saved tab: opened in its open window, or in a new window when it
    /// sits in a saved one (or in none).
    fn open_saved_tab(&mut self, id: NodeId, url: String, tree: &Tree, out: &mut Calls) {
        let since = Instant::now();
        if let Some(win) = open_window_of(tree, id) {
            self.expects
                .push(Expect::tab(id, Some(win), url.clone(), since));
            self.call(
                "tabs.create",
                vec![json!({"windowId": win, "url": url, "active": true})],
                out,
            );
            self.focus_window(win, out);
            return;
        }
        if let Some(w) = saved_window_of(tree, id) {
            self.expects.push(Expect::window(w, since));
        }
        self.expects.push(Expect::tab(id, None, url.clone(), since));
        self.call(
            "windows.create",
            vec![json!({"url": [url], "focused": true})],
            out,
        );
    }

    /// A saved window or a group: all its saved tabs in one new window, at
    /// the window's old place and size when known.
    fn open_saved_window(&mut self, node: &Node, out: &mut Calls) {
        let since = Instant::now();
        let mut urls = Vec::new();
        node.walk(&mut |n, d| {
            if d > 0
                && matches!(
                    n.kind,
                    NodeKind::SavedTab | NodeKind::WaitingTab | NodeKind::AttachWaitingTab
                )
                && let Some(u) = &n.url
            {
                urls.push((n.id, u.clone()));
            }
        });
        if urls.is_empty() {
            return;
        }
        if node.kind != NodeKind::Group {
            self.expects.push(Expect::window(node.id, since));
        }
        for (nid, u) in &urls {
            self.expects.push(Expect::tab(*nid, None, u.clone(), since));
        }
        let list: Vec<&str> = urls.iter().map(|(_, u)| u.as_str()).collect();
        let mut create = json!({"url": list, "focused": true});
        if let Some(r) = node.data.get("rect").and_then(Value::as_str) {
            let p: Vec<i64> = r.split('_').filter_map(|x| x.parse().ok()).collect();
            if let [left, top, width, height] = p[..] {
                create["left"] = json!(left);
                create["top"] = json!(top);
                create["width"] = json!(width);
                create["height"] = json!(height);
            }
        }
        self.call("windows.create", vec![create], out);
    }

    /// Close open tabs/windows, keeping their nodes as saved.
    pub fn close(&mut self, ids: &[NodeId], tree: &Tree) -> Calls {
        let mut out = Vec::new();
        for &id in ids {
            let Some(n) = tree.root.find(id) else {
                continue;
            };
            match n.kind {
                NodeKind::Tab => {
                    if let Some(tab) = int(n, "id") {
                        self.closing.insert(tab);
                        self.call("tabs.remove", vec![json!(tab)], &mut out);
                    }
                }
                NodeKind::Window => {
                    if let Some(w) = int(n, "id") {
                        self.call("windows.remove", vec![json!(w)], &mut out);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Unload open tabs (the active tab of a window can't be: Chrome says so).
    pub fn unload(&mut self, ids: &[NodeId], tree: &Tree) -> Calls {
        let mut out = Vec::new();
        for &id in ids {
            if let Some(n) = tree.root.find(id)
                && n.kind == NodeKind::Tab
                && n.data.get("discarded").is_none()
                && let Some(tab) = int(n, "id")
            {
                self.call("tabs.discard", vec![json!(tab)], &mut out);
            }
        }
        out
    }

    /// Side effects of a viewer's edit: trashing an open tab closes it.
    pub fn user_edit(&mut self, ops: &[Op], tree: &Tree) -> Calls {
        let mut out = Vec::new();
        for op in ops {
            if let Op::Move { id, .. } = op {
                self.follow_move(*id, tree, &mut out);
            }
            if let Op::Trash { id, .. } = op
                && let Some(ti) = tree.trash_index(*id)
            {
                let node = &tree.trash[ti].node;
                let mut tabs = Vec::new();
                let mut windows = Vec::new();
                node.walk(&mut |n, _| match n.kind {
                    NodeKind::Tab => tabs.extend(int(n, "id")),
                    NodeKind::Window => windows.extend(int(n, "id")),
                    _ => {}
                });
                for w in windows {
                    self.call("windows.remove", vec![json!(w)], &mut out);
                }
                if !tabs.is_empty() {
                    // If the nodes come back (undo) before Chrome reports the
                    // close, that report must save them, not trash them again.
                    self.closing.extend(tabs.iter().copied());
                    self.call("tabs.remove", vec![json!(tabs)], &mut out);
                }
            }
        }
        out
    }
}

impl Browser {
    /// A node was dragged in the tree: move its open tabs in the browser to
    /// match, if it now sits inside an open window. The tabs go where the
    /// tree has them (order of open tabs in the window, depth first), so
    /// the browser's move events then find them already in place.
    fn follow_move(&mut self, id: NodeId, tree: &Tree, out: &mut Calls) {
        let Some(moved) = tree.root.find(id) else {
            return;
        };
        let mut tabs = Vec::new();
        moved.walk(&mut |n, _| {
            if n.kind == NodeKind::Tab
                && let Some(t) = int(n, "id")
            {
                tabs.push(t);
            }
        });
        if tabs.is_empty() {
            return;
        }
        // The open window it is in now, if any.
        let mut at = id;
        let window = loop {
            match tree.root.locate(at) {
                Some((parent, _)) => match tree.root.find(parent) {
                    Some(p) if p.kind == NodeKind::Window => break Some(p),
                    Some(p) if p.id == tree.root.id => break None,
                    Some(p) => at = p.id,
                    None => break None,
                },
                None => break None,
            }
        };
        let Some(w) = window else { return };
        let Some(wid) = int(w, "id") else { return };
        let mut order = Vec::new();
        w.walk(&mut |n, _| {
            if n.kind == NodeKind::Tab
                && let Some(t) = int(n, "id")
            {
                order.push(t);
            }
        });
        let Some(index) = order.iter().position(|&t| t == tabs[0]) else {
            return;
        };
        self.call(
            "tabs.move",
            vec![json!(tabs), json!({"windowId": wid, "index": index})],
            out,
        );
    }
}

fn int(n: &Node, k: &str) -> Option<i64> {
    n.data.get(k).and_then(Value::as_i64)
}

fn tab_url(t: &BrowserTab) -> Option<&str> {
    t.url
        .as_deref()
        .filter(|u| !u.is_empty())
        .or(t.pending_url.as_deref())
}

fn tracked_tab(tree: &Tree, tab_id: i64) -> bool {
    find_open_tab(tree, tab_id).is_some()
}

fn find_open_tab(tree: &Tree, tab_id: i64) -> Option<NodeId> {
    tree.root
        .find_by(|n| n.kind == NodeKind::Tab && int(n, "id") == Some(tab_id))
        .map(|n| n.id)
}

/// Browser id of the open window containing `id`.
fn open_window_of(tree: &Tree, id: NodeId) -> Option<i64> {
    tree.root
        .children
        .iter()
        .find(|w| w.contains(id))
        .filter(|w| w.kind == NodeKind::Window)
        .and_then(|w| int(w, "id"))
}

fn saved_window_of(tree: &Tree, id: NodeId) -> Option<NodeId> {
    tree.root
        .children
        .iter()
        .find(|w| w.contains(id))
        .filter(|w| matches!(w.kind, NodeKind::SavedWindow | NodeKind::WaitingWindow))
        .map(|w| w.id)
}
