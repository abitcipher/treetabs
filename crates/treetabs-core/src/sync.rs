//! Keeps the tree in step with the browser's windows and tabs.
//!
//! The [`Engine`] turns browser events (and full snapshots after a
//! (re)connect) into [`Op`]s. It applies them one by one through [`Apply`],
//! so each op is computed against the tree as it is after the previous one.
//!
//! Rules (Tabs Outliner behaviour, minus its data loss):
//! * new tabs are nested under their opener when that keeps browser order;
//! * a tab closed by the user goes to the trash, its children move up;
//!   protected tabs (custom title, notes, …) become saved tabs instead;
//! * tabs closed as part of a window close become saved tabs, and the
//!   window becomes a saved window;
//! * tabs trashed just before their window closed are brought back (Chrome
//!   sometimes reports a window close as separate tab closes);
//! * after a restart, live windows are matched to stored ones by URLs, not
//!   by ids (ids change on every browser start).

use crate::model::{EditError, Node, NodeId, NodeKind, Tree};
use crate::ops::{Op, now_ms};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// Pages of the treetabs extension itself (its tree window): never tracked.
pub const OWN_PAGES: &str = "chrome-extension://ipglemijkiaehilbokjjppicafjcbcbg/";

/// The extension's own pages (the tree window) are not part of the tree.
/// Matched by page name, not by id: a store install has another id than
/// the unpacked build, and Firefox gives each install its own uuid.
pub fn is_own_page(url: &str) -> bool {
    let ours = |rest: &str| rest.split_once('/').is_some_and(|(_, page)| page == "tree.html" || page.starts_with("tree.html?") || page.starts_with("tree.html#"));
    url.strip_prefix("chrome-extension://").is_some_and(ours) || url.strip_prefix("moz-extension://").is_some_and(ours)
}

/// Tabs trashed this recently before their window closed are restored.
const WINDOW_CLOSE_GRACE_MS: i64 = 5_000;
/// Minimum share of common URLs to treat two windows as the same.
const WINDOW_MATCH_MIN: f64 = 0.5;

/// Something ops can be applied to: a bare [`Tree`] or a journaled store.
pub trait Apply {
    fn tree(&self) -> &Tree;
    fn apply_op(&mut self, op: Op) -> Result<(), EditError>;
}

impl Apply for Tree {
    fn tree(&self) -> &Tree {
        self
    }
    fn apply_op(&mut self, op: Op) -> Result<(), EditError> {
        self.apply(&op).map(|_| ())
    }
}

impl Apply for crate::store::Store {
    fn tree(&self) -> &Tree {
        crate::store::Store::tree(self)
    }
    fn apply_op(&mut self, op: Op) -> Result<(), EditError> {
        self.apply(op).map(|_| ()).map_err(|e| match e {
            crate::store::StoreError::Edit(e) => e,
            other => {
                log::error!("store write failed: {other}");
                EditError::Io(other.to_string())
            }
        })
    }
}

/// `chrome.tabs.Tab` (the fields we use).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTab {
    pub id: i64,
    pub window_id: i64,
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub pending_url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub fav_icon_url: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub opener_tab_id: Option<i64>,
    #[serde(default)]
    pub group_id: Option<i64>,
    #[serde(default)]
    pub incognito: bool,
    /// Unloaded by the browser (memory freed, still in its window).
    #[serde(default)]
    pub discarded: bool,
}

impl BrowserTab {
    fn url(&self) -> Option<&str> {
        self.url.as_deref().filter(|u| !u.is_empty()).or(self.pending_url.as_deref())
    }

    /// One of our own extension pages (the tree window).
    pub fn is_own_page(&self) -> bool {
        self.url().is_some_and(is_own_page)
    }

    /// Only an explicit new-tab page. Chrome creates link-opened tabs with
    /// an empty url (filled in by a later update), so no url is not "new tab".
    fn is_new_tab_page(&self) -> bool {
        self.url().is_some_and(|u| {
            u.starts_with("chrome://newtab") || u.starts_with("edge://newtab") || u == "about:newtab" || u == "about:home"
        })
    }
}

/// `chrome.windows.Window` (the fields we use).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserWindow {
    pub id: i64,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub left: Option<i64>,
    #[serde(default)]
    pub top: Option<i64>,
    #[serde(default)]
    pub width: Option<i64>,
    #[serde(default)]
    pub height: Option<i64>,
    #[serde(default)]
    pub incognito: bool,
    #[serde(default)]
    pub tabs: Vec<BrowserTab>,
}

