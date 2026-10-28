//! treetabs browser extension.
//!
//! With the companion app it is thin: it forwards tab, window and bookmark
//! events to the native host, answers "resync" with a full window
//! snapshot, and executes `chrome.*` calls the host asks for. All tree
//! logic lives in the host.
//!
//! Without the app it runs the same tree logic itself and keeps the tree
//! in the browser's extension storage (see [`alone`]). Which one, is
//! decided each time the background script starts (see [`Mode`]).

mod alone;
mod storage;

use js_sys::{Array, Function, Object, Promise, Reflect};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use treetabs_core::protocol::ServerMsg;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

#[oxichrome::extension(
    name = "treetabs",
    version = "0.4.6",
    description = "Tree of windows and tabs, kept in sync with bookmarks",
    permissions = ["tabs", "bookmarks", "nativeMessaging", "storage", "unlimitedStorage", "favicon"]
)]
struct Extension;

/// Native host name (see `treetabs install-host`).
const HOST: &str = "dev.treetabs.host";

/// Set once the companion app has answered: from then on the extension
/// waits for the app instead of starting a tree of its own.
const HOST_SEEN: &str = "hostSeen";

/// Events forwarded to the host.
const EVENTS: &[&str] = &[
    "tabs.onCreated",
    "tabs.onUpdated",
    "tabs.onRemoved",
    "tabs.onMoved",
    "tabs.onAttached",
    "tabs.onReplaced",
    "windows.onCreated",
    "windows.onRemoved",
    "windows.onBoundsChanged",
    "bookmarks.onCreated",
    "bookmarks.onRemoved",
    "bookmarks.onChanged",
    "bookmarks.onMoved",
    "bookmarks.onChildrenReordered",
    "bookmarks.onImportBegan",
    "bookmarks.onImportEnded",
];

/// The user's choice in Settings > Storage: "on" or "off". Without a
/// choice the extension uses the app when it is installed.
const COMPANION_APP: &str = "companionApp";

/// For tests only: a system name used instead of the browser's
/// `runtime.getPlatformInfo().os`, to try other systems on Linux.
const TEST_PLATFORM: &str = "testPlatform";

/// The user's choice about the companion app (see [`COMPANION_APP`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    /// Use the app when it is installed.
    Auto,
    /// Use the app; without it, keep the tree in the browser meanwhile.
    On,
    /// Never use the app.
    Off,
}

/// Who owns the tree, decided at each start of the background script.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// Waiting for the companion app's first answer.
    Deciding,
    /// The companion app owns the tree.
    Host,
    /// No app: the extension owns the tree (see [`alone`]).
    Alone,
    /// No app, and the tree in the browser's storage does not load.
    Failed(String),
}

thread_local! {
    static MODE: RefCell<Mode> = const { RefCell::new(Mode::Deciding) };
    /// Whether the companion app ever answered (see [`HOST_SEEN`]).
    static SEEN: Cell<bool> = const { Cell::new(false) };
    /// The current port got a message from the host.
    static ANSWERED: Cell<bool> = const { Cell::new(false) };
    static CHOICE: Cell<Choice> = const { Cell::new(Choice::Auto) };
    /// Whether the companion app exists for this system; `None` until
    /// known. Without it the app is never started or mentioned.
    static APP_EXISTS: Cell<Option<bool>> = const { Cell::new(None) };
    /// The tree is on its way from the app to the browser: browser events
    /// are not sent to the app any more (the snapshot taken afterwards
    /// covers them).
    static LEAVING: Cell<bool> = const { Cell::new(false) };
    static PORT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
    static RETRY_MS: RefCell<i32> = const { RefCell::new(500) };
    /// Open tree-window pages, by viewer id.
    static VIEWERS: RefCell<HashMap<u32, JsValue>> = RefCell::new(HashMap::new());
    static NEXT_VIEWER: RefCell<u32> = const { RefCell::new(1) };
    /// Chunks of large host messages still being assembled, by viewer id.
    static PARTS: RefCell<HashMap<u32, String>> = RefCell::new(HashMap::new());
    /// While a snapshot is being taken, events wait here and are sent after
    /// it, so the host never gets an older snapshot after newer events.
    static HELD: RefCell<Option<Vec<JsValue>>> = const { RefCell::new(None) };
    static BROWSER_ID: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// The tree window: a popup docked to the left of the screen.
const TREE_PAGE: &str = "tree.html";
const TREE_WIDTH: i32 = 420;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(f: &Function, ms: i32) -> JsValue;
}

