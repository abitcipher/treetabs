//! Tree window of the extension. The page talks to the background service
//! worker over a `chrome.runtime` port named "viewer"; the worker relays to
//! the native host, which connects to the treetabs server as a viewer.

use js_sys::{Function, Object, Reflect};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use treetabs_core::protocol::{ClientMsg, ServerMsg};
use treetabs_ui::{Poll, Transport};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

#[derive(Default)]
struct Inbox {
    msgs: VecDeque<ServerMsg>,
    lost: bool,
}

struct PortTransport {
    port: Option<JsValue>,
    inbox: Rc<RefCell<Inbox>>,
    ctx: egui::Context,
    /// `chrome-extension://<id>`
    origin: String,
    /// A file picked for import, once read.
    picked: Rc<RefCell<Option<(String, Vec<u8>)>>>,
}

fn chrome_path(p: &str) -> Result<JsValue, JsValue> {
    // `browser` in Firefox, `chrome` elsewhere.
    let g = js_sys::global();
    let mut v = match Reflect::get(&g, &"browser".into()) {
        Ok(b) if b.is_object() => b,
        _ => Reflect::get(&g, &"chrome".into())?,
    };
    for part in p.split('.') {
        v = Reflect::get(&v, &part.into())?;
    }
    Ok(v)
}

fn add_listener(target: &JsValue, event: &str, cb: &JsValue) -> Result<(), JsValue> {
    let e = Reflect::get(target, &event.into())?;
    let add: Function = Reflect::get(&e, &"addListener".into())?.dyn_into()?;
    add.call1(&e, cb)?;
    Ok(())
}

impl PortTransport {
    fn open(&mut self) -> Result<(), JsValue> {
        let runtime = chrome_path("runtime")?;
        let connect: Function = Reflect::get(&runtime, &"connect".into())?.dyn_into()?;
        let info = Object::new();
        Reflect::set(&info, &"name".into(), &"viewer".into())?;
        let port = connect.call1(&runtime, &info)?;

        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        let on_msg = Closure::<dyn FnMut(JsValue)>::new(move |m: JsValue| {
            let Some(text) = m.as_string() else { return };
            match serde_json::from_str::<ServerMsg>(&text) {
                Ok(msg) => inbox.borrow_mut().msgs.push_back(msg),
                Err(e) => web_sys::console::warn_1(&format!("[treetabs] bad message: {e}").into()),
            }
            ctx.request_repaint();
        });
        let inbox = self.inbox.clone();
        let ctx = self.ctx.clone();
        let on_disconnect = Closure::<dyn FnMut(JsValue)>::new(move |_p: JsValue| {
            inbox.borrow_mut().lost = true;
            ctx.request_repaint();
        });
        add_listener(&port, "onMessage", on_msg.as_ref())?;
        add_listener(&port, "onDisconnect", on_disconnect.as_ref())?;
        on_msg.forget();
        on_disconnect.forget();
        self.port = Some(port);
        Ok(())
    }
}

impl Transport for PortTransport {
    fn send(&mut self, msg: &ClientMsg) -> Result<(), String> {
        let port = self.port.as_ref().ok_or("not connected")?;
        let text = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        let post: Function = Reflect::get(port, &"postMessage".into())
            .and_then(|f| f.dyn_into().map_err(|_| JsValue::NULL))
            .map_err(|_| "no postMessage".to_string())?;
        post.call1(port, &text.into()).map(|_| ()).map_err(|e| format!("{e:?}"))
    }

    fn poll(&mut self) -> Poll {
        let mut inbox = self.inbox.borrow_mut();
        let p = Poll { msgs: inbox.msgs.drain(..).collect(), lost: inbox.lost };
        if inbox.lost {
            inbox.lost = false;
            drop(inbox);
            self.port = None;
        }
        p
    }

    fn reconnect(&mut self) -> Result<(), String> {
        self.port = None;
        self.open().map_err(|e| format!("{e:?}"))
    }

    fn connected(&self) -> bool {
        self.port.is_some()
    }

    fn describe(&self) -> String {
        treetabs_ui::i18n::tr("via the extension").into()
    }

    fn load_settings(&self) -> Option<String> {
        local_storage()?.get_item(SETTINGS_KEY).ok().flatten()
    }

    fn save_settings(&mut self, json: &str) {
        if let Some(s) = local_storage() {
            let _ = s.set_item(SETTINGS_KEY, json);
        }
    }

    /// A download, made from a Blob in the page.
    fn save_file(&mut self, name: &str, mime: &str, data: &[u8]) -> Result<Option<String>, String> {
        download(name, mime, data).map_err(|e| format!("{e:?}"))?;
        Ok(Some(name.to_owned()))
    }

    /// The browser's file picker; the file is read in the background and handed over by `take_file`.
    fn open_file(&mut self) {
        if let Err(e) = pick_file(self.picked.clone(), self.ctx.clone()) {
            web_sys::console::error_2(&"[treetabs] cannot open the file picker".into(), &e);
        }
    }

    fn take_file(&mut self) -> Option<(String, Vec<u8>)> {
        self.picked.borrow_mut().take()
    }

    /// Chrome's own favicon cache (needs the "favicon" permission): works for saved tabs too, and needs no network access
    fn favicon_uri(&self, n: &treetabs_core::Node) -> Option<String> {
        if self.origin.starts_with("moz-extension:") {
            /// Firefox has no favicon cache for add-ons: the icon the tab reported (http needs the optional "all websites" permission
            let f = n.favicon.as_deref()?;
            return (f.starts_with("data:image/") || f.starts_with("https://") || f.starts_with("http://")).then(|| f.to_owned());
        }
        let url = n.url.as_deref().filter(|u| u.starts_with("http://") || u.starts_with("https://"))?;
        let page = js_sys::encode_uri_component(url);
        Some(format!("{}/_favicon/?pageUrl={page}&size=32", self.origin))
    }
}