/// A browser event, decoded from `{name, args}` as sent by the extension.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserEvent {
    TabCreated(BrowserTab),
    TabUpdated(BrowserTab),
    TabRemoved { tab_id: i64, window_id: i64, window_closing: bool },
    TabMoved { tab_id: i64, window_id: i64, to: usize },
    TabAttached { tab_id: i64, window_id: i64, position: usize },
    TabReplaced { added: i64, removed: i64 },
    WindowCreated(BrowserWindow),
    WindowRemoved(i64),
    WindowBoundsChanged(BrowserWindow),
}

impl BrowserEvent {
    /// Decode a `chrome.*` event by its listener name and arguments.
    /// Returns `Ok(None)` for events we don't track.
    pub fn from_chrome(name: &str, args: &[Value]) -> Result<Option<Self>, serde_json::Error> {
        let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
        let int = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(-1);
        Ok(Some(match name {
            "tabs.onCreated" => BrowserEvent::TabCreated(serde_json::from_value(arg(0))?),
            "tabs.onUpdated" => BrowserEvent::TabUpdated(serde_json::from_value(arg(2))?),
            "tabs.onRemoved" => {
                let info = arg(1);
                BrowserEvent::TabRemoved {
                    tab_id: arg(0).as_i64().unwrap_or(-1),
                    window_id: int(&info, "windowId"),
                    window_closing: info.get("isWindowClosing").and_then(Value::as_bool).unwrap_or(false),
                }
            }
            "tabs.onMoved" => {
                let info = arg(1);
                BrowserEvent::TabMoved {
                    tab_id: arg(0).as_i64().unwrap_or(-1),
                    window_id: int(&info, "windowId"),
                    to: int(&info, "toIndex").max(0) as usize,
                }
            }
            "tabs.onAttached" => {
                let info = arg(1);
                BrowserEvent::TabAttached {
                    tab_id: arg(0).as_i64().unwrap_or(-1),
                    window_id: int(&info, "newWindowId"),
                    position: int(&info, "newPosition").max(0) as usize,
                }
            }
            "tabs.onReplaced" => BrowserEvent::TabReplaced {
                added: arg(0).as_i64().unwrap_or(-1),
                removed: arg(1).as_i64().unwrap_or(-1),
            },
            "windows.onCreated" => BrowserEvent::WindowCreated(serde_json::from_value(arg(0))?),
            "windows.onRemoved" => BrowserEvent::WindowRemoved(arg(0).as_i64().unwrap_or(-1)),
            "windows.onBoundsChanged" => BrowserEvent::WindowBoundsChanged(serde_json::from_value(arg(0))?),
            _ => return Ok(None),
        }))
    }
}

/// Browser ids stored on open nodes.
fn browser_id(n: &Node) -> Option<i64> {
    n.data.get("id").and_then(Value::as_i64)
}

fn is_protected(n: &Node) -> bool {
    n.custom_title.is_some()
        || n.custom_favicon.is_some()
        || !n.marks.is_empty()
        || n.children.iter().any(|c| matches!(c.kind, NodeKind::Note | NodeKind::Group | NodeKind::Separator))
}

/// Locations of the open windows and tabs in the tree.
/// Which browser profile an open window belongs to (`data.browser`).
fn browser_tag(n: &Node) -> Option<&str> {
    n.data.get("browser").and_then(Value::as_str)
}

#[derive(Default)]
struct Index {
    windows: HashMap<i64, NodeId>,
    tabs: HashMap<i64, NodeId>,
    /// Open tab nodes per window node, in tree (pre-)order.
    order: HashMap<NodeId, Vec<NodeId>>,
}

impl Index {
    /// Open windows of `browser` (and untagged ones, from before tagging).
    fn build(tree: &Tree, browser: Option<&str>) -> Index {
        let mut ix = Index::default();
        for w in &tree.root.children {
            if w.kind != NodeKind::Window || browser_tag(w).is_some_and(|b| Some(b) != browser) {
                continue;
            }
            if let Some(id) = browser_id(w) {
                ix.windows.insert(id, w.id);
            }
            let mut order = Vec::new();
            w.walk(&mut |n, d| {
                if d > 0 && n.kind == NodeKind::Tab {
                    order.push(n.id);
                    if let Some(id) = browser_id(n) {
                        ix.tabs.insert(id, n.id);
                    }
                }
            });
            ix.order.insert(w.id, order);
        }
        ix
    }
}