/// Wait `ms` milliseconds.
async fn sleep(ms: i32) {
    let p = Promise::new(&mut |resolve, _| {
        set_timeout(&resolve, ms);
    });
    let _ = JsFuture::from(p).await;
}

/// The extension API: `browser` in Firefox (its APIs return promises), `chrome` elsewhere.
fn chrome() -> JsValue {
    let g = js_sys::global();
    match Reflect::get(&g, &"browser".into()) {
        Ok(b) if b.is_object() => b,
        _ => Reflect::get(&g, &"chrome".into()).unwrap_or(JsValue::UNDEFINED),
    }
}

/// `chrome.a.b.c` as a value.
fn path(p: &str) -> Result<JsValue, JsValue> {
    let mut v = chrome();
    for part in p.split('.') {
        v = Reflect::get(&v, &part.into())?;
        if v.is_undefined() {
            return Err(format!("chrome.{p} is not available").into());
        }
    }
    Ok(v)
}

fn obj(pairs: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (k, v) in pairs {
        let _ = Reflect::set(&o, &(*k).into(), v);
    }
    o.into()
}

fn post(msg: &JsValue) {
    PORT.with(|p| {
        if let Some(port) = p.borrow().as_ref()
            && let Ok(f) = Reflect::get(port, &"postMessage".into())
        {
            let f: Function = f.unchecked_into();
            if let Err(e) = f.call1(port, msg) {
                web_sys::console::warn_2(&"[treetabs] postMessage failed".into(), &e);
            }
        }
    });
}

#[oxichrome::background]
async fn start() {
    // Listeners first, so nothing is missed while the port connects. Events
    // without a port are dropped; the snapshot sent on connect covers them
    for name in EVENTS {
        add_listener(name);
    }
    listen("action.onClicked", |_tab| {
        spawn_local(async { open_tree_window().await })
    });
    // Same action for keyboard shortcuts and tests: `treetabsOpenTree()`.
    let open = Closure::<dyn FnMut()>::new(|| spawn_local(async { open_tree_window().await }));
    let _ = Reflect::set(&js_sys::global(), &"treetabsOpenTree".into(), open.as_ref());
    open.forget();
    listen("runtime.onConnect", viewer_connected);
    let _ = log::set_logger(&ConsoleLog).map(|()| log::set_max_level(log::LevelFilter::Info));
    // For tests: which mode, and the tree when the extension owns it.
    let mode = Closure::<dyn FnMut() -> JsValue>::new(|| {
        MODE.with(|m| format!("{:?}", m.borrow()))
            .to_lowercase()
            .into()
    });
    let _ = Reflect::set(&js_sys::global(), &"treetabsMode".into(), mode.as_ref());
    mode.forget();
    let tree = Closure::<dyn FnMut() -> JsValue>::new(|| {
        alone::tree_json().map_or(JsValue::NULL, JsValue::from)
    });
    let _ = Reflect::set(&js_sys::global(), &"treetabsTree".into(), tree.as_ref());
    tree.forget();
    spawn_local(async {
        let keys = Array::of3(
            &HOST_SEEN.into(),
            &COMPANION_APP.into(),
            &TEST_PLATFORM.into(),
        );
        let saved = call("storage.local.get", &Array::of1(&keys))
            .await
            .unwrap_or(JsValue::UNDEFINED);
        let seen = Reflect::get(&saved, &HOST_SEEN.into()).is_ok_and(|v| v.is_truthy());
        SEEN.with(|s| s.set(seen));
        let choice = match Reflect::get(&saved, &COMPANION_APP.into())
            .ok()
            .and_then(|v| v.as_string())
            .as_deref()
        {
            Some("on") => Choice::On,
            Some("off") => Choice::Off,
            _ => Choice::Auto,
        };
        CHOICE.with(|c| c.set(choice));
        let exists = app_exists(&saved).await;
        APP_EXISTS.with(|a| a.set(Some(exists)));
        VIEWERS.with(|v| {
            for port in v.borrow().values() {
                post_to(port, &app_exists_json(exists));
            }
        });
        if !exists || choice == Choice::Off {
            go_alone();
        } else {
            connect();
        }
    });
}

