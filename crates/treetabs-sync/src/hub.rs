//! The tree's owner, without any transport: it takes client messages,
//! runs the sync [`Engine`] and the bookmarks bridge on the store, and
//! collects the messages to send back.
//!
//! Two callers run it: the [`server`](crate::server) behind a Unix socket
//! (the companion app or the GUI), and the extension on its own, which
//! keeps the store in the browser's extension storage. What differs
//! between them (files, backups, moving the store) is a [`Backend`].

use crate::bridge::Browser;
use crate::protocol::{ClientMsg, Role, ServerMsg, StoreKind};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::time::Duration;
use treetabs_core::backups::BackupInfo;
use treetabs_core::ops::now_ms;
use treetabs_core::store::TreeStore;
use treetabs_core::sync::{Apply, BrowserEvent, BrowserWindow, Engine};
use treetabs_core::{EditError, NodeKind, Op, Tree};
use web_time::Instant;

/// How often the bookmark mirror checks for work.
pub const TICK: Duration = Duration::from_millis(250);

/// How often to check whether today's export is written.
const EXPORT_CHECK: Duration = Duration::from_secs(60);

/// Whether an export is skipped when today's copy exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportWhen {
    /// Once a day: nothing to do when today's copy exists.
    Daily,
    /// Now, replacing today's copy.
    Now,
}

/// Where the hub keeps what is not the tree itself.
pub trait Backend {
    type Store: TreeStore;

    /// A small file of the store (`settings.json`, `profiles.json`,
    /// `bookmarks.json`), if it exists.
    fn read(&self, name: &str) -> Option<Vec<u8>>;

    /// Replace a small file of the store.
    ///
    /// # Errors
    ///
    /// When it cannot be written.
    fn write(&mut self, name: &str, data: &[u8]) -> io::Result<()>;

    /// Where the store is, for people: a folder, or a description.
    fn location(&self) -> String;

    fn kind(&self) -> StoreKind;

    /// Daily copies kept when `settings.json` does not say.
    fn default_daily_exports(&self) -> usize {
        30
    }

    /// All backup copies, newest first.
    fn backups(&self) -> Vec<BackupInfo>;

    /// One backup's tree. `None` when the answer comes later: the backend
    /// then sends [`ServerMsg::BackupTree`] to `client` itself.
    fn read_backup(&mut self, client: u64, name: &str) -> Option<Result<Tree, String>>;

    /// Write today's copy (keeping the newest `keep`). Returns where it
    /// went, or `None` when [`ExportWhen::Daily`] found it written already.
    ///
    /// # Errors
    ///
    /// When the copy cannot be written.
    fn export(
        &mut self,
        tree: &Tree,
        keep: usize,
        now: i64,
        when: ExportWhen,
    ) -> io::Result<Option<String>>;

    /// Keep `tree` as a backup named `name` (a plain `.json` file name),
    /// outside the daily rotation. Returns the name it got.
    ///
    /// # Errors
    ///
    /// When it cannot be written.
    fn save_backup(&mut self, tree: &Tree, name: &str) -> io::Result<String>;

    /// Copy the store to the folder `to` and use it from now on. Returns
    /// the new place.
    ///
    /// # Errors
    ///
    /// When this backend cannot move, or the copy fails.
    fn move_store(&mut self, _store: &mut Self::Store, _to: &str) -> io::Result<String> {
        Err(io::Error::other(
            "this tree is kept in the browser and cannot be moved",
        ))
    }
}

/// `settings.json`.
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Settings {
    /// Mirror the tree into a bookmark folder.
    bookmarks: Option<bool>,
    /// Daily exports to keep (0 = no exports).
    daily_exports: Option<usize>,
    /// Remove trash items older than this many days (0 = never).
    trash_days: u32,
}

/// Change one key of `settings.json`, keeping the others.
fn write_setting(backend: &mut impl Backend, key: &str, value: Value) -> io::Result<()> {
    let mut v: Value = backend
        .read("settings.json")
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    v[key] = value;
    let json = serde_json::to_vec_pretty(&v).map_err(io::Error::other)?;
    backend.write("settings.json", &json)
}

struct Client {
    role: Option<Role>,
    /// Browser profile id, for browser bridges.
    browser: Option<String>,
    name: String,
}

/// Records every op applied, so it can be broadcast afterwards.
struct Recorder<'a, S> {
    store: &'a mut S,
    ops: Vec<Op>,
}