/// Fields for an open tab node from a browser tab.
fn tab_fields(base: Option<&Node>, t: &BrowserTab) -> crate::ops::NodeFields {
    let mut f = base.map(Node::fields).unwrap_or_else(|| Node::new(NodeKind::Tab).fields());
    f.kind = NodeKind::Tab;
    f.title = t.title.clone().or(f.title);
    f.url = t.url().map(str::to_owned).or(f.url);
    // Keep the old favicon while a page is loading without one.
    if let Some(icon) = t.fav_icon_url.clone().filter(|i| !i.is_empty()) {
        f.favicon = Some(icon);
    }
    f.data.insert("id".into(), json!(t.id));
    f.data.insert("windowId".into(), json!(t.window_id));
    set_or_remove(&mut f.data, "pinned", t.pinned.then_some(json!(true)));
    set_or_remove(&mut f.data, "discarded", t.discarded.then_some(json!(true)));
    set_or_remove(&mut f.data, "openerTabId", t.opener_tab_id.map(|v| json!(v)));
    set_or_remove(&mut f.data, "groupId", t.group_id.filter(|&g| g >= 0).map(|v| json!(v)));
    f.data.remove("crashDetectedDate");
    f
}

fn window_fields(base: Option<&Node>, w: &BrowserWindow, browser: Option<&str>) -> crate::ops::NodeFields {
    let mut f = base.map(Node::fields).unwrap_or_else(|| Node::new(NodeKind::Window).fields());
    f.kind = NodeKind::Window;
    f.data.insert("id".into(), json!(w.id));
    if let Some(b) = browser {
        f.data.insert("browser".into(), json!(b));
    }
    set_or_remove(&mut f.data, "type", w.kind.clone().map(Value::String));
    set_or_remove(&mut f.data, "state", w.state.clone().map(Value::String));
    if let (Some(l), Some(t), Some(wd), Some(h)) = (w.left, w.top, w.width, w.height) {
        // Same encoding as Tabs Outliner.
        f.data.insert("rect".into(), json!(format!("{l}_{t}_{wd}_{h}")));
    }
    if w.incognito {
        f.data.insert("incognito".into(), json!(true));
    }
    f.data.remove("closeDate");
    f.data.remove("crashDetectedDate");
    f
}

fn set_or_remove(m: &mut serde_json::Map<String, Value>, k: &str, v: Option<Value>) {
    match v {
        Some(v) => {
            m.insert(k.into(), v);
        }
        None => {
            m.remove(k);
        }
    }
}

/// Saved version of an open node's fields.
fn saved_fields(n: &Node, note_key: Option<(&str, i64)>) -> crate::ops::NodeFields {
    let mut f = n.fields();
    f.kind = match n.kind {
        NodeKind::Window => NodeKind::SavedWindow,
        NodeKind::Tab => NodeKind::SavedTab,
        ref k => k.clone(),
    };
    // Browser ids are meaningless once closed.
    f.data.remove("id");
    f.data.remove("windowId");
    f.data.remove("openerTabId");
    if let Some((k, v)) = note_key {
        f.data.insert(k.into(), json!(v));
    }
    f
}

#[derive(Default)]
pub struct Engine {
    /// Tabs closed without `isWindowClosing`, kept for the window-close check.
    recent_tab_trash: Vec<(i64, NodeId, i64)>, // (window id, node id, time)
    /// Windows for which Chrome did report `isWindowClosing`: their close
    /// was reported correctly, so earlier tab closes were real.
    closing_reported: HashSet<i64>,
    /// `windows.onCreated` details, used when the window's first tab arrives.
    new_windows: HashMap<i64, BrowserWindow>,
    /// Profile id of the browser whose events are being handled. Windows of
    /// other profiles are left alone (window and tab ids are per browser).
    pub browser: Option<String>,
}

impl Engine {
    pub fn new() -> Self {
        Engine::default()
    }

