//! The server: sole owner of the [`Store`], runs the [`Hub`] and relays
//! its messages to every connected client over a Unix socket.
//!
//! All state lives on one thread; socket readers just forward messages to
//! it through a channel.

use crate::hub::{Backend, ExportWhen, Hub, TICK};
use crate::protocol::{ClientMsg, ServerMsg, StoreKind, read_line, write_line};
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use treetabs_core::Tree;
use treetabs_core::backups::BackupInfo;
use treetabs_core::store::{Store, StoreError};

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

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("server.sock")
}

/// The store folder as the hub's [`Backend`]: small files and backups
/// live in it.
pub struct DirBackend {
    dir: PathBuf,
}

impl DirBackend {
    pub fn new(dir: &Path) -> Self {
        DirBackend {
            dir: dir.to_owned(),
        }
    }
}

impl Backend for DirBackend {
    type Store = Store;

    fn read(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.dir.join(name)).ok()
    }

    fn write(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        treetabs_core::store::write_atomic(&self.dir.join(name), data)
    }

    fn location(&self) -> String {
        self.dir.display().to_string()
    }

    fn kind(&self) -> StoreKind {
        StoreKind::Folder
    }

    fn backups(&self) -> Vec<BackupInfo> {
        treetabs_core::backups::list(&self.dir)
    }

    fn read_backup(&mut self, _client: u64, name: &str) -> Option<Result<Tree, String>> {
        Some(treetabs_core::backups::read(&self.dir, name).map_err(|e| e.to_string()))
    }

    fn export(
        &mut self,
        tree: &Tree,
        keep: usize,
        now: i64,
        when: ExportWhen,
    ) -> std::io::Result<Option<String>> {
        let path = match when {
            ExportWhen::Daily => treetabs_core::backups::export_daily(&self.dir, tree, keep, now)?,
            ExportWhen::Now => Some(treetabs_core::backups::export_now(
                &self.dir, tree, keep, now,
            )?),
        };
        Ok(path.map(|p| p.display().to_string()))
    }

    fn save_backup(&mut self, tree: &Tree, name: &str) -> std::io::Result<String> {
        treetabs_core::backups::save_copy(&self.dir, tree, name)?;
        Ok(name.to_owned())
    }

    /// Copy everything of the store to `to` (which must not hold a tree
    /// yet), and point the configuration at it: the host's config next to
    /// its binary, and the user config if it named the old folder.
    fn move_store(&mut self, store: &mut Store, to: &str) -> std::io::Result<String> {
        let to = Path::new(to);
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
        store.compact().map_err(std::io::Error::other)?;
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
        Ok(to.display().to_string())
    }
}

enum Incoming {
    Connected(u64, UnixStream),
    Msg(u64, ClientMsg),
    Closed(u64),
    Shutdown,
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
    let hub = Hub::new(store, DirBackend::new(dir));
    let thread = thread::Builder::new()
        .name("treetabs-server".into())
        .spawn(move || {
            let mut s = State {
                hub,
                writers: HashMap::new(),
                tx: main_tx,
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

struct State {
    hub: Hub<DirBackend>,
    /// Each client's writer thread.
    writers: HashMap<u64, Sender<ServerMsg>>,
    tx: Sender<Incoming>,
}

impl State {
    fn run(&mut self, rx: Receiver<Incoming>) {
        loop {
            let msg = match rx.recv_timeout(TICK) {
                Ok(m) => Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            self.hub.tick();
            match msg {
                None => {}
                Some(Incoming::Shutdown) => break,
                Some(Incoming::Connected(id, conn)) => self.connected(id, conn),
                Some(Incoming::Closed(id)) => {
                    self.writers.remove(&id);
                    self.hub.closed(id);
                }
                Some(Incoming::Msg(id, m)) => self.hub.message(id, m),
            }
            self.deliver();
            if self.hub.stopping() {
                break;
            }
        }
    }

    fn deliver(&mut self) {
        for (id, m) in self.hub.take_out() {
            if let Some(w) = self.writers.get(&id) {
                let _ = w.send(m);
            }
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
        self.writers.insert(id, ctx);
        self.hub.connected(id);
    }
}

/// Helper for building `chrome.*` call args.
pub fn args(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        other => vec![other],
    }
}
