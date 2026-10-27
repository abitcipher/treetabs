//! The server: sole owner of the [`Store`], runs the sync [`Engine`] and
//! relays changes to every connected client over a Unix socket.
//!
//! All state lives on one thread; socket readers just forward messages to
//! it through a channel.

use crate::bridge::Browser;
use crate::protocol::{ClientMsg, Role, ServerMsg, read_line, write_line};
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use treetabs_core::ops::now_ms;
use treetabs_core::store::{Store, StoreError};
use treetabs_core::sync::{Apply, BrowserEvent, BrowserWindow, Engine};
use treetabs_core::{EditError, Op, Tree};

/// How often the bookmark mirror checks for work.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// `<store>/settings.json`.
#[derive(serde::Deserialize)]
#[serde(default)]
struct Settings {
    /// Mirror the tree into a bookmark folder.
    bookmarks: bool,
    /// Daily exports to keep in `<store>/exports/` (0 = no exports).
    daily_exports: usize,
    /// Remove trash items older than this many days (0 = never).
    trash_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            bookmarks: true,
            daily_exports: 30,
            trash_days: 0,
        }
    }
}

/// Files that belong to the running server, not to the tree.
const LIVE_FILES: &[&str] = &["lock", "server.sock", "host.log"];

fn copy_store(from: &Path, to: &Path) -> std::io::Result<()> {
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        if LIVE_FILES.iter().any(|f| name == *f) {
            continue;
        }
        let dest = to.join(&name);
        if e.file_type()?.is_dir() {
            std::fs::create_dir_all(&dest)?;
            copy_store(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

/// Change one key of `<store>/settings.json`, keeping the others.
fn write_setting(dir: &Path, key: &str, value: Value) -> std::io::Result<()> {
    let path = dir.join("settings.json");
    let mut v: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    v[key] = value;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&v).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, path)
}

/// How often to check whether today's export is written.
const EXPORT_CHECK: std::time::Duration = std::time::Duration::from_secs(60);

impl Settings {
    fn read(dir: &Path) -> Settings {
        let mut s: Settings = std::fs::read(dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if std::env::var("TREETABS_BOOKMARKS").is_ok_and(|v| v == "0") {
            s.bookmarks = false;
        }
        s
    }
}

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("server.sock")
}

enum Incoming {
    Connected(u64, UnixStream),
    Msg(u64, ClientMsg),
    Closed(u64),
    Shutdown,
}

struct Client {
    role: Option<Role>,
    /// Browser profile id, for browser bridges.
    browser: Option<String>,
    name: String,
    tx: Sender<ServerMsg>,
}

/// Records every op applied, so it can be broadcast afterwards.
struct Recorder<'a> {
    store: &'a mut Store,
    ops: Vec<Op>,
}

impl Apply for Recorder<'_> {
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

/// Handle to a running server. Dropping it shuts the server down cleanly
/// (snapshot written, socket removed).
pub struct ServerHandle {
    tx: Sender<Incoming>,
    thread: Option<JoinHandle<()>>,
    socket: PathBuf,
}

impl ServerHandle {
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.tx.send(Incoming::Shutdown);
        // Unblock the accept loop.
        let _ = UnixStream::connect(&self.socket);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Open the store in `dir` and start serving. Fails with
/// [`StoreError::Locked`] when another process owns the store.
///
/// # Errors
///
/// Also when the store cannot be opened or the socket cannot be made.
pub fn start(dir: &Path) -> Result<ServerHandle, StoreError> {
    let store = Store::open(dir)?;
    for w in &store.warnings {
        log::warn!("store: {w}");
    }
    let sock = socket_path(dir);
    // We hold the store lock, so any socket file left behind is stale.
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    let (tx, rx) = mpsc::channel();

    let accept_tx = tx.clone();
    thread::Builder::new()
        .name("treetabs-accept".into())
        .spawn(move || {
            let mut next = 1u64;
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                if accept_tx.send(Incoming::Connected(next, conn)).is_err() {
                    break;
                }
                next += 1;
            }
        })?;