    /// Handle one event. Returns how many ops were applied.
    pub fn handle(&mut self, t: &mut impl Apply, ev: &BrowserEvent) -> Result<usize, EditError> {
        let mut n = 0;
        let mut ap = |t: &mut dyn ApplyDyn, op: Op| -> Result<(), EditError> {
            n += 1;
            t.apply_dyn(op)
        };
        let t: &mut dyn ApplyDyn = t;
        match ev {
            // Window nodes are created with their first tracked tab, so
            // windows holding only our own pages never show up.
            BrowserEvent::WindowCreated(w) => {
                self.new_windows.retain(|_, x| x.id != w.id);
                if self.new_windows.len() > 100 {
                    self.new_windows.clear();
                }
                self.new_windows.insert(w.id, w.clone());
            }
            BrowserEvent::WindowBoundsChanged(w) => {
                let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
                if let Some(&nid) = ix.windows.get(&w.id) {
                    let node = t.tree_dyn().root.find(nid).expect("indexed");
                    let f = window_fields(Some(node), w, self.browser.as_deref());
                    if f != node.fields() {
                        ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
                    }
                }
            }
            BrowserEvent::WindowRemoved(wid) => self.window_removed(t, *wid, &mut ap)?,
            BrowserEvent::TabCreated(tab) if tab.is_own_page() => {}
            // Firefox creates the tab as about:blank first: once it turns
            // out to be our own page, it goes (children move up).
            BrowserEvent::TabUpdated(tab) if tab.is_own_page() => {
                let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
                if let Some(&nid) = ix.tabs.get(&tab.id) {
                    let (p, i) = t.tree_dyn().root.locate(nid).ok_or(EditError::NotFound(nid))?;
                    let kids: Vec<NodeId> = t.tree_dyn().root.find(nid).expect("indexed").children.iter().map(|c| c.id).collect();
                    for (k, c) in kids.into_iter().enumerate() {
                        ap(t, Op::Move { id: c, parent: p, index: i + 1 + k })?;
                    }
                    ap(t, Op::Delete { id: nid })?;
                    // Its window was made for it alone (the tree window popup).
                    if let Some(w) = t.tree_dyn().root.find(p)
                        && w.kind == NodeKind::Window
                        && w.children.is_empty()
                    {
                        ap(t, Op::Delete { id: p })?;
                    }
                }
            }
            BrowserEvent::TabCreated(tab) => {
                let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
                if !ix.tabs.contains_key(&tab.id) {
                    self.insert_tab(t, tab, &mut ap)?;
                }
            }
            BrowserEvent::TabUpdated(tab) => {
                let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
                match ix.tabs.get(&tab.id) {
                    Some(&nid) => {
                        let node = t.tree_dyn().root.find(nid).expect("indexed");
                        let f = tab_fields(Some(node), tab);
                        if f != node.fields() {
                            ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
                        }
                    }
                    // Missed its creation: add it now.
                    None => self.insert_tab(t, tab, &mut ap)?,
                }
            }
            BrowserEvent::TabRemoved { tab_id, window_id, window_closing } => {
                let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
                let Some(&nid) = ix.tabs.get(tab_id) else { return Ok(0) };
                let node = t.tree_dyn().root.find(nid).expect("indexed");
                if *window_closing {
                    self.closing_reported.insert(*window_id);
                }
                if *window_closing || is_protected(node) {
                    let f = saved_fields(node, None);
                    ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
                } else {
                    self.trash_promoting_children(t, nid, &mut ap)?;
                    let now = now_ms();
                    self.recent_tab_trash.retain(|(_, _, at)| now - at < WINDOW_CLOSE_GRACE_MS);
                    self.recent_tab_trash.push((*window_id, nid, now));
                }
            }
            BrowserEvent::TabMoved { tab_id, window_id, to } => {
                self.place_existing(t, *tab_id, *window_id, *to, &mut ap)?;
            }
            BrowserEvent::TabAttached { tab_id, window_id, position } => {
                self.place_existing(t, *tab_id, *window_id, *position, &mut ap)?;
            }
            BrowserEvent::TabReplaced { added, removed } => {
                let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
                if let Some(&nid) = ix.tabs.get(removed) {
                    let node = t.tree_dyn().root.find(nid).expect("indexed");
                    let mut f = node.fields();
                    f.data.insert("id".into(), json!(added));
                    ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
                }
            }
        }
        Ok(n)
    }

