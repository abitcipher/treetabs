//! Native messaging host: started by the browser for the treetabs extension.
//!
//! Relays between the extension (stdin/stdout, Chrome framing) and the
//! treetabs server (Unix socket), starting the server in this process when
//! no other process (for example the GUI) runs it. If the server goes away,
//! it reconnects (taking over if needed) and asks the extension to resync.
//!
//! It also carries the extension's tree-window pages: each open page gets
//! its own viewer connection to the server, and its messages travel inside
//! `viewer_*` envelopes (split into parts: Chrome limits host messages to 1 MB).

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Stdout, Write, stdin, stdout};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use treetabs_sync::client::Sender;
use treetabs_sync::protocol::native;
use treetabs_sync::{ClientMsg, Role, ServerMsg, config};

/// Bytes of a viewer message per part. JSON escaping can double the size,
/// so this stays well under Chrome's 1 MB limit.
const PART: usize = 400 * 1024;

type Out = Arc<Mutex<Stdout>>;
type Viewers = Arc<Mutex<HashMap<u64, Option<Sender>>>>;

fn write_out(out: &Out, v: &Value) {
    let mut o = out.lock().expect("stdout lock");
    if let Err(e) = native::write(&mut *o, v) {
        log::error!("writing to browser: {e}");
    }
    let _ = o.flush();
}

/// Send a viewer message to its page, in parts if needed.
fn write_viewer(out: &Out, vid: u64, text: &str) {
    let mut start = 0;
    while start < text.len() || start == 0 {
        let mut end = (start + PART).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let last = end == text.len();
        write_out(
            out,
            &json!({"type": "viewer_msg", "vid": vid, "part": &text[start..end], "last": last}),
        );
        if last {
            break;
        }
        start = end;
    }
}

/// One tree-window page: a viewer connection kept up while the page is open.
fn run_viewer(vid: u64, dir: PathBuf, out: Out, viewers: Viewers) {
    loop {
        if !viewers.lock().expect("viewers").contains_key(&vid) {
            return;
        }
        match treetabs_sync::connect(&dir, Role::Viewer, "tree window", || {}) {
            Ok(conn) => {
                let (sender, rx, server) = conn.split();
                if let Some(slot) = viewers.lock().expect("viewers").get_mut(&vid) {
                    *slot = Some(sender);
                } else {
                    sender.shutdown();
                    return;
                }
                for m in rx {
                    match serde_json::to_string(&m) {
                        Ok(text) => write_viewer(&out, vid, &text),
                        Err(e) => log::error!("viewer {vid}: {e}"),
                    }
                }
                drop(server);
            }
            Err(e) => log::warn!("viewer {vid}: cannot reach server: {e}"),
        }
        thread::sleep(Duration::from_millis(300));
    }
}

fn viewer_envelope(v: &Value, dir: &Path, out: &Out, viewers: &Viewers) -> bool {
    let vid = v.get("vid").and_then(Value::as_u64).unwrap_or(0);
    match v.get("type").and_then(Value::as_str) {
        Some("viewer_open") => {
            let mut map = viewers.lock().expect("viewers");
            if let std::collections::hash_map::Entry::Vacant(e) = map.entry(vid) {
                e.insert(None);
                let (dir, out, viewers) = (dir.to_owned(), out.clone(), viewers.clone());
                thread::spawn(move || run_viewer(vid, dir, out, viewers));
            }
            true
        }
        Some("viewer_msg") => {
            let text = v.get("msg").and_then(Value::as_str).unwrap_or_default();
            match serde_json::from_str::<ClientMsg>(text) {
                Ok(msg) => {
                    let sender = viewers
                        .lock()
                        .expect("viewers")
                        .get(&vid)
                        .cloned()
                        .flatten();
                    match sender {
                        Some(s) => {
                            if let Err(e) = s.send(&msg) {
                                log::warn!("viewer {vid}: {e}");
                            }
                        }
                        None => log::warn!("viewer {vid}: not connected yet, message dropped"),
                    }
                }
                Err(e) => log::warn!("viewer {vid}: bad message: {e}"),
            }
            true
        }
        Some("viewer_close") => {
            if let Some(Some(s)) = viewers.lock().expect("viewers").remove(&vid) {
                s.shutdown();
            }
            true
        }
        _ => false,
    }
}

enum Ev {
    Ext(Option<Value>),
    ServerGone,
}