impl<S: TreeStore> Apply for Recorder<'_, S> {
    fn tree(&self) -> &Tree {
        self.store.tree()
    }
    fn apply_op(&mut self, op: Op) -> Result<(), EditError> {
        let rec = op.clone();
        self.store.apply_op(op)?;
        self.ops.push(rec);
        Ok(())
    }
}

/// Window and tab ids that exist in a browser right now, per profile.
#[derive(Default)]
struct Live {
    windows: HashSet<i64>,
    tabs: HashSet<i64>,
}

pub struct Hub<B: Backend> {
    pub backend: B,
    store: B::Store,
    engine: Engine,
    browser: Browser,
    clients: HashMap<u64, Client>,
    /// Messages to deliver, in order.
    out: Vec<(u64, ServerMsg)>,
    /// Live ids per browser profile, to tell open nodes from stale ones.
    live: HashMap<Option<String>, Live>,
    daily_exports: usize,
    trash_days: u32,
    /// Readable names of browser profiles (`profiles.json`).
    profiles: BTreeMap<String, String>,
    last_tick: Instant,
    last_export: Instant,
    stopping: bool,
}

impl<B: Backend> Hub<B> {
    pub fn new(store: B::Store, backend: B) -> Self {
        let settings: Settings = backend
            .read("settings.json")
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let bookmarks = settings.bookmarks.unwrap_or(true)
            && !std::env::var("TREETABS_BOOKMARKS").is_ok_and(|v| v == "0");
        let mut browser = Browser::default();
        browser
            .mirror
            .configure(bookmarks, backend.read("bookmarks.json").as_deref());
        let profiles = backend
            .read("profiles.json")
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Hub {
            daily_exports: settings
                .daily_exports
                .unwrap_or_else(|| backend.default_daily_exports()),
            trash_days: settings.trash_days,
            backend,
            store,
            engine: Engine::new(),
            browser,
            clients: HashMap::new(),
            out: Vec::new(),
            live: HashMap::new(),
            profiles,
            last_tick: Instant::now(),
            // First check a minute after start: by then the browser has
            // sent its windows, so the copy is not missing the open tabs.
            last_export: Instant::now(),
            stopping: false,
        }
    }

    pub fn store(&self) -> &B::Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut B::Store {
        &mut self.store
    }

    pub fn tree(&self) -> &Tree {
        self.store.tree()
    }

    /// The messages to deliver since the last call, in order.
    pub fn take_out(&mut self) -> Vec<(u64, ServerMsg)> {
        std::mem::take(&mut self.out)
    }

    /// The store was moved: the caller stops after delivering the messages.
    pub fn stopping(&self) -> bool {
        self.stopping
    }

    /// Call often (every [`TICK`] or more): bookmark sync, daily copy and
    /// trash expiry run when they are due.
    pub fn tick(&mut self) {
        if self.last_tick.elapsed() >= TICK {
            self.last_tick = Instant::now();
            let calls = self.browser.tick(self.store.tree());
            self.dispatch_calls(calls);
            self.save_bookmark_map();
        }
        if self.last_export.elapsed() >= EXPORT_CHECK {
            self.last_export = Instant::now();
            self.daily_export();
            self.expire_trash();
        }
    }

    /// A new client; it says who it is with [`ClientMsg::Hello`].
    pub fn connected(&mut self, id: u64) {
        self.clients.insert(
            id,
            Client {
                role: None,
                browser: None,
                name: String::new(),
            },
        );
    }

    pub fn closed(&mut self, id: u64) {
        if let Some(c) = self.clients.remove(&id) {
            log::info!("client {id} ({}) disconnected", c.name);
            if c.role == Some(Role::Browser) {
                self.browser.disconnected(id);
                self.save_bookmark_map();
            }
            self.status("");
        }
    }

    fn save_bookmark_map(&mut self) {
        if let Some(b) = self.browser.mirror.take_saved()
            && let Err(e) = self.backend.write("bookmarks.json", &b)
        {
            log::warn!("cannot save bookmarks.json: {e}");
        }
    }

    fn daily_export(&mut self) {
        match self.backend.export(
            self.store.tree(),
            self.daily_exports,
            now_ms(),
            ExportWhen::Daily,
        ) {
            Ok(Some(p)) => log::info!("daily export: {p}"),
            Ok(None) => {}
            Err(e) => log::warn!("daily export failed: {e}"),
        }
    }