    /// Bring the tree in line with a full list of live windows (with tabs).
    /// Used after every (re)connect, so missed events don't matter.
    pub fn reconcile(&mut self, t: &mut impl Apply, live: &[BrowserWindow]) -> Result<usize, EditError> {
        let mut n = 0;
        let mut ap = |t: &mut dyn ApplyDyn, op: Op| -> Result<(), EditError> {
            n += 1;
            t.apply_dyn(op)
        };
        let t: &mut dyn ApplyDyn = t;
        // Leave out our own pages, and windows that only hold them.
        let live: Vec<BrowserWindow> = live
            .iter()
            .map(|w| BrowserWindow { tabs: w.tabs.iter().filter(|t| !t.is_own_page()).cloned().collect(), ..w.clone() })
            .filter(|w| !w.tabs.is_empty())
            .collect();
        let live = &live[..];
        let tree = t.tree_dyn();

        // Stored open windows and their open tabs' URLs.
        struct Stored {
            node: NodeId,
            browser_id: Option<i64>,
            urls: Vec<String>,
        }
        let stored: Vec<Stored> = tree
            .root
            .children
            .iter()
            .filter(|w| w.kind == NodeKind::Window && browser_tag(w).is_none_or(|b| Some(b) == self.browser.as_deref()))
            .map(|w| {
                let mut urls = Vec::new();
                w.walk(&mut |n, d| {
                    if d > 0 && n.kind == NodeKind::Tab
                        && let Some(u) = &n.url
                    {
                        urls.push(u.clone());
                    }
                });
                Stored { node: w.id, browser_id: browser_id(w), urls }
            })
            .collect();

        let live_urls: Vec<Vec<String>> = live
            .iter()
            .map(|w| w.tabs.iter().filter_map(|t| t.url().map(str::to_owned)).collect())
            .collect();

        // Score every pair; same browser id gets a bonus (same session).
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for (li, lw) in live.iter().enumerate() {
            for (si, sw) in stored.iter().enumerate() {
                let mut s = similarity(&live_urls[li], &sw.urls);
                if sw.browser_id == Some(lw.id) {
                    s += 0.25;
                }
                if s >= WINDOW_MATCH_MIN {
                    pairs.push((s, li, si));
                }
            }
        }
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut live_match: Vec<Option<usize>> = vec![None; live.len()];
        let mut stored_used = vec![false; stored.len()];
        for (_, li, si) in pairs {
            if live_match[li].is_none() && !stored_used[si] {
                live_match[li] = Some(si);
                stored_used[si] = true;
            }
        }

        // Stored windows without a live match were closed while we weren't
        // watching: save them, like a crash recovery.
        let now = now_ms();
        for (si, sw) in stored.iter().enumerate() {
            if !stored_used[si] {
                self.save_window_subtree(t, sw.node, Some(("crashDetectedDate", now)), &mut ap)?;
            }
        }

        for (li, lw) in live.iter().enumerate() {
            match live_match[li] {
                None => {
                    // Browser ids from the stored tree may clash with this one.
                    self.clear_stale_ids(t, lw, &mut ap)?;
                    self.insert_window(t, lw, &mut ap)?;
                    for tab in &lw.tabs {
                        self.insert_tab(t, tab, &mut ap)?;
                    }
                }
                Some(si) => {
                    let wnode = stored[si].node;
                    let node = t.tree_dyn().root.find(wnode).expect("stored");
                    let f = window_fields(Some(node), lw, self.browser.as_deref());
                    if f != node.fields() {
                        ap(t, Op::Update { id: wnode, fields: Box::new(f) })?;
                    }
                    self.reconcile_tabs(t, wnode, lw, &mut ap)?;
                }
            }
        }
        Ok(n)
    }