fn main() {
    let dir = config::store_dir();
    let _ = std::fs::create_dir_all(&dir);
    init_log(&dir);
    log::info!(
        "host started, store {}, args {:?}",
        dir.display(),
        std::env::args().collect::<Vec<_>>()
    );

    let out: Out = Arc::new(Mutex::new(stdout()));
    let viewers: Viewers = Arc::default();
    let (tx, rx) = mpsc::channel::<Ev>();
    spawn_browser_reader(tx.clone());

    let mut backoff = Duration::from_millis(200);
    loop {
        let conn = match treetabs_sync::connect(&dir, Role::Browser, "browser", || {}) {
            Ok(c) => c,
            Err(e) => {
                log::error!("cannot reach server: {e}");
                thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
                // Keep draining the extension so it isn't blocked meanwhile.
                while let Ok(ev) = rx.try_recv() {
                    if let Ev::Ext(None) = ev {
                        return;
                    }
                }
                continue;
            }
        };
        backoff = Duration::from_millis(200);
        log::info!("connected (server in this process: {})", conn.is_server());
        let (sender, srx, server) = conn.split();
        spawn_server_forwarder(srx, out.clone(), tx.clone());
        match forward_browser(&rx, &sender, &dir, &out, &viewers) {
            Session::Reconnect => {}
            Session::BrowserClosed => {
                // Stops an in-process server cleanly (snapshot written).
                drop(server);
                return;
            }
        }
    }
}

/// How a connection to the server ended.
enum Session {
    /// The server went away (or a write failed): connect again.
    Reconnect,
    /// The browser closed its side: the host stops.
    BrowserClosed,
}

/// Messages from the browser (native messaging on stdin) into `tx`; `None`
/// at the end.
fn spawn_browser_reader(tx: mpsc::Sender<Ev>) {
    thread::spawn(move || {
        let mut input = stdin().lock();
        loop {
            match native::read(&mut input) {
                Ok(Some(v)) => {
                    if tx.send(Ev::Ext(Some(v))).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    log::error!("reading from browser: {e}");
                    break;
                }
            }
        }
        let _ = tx.send(Ev::Ext(None));
    });
}

/// Server calls to the browser, until the server goes away.
fn spawn_server_forwarder(srx: mpsc::Receiver<ServerMsg>, out: Out, gone_tx: mpsc::Sender<Ev>) {
    thread::spawn(move || {
        for m in srx {
            let v = match m {
                ServerMsg::Call { req, api, args } => {
                    json!({"type": "call", "req": req, "api": api, "args": args})
                }
                ServerMsg::Resync => json!({"type": "resync"}),
                ServerMsg::StoreMoved { dir } => {
                    // The browser starts the host again, which reads the
                    // new folder from its config.
                    log::info!("store moved to {dir}: exiting so the browser restarts the host");
                    std::process::exit(0);
                }
                _ => continue,
            };
            write_out(&out, &v);
        }
        let _ = gone_tx.send(Ev::ServerGone);
    });
}

/// Browser messages to the server (and to tree windows), until one side
/// goes away.
fn forward_browser(
    rx: &mpsc::Receiver<Ev>,
    sender: &Sender,
    dir: &Path,
    out: &Out,
    viewers: &Viewers,
) -> Session {
    loop {
        match rx.recv() {
            Ok(Ev::Ext(Some(v))) => {
                if viewer_envelope(&v, dir, out, viewers) {
                    continue;
                }
                let Some(msg) = to_client_msg(v) else {
                    continue;
                };
                if let Err(e) = sender.send(&msg) {
                    log::warn!("server write failed: {e}");
                    return Session::Reconnect;
                }
            }
            Ok(Ev::Ext(None)) | Err(_) => {
                log::info!("browser closed the connection");
                for s in viewers
                    .lock()
                    .expect("viewers")
                    .drain()
                    .filter_map(|(_, s)| s)
                {
                    s.shutdown();
                }
                return Session::BrowserClosed;
            }
            Ok(Ev::ServerGone) => {
                log::warn!("server went away, reconnecting");
                return Session::Reconnect;
            }
        }
    }
}

fn to_client_msg(v: Value) -> Option<ClientMsg> {
    let t = v.get("type").and_then(Value::as_str)?;
    Some(match t {
        "snapshot" => ClientMsg::BrowserSnapshot {
            windows: v
                .get("windows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            browser: v.get("browser").and_then(Value::as_str).map(str::to_owned),
            label: v.get("label").and_then(Value::as_str).map(str::to_owned),
        },
        "event" => ClientMsg::BrowserEvent {
            name: v.get("name").and_then(Value::as_str)?.to_owned(),
            args: v
                .get("args")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        },
        "result" => ClientMsg::CallResult {
            req: v.get("req").and_then(Value::as_u64)?,
            ok: v.get("ok").cloned().filter(|x| !x.is_null()),
            err: v.get("err").and_then(Value::as_str).map(str::to_owned),
        },
        "log" => {
            log::info!(
                "extension: {}",
                v.get("msg").and_then(Value::as_str).unwrap_or("")
            );
            return None;
        }
        other => {
            log::warn!("unknown message from extension: {other}");
            return None;
        }
    })
}

fn init_log(dir: &Path) {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("host.log"));
    let mut b = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Ok(f) = file {
        b.target(env_logger::Target::Pipe(Box::new(f)));
    }
    let _ = b.try_init();
}