const SETTINGS_KEY: &str = "treetabs.ui";

fn download(name: &str, mime: &str, data: &[u8]) -> Result<(), JsValue> {
    let document = web_sys::window().ok_or("no window")?.document().ok_or("no document")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(data));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let a: web_sys::HtmlAnchorElement = document.create_element("a")?.dyn_into()?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    /// Give the download a moment before the URL goes
    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    web_sys::window().ok_or("no window")?.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000)?;
    Ok(())
}

fn pick_file(slot: Rc<RefCell<Option<(String, Vec<u8>)>>>, ctx: egui::Context) -> Result<(), JsValue> {
    let document = web_sys::window().ok_or("no window")?.document().ok_or("no document")?;
    let input: web_sys::HtmlInputElement = document.create_element("input")?.dyn_into()?;
    input.set_type("file");
    input.set_accept(".json,application/json");
    let picked = input.clone();
    let on_change = Closure::once_into_js(move || {
        let Some(file) = picked.files().and_then(|f| f.get(0)) else { return };
        let name = file.name();
        wasm_bindgen_futures::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buf) => *slot.borrow_mut() = Some((name, js_sys::Uint8Array::new(&buf).to_vec())),
                Err(e) => web_sys::console::error_2(&"[treetabs] cannot read the file".into(), &e),
            }
            ctx.request_repaint();
        });
    });
    input.set_onchange(Some(on_change.unchecked_ref()));
    /// Allowed: this runs right after the user's click (transient activation)
    input.click();
    Ok(())
}

/// Loads extension URLs (Chrome's `_favicon` service) and `data:` icons for egui's image loaders, which only fetch http(s) themselves
#[derive(Default)]
struct ExtensionLoader {
    cache: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Fetch>>>,
    prefix: String,
}

#[derive(Clone)]
enum Fetch {
    Pending,
    Ready(std::sync::Arc<[u8]>),
    Failed(String),
}

impl egui::load::BytesLoader for ExtensionLoader {
    fn id(&self) -> &str {
        "treetabs::ExtensionLoader"
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> egui::load::BytesLoadResult {
        use egui::load::{BytesPoll, LoadError};
        /// Our own pages, and data: icons (fetch reads those too).
        if !(uri.starts_with("data:image/") || (!self.prefix.is_empty() && uri.starts_with(&self.prefix))) {
            return Err(LoadError::NotSupported);
        }
        let mut cache = self.cache.lock().expect("single thread");
        match cache.get(uri) {
            Some(Fetch::Ready(b)) => {
                // No mime: the image loader looks at the bytes (png, ico, …).
                return Ok(BytesPoll::Ready { size: None, bytes: egui::load::Bytes::Shared(b.clone()), mime: None });
            }
            Some(Fetch::Failed(e)) => return Err(LoadError::Loading(e.clone())),
            Some(Fetch::Pending) => return Ok(BytesPoll::Pending { size: None }),
            None => {}
        }
        cache.insert(uri.to_owned(), Fetch::Pending);
        let (cache, ctx, uri) = (self.cache.clone(), ctx.clone(), uri.to_owned());
        wasm_bindgen_futures::spawn_local(async move {
            let r = fetch_bytes(&uri).await.map_err(|e| format!("{e:?}"));
            let entry = match r {
                Ok(b) => Fetch::Ready(b.into()),
                Err(e) => Fetch::Failed(e),
            };
            cache.lock().expect("single thread").insert(uri, entry);
            ctx.request_repaint();
        });
        Ok(BytesPoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.cache.lock().expect("single thread").remove(uri);
    }

    fn forget_all(&self) {
        self.cache.lock().expect("single thread").clear();
    }

    fn byte_size(&self) -> usize {
        let cache = self.cache.lock().expect("single thread");
        cache.values().map(|f| if let Fetch::Ready(b) = f { b.len() } else { 0 }).sum()
    }

    fn has_pending(&self) -> bool {
        self.cache.lock().expect("single thread").values().any(|f| matches!(f, Fetch::Pending))
    }
}

async fn fetch_bytes(uri: &str) -> Result<Vec<u8>, JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(uri)).await?.dyn_into()?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()).into());
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer()?).await?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[wasm_bindgen(start)]
pub fn start() {
    /// Rust log messages go to the page console.
    let _ = eframe::WebLogger::init(log::LevelFilter::Info);
    let document = web_sys::window().expect("window").document().expect("document");
    let canvas: web_sys::HtmlCanvasElement =
        document.get_element_by_id("tree").expect("#tree canvas").dyn_into().expect("canvas");
    wasm_bindgen_futures::spawn_local(async move {
        let runner = eframe::WebRunner::new();
        let r = runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| {
                    egui_extras::install_image_loaders(&cc.egui_ctx);
                    let origin = web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default();
                    let loader = ExtensionLoader { prefix: format!("{origin}/"), ..Default::default() };
                    cc.egui_ctx.add_bytes_loader(std::sync::Arc::new(loader));
                    let mut t = PortTransport { port: None, inbox: Default::default(), ctx: cc.egui_ctx.clone(), origin, picked: Default::default() };
                    if let Err(e) = t.open() {
                        web_sys::console::error_2(&"[treetabs] cannot reach the extension".into(), &e);
                    }
                    Ok(Box::new(treetabs_ui::App::new(Box::new(t), None)))
                }),
            )
            .await;
        if let Err(e) = r {
            web_sys::console::error_2(&"[treetabs] failed to start".into(), &e);
        }
    });
}