    fn reconcile_tabs(
        &mut self,
        t: &mut dyn ApplyDyn,
        wnode: NodeId,
        lw: &BrowserWindow,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        // Open tab nodes of this window, in order, with id and url.
        let mut nodes: Vec<(NodeId, Option<i64>, Option<String>)> = Vec::new();
        t.tree_dyn().root.find(wnode).expect("window").walk(&mut |n, d| {
            if d > 0 && n.kind == NodeKind::Tab {
                nodes.push((n.id, browser_id(n), n.url.clone()));
            }
        });
        let mut used = vec![false; nodes.len()];
        let mut matched: Vec<Option<NodeId>> = vec![None; lw.tabs.len()];
        // 1) same tab id and url
        for (i, tab) in lw.tabs.iter().enumerate() {
            if let Some(k) = (0..nodes.len()).find(|&k| {
                !used[k] && nodes[k].1 == Some(tab.id) && nodes[k].2.as_deref() == tab.url()
            }) {
                used[k] = true;
                matched[i] = Some(nodes[k].0);
            }
        }
        // 2) same url, first unused in order
        for (i, tab) in lw.tabs.iter().enumerate() {
            if matched[i].is_some() {
                continue;
            }
            if let Some(k) = (0..nodes.len()).find(|&k| !used[k] && nodes[k].2.as_deref() == tab.url()) {
                used[k] = true;
                matched[i] = Some(nodes[k].0);
            }
        }
        // Unmatched stored tabs: closed while we weren't watching -> saved.
        for (k, (nid, _, _)) in nodes.iter().enumerate() {
            if !used[k] {
                let node = t.tree_dyn().root.find(*nid).expect("tab");
                let f = saved_fields(node, Some(("crashDetectedDate", now_ms())));
                ap(t, Op::Update { id: *nid, fields: Box::new(f) })?;
            }
        }
        for (i, tab) in lw.tabs.iter().enumerate() {
            match matched[i] {
                Some(nid) => {
                    let node = t.tree_dyn().root.find(nid).expect("tab");
                    let f = tab_fields(Some(node), tab);
                    if f != node.fields() {
                        ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
                    }
                }
                None => {
                    self.clear_stale_tab_id(t, tab.id, ap)?;
                    self.insert_tab(t, tab, ap)?;
                }
            }
        }
        Ok(())
    }

    /// Drop browser ids from stored nodes that collide with a new live window.
    fn clear_stale_ids(
        &mut self,
        t: &mut dyn ApplyDyn,
        lw: &BrowserWindow,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
        if let Some(&nid) = ix.windows.get(&lw.id) {
            let mut f = t.tree_dyn().root.find(nid).expect("indexed").fields();
            f.data.remove("id");
            ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
        }
        for tab in &lw.tabs {
            self.clear_stale_tab_id(t, tab.id, ap)?;
        }
        Ok(())
    }

    fn clear_stale_tab_id(
        &mut self,
        t: &mut dyn ApplyDyn,
        tab_id: i64,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
        if let Some(&nid) = ix.tabs.get(&tab_id) {
            let mut f = t.tree_dyn().root.find(nid).expect("indexed").fields();
            f.data.remove("id");
            ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
        }
        Ok(())
    }