    let main_tx = tx.clone();
    let sock2 = sock.clone();
    let settings = Settings::read(dir);
    let dir_owned = dir.to_owned();
    let dir_owned2 = dir.to_owned();
    let thread = thread::Builder::new()
        .name("treetabs-server".into())
        .spawn(move || {
            let mut browser = Browser::default();
            browser
                .mirror
                .configure(settings.bookmarks, dir_owned.join("bookmarks.json"));
            let mut s = State {
                live: HashMap::new(),
                store,
                engine: Engine::new(),
                clients: HashMap::new(),
                tx: main_tx,
                browser,
                dir: dir_owned,
                daily_exports: settings.daily_exports,
                trash_days: settings.trash_days,
                profiles: std::fs::read(dir_owned2.join("profiles.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default(),
            };
            s.run(rx);
            let _ = std::fs::remove_file(&sock2);
            log::info!("server stopped");
        })?;
    log::info!("server started on {}", sock.display());
    Ok(ServerHandle {
        tx,
        thread: Some(thread),
        socket: sock,
    })
}

/// Window and tab ids that exist in a browser right now, per profile.
#[derive(Default)]
struct Live {
    windows: std::collections::HashSet<i64>,
    tabs: std::collections::HashSet<i64>,
}

struct State {
    /// Live ids per browser profile, to tell open nodes from stale ones.
    live: HashMap<Option<String>, Live>,
    store: Store,
    engine: Engine,
    clients: HashMap<u64, Client>,
    tx: Sender<Incoming>,
    browser: Browser,
    dir: PathBuf,
    daily_exports: usize,
    trash_days: u32,
    /// Readable names of browser profiles (`<store>/profiles.json`).
    profiles: std::collections::BTreeMap<String, String>,
}

impl State {
    fn run(&mut self, rx: Receiver<Incoming>) {
        let mut last_tick = std::time::Instant::now();
        // First check a minute after start: by then the browser has sent
        // its windows, so the copy is not missing the open tabs.
        let mut last_export = std::time::Instant::now();
        loop {
            let msg = match rx.recv_timeout(TICK) {
                Ok(m) => Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if last_tick.elapsed() >= TICK {
                last_tick = std::time::Instant::now();
                let calls = self.browser.tick(self.store.tree());
                self.dispatch_calls(calls);
            }
            if last_export.elapsed() >= EXPORT_CHECK {
                last_export = std::time::Instant::now();
                self.daily_export();
                self.expire_trash();
            }
            let Some(msg) = msg else { continue };
            match msg {
                Incoming::Shutdown => break,
                Incoming::Connected(id, conn) => self.connected(id, conn),
                Incoming::Closed(id) => {
                    if let Some(c) = self.clients.remove(&id) {
                        log::info!("client {id} ({}) disconnected", c.name);
                        if c.role == Some(Role::Browser) {
                            self.browser.disconnected(id);
                        }
                        self.status("");
                    }
                }
                Incoming::Msg(id, m) => self.message(id, m),
            }
        }
    }

    fn daily_export(&mut self) {
        match treetabs_core::backups::export_daily(
            &self.dir,
            self.store.tree(),
            self.daily_exports,
            now_ms(),
        ) {
            Ok(Some(p)) => log::info!("daily export: {}", p.display()),
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
                && let Err(e) = treetabs_core::backups::export_now(
                    &self.dir,
                    self.store.tree(),
                    self.daily_exports,
                    now_ms(),
                )
            {
                log::warn!("export before removing old trash failed: {e}");
            }
            self.apply_ops(vec![Op::PurgeTrash { before }]);
        }
    }

    /// Copy everything of the store to `to` (which must not hold a tree
    /// yet), and point the configuration at it: the host's config next to
    /// its binary, and the user config if it named the old folder.
    fn move_store(&mut self, to: &Path) -> std::io::Result<PathBuf> {
        if !to.is_absolute() {
            return Err(std::io::Error::other("the folder must be a full path"));
        }
        std::fs::create_dir_all(to)?;
        let to = to.canonicalize()?;
        if to == self.dir.canonicalize()? {
            return Err(std::io::Error::other("that is the current folder"));
        }
        if to.join("tree.json").exists() {
            return Err(std::io::Error::other("that folder already holds a tree"));
        }
        // Writable? (A snap browser can only write inside its snap folder.)
        let probe = to.join(".treetabs-probe");
        std::fs::write(&probe, b"")?;
        std::fs::remove_file(&probe)?;
        // Everything on disk first, then a copy without the live files.
        self.store.compact().map_err(std::io::Error::other)?;
        copy_store(&self.dir, &to)?;
        let cfg = crate::config::Config {
            store_dir: Some(to.clone()),
        };
        let json = serde_json::to_vec_pretty(&cfg).map_err(std::io::Error::other)?;
        if let Ok(exe) = std::env::current_exe()
            && exe.file_stem().is_some_and(|n| n == "treetabs-host")
        {
            std::fs::write(crate::config::host_config_path(&exe), &json)?;
        }
        let user = crate::config::user_config_path();
        let points_here = crate::config::Config::read(&user)
            .and_then(|c| c.store_dir)
            .is_none_or(|d| d.canonicalize().ok() == self.dir.canonicalize().ok());
        if points_here {
            // May fail in a confined browser; the host config is what counts there.
            if let Some(parent) = user.parent()
                && std::fs::create_dir_all(parent).is_ok()
            {
                let _ = std::fs::write(&user, &json);
            }
        }
        Ok(to)
    }

    fn backups_msg(&self, message: Option<String>) -> ServerMsg {
        ServerMsg::Backups {
            list: treetabs_core::backups::list(&self.dir),
            dir: self.dir.display().to_string(),
            message,
        }
    }

    fn connected(&mut self, id: u64, conn: UnixStream) {
        let Ok(read_half) = conn.try_clone() else {
            return;
        };
        let (ctx, crx) = mpsc::channel::<ServerMsg>();
        // Writer thread: never block the main loop on a slow client.
        let mut w = conn;
        thread::spawn(move || {
            for m in crx {
                if write_line(&mut w, &m).is_err() {
                    break;
                }
            }
        });
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut r = BufReader::new(read_half);
            loop {
                match read_line::<ClientMsg>(&mut r) {
                    Ok(Some(m)) => {
                        if tx.send(Incoming::Msg(id, m)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        log::warn!("client {id}: bad message: {e}");
                        break;
                    }
                }
            }
            let _ = tx.send(Incoming::Closed(id));
        });
        self.clients.insert(
            id,
            Client {
                role: None,
                browser: None,
                name: String::new(),
                tx: ctx,
            },
        );
    }

    fn send(&self, id: u64, m: ServerMsg) {
        if let Some(c) = self.clients.get(&id) {
            let _ = c.tx.send(m);
        }
    }

    fn broadcast_viewers(&self, m: &ServerMsg) {
        for c in self.clients.values() {
            if c.role == Some(Role::Viewer) {
                let _ = c.tx.send(m.clone());
            }
        }
    }

    fn browsers(&self) -> usize {
        self.clients
            .values()
            .filter(|c| c.role == Some(Role::Browser))
            .count()
    }

    fn status(&self, message: &str) {
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
        f: impl FnOnce(&mut Engine, &mut Recorder) -> Result<usize, EditError>,
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

    fn message(&mut self, id: u64, m: ClientMsg) {
        match m {
            ClientMsg::Hello { role, name } => self.on_hello(id, role, name),
            ClientMsg::BrowserSnapshot {
                windows,
                browser,
                label,
            } => self.on_browser_snapshot(id, windows, browser, label),
            ClientMsg::BrowserEvent { name, args } => self.on_browser_event(id, &name, &args),
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
            ClientMsg::ListBackups => self.send(id, self.backups_msg(None)),
            ClientMsg::ExportNow => self.on_export_now(id),
            ClientMsg::ReadBackup { name } => self.on_read_backup(id, name),
            ClientMsg::SetTrashDays { days } => self.on_set_trash_days(days),
            ClientMsg::MoveStore { dir } => self.on_move_store(id, &dir),
        }
    }

    fn send_status(&self, id: u64, message: String) {
        self.send(
            id,
            ServerMsg::Status {
                browsers: self.browsers(),
                message,
            },
        );
    }

    fn store_options(&self) -> ServerMsg {
        ServerMsg::StoreOptions {
            trash_days: self.trash_days,
            dir: self.dir.display().to_string(),
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
                let snap = ServerMsg::Snapshot {
                    seq: self.store.seq(),
                    tree: Box::new(self.store.tree().clone()),
                };
                self.send(id, snap);
                self.send(id, self.store_options());
                self.send(
                    id,
                    ServerMsg::Profiles {
                        names: self.profiles.clone(),
                    },
                );
                self.status("");
            }
            Role::Browser => {
                self.browser.connected(id);
                self.send(id, ServerMsg::Resync);
                self.status("browser connected");
            }
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
                && let Err(e) =
                    treetabs_core::store::write_atomic(&self.dir.join("profiles.json"), &j)
            {
                log::warn!("cannot save profiles.json: {e}");
            }
            self.broadcast_viewers(&ServerMsg::Profiles {
                names: self.profiles.clone(),
            });
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
        let message = match treetabs_core::backups::export_now(
            &self.dir,
            self.store.tree(),
            keep,
            now_ms(),
        ) {
            Ok(p) => format!("Saved {}", p.display()),
            Err(e) => format!("Export failed: {e}"),
        };
        self.send(id, self.backups_msg(Some(message)));
    }

    fn on_read_backup(&mut self, id: u64, name: String) {
        let msg = match treetabs_core::backups::read(&self.dir, &name) {
            Ok(t) => ServerMsg::BackupTree {
                name,
                tree: Some(Box::new(t)),
                err: None,
            },
            Err(e) => ServerMsg::BackupTree {
                name,
                tree: None,
                err: Some(e.to_string()),
            },
        };
        self.send(id, msg);
    }

    fn on_set_trash_days(&mut self, days: u32) {
        self.trash_days = days;
        if let Err(e) = write_setting(&self.dir, "trash_days", days.into()) {
            log::warn!("cannot save settings.json: {e}");
        }
        self.broadcast_viewers(&self.store_options());
        self.expire_trash();
    }

    /// Move the store; on success every client is told and the server
    /// stops, so whoever runs next opens the new folder.
    fn on_move_store(&mut self, id: u64, dir: &str) {
        match self.move_store(Path::new(dir)) {
            Ok(new) => {
                log::info!("store moved to {}", new.display());
                let msg = ServerMsg::StoreMoved {
                    dir: new.display().to_string(),
                };
                for &c in self.clients.keys() {
                    self.send(c, msg.clone());
                }
                // The run loop is still reading its channel; if it is gone
                // the server is stopping anyway.
                let _ = self.tx.send(Incoming::Shutdown);
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
                    treetabs_core::NodeKind::Tab => {
                        bid.is_some_and(|b| sets.iter().any(|l| l.tabs.contains(&b)))
                    }
                    treetabs_core::NodeKind::Window => {
                        bid.is_some_and(|b| sets.iter().any(|l| l.windows.contains(&b)))
                    }
                    _ => true,
                };
                if !alive {
                    let mut f = n.fields();
                    f.kind = if n.kind == treetabs_core::NodeKind::Tab {
                        treetabs_core::NodeKind::SavedTab
                    } else {
                        treetabs_core::NodeKind::SavedWindow
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

/// Helper for building `chrome.*` call args.
pub fn args(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        other => vec![other],
    }
}