/// Whether the companion app exists for this system (see
/// [`treetabs_core::protocol::companion_app_exists`]). If the browser does
/// not say, as if it did: the extension tries the app and falls back.
async fn app_exists(saved: &JsValue) -> bool {
    let os = match Reflect::get(saved, &TEST_PLATFORM.into())
        .ok()
        .and_then(|v| v.as_string())
    {
        Some(os) => Some(os),
        None => call("runtime.getPlatformInfo", &Array::new())
            .await
            .ok()
            .and_then(|info| Reflect::get(&info, &"os".into()).ok())
            .and_then(|os| os.as_string()),
    };
    os.is_none_or(|os| treetabs_core::protocol::companion_app_exists(&os))
}

/// Tells a tree window whether to offer the companion app. Handled by the
/// window itself, not by the editor.
fn app_exists_json(exists: bool) -> JsValue {
    serde_json::json!({ "type": "companion_app", "exists": exists })
        .to_string()
        .into()
}

async fn save_choice(choice: Choice) {
    CHOICE.with(|c| c.set(choice));
    let value = match choice {
        Choice::On => "on",
        Choice::Off => "off",
        Choice::Auto => return,
    };
    let _ = call(
        "storage.local.set",
        &Array::of1(&obj(&[(COMPANION_APP, value.into())])),
    )
    .await;
}

/// Tell every open tree window something.
fn status_all(message: &str) {
    VIEWERS.with(|v| {
        for port in v.borrow().values() {
            post_to(port, &status_json(message));
        }
    });
}

/// A tree window asks to turn the companion app on or off. Returns true
/// when it was such a request (handled here, not passed on).
fn companion_request(vid: u32, text: &str) -> bool {
    let Ok(treetabs_core::protocol::ClientMsg::UseCompanionApp { on }) = serde_json::from_str(text)
    else {
        return false;
    };
    // No app for this system: the window does not offer it either.
    if APP_EXISTS.with(Cell::get) == Some(false) {
        return true;
    }
    match (mode(), on) {
        (Mode::Alone, true) => spawn_local(use_app()),
        // The app sends the tree along (see `leave_app`).
        (Mode::Host, false) if PORT.with(|p| p.borrow().is_some()) => {
            LEAVING.with(|l| l.set(true));
            post(&obj(&[
                ("type", "viewer_msg".into()),
                ("vid", vid.into()),
                ("msg", text.into()),
            ]));
        }
        // The app is not running: nothing to take along. The bookmark
        // folder is read back into the new tree.
        (Mode::Host | Mode::Deciding, false) => spawn_local(async {
            save_choice(Choice::Off).await;
            PORT.with(|p| p.borrow_mut().take());
            go_alone();
            status_all("The companion app was not running: the tree is rebuilt from the bookmarks");
        }),
        (Mode::Failed(_), false) => {}
        // Already as asked.
        _ => status_all(""),
    }
    true
}

/// Turn the app on: stop owning the tree, and connect; the tree kept in
/// the browser is handed over on the app's first message.
async fn use_app() {
    save_choice(Choice::On).await;
    alone::stop().await;
    set_mode(Mode::Deciding);
    connect();
}

/// The app sent the tree for the browser to keep: save it, leave the app,
/// and own the tree from now on.
async fn leave_app(text: &str) {
    let Ok(ServerMsg::Detached {
        seq,
        tree,
        bookmarks,
    }) = serde_json::from_str::<ServerMsg>(text)
    else {
        return;
    };
    if let Err(e) = storage::keep_detached(*tree, bookmarks, seq).await {
        web_sys::console::error_1(&format!("[treetabs] cannot keep the tree: {e}").into());
        status_all(&format!("Cannot keep the tree in the browser: {e}"));
        LEAVING.with(|l| l.set(false));
        return;
    }
    save_choice(Choice::Off).await;
    if let Some(port) = PORT.with(|p| p.borrow_mut().take())
        && let Ok(f) = Reflect::get(&port, &"disconnect".into())
    {
        let _ = f.unchecked_into::<Function>().call0(&port);
    }
    LEAVING.with(|l| l.set(false));
    go_alone();
}