    fn insert_window(
        &mut self,
        t: &mut dyn ApplyDyn,
        w: &BrowserWindow,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<NodeId, EditError> {
        let mut node = Node::new(NodeKind::Window);
        node.set_fields(window_fields(None, w, self.browser.as_deref()));
        let id = node.id;
        let root = &t.tree_dyn().root;
        let (parent, index) = (root.id, root.children.len());
        ap(t, Op::Insert { parent, index, node: Box::new(node) })?;
        Ok(id)
    }

    /// Window node for a browser window id, creating it if needed.
    fn window_node(
        &mut self,
        t: &mut dyn ApplyDyn,
        window_id: i64,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<NodeId, EditError> {
        if let Some(&w) = Index::build(t.tree_dyn(), self.browser.as_deref()).windows.get(&window_id) {
            return Ok(w);
        }
        if let Some(w) = self.new_windows.remove(&window_id) {
            return self.insert_window(t, &w, ap);
        }
        let w = BrowserWindow {
            id: window_id,
            kind: None,
            state: None,
            left: None,
            top: None,
            width: None,
            height: None,
            incognito: false,
            tabs: vec![],
        };
        self.insert_window(t, &w, ap)
    }

    fn insert_tab(
        &mut self,
        t: &mut dyn ApplyDyn,
        tab: &BrowserTab,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        let wnode = self.window_node(t, tab.window_id, ap)?;
        let mut node = Node::new(NodeKind::Tab);
        node.set_fields(tab_fields(None, tab));
        let (parent, index) = placement(t.tree_dyn(), wnode, tab, None);
        ap(t, Op::Insert { parent, index, node: Box::new(node) })
    }

    /// Move an existing open tab node to browser position `to` in `window_id`.
    fn place_existing(
        &mut self,
        t: &mut dyn ApplyDyn,
        tab_id: i64,
        window_id: i64,
        to: usize,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
        let Some(&nid) = ix.tabs.get(&tab_id) else { return Ok(()) };
        let wnode = self.window_node(t, window_id, ap)?;
        let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
        let order = ix.order.get(&wnode).cloned().unwrap_or_default();
        // Already in the right place?
        let without: Vec<NodeId> = order.iter().copied().filter(|&n| n != nid).collect();
        if order.iter().position(|&n| n == nid) == Some(to) && order.len() == without.len() + 1 {
            // Placed already (the user dragged it there in the tree and we
            // moved the browser tab to match): just note its new window.
            let node = t.tree_dyn().root.find(nid).expect("tab");
            if node.data.get("windowId").and_then(Value::as_i64) != Some(window_id) {
                let mut f = node.fields();
                f.data.insert("windowId".into(), json!(window_id));
                ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
            }
            return Ok(());
        }
        // Children stay where they are: move them up into the node's place.
        let (p, i) = t.tree_dyn().root.locate(nid).ok_or(EditError::NotFound(nid))?;
        let kids: Vec<NodeId> = t.tree_dyn().root.find(nid).expect("tab").children.iter().map(|c| c.id).collect();
        for (k, c) in kids.into_iter().enumerate() {
            ap(t, Op::Move { id: c, parent: p, index: i + 1 + k })?;
        }
        let mut f = t.tree_dyn().root.find(nid).expect("tab").fields();
        f.data.insert("windowId".into(), json!(window_id));
        ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
        let fake = BrowserTab {
            id: tab_id,
            window_id,
            index: to,
            url: None,
            pending_url: None,
            title: None,
            fav_icon_url: None,
            pinned: false,
            discarded: false,
            opener_tab_id: None,
            group_id: None,
            incognito: false,
        };
        let (parent, index) = placement(t.tree_dyn(), wnode, &fake, Some(nid));
        ap(t, Op::Move { id: nid, parent, index })
    }

    fn trash_promoting_children(
        &mut self,
        t: &mut dyn ApplyDyn,
        nid: NodeId,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        let (p, i) = t.tree_dyn().root.locate(nid).ok_or(EditError::NotFound(nid))?;
        let kids: Vec<NodeId> = t.tree_dyn().root.find(nid).expect("node").children.iter().map(|c| c.id).collect();
        for (k, c) in kids.into_iter().enumerate() {
            ap(t, Op::Move { id: c, parent: p, index: i + 1 + k })?;
        }
        ap(t, Op::Trash { id: nid, at: now_ms() })
    }

    fn window_removed(
        &mut self,
        t: &mut dyn ApplyDyn,
        window_id: i64,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        let ix = Index::build(t.tree_dyn(), self.browser.as_deref());
        let Some(&wnode) = ix.windows.get(&window_id) else { return Ok(()) };
        // Undo tab closes that were really part of this window close.
        let now = now_ms();
        let reported = self.closing_reported.remove(&window_id);
        let recent: Vec<NodeId> = self
            .recent_tab_trash
            .iter()
            .filter(|(w, _, at)| !reported && *w == window_id && now - at < WINDOW_CLOSE_GRACE_MS)
            .map(|(_, n, _)| *n)
            .collect();
        self.recent_tab_trash.retain(|(w, _, _)| *w != window_id);
        for nid in recent.into_iter().rev() {
            let Some(ti) = t.tree_dyn().trash_index(nid) else { continue };
            let item = &t.tree_dyn().trash[ti];
            let (parent, index) = (item.parent, item.index);
            let parent_ok = t.tree_dyn().root.find(parent).is_some_and(|p| index <= p.children.len());
            let (parent, index) = if parent_ok { (parent, index) } else { (wnode, 0) };
            ap(t, Op::Restore { id: nid, parent, index })?;
        }
        let has_children = !t.tree_dyn().root.find(wnode).expect("window").children.is_empty();
        if has_children || is_protected(t.tree_dyn().root.find(wnode).expect("window")) {
            self.save_window_subtree(t, wnode, Some(("closeDate", now)), ap)
        } else {
            ap(t, Op::Trash { id: wnode, at: now })
        }
    }

    /// Turn a window and every open tab inside it into saved nodes.
    fn save_window_subtree(
        &mut self,
        t: &mut dyn ApplyDyn,
        wnode: NodeId,
        stamp: Option<(&str, i64)>,
        ap: &mut dyn FnMut(&mut dyn ApplyDyn, Op) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        let mut open = Vec::new();
        t.tree_dyn().root.find(wnode).expect("window").walk(&mut |n, d| {
            if d > 0 && n.kind == NodeKind::Tab {
                open.push(n.id);
            }
        });
        for nid in open {
            let f = saved_fields(t.tree_dyn().root.find(nid).expect("tab"), None);
            ap(t, Op::Update { id: nid, fields: Box::new(f) })?;
        }
        let f = saved_fields(t.tree_dyn().root.find(wnode).expect("window"), stamp);
        ap(t, Op::Update { id: wnode, fields: Box::new(f) })
    }
}

/// Where to insert an open tab so the tree's open-tab order matches the
/// browser's tab order. `moving` is excluded from the order.
fn placement(tree: &Tree, wnode: NodeId, tab: &BrowserTab, moving: Option<NodeId>) -> (NodeId, usize) {
    let win = tree.root.find(wnode).expect("window");
    let mut order: Vec<NodeId> = Vec::new();
    win.walk(&mut |n, d| {
        if d > 0 && n.kind == NodeKind::Tab && Some(n.id) != moving {
            order.push(n.id);
        }
    });
    let i = tab.index.min(order.len());
    let prev = i.checked_sub(1).map(|k| order[k]);
    let next = order.get(i).copied();

    // Nest under the opener when the tab lands right after the opener's
    // subtree part that ends at `prev`.
    if let (Some(op_tab), Some(prev)) = (tab.opener_tab_id, prev)
        && !tab.is_new_tab_page()
    {
        let opener = win.find_by(|n| n.kind == NodeKind::Tab && browser_id(n) == Some(op_tab) && Some(n.id) != moving);
        if let Some(opener) = opener
            && opener.contains(prev)
            && next.is_none_or(|nx| !opener.contains(nx) || opener.children.iter().any(|c| c.id == nx))
        {
            // Child of the opener, right after the child holding `prev`.
            let index = if opener.id == prev {
                0
            } else {
                opener
                    .children
                    .iter()
                    .filter(|c| Some(c.id) != moving)
                    .position(|c| c.contains(prev))
                    .map_or(0, |k| k + 1)
            };
            return (opener.id, index);
        }
    }
    // Right before the next open tab, as its sibling.
    if let Some(nx) = next {
        let (p, _) = tree.root.locate(nx).expect("in tree");
        let kids: Vec<NodeId> = tree.root.find(p).expect("parent").children.iter().map(|c| c.id).filter(|&c| Some(c) != moving).collect();
        let k = kids.iter().position(|&c| c == nx).expect("child");
        return (p, k);
    }
    // At the end of the window.
    let len = win.children.iter().filter(|c| Some(c.id) != moving).count();
    (wnode, len)
}

impl Node {
    /// First node in this subtree (pre-order) matching `f`.
    pub fn find_by(&self, f: impl Fn(&Node) -> bool + Copy) -> Option<&Node> {
        if f(self) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find_by(f))
    }
}

/// Share of URLs two windows have in common (multiset Jaccard).
fn similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let mut count: HashMap<&str, i64> = HashMap::new();
    for u in a {
        *count.entry(u).or_default() += 1;
    }
    let mut common = 0;
    let mut seen: HashSet<usize> = HashSet::new();
    for (k, u) in b.iter().enumerate() {
        if let Some(c) = count.get_mut(u.as_str())
            && *c > 0
        {
            *c -= 1;
            common += 1;
            seen.insert(k);
        }
    }
    let union = a.len() + b.len() - common;
    common as f64 / union as f64
}

/// Object-safe view of [`Apply`] used internally.
trait ApplyDyn {
    fn tree_dyn(&self) -> &Tree;
    fn apply_dyn(&mut self, op: Op) -> Result<(), EditError>;
}

impl<T: Apply> ApplyDyn for T {
    fn tree_dyn(&self) -> &Tree {
        self.tree()
    }
    fn apply_dyn(&mut self, op: Op) -> Result<(), EditError> {
        self.apply_op(op)
    }
}

#[cfg(test)]
mod tests;