    /// Drop trash items older than `trash_days` (they stay in the daily
    /// exports).
    fn expire_trash(&mut self) {
        if self.trash_days == 0 {
            return;
        }
        let before = now_ms() - i64::from(self.trash_days) * 86_400_000;
        let old = self
            .store
            .tree()
            .trash
            .iter()
            .filter(|t| t.removed_at < before)
            .count();
        if old > 0 {
            log::info!(
                "removing {old} trash item(s) older than {} days",
                self.trash_days
            );
            // Today's copy keeps them, so they can still be brought back.
            if self.daily_exports > 0
                && let Err(e) = self.backend.export(
                    self.store.tree(),
                    self.daily_exports,
                    now_ms(),
                    ExportWhen::Now,
                )
            {
                log::warn!("export before removing old trash failed: {e}");
            }
            self.apply_ops(vec![Op::PurgeTrash { before }]);
        }
    }

    fn backups_msg(&self, message: Option<String>) -> ServerMsg {
        ServerMsg::Backups {
            list: self.backend.backups(),
            dir: self.backend.location(),
            message,
        }
    }

    fn send(&mut self, id: u64, m: ServerMsg) {
        if self.clients.contains_key(&id) {
            self.out.push((id, m));
        }
    }

    fn broadcast_viewers(&mut self, m: &ServerMsg) {
        let viewers: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, c)| c.role == Some(Role::Viewer))
            .map(|(&id, _)| id)
            .collect();
        for id in viewers {
            self.out.push((id, m.clone()));
        }
    }

    fn browsers(&self) -> usize {
        self.clients
            .values()
            .filter(|c| c.role == Some(Role::Browser))
            .count()
    }

    fn status(&mut self, message: &str) {
        let m = ServerMsg::Status {
            browsers: self.browsers(),
            message: message.to_owned(),
        };
        self.broadcast_viewers(&m);
    }

    fn published(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        let m = ServerMsg::Applied {
            seq: self.store.seq(),
            ops,
        };
        self.broadcast_viewers(&m);
        self.after_change();
    }

    /// Run `f` with a recording view of the store and broadcast what it did.
    fn with_engine(
        &mut self,
        f: impl FnOnce(&mut Engine, &mut Recorder<B::Store>) -> Result<usize, EditError>,
    ) {
        let mut rec = Recorder {
            store: &mut self.store,
            ops: Vec::new(),
        };
        if let Err(e) = f(&mut self.engine, &mut rec) {
            log::error!("sync error: {e}");
        }
        let ops = rec.ops;
        self.published(ops);
    }

    fn after_change(&mut self) {
        let calls = self.browser.tree_changed(self.store.tree());
        self.dispatch_calls(calls);
    }

    fn dispatch_calls(&mut self, calls: Vec<(u64, ServerMsg)>) {
        for (client, m) in calls {
            self.send(client, m);
        }
    }

    pub fn message(&mut self, id: u64, m: ClientMsg) {
        match m {
            ClientMsg::Hello { role, name } => self.on_hello(id, role, name),
            ClientMsg::BrowserSnapshot {
                windows,
                browser,
                label,
            } => self.on_browser_snapshot(id, windows, browser, label),
            ClientMsg::BrowserEvent { name, args } => self.on_browser_event(id, &name, &args),
            ClientMsg::AdoptTree {
                tree,
                bookmarks,
                since,
            } => self.on_adopt(id, *tree, bookmarks, since),
            ClientMsg::UseCompanionApp { on } => {
                if !on {
                    self.on_detach(id);
                }
            }
            ClientMsg::CallResult { req, ok, err } => {
                let (ops, calls) = self.browser.call_result(req, ok, err, self.store.tree());
                self.apply_ops(ops);
                self.dispatch_calls(calls);
            }
            ClientMsg::Edit { req, ops } => self.on_edit(id, req, ops),
            ClientMsg::Activate { id: node } => {
                let calls = self.browser.activate(node, self.store.tree());
                if calls.is_empty() {
                    self.send_status(id, "no browser connected".into());
                }
                self.dispatch_calls(calls);
            }
            ClientMsg::Unload { ids } => {
                let calls = self.browser.unload(&ids, self.store.tree());
                self.dispatch_calls(calls);
            }
            ClientMsg::CloseInBrowser { ids } => {
                let calls = self.browser.close(&ids, self.store.tree());
                self.dispatch_calls(calls);
            }
            ClientMsg::ListBackups => {
                let m = self.backups_msg(None);
                self.send(id, m);
            }
            ClientMsg::ExportNow => self.on_export_now(id),
            ClientMsg::ReadBackup { name } => self.on_read_backup(id, name),
            ClientMsg::SetTrashDays { days } => self.on_set_trash_days(days),
            ClientMsg::MoveStore { dir } => self.on_move_store(id, &dir),
        }
    }

    fn send_status(&mut self, id: u64, message: String) {
        let m = ServerMsg::Status {
            browsers: self.browsers(),
            message,
        };
        self.send(id, m);
    }

    fn store_options(&self) -> ServerMsg {
        ServerMsg::StoreOptions {
            trash_days: self.trash_days,
            dir: self.backend.location(),
            kind: self.backend.kind(),
        }
    }

    fn viewer_snapshot(&self) -> ServerMsg {
        ServerMsg::Snapshot {
            seq: self.store.seq(),
            tree: Box::new(self.store.tree().clone()),
        }
    }

    /// A client says who it is: a viewer gets the tree and the options, a
    /// browser is asked for all its windows.
    fn on_hello(&mut self, id: u64, role: Role, name: String) {
        log::info!("client {id}: {name} ({role:?})");
        if let Some(c) = self.clients.get_mut(&id) {
            c.role = Some(role);
            c.name = name;
        }
        match role {
            Role::Viewer => {
                let snap = self.viewer_snapshot();
                self.send(id, snap);
                let opts = self.store_options();
                self.send(id, opts);
                let names = self.profiles.clone();
                self.send(id, ServerMsg::Profiles { names });
                self.status("");
            }
            Role::Browser => {
                self.browser.connected(id);
                self.send(id, ServerMsg::Resync);
                self.status("browser connected");
            }
        }
    }

    /// The tree the extension kept on its own: the store's tree when the
    /// store is new, a backup otherwise.
    /// A browser stops using the companion app: its tree window gets the
    /// tree and the bookmark map to take along. The store keeps them too.
    fn on_detach(&mut self, id: u64) {
        self.save_bookmark_map();
        let bookmarks = self
            .backend
            .read("bookmarks.json")
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        log::info!("a browser takes the tree along (seq {})", self.store.seq());
        let m = ServerMsg::Detached {
            seq: self.store.seq(),
            tree: Box::new(self.store.tree().clone()),
            bookmarks,
        };
        self.send(id, m);
    }

    fn on_adopt(&mut self, id: u64, tree: Tree, bookmarks: Option<String>, since: Option<u64>) {
        let nodes = tree.root.count().saturating_sub(1);
        // A tree that left this store and comes back to it unchanged.
        let returning = since.is_some_and(|s| s == self.store.seq());
        let into_store = self.store.is_new() || returning;
        if into_store {
            match self.store.adopt(tree.clone()) {
                Ok(()) => {
                    log::info!("adopted the browser's tree ({nodes} nodes)");
                    // Its bookmark folder stays bound to the same nodes.
                    if let Some(map) = bookmarks {
                        if let Err(e) = self.backend.write("bookmarks.json", map.as_bytes()) {
                            log::warn!("cannot save bookmarks.json: {e}");
                        }
                        let enabled = self.browser.mirror.enabled;
                        self.browser.mirror.configure(enabled, Some(map.as_bytes()));
                    }
                    self.send(id, ServerMsg::Adopted { backup: None });
                    let snap = self.viewer_snapshot();
                    self.broadcast_viewers(&snap);
                    self.status("The tree kept in the browser is now stored here");
                    self.after_change();
                    return;
                }
                Err(e) => log::warn!("cannot adopt the browser's tree: {e}"),
            }
        }
        let name = format!("from-browser-{}.json", now_ms());
        match self.backend.save_backup(&tree, &name) {
            Ok(name) => {
                log::info!("browser's tree ({nodes} nodes) saved as backup {name}");
                // Its bookmark folder mirrors that tree, not this one.
                self.browser.mirror.leave_folder();
                self.send(
                    id,
                    ServerMsg::Adopted {
                        backup: Some(name.clone()),
                    },
                );
                self.status(&format!(
                    "The tree kept in the browser was saved as the backup {name}: Data → Backups can add it to the tree"
                ));
            }
            // No answer: the extension keeps its copy and offers it again.
            Err(e) => log::warn!("cannot save the browser's tree: {e}"),
        }
    }

    /// All windows of a browser profile: remember the profile's name and
    /// its live ids, then bring the tree in line.
    fn on_browser_snapshot(
        &mut self,
        id: u64,
        windows: Vec<Value>,
        browser: Option<String>,
        label: Option<String>,
    ) {
        if let Some(c) = self.clients.get_mut(&id) {
            c.browser = browser.clone();
        }
        if let (Some(b), Some(l)) = (&browser, label)
            && self.profiles.get(b) != Some(&l)
        {
            self.profiles.insert(b.clone(), l);
            if let Ok(j) = serde_json::to_vec_pretty(&self.profiles)
                && let Err(e) = self.backend.write("profiles.json", &j)
            {
                log::warn!("cannot save profiles.json: {e}");
            }
            let names = self.profiles.clone();
            self.broadcast_viewers(&ServerMsg::Profiles { names });
        }
        self.engine.browser = browser;
        let parsed: Vec<BrowserWindow> = windows
            .into_iter()
            .filter_map(|w| {
                serde_json::from_value(w)
                    .inspect_err(|e| log::warn!("bad window in snapshot: {e}"))
                    .ok()
            })
            .collect();
        log::info!("browser snapshot: {} window(s)", parsed.len());
        let live = Live {
            windows: parsed.iter().map(|w| w.id).collect(),
            tabs: parsed
                .iter()
                .flat_map(|w| w.tabs.iter().map(|t| t.id))
                .collect(),
        };
        self.live.insert(self.engine.browser.clone(), live);
        self.with_engine(|e, rec| e.reconcile(rec, &parsed));
        let calls = self.browser.resynced(id, self.store.tree());
        self.dispatch_calls(calls);
    }

    /// A `chrome.*` event: bookmark events go to the mirror, the others to
    /// the sync engine (unless they answer one of our own requests).
    fn on_browser_event(&mut self, id: u64, name: &str, args: &[Value]) {
        self.engine.browser = self.clients.get(&id).and_then(|c| c.browser.clone());
        if name.starts_with("bookmarks.") {
            let (ops, calls) = self.browser.bookmark_event(name, args, self.store.tree());
            self.apply_ops(ops);
            self.dispatch_calls(calls);
            return;
        }
        match BrowserEvent::from_chrome(name, args) {
            Ok(Some(ev)) => {
                log::debug!("event {name}: {ev:?}");
                self.track_live(&ev);
                let calls = self.browser.before_event(&ev, self.store.tree());
                self.dispatch_calls(calls);
                match self.browser.take_expectation(&ev, self.store.tree()) {
                    Some(ops) => self.apply_ops(ops),
                    None => self.with_engine(|e, rec| e.handle(rec, &ev)),
                }
            }
            Ok(None) => {}
            Err(e) => log::warn!("bad {name} event: {e}"),
        }
    }

    /// A viewer's edit: applied as one step, answered with the ops that
    /// undo it, and carried out in the browser where it closes tabs.
    fn on_edit(&mut self, id: u64, req: u64, ops: Vec<Op>) {
        log::debug!(
            "edit {req}: {:?}",
            ops.iter().map(Op::name).collect::<Vec<_>>()
        );
        match self.store.apply_batch(ops.clone()) {
            Ok(inv) => {
                let mut inverse: Vec<Op> = inv.into_iter().flatten().collect();
                inverse.reverse();
                self.send(
                    id,
                    ServerMsg::EditResult {
                        req,
                        inverse,
                        err: None,
                    },
                );
                let calls = self.browser.user_edit(&ops, self.store.tree());
                let fix = self.stale_open_nodes(&ops);
                log::debug!("stale open nodes fixed: {}", fix.len());
                self.published(ops);
                self.dispatch_calls(calls);
                self.apply_ops(fix);
            }
            Err(e) => self.send(
                id,
                ServerMsg::EditResult {
                    req,
                    inverse: vec![],
                    err: Some(e.to_string()),
                },
            ),
        }
    }

    fn on_export_now(&mut self, id: u64) {
        let keep = self.daily_exports.max(1);
        let message = match self
            .backend
            .export(self.store.tree(), keep, now_ms(), ExportWhen::Now)
        {
            Ok(p) => format!("Saved {}", p.unwrap_or_default()),
            Err(e) => format!("Export failed: {e}"),
        };
        let m = self.backups_msg(Some(message));
        self.send(id, m);
    }

    fn on_read_backup(&mut self, id: u64, name: String) {
        let Some(r) = self.backend.read_backup(id, &name) else {
            return;
        };
        let msg = match r {
            Ok(t) => ServerMsg::BackupTree {
                name,
                tree: Some(Box::new(t)),
                err: None,
            },
            Err(e) => ServerMsg::BackupTree {
                name,
                tree: None,
                err: Some(e),
            },
        };
        self.send(id, msg);
    }

    fn on_set_trash_days(&mut self, days: u32) {
        self.trash_days = days;
        if let Err(e) = write_setting(&mut self.backend, "trash_days", days.into()) {
            log::warn!("cannot save settings.json: {e}");
        }
        let opts = self.store_options();
        self.broadcast_viewers(&opts);
        self.expire_trash();
    }

    /// Move the store; on success every client is told and the hub stops,
    /// so whoever runs next opens the new folder.
    fn on_move_store(&mut self, id: u64, dir: &str) {
        match self.backend.move_store(&mut self.store, dir) {
            Ok(new) => {
                log::info!("store moved to {new}");
                let msg = ServerMsg::StoreMoved { dir: new };
                let all: Vec<u64> = self.clients.keys().copied().collect();
                for c in all {
                    self.send(c, msg.clone());
                }
                self.stopping = true;
            }
            Err(e) => {
                log::warn!("moving the store to {dir} failed: {e}");
                self.send_status(id, format!("Cannot move the tree: {e}"));
            }
        }
    }

    fn track_live(&mut self, ev: &BrowserEvent) {
        // Until a browser's first snapshot its profile is unknown; the
        // snapshot is the full truth anyway.
        if self.engine.browser.is_none() {
            return;
        }
        let live = self.live.entry(self.engine.browser.clone()).or_default();
        match ev {
            BrowserEvent::TabCreated(t) | BrowserEvent::TabUpdated(t) => {
                live.tabs.insert(t.id);
                live.windows.insert(t.window_id);
            }
            BrowserEvent::TabRemoved { tab_id, .. } => {
                live.tabs.remove(tab_id);
            }
            BrowserEvent::TabReplaced { added, removed } => {
                live.tabs.remove(removed);
                live.tabs.insert(*added);
            }
            BrowserEvent::WindowCreated(w) => {
                live.windows.insert(w.id);
            }
            BrowserEvent::WindowRemoved(w) => {
                live.windows.remove(w);
            }
            _ => {}
        }
    }

    /// Nodes a viewer put back (undo, restore, insert) may say "open" for
    /// tabs that no longer exist. Ops that turn those into saved nodes.
    fn stale_open_nodes(&self, ops: &[Op]) -> Vec<Op> {
        let tree = self.store.tree();
        let mut out = Vec::new();
        for op in ops {
            let id = match op {
                Op::Restore { id, .. } => *id,
                Op::Insert { node, .. } => node.id,
                _ => continue,
            };
            let Some(sub) = tree.root.find(id) else {
                continue;
            };
            // Profile of the top-level window holding this subtree.
            let browser = tree
                .root
                .children
                .iter()
                .find(|w| w.contains(id))
                .and_then(|w| w.data.get("browser"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            // Outside any tagged window: check every profile.
            let sets: Vec<&Live> = match &browser {
                Some(_) => self.live.get(&browser).into_iter().collect(),
                None => self.live.values().collect(),
            };
            sub.walk(&mut |n, _| {
                let bid = n.data.get("id").and_then(Value::as_i64);
                let alive = match n.kind {
                    NodeKind::Tab => bid.is_some_and(|b| sets.iter().any(|l| l.tabs.contains(&b))),
                    NodeKind::Window => {
                        bid.is_some_and(|b| sets.iter().any(|l| l.windows.contains(&b)))
                    }
                    _ => true,
                };
                if !alive {
                    let mut f = n.fields();
                    f.kind = if n.kind == NodeKind::Tab {
                        NodeKind::SavedTab
                    } else {
                        NodeKind::SavedWindow
                    };
                    f.data.remove("id");
                    f.data.remove("windowId");
                    out.push(Op::Update {
                        id: n.id,
                        fields: Box::new(f),
                    });
                }
            });
        }
        out
    }

    /// Apply ops computed outside the engine (bookmark sync, expectations).
    fn apply_ops(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        let mut applied = Vec::new();
        for op in ops {
            match self.store.apply(op.clone()) {
                Ok(_) => applied.push(op),
                Err(e) => log::warn!("{} op rejected: {e}", op.name()),
            }
        }
        self.published(applied);
    }
}