/// `log` messages (from the hub) on the service worker console.
struct ConsoleLog;

impl log::Log for ConsoleLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        let text: JsValue = format!("[treetabs] {}", r.args()).into();
        match r.level() {
            log::Level::Error => web_sys::console::error_1(&text),
            log::Level::Warn => web_sys::console::warn_1(&text),
            _ => web_sys::console::log_1(&text),
        }
    }
    fn flush(&self) {}
}

fn mode() -> Mode {
    MODE.with(|m| m.borrow().clone())
}

fn set_mode(m: Mode) {
    MODE.with(|x| *x.borrow_mut() = m);
}

/// The companion app is not installed (Chrome and Firefox wording).
fn host_missing(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("not found") || e.contains("forbidden") || e.contains("no such native application")
}

/// No companion app: own the tree from now on.
fn go_alone() {
    set_mode(Mode::Alone);
    PORT.with(|p| p.borrow_mut().take());
    spawn_local(async move {
        if let Err(e) = alone::start().await {
            web_sys::console::error_2(&"[treetabs] cannot load the tree".into(), &e.clone().into());
            let msg = format!("Cannot read the tree kept in the browser: {e}");
            VIEWERS.with(|v| {
                for port in v.borrow().values() {
                    post_to(port, &status_json(&msg));
                }
            });
            set_mode(Mode::Failed(msg));
        }
    });
}

/// Add a one-argument listener to `chrome.<event>`
fn listen(event: &str, f: impl FnMut(JsValue) + 'static) {
    let Ok(ev) = path(event) else { return };
    let cb = Closure::<dyn FnMut(JsValue)>::new(f);
    if let Ok(add) = Reflect::get(&ev, &"addListener".into()) {
        let _ = add.unchecked_into::<Function>().call1(&ev, cb.as_ref());
    }
    cb.forget();
}

fn post_to(port: &JsValue, msg: &JsValue) {
    if let Ok(f) = Reflect::get(port, &"postMessage".into()) {
        let _ = f.unchecked_into::<Function>().call1(port, msg);
    }
}

/// Focus the tree window if it is open, otherwise open it.
async fn open_tree_window() {
    let url = path("runtime.getURL")
        .and_then(|f| {
            f.unchecked_into::<Function>()
                .call1(&path("runtime").unwrap(), &TREE_PAGE.into())
        })
        .ok()
        .and_then(|u| u.as_string())
        .unwrap_or_default();
    // Look for an existing tree window.
    let query = Array::of1(&obj(&[("url", url.clone().into())]));
    if let Ok(tabs) = call("tabs.query", &query).await {
        let tabs: Array = tabs.unchecked_into();
        if let Some(tab) = tabs.iter().next() {
            let win = Reflect::get(&tab, &"windowId".into()).unwrap_or(JsValue::NULL);
            let _ = call(
                "windows.update",
                &Array::of2(&win, &obj(&[("focused", true.into())])),
            )
            .await;
            return;
        }
    }
    let spec = obj(&[("url", url.into()), ("type", "popup".into())]);
    // Where the user left it last time (saved by tree.js); otherwise the
    // height of the current window, at the left screen edge.
    let saved = call("storage.local.get", &Array::of1(&"treeBounds".into()))
        .await
        .ok()
        .and_then(|v| Reflect::get(&v, &"treeBounds".into()).ok())
        .filter(|b| b.is_object());
    let num = |o: &JsValue, k: &str| Reflect::get(o, &k.into()).ok().and_then(|v| v.as_f64());
    let bounds = match saved {
        Some(b)
            if num(&b, "width").unwrap_or(0.0) >= 200.0
                && num(&b, "height").unwrap_or(0.0) >= 200.0 =>
        {
            ["left", "top", "width", "height"].map(|k| (k, num(&b, k).unwrap_or(0.0)))
        }
        _ => {
            let height = call("windows.getLastFocused", &Array::new())
                .await
                .ok()
                .and_then(|w| num(&w, "height"))
                .unwrap_or(900.0);
            [
                ("left", 0.0),
                ("top", 0.0),
                ("width", f64::from(TREE_WIDTH)),
                ("height", height),
            ]
        }
    };
    for (k, v) in bounds {
        let _ = Reflect::set(&spec, &k.into(), &v.round().into());
    }
    if let Err(e) = call("windows.create", &Array::of1(&spec)).await {
        web_sys::console::error_2(&"[treetabs] cannot open the tree window".into(), &e);
    }
}

