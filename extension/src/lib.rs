//! treetabs browser extension.
//!
//! Deliberately thin: it forwards tab, window and bookmark events to the
//! native host, answers "resync" with a full window snapshot, and executes
//! `chrome.*` calls the host asks for. All tree logic lives in the host.

use js_sys::{Array, Function, Object, Promise, Reflect};
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

#[oxichrome::extension(
    name = "treetabs",
    version = "0.2.0",
    description = "Tree of windows and tabs, kept in sync with bookmarks",
    permissions = ["tabs", "bookmarks", "nativeMessaging", "storage", "favicon"]
)]
struct Extension;

/// Native host name (see `treetabs install-host`).
const HOST: &str = "dev.treetabs.host";

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

thread_local! {
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
    listen("action.onClicked", |_tab| spawn_local(async { open_tree_window().await }));
    // Same action for keyboard shortcuts and tests: `treetabsOpenTree()`.
    let open = Closure::<dyn FnMut()>::new(|| spawn_local(async { open_tree_window().await }));
    let _ = Reflect::set(&js_sys::global(), &"treetabsOpenTree".into(), open.as_ref());
    open.forget();
    listen("runtime.onConnect", viewer_connected);
    connect();
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
        .and_then(|f| f.unchecked_into::<Function>().call1(&path("runtime").unwrap(), &TREE_PAGE.into()))
        .ok()
        .and_then(|u| u.as_string())
        .unwrap_or_default();
    // Look for an existing tree window.
    let query = Array::of1(&obj(&[("url", url.clone().into())]));
    if let Ok(tabs) = call("tabs.query", &query).await {
        let tabs: Array = tabs.unchecked_into();
        if let Some(tab) = tabs.iter().next() {
            let win = Reflect::get(&tab, &"windowId".into()).unwrap_or(JsValue::NULL);
            let _ = call("windows.update", &Array::of2(&win, &obj(&[("focused", true.into())]))).await;
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
        Some(b) if num(&b, "width").unwrap_or(0.0) >= 200.0 && num(&b, "height").unwrap_or(0.0) >= 200.0 => {
            ["left", "top", "width", "height"].map(|k| (k, num(&b, k).unwrap_or(0.0)))
        }
        _ => {
            let height = call("windows.getLastFocused", &Array::new())
                .await
                .ok()
                .and_then(|w| num(&w, "height"))
                .unwrap_or(900.0);
            [("left", 0.0), ("top", 0.0), ("width", f64::from(TREE_WIDTH)), ("height", height)]
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
    if Reflect::get(&port, &"name".into()).ok().and_then(|n| n.as_string()).as_deref() != Some("viewer") {
        return;
    }
    let vid = NEXT_VIEWER.with(|n| {
        let v = *n.borrow();
        *n.borrow_mut() += 1;
        v
    });
    let on_msg = Closure::<dyn FnMut(JsValue)>::new(move |m: JsValue| {
        post(&obj(&[("type", "viewer_msg".into()), ("vid", vid.into()), ("msg", m)]));
    });
    let on_disconnect = Closure::<dyn FnMut(JsValue)>::new(move |_p: JsValue| {
        VIEWERS.with(|v| v.borrow_mut().remove(&vid));
        PARTS.with(|p| p.borrow_mut().remove(&vid));
        post(&obj(&[("type", "viewer_close".into()), ("vid", vid.into())]));
    });
    for (ev, cb) in [("onMessage", on_msg.as_ref()), ("onDisconnect", on_disconnect.as_ref())] {
        if let Ok(e) = Reflect::get(&port, &ev.into())
            && let Ok(add) = Reflect::get(&e, &"addListener".into())
        {
            let _ = add.unchecked_into::<Function>().call1(&e, cb);
        }
    }
    on_msg.forget();
    on_disconnect.forget();
    VIEWERS.with(|v| v.borrow_mut().insert(vid, port.clone()));
    if PORT.with(|p| p.borrow().is_some()) {
        post(&obj(&[("type", "viewer_open".into()), ("vid", vid.into())]));
    } else {
        post_to(&port, &host_down_status());
    }
}

fn host_down_status() -> JsValue {
    r#"{"type":"status","browsers":0,"message":"treetabs host is not running (see the extension's service worker console)"}"#.into()
}

/// Message for a viewer from the host; large ones arrive in parts.
fn viewer_message(msg: &JsValue) {
    let vid = Reflect::get(msg, &"vid".into()).ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as u32;
    let part = Reflect::get(msg, &"part".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
    let last = Reflect::get(msg, &"last".into()).ok().and_then(|v| v.as_bool()).unwrap_or(true);
    let full = PARTS.with(|p| {
        let mut p = p.borrow_mut();
        let buf = p.entry(vid).or_default();
        buf.push_str(&part);
        if last { p.remove(&vid) } else { None }
    });
    if let Some(text) = full {
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
    let cb = Closure::<dyn FnMut(JsValue, JsValue, JsValue)>::new(move |a: JsValue, b: JsValue, c: JsValue| {
        let args = Array::new();
        for v in [a, b, c] {
            if v.is_undefined() {
                break;
            }
            args.push(&v);
        }
        let msg = obj(&[("type", "event".into()), ("name", name.into()), ("args", args.into())]);
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
    });
    if let Ok(add) = Reflect::get(&event, &"addListener".into()) {
        let add: Function = add.unchecked_into();
        let _ = add.call1(&event, cb.as_ref());
    }
    cb.forget();
}

fn connect() {
    let port = match path("runtime.connectNative")
        .and_then(|f| f.unchecked_into::<Function>().call1(&path("runtime").unwrap(), &HOST.into()))
    {
        Ok(p) => p,
        Err(e) => {
            web_sys::console::error_2(&"[treetabs] connectNative failed".into(), &e);
            schedule_reconnect();
            return;
        }
    };
    let on_message = Closure::<dyn FnMut(JsValue)>::new(|msg: JsValue| {
        spawn_local(async move { handle(msg).await });
    });
    let on_disconnect = Closure::<dyn FnMut(JsValue)>::new(|_port: JsValue| {
        let err = path("runtime.lastError")
            .ok()
            .and_then(|e| Reflect::get(&e, &"message".into()).ok())
            .and_then(|m| m.as_string())
            .unwrap_or_default();
        web_sys::console::warn_1(&format!("[treetabs] host disconnected {err}").into());
        PORT.with(|p| p.borrow_mut().take());
        VIEWERS.with(|v| {
            for port in v.borrow().values() {
                post_to(port, &host_down_status());
            }
        });
        schedule_reconnect();
    });
    for (ev, cb) in [("onMessage", on_message.as_ref()), ("onDisconnect", on_disconnect.as_ref())] {
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
    let kind = Reflect::get(&msg, &"type".into()).ok().and_then(|t| t.as_string()).unwrap_or_default();
    match kind.as_str() {
        "resync" => {
            let id = browser_id().await;
            HELD.with(|h| *h.borrow_mut() = Some(Vec::new()));
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
            let api = Reflect::get(&msg, &"api".into()).ok().and_then(|a| a.as_string()).unwrap_or_default();
            let args: Array = Reflect::get(&msg, &"args".into()).map(|a| a.unchecked_into()).unwrap_or_default();
            let reply = match call(&api, &args).await {
                Ok(v) => obj(&[("type", "result".into()), ("req", req), ("ok", v)]),
                Err(e) => {
                    let text = Reflect::get(&e, &"message".into())
                        .ok()
                        .and_then(|m| m.as_string())
                        .or_else(|| e.as_string())
                        .unwrap_or_else(|| format!("{e:?}"));
                    obj(&[("type", "result".into()), ("req", req), ("err", text.into())])
                }
            };
            post(&reply);
        }
        "viewer_msg" => viewer_message(&msg),
        other => web_sys::console::warn_1(&format!("[treetabs] unknown message {other}").into()),
    }
}

/// A readable name for this browser ("Firefox", "Brave", …), shown next
/// to its windows when the tree has more than one profile.
fn browser_label() -> String {
    let g = js_sys::global();
    let nav = Reflect::get(&g, &"navigator".into()).unwrap_or(JsValue::NULL);
    let ua = Reflect::get(&nav, &"userAgent".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
    let has = |k: &str| Reflect::get(&nav, &k.into()).is_ok_and(|v| !v.is_undefined());
    // Chrome tells its brand only here.
    let brands = Reflect::get(&nav, &"userAgentData".into())
        .and_then(|d| Reflect::get(&d, &"brands".into()))
        .map(|b| Array::from(&b).iter().filter_map(|x| Reflect::get(&x, &"brand".into()).ok()?.as_string()).collect::<Vec<_>>())
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
    let (ns, name) = api.rsplit_once('.').ok_or_else(|| JsValue::from_str("bad api name"))?;
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
