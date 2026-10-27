//! Running without the companion app: the extension owns the tree. The
//! same hub as in the app runs here, with the tree kept in the browser's
//! extension storage (see [`crate::storage`]).
//!
//! The browser is the hub's client [`BROWSER`]; each tree-window page is a
//! viewer with its own id. The hub's calls to the browser are made here,
//! and their results go back to it, as the app's host would do.

use crate::storage::{self, BrowserBackend};
use crate::{browser_id, browser_label, call, from_js, post_viewer, snapshot, to_js};
use js_sys::{Array, Function};
use std::cell::RefCell;
use treetabs_core::memstore::MemStore;
use treetabs_core::protocol::{ClientMsg, Role, ServerMsg};
use treetabs_sync::hub::{Hub, TICK};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

/// Client id of the browser itself (viewers count from 1).
const BROWSER: u64 = 0;

/// Chrome stops an idle background script after 30 s, and missed events
/// make the tree worse (a closed tab is kept as "crash detected" instead
/// of going to the trash). An extension call this often keeps it running.
const KEEP_ALIVE_MS: i32 = 20_000;

thread_local! {
    static HUB: RefCell<Option<Hub<BrowserBackend>>> = const { RefCell::new(None) };
    /// While a window snapshot is taken, browser events wait here, so the
    /// hub never gets an older snapshot after newer events.
    static HELD: RefCell<Option<Vec<ClientMsg>>> = const { RefCell::new(None) };
    /// The tick and keep-alive timers run (they do nothing without a hub).
    static TIMERS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Stop owning the tree (the companion app takes it): everything is
/// saved first.
pub async fn stop() {
    pump();
    HUB.with(|h| h.borrow_mut().take());
    HELD.with(|h| h.borrow_mut().take());
    storage::idle().await;
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setInterval)]
    fn set_interval(f: &Function, ms: i32) -> JsValue;
}

fn with_hub<R>(f: impl FnOnce(&mut Hub<BrowserBackend>) -> R) -> Option<R> {
    HUB.with(|h| h.borrow_mut().as_mut().map(f))
}

/// Load the tree from storage and start.
///
/// # Errors
///
/// When the saved tree does not load; nothing is written then.
pub async fn start() -> Result<(), String> {
    let store = match storage::load_store().await? {
        Some(s) => s,
        None => MemStore::open(None).map_err(|e| e.to_string())?,
    };
    for w in &store.warnings {
        log::warn!("store: {w}");
    }
    let backend = storage::load_backend().await;
    let mut hub = Hub::new(store, backend);
    hub.connected(BROWSER);
    hub.message(
        BROWSER,
        ClientMsg::Hello {
            role: Role::Browser,
            name: "browser".into(),
        },
    );
    HUB.with(|h| *h.borrow_mut() = Some(hub));
    log::info!("running without the companion app");
    // Pages opened before now (also while loading) get the tree now.
    for vid in crate::viewer_ids() {
        viewer_open(vid);
    }
    pump();
    if TIMERS.with(|t| t.replace(true)) {
        // Started before (the app was turned on and off again).
        return Ok(());
    }
    let tick = Closure::<dyn FnMut()>::new(|| {
        with_hub(Hub::tick);
        pump();
    });
    let ms = i32::try_from(TICK.as_millis()).unwrap_or(250);
    set_interval(tick.as_ref().unchecked_ref(), ms);
    tick.forget();
    let keep_alive = Closure::<dyn FnMut()>::new(|| {
        spawn_local(async {
            let _ = call("runtime.getPlatformInfo", &Array::new()).await;
        });
    });
    set_interval(keep_alive.as_ref().unchecked_ref(), KEEP_ALIVE_MS);
    keep_alive.forget();
    Ok(())
}

/// A browser event (`tabs.onCreated`, …) with its arguments.
pub fn event(name: &str, args: &JsValue) {
    let args = match from_js(args) {
        serde_json::Value::Array(a) => a,
        _ => Vec::new(),
    };
    let msg = ClientMsg::BrowserEvent {
        name: name.to_owned(),
        args,
    };
    let held = HELD.with(|h| match h.borrow_mut().as_mut() {
        Some(q) => {
            q.push(msg.clone());
            true
        }
        None => false,
    });
    if !held {
        with_hub(|h| h.message(BROWSER, msg));
        pump();
    }
}

pub fn viewer_open(vid: u32) {
    let id = u64::from(vid);
    with_hub(|h| {
        h.connected(id);
        h.message(
            id,
            ClientMsg::Hello {
                role: Role::Viewer,
                name: "tree window".into(),
            },
        );
    });
    pump();
}

/// A message from a tree-window page: a [`ClientMsg`] as JSON text.
pub fn viewer_msg(vid: u32, text: &str) {
    match serde_json::from_str::<ClientMsg>(text) {
        Ok(m) => {
            with_hub(|h| h.message(u64::from(vid), m));
            pump();
        }
        Err(e) => log::warn!("viewer {vid}: bad message: {e}"),
    }
}

pub fn viewer_close(vid: u32) {
    with_hub(|h| h.closed(u64::from(vid)));
    pump();
}

/// The tree as JSON (for tests).
pub fn tree_json() -> Option<String> {
    with_hub(|h| serde_json::to_string(h.tree()).ok()).flatten()
}

/// Deliver the hub's messages and save what changed.
fn pump() {
    let Some((out, writes)) = with_hub(|h| (h.take_out(), h.store_mut().take_writes())) else {
        return;
    };
    storage::queue_store_writes(writes);
    storage::flush();
    for (id, m) in out {
        if id == BROWSER {
            to_browser(m);
        } else {
            post_viewer(id, &m);
        }
    }
}

/// The hub asks the browser for something.
fn to_browser(m: ServerMsg) {
    match m {
        ServerMsg::Call { req, api, args } => spawn_local(async move {
            let args: Array = to_js(&args).unchecked_into();
            let msg = match call(&api, &args).await {
                Ok(v) => ClientMsg::CallResult {
                    req,
                    ok: Some(from_js(&v)).filter(|x| !x.is_null()),
                    err: None,
                },
                Err(e) => ClientMsg::CallResult {
                    req,
                    ok: None,
                    err: Some(crate::error_text(&e)),
                },
            };
            with_hub(|h| h.message(BROWSER, msg));
            pump();
        }),
        ServerMsg::Resync => spawn_local(resync()),
        _ => {}
    }
}

/// All windows with their tabs, for the hub to set the tree right.
async fn resync() {
    HELD.with(|h| *h.borrow_mut() = Some(Vec::new()));
    let id = browser_id().await.as_string();
    let snap = snapshot().await;
    let held = HELD.with(|h| h.borrow_mut().take()).unwrap_or_default();
    match snap {
        Ok(w) => {
            let windows = match from_js(&w) {
                serde_json::Value::Array(a) => a,
                _ => Vec::new(),
            };
            let msg = ClientMsg::BrowserSnapshot {
                windows,
                browser: id,
                label: Some(browser_label()),
            };
            with_hub(|h| h.message(BROWSER, msg));
        }
        Err(e) => web_sys::console::error_2(&"[treetabs] snapshot failed".into(), &e),
    }
    for m in held {
        with_hub(|h| h.message(BROWSER, m));
    }
    pump();
}