/// A tree-window page connected: relay its messages to the host.
fn viewer_connected(port: JsValue) {
    if Reflect::get(&port, &"name".into())
        .ok()
        .and_then(|n| n.as_string())
        .as_deref()
        != Some("viewer")
    {
        return;
    }
    let vid = NEXT_VIEWER.with(|n| {
        let v = *n.borrow();
        *n.borrow_mut() += 1;
        v
    });
    let on_msg = Closure::<dyn FnMut(JsValue)>::new(move |m: JsValue| match mode() {
        _ if companion_request(vid, &m.as_string().unwrap_or_default()) => {}
        Mode::Alone => alone::viewer_msg(vid, &m.as_string().unwrap_or_default()),
        _ => post(&obj(&[
            ("type", "viewer_msg".into()),
            ("vid", vid.into()),
            ("msg", m),
        ])),
    });
    let on_disconnect = Closure::<dyn FnMut(JsValue)>::new(move |_p: JsValue| {
        VIEWERS.with(|v| v.borrow_mut().remove(&vid));
        PARTS.with(|p| p.borrow_mut().remove(&vid));
        match mode() {
            Mode::Alone => alone::viewer_close(vid),
            _ => post(&obj(&[
                ("type", "viewer_close".into()),
                ("vid", vid.into()),
            ])),
        }
    });
    for (ev, cb) in [
        ("onMessage", on_msg.as_ref()),
        ("onDisconnect", on_disconnect.as_ref()),
    ] {
        if let Ok(e) = Reflect::get(&port, &ev.into())
            && let Ok(add) = Reflect::get(&e, &"addListener".into())
        {
            let _ = add.unchecked_into::<Function>().call1(&e, cb);
        }
    }
    on_msg.forget();
    on_disconnect.forget();
    VIEWERS.with(|v| v.borrow_mut().insert(vid, port.clone()));
    if let Some(exists) = APP_EXISTS.with(Cell::get) {
        post_to(&port, &app_exists_json(exists));
    }
    match mode() {
        Mode::Alone => alone::viewer_open(vid),
        Mode::Failed(msg) => post_to(&port, &status_json(&msg)),
        // Opened for the host once it answers (see `connect`).
        Mode::Deciding => {}
        Mode::Host => {
            if PORT.with(|p| p.borrow().is_some()) {
                post(&obj(&[("type", "viewer_open".into()), ("vid", vid.into())]));
            } else {
                post_to(&port, &host_down_status());
            }
        }
    }
}

fn status_json(message: &str) -> JsValue {
    let m = ServerMsg::Status {
        browsers: 0,
        message: message.to_owned(),
    };
    serde_json::to_string(&m).unwrap_or_default().into()
}

fn host_down_status() -> JsValue {
    status_json(
        "The treetabs companion app is not running. If you removed it on purpose, choose Settings → Storage → Stop using the companion app",
    )
}

/// The open tree-window pages.
fn viewer_ids() -> Vec<u32> {
    VIEWERS.with(|v| v.borrow().keys().copied().collect())
}

/// Send a hub message to a tree-window page.
fn post_viewer(vid: u64, msg: &ServerMsg) {
    let Ok(vid) = u32::try_from(vid) else { return };
    let Ok(text) = serde_json::to_string(msg) else {
        return;
    };
    VIEWERS.with(|v| {
        if let Some(port) = v.borrow().get(&vid) {
            post_to(port, &text.into());
        }
    });
}

/// A Rust value as a JS value (through JSON).
fn to_js(v: &impl serde::Serialize) -> JsValue {
    serde_json::to_string(v)
        .ok()
        .and_then(|t| js_sys::JSON::parse(&t).ok())
        .unwrap_or(JsValue::NULL)
}

/// A JS value as JSON (`null` for `undefined` and for what JSON cannot hold).
fn from_js(v: &JsValue) -> serde_json::Value {
    if v.is_undefined() {
        return serde_json::Value::Null;
    }
    js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// The message of a JS error (or the value itself as text).
fn error_text(e: &JsValue) -> String {
    Reflect::get(e, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

/// Message for a viewer from the host; large ones arrive in parts.
fn viewer_message(msg: &JsValue) {
    let vid = Reflect::get(msg, &"vid".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u32;
    let part = Reflect::get(msg, &"part".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let last = Reflect::get(msg, &"last".into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let full = PARTS.with(|p| {
        let mut p = p.borrow_mut();
        let buf = p.entry(vid).or_default();
        buf.push_str(&part);
        if last { p.remove(&vid) } else { None }
    });
    if let Some(text) = full {
        if text.starts_with(r#"{"type":"detached""#) {
            spawn_local(async move { leave_app(&text).await });
            return;
        }
        VIEWERS.with(|v| {
            if let Some(port) = v.borrow().get(&vid) {
                post_to(port, &text.into());
            }
        });
    }
}

fn add_listener(name: &'static str) {
    let Ok(event) = path(name) else {
        web_sys::console::warn_1(&format!("[treetabs] no event {name}").into());
        return;
    };
    let cb = Closure::<dyn FnMut(JsValue, JsValue, JsValue)>::new(
        move |a: JsValue, b: JsValue, c: JsValue| {
            let args = Array::new();
            for v in [a, b, c] {
                if v.is_undefined() {
                    break;
                }
                args.push(&v);
            }
            match mode() {
                Mode::Alone => return alone::event(name, &args),
                Mode::Host if LEAVING.with(Cell::get) => return,
                Mode::Host => {}
                // Events before the mode is known: the snapshot taken
                // after it covers them.
                Mode::Deciding | Mode::Failed(_) => return,
            }
            let msg = obj(&[
                ("type", "event".into()),
                ("name", name.into()),
                ("args", args.into()),
            ]);
            let held = HELD.with(|h| match h.borrow_mut().as_mut() {
                Some(q) => {
                    q.push(msg.clone());
                    true
                }
                None => false,
            });
            if !held {
                post(&msg);
            }
        },
    );
    if let Ok(add) = Reflect::get(&event, &"addListener".into()) {
        let add: Function = add.unchecked_into();
        let _ = add.call1(&event, cb.as_ref());
    }
    cb.forget();
}

fn connect() {
    let port = match path("runtime.connectNative").and_then(|f| {
        f.unchecked_into::<Function>()
            .call1(&path("runtime").unwrap(), &HOST.into())
    }) {
        Ok(p) => p,
        Err(e) => {
            web_sys::console::error_2(&"[treetabs] connectNative failed".into(), &e);
            if mode() == Mode::Deciding
                && (!SEEN.with(Cell::get) || CHOICE.with(Cell::get) == Choice::On)
            {
                go_alone();
            } else {
                schedule_reconnect();
            }
            return;
        }
    };
    ANSWERED.with(|a| a.set(false));
    let on_message = Closure::<dyn FnMut(JsValue)>::new(|msg: JsValue| {
        if !ANSWERED.with(|a| a.replace(true)) {
            host_answered();
        }
        spawn_local(async move { handle(msg).await });
    });
    let on_disconnect = Closure::<dyn FnMut(JsValue)>::new(|port: JsValue| {
        // Chrome sets `runtime.lastError`, Firefox the port's `error` (its
        // `runtime.lastError` is there, but null).
        let message = |e: JsValue| {
            Some(e)
                .filter(|e| !e.is_undefined() && !e.is_null())
                .and_then(|e| Reflect::get(&e, &"message".into()).ok())
                .and_then(|m| m.as_string())
        };
        let err = path("runtime.lastError")
            .ok()
            .and_then(message)
            .or_else(|| Reflect::get(&port, &"error".into()).ok().and_then(message))
            .unwrap_or_default();
        web_sys::console::warn_1(&format!("[treetabs] host disconnected {err}").into());
        let choice = CHOICE.with(Cell::get);
        if mode() == Mode::Deciding
            && host_missing(&err)
            && (!SEEN.with(Cell::get) || choice == Choice::On)
        {
            go_alone();
            if choice == Choice::On {
                status_all("The companion app is not installed: the tree stays in the browser");
            }
            return;
        }
        set_mode(Mode::Host);
        PORT.with(|p| p.borrow_mut().take());
        VIEWERS.with(|v| {
            for port in v.borrow().values() {
                post_to(port, &host_down_status());
            }
        });
        schedule_reconnect();
    });
    for (ev, cb) in [
        ("onMessage", on_message.as_ref()),
        ("onDisconnect", on_disconnect.as_ref()),
    ] {
        if let Ok(e) = Reflect::get(&port, &ev.into())
            && let Ok(add) = Reflect::get(&e, &"addListener".into())
        {
            let _ = add.unchecked_into::<Function>().call1(&e, cb);
        }
    }
    on_message.forget();
    on_disconnect.forget();
    PORT.with(|p| *p.borrow_mut() = Some(port));
    RETRY_MS.with(|r| *r.borrow_mut() = 500);
    // Pages opened while the host was away.
    let vids: Vec<u32> = VIEWERS.with(|v| v.borrow().keys().copied().collect());
    for vid in vids {
        post(&obj(&[("type", "viewer_open".into()), ("vid", vid.into())]));
    }
    web_sys::console::log_1(&"[treetabs] connected to host".into());
}

/// The companion app answered for the first time on this port.
fn host_answered() {
    set_mode(Mode::Host);
    if !SEEN.with(|s| s.replace(true)) {
        spawn_local(async {
            let _ = call(
                "storage.local.set",
                &Array::of1(&obj(&[(HOST_SEEN, true.into())])),
            )
            .await;
        });
    }
}

/// The tree kept in the browser from a time without the companion app,
/// if there is one worth handing over.
async fn tree_to_hand_over() -> Option<JsValue> {
    let store = match storage::load_store().await {
        Ok(Some(s)) => s,
        Ok(None) => return None,
        Err(e) => {
            web_sys::console::error_1(
                &format!("[treetabs] the tree kept in the browser does not load: {e}").into(),
            );
            return None;
        }
    };
    use treetabs_core::TreeStore;
    if store.is_new() {
        return None;
    }
    let json = serde_json::to_string(treetabs_core::sync::Apply::tree(&store)).ok()?;
    // Chrome takes at most 64 MiB per message to a host.
    if json.len() > 32 * 1024 * 1024 {
        web_sys::console::error_1(&"[treetabs] the tree kept in the browser is too large to hand over; export it from the Data page".into());
        return None;
    }
    js_sys::JSON::parse(&json).ok()
}

fn schedule_reconnect() {
    let ms = RETRY_MS.with(|r| {
        let ms = *r.borrow();
        *r.borrow_mut() = (ms * 2).min(30_000);
        ms
    });
    let f = Closure::once_into_js(connect);
    set_timeout(f.unchecked_ref(), ms);
}

async fn handle(msg: JsValue) {
    let kind = Reflect::get(&msg, &"type".into())
        .ok()
        .and_then(|t| t.as_string())
        .unwrap_or_default();
    match kind.as_str() {
        "resync" => {
            let id = browser_id().await;
            HELD.with(|h| *h.borrow_mut() = Some(Vec::new()));
            // Before the snapshot: a new store takes this tree as its own.
            if let Some(tree) = tree_to_hand_over().await {
                web_sys::console::log_1(
                    &"[treetabs] handing the browser's tree to the companion app".into(),
                );
                let map = storage::bookmark_map()
                    .await
                    .map_or(JsValue::NULL, JsValue::from);
                let since = storage::detached_at()
                    .await
                    .map_or(JsValue::NULL, |s| JsValue::from_f64(s as f64));
                post(&obj(&[
                    ("type", "adopt".into()),
                    ("tree", tree),
                    ("bookmarks", map),
                    ("since", since),
                ]));
            }
            let snap = snapshot().await;
            let held = HELD.with(|h| h.borrow_mut().take()).unwrap_or_default();
            match snap {
                Ok(windows) => post(&obj(&[
                    ("type", "snapshot".into()),
                    ("windows", windows),
                    ("browser", id),
                    ("label", browser_label().into()),
                ])),
                Err(e) => web_sys::console::error_2(&"[treetabs] snapshot failed".into(), &e),
            }
            for m in held {
                post(&m);
            }
        }
        "call" => {
            let req = Reflect::get(&msg, &"req".into()).unwrap_or(JsValue::NULL);
            let api = Reflect::get(&msg, &"api".into())
                .ok()
                .and_then(|a| a.as_string())
                .unwrap_or_default();
            let args: Array = Reflect::get(&msg, &"args".into())
                .map(|a| a.unchecked_into())
                .unwrap_or_default();
            let reply = match call(&api, &args).await {
                Ok(v) => obj(&[("type", "result".into()), ("req", req), ("ok", v)]),
                Err(e) => obj(&[
                    ("type", "result".into()),
                    ("req", req),
                    ("err", error_text(&e).into()),
                ]),
            };
            post(&reply);
        }
        "viewer_msg" => viewer_message(&msg),
        // The app keeps the tree now (as its tree or as a backup).
        "adopted" => storage::clear_all().await,
        other => web_sys::console::warn_1(&format!("[treetabs] unknown message {other}").into()),
    }
}

/// A readable name for this browser ("Firefox", "Brave", …), shown next
/// to its windows when the tree has more than one profile.
fn browser_label() -> String {
    let g = js_sys::global();
    let nav = Reflect::get(&g, &"navigator".into()).unwrap_or(JsValue::NULL);
    let ua = Reflect::get(&nav, &"userAgent".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let has = |k: &str| Reflect::get(&nav, &k.into()).is_ok_and(|v| !v.is_undefined());
    // Chrome tells its brand only here.
    let brands = Reflect::get(&nav, &"userAgentData".into())
        .and_then(|d| Reflect::get(&d, &"brands".into()))
        .map(|b| {
            Array::from(&b)
                .iter()
                .filter_map(|x| Reflect::get(&x, &"brand".into()).ok()?.as_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if ua.contains("Firefox/") {
        "Firefox".into()
    } else if has("brave") {
        "Brave".into()
    } else if ua.contains("OPR/") {
        "Opera".into()
    } else if ua.contains("Edg/") {
        "Edge".into()
    } else if brands.iter().any(|b| b == "Google Chrome") {
        "Chrome".into()
    } else {
        "Chromium".into()
    }
}

async fn snapshot() -> Result<JsValue, JsValue> {
    let args = Array::of1(&obj(&[("populate", true.into())]));
    call("windows.getAll", &args).await
}

/// Random id of this browser profile, created once and kept in storage.
async fn browser_id() -> JsValue {
    if let Some(id) = BROWSER_ID.with(|b| b.borrow().clone()) {
        return id;
    }
    let id = load_browser_id().await;
    BROWSER_ID.with(|b| *b.borrow_mut() = Some(id.clone()));
    id
}

async fn load_browser_id() -> JsValue {
    let key = "browserId";
    if let Ok(v) = call("storage.local.get", &Array::of1(&key.into())).await
        && let Ok(id) = Reflect::get(&v, &key.into())
        && id.is_string()
    {
        return id;
    }
    let crypto = Reflect::get(&js_sys::global(), &"crypto".into()).unwrap_or(JsValue::NULL);
    let id = Reflect::get(&crypto, &"randomUUID".into())
        .ok()
        .and_then(|f| f.dyn_into::<Function>().ok())
        .and_then(|f| f.call0(&crypto).ok())
        .unwrap_or_else(|| format!("{}", js_sys::Math::random()).into());
    let _ = call("storage.local.set", &Array::of1(&obj(&[(key, id.clone())]))).await;
    id
}

/// Call `chrome.<api>(...args)`; MV3 APIs return promises.
async fn call(api: &str, args: &Array) -> Result<JsValue, JsValue> {
    let (ns, name) = api
        .rsplit_once('.')
        .ok_or_else(|| JsValue::from_str("bad api name"))?;
    let target = path(ns)?;
    let f: Function = Reflect::get(&target, &name.into())?
        .dyn_into()
        .map_err(|_| JsValue::from_str(&format!("chrome.{api} is not a function")))?;
    let r = f.apply(&target, args)?;
    match r.dyn_into::<Promise>() {
        Ok(p) => JsFuture::from(p).await,
        Err(v) => Ok(v),
    }
}
