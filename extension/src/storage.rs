//! The tree in the browser's extension storage, for running without the
//! companion app.
//!
//! Keys in `storage.local` (every value is JSON text):
//! - `tt.tree`: the snapshot, a portable tree with `journal_seq`;
//! - `tt.j.<seq>`: one journal entry each; loading reads `seq + 1`,
//!   `seq + 2`, … until one is missing;
//! - `tt.f.<name>`: the hub's small files (`settings.json`, …);
//! - `tt.exports`: the list of copies, and `tt.x.<name>`: one copy each.
//!
//! Writes go through one queue, in order; everything waiting is written
//! together, and one `storage.local.set` is all or nothing, so a batch of
//! journal entries is never half saved.

use crate::{call, post_viewer};
use js_sys::{Array, Object, Reflect};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use treetabs_core::Tree;
use treetabs_core::backups::{BackupInfo, BackupKind};
use treetabs_core::memstore::{MemStore, Writes};
use treetabs_core::protocol::{ServerMsg, StoreKind};
use treetabs_sync::hub::{Backend, ExportWhen};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

pub const TREE: &str = "tt.tree";
const EXPORTS: &str = "tt.exports";
const FILES: [&str; 3] = ["settings.json", "profiles.json", "bookmarks.json"];
/// Journal keys asked for at once while loading.
const JOURNAL_CHUNK: u64 = 256;

fn journal_key(seq: u64) -> String {
    format!("tt.j.{seq}")
}

fn file_key(name: &str) -> String {
    format!("tt.f.{name}")
}

fn copy_key(name: &str) -> String {
    format!("tt.x.{name}")
}

enum Write {
    Set(Vec<(String, String)>),
    Remove(Vec<String>),
}

thread_local! {
    static QUEUE: RefCell<Vec<Write>> = const { RefCell::new(Vec::new()) };
    static WRITING: Cell<bool> = const { Cell::new(false) };
}

fn queue_set(key: String, value: String) {
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        match q.last_mut() {
            Some(Write::Set(pairs)) => {
                pairs.retain(|(k, _)| *k != key);
                pairs.push((key, value));
            }
            _ => q.push(Write::Set(vec![(key, value)])),
        }
    });
}

fn queue_remove(keys: Vec<String>) {
    if !keys.is_empty() {
        QUEUE.with(|q| q.borrow_mut().push(Write::Remove(keys)));
    }
}

/// Queue what the store needs saved.
pub fn queue_store_writes(w: Writes) {
    for e in w.entries {
        match serde_json::to_string(&e) {
            Ok(json) => queue_set(journal_key(e.seq), json),
            Err(err) => log::error!("cannot save journal entry {}: {err}", e.seq),
        }
    }
    if let Some(snap) = w.snapshot {
        queue_set(TREE.into(), snap);
        if let Some((a, b)) = w.drop_journal {
            queue_remove((a..=b).map(journal_key).collect());
        }
    }
}

/// Start writing the queue, unless a write is under way (it goes on with
/// whatever is queued when it ends).
pub fn flush() {
    if WRITING.with(Cell::get) || QUEUE.with(|q| q.borrow().is_empty()) {
        return;
    }
    WRITING.with(|w| w.set(true));
    spawn_local(async {
        loop {
            let batch: Vec<Write> = QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
            if batch.is_empty() {
                break;
            }
            for w in batch {
                let r = match w {
                    Write::Set(pairs) => {
                        let o = Object::new();
                        for (k, v) in &pairs {
                            let _ = Reflect::set(&o, &k.into(), &v.into());
                        }
                        call("storage.local.set", &Array::of1(&o)).await
                    }
                    Write::Remove(keys) => {
                        let a: Array = keys.iter().map(|k| JsValue::from_str(k)).collect();
                        call("storage.local.remove", &Array::of1(&a)).await
                    }
                };
                if let Err(e) = r {
                    web_sys::console::error_2(&"[treetabs] saving to storage failed".into(), &e);
                }
            }
        }
        WRITING.with(|w| w.set(false));
    });
}

/// `storage.local.get(keys)`.
async fn get(keys: &[String]) -> Result<JsValue, JsValue> {
    let a: Array = keys.iter().map(|k| JsValue::from_str(k)).collect();
    call("storage.local.get", &Array::of1(&a)).await
}

fn text(o: &JsValue, key: &str) -> Option<String> {
    Reflect::get(o, &key.into()).ok()?.as_string()
}

/// The tree kept in the browser, or `None` when there is none.
///
/// # Errors
///
/// When the saved snapshot does not load (it is then left as it is).
pub async fn load_store() -> Result<Option<MemStore>, String> {
    let o = get(&[TREE.to_owned()])
        .await
        .map_err(|e| format!("{e:?}"))?;
    let Some(snap) = text(&o, TREE) else {
        return Ok(None);
    };
    let mut store = MemStore::open(Some(&snap)).map_err(|e| e.to_string())?;
    'chunks: loop {
        let first = store.next_seq();
        let keys: Vec<String> = (first..first + JOURNAL_CHUNK).map(journal_key).collect();
        let o = get(&keys).await.map_err(|e| format!("{e:?}"))?;
        for k in &keys {
            match text(&o, k) {
                Some(entry) if store.replay(&entry) => {}
                _ => break 'chunks,
            }
        }
    }
    Ok(Some(store))
}

/// The hub's files and the list of copies.
pub async fn load_backend() -> BrowserBackend {
    let mut keys: Vec<String> = FILES.iter().map(|f| file_key(f)).collect();
    keys.push(EXPORTS.into());
    let o = get(&keys).await.unwrap_or(JsValue::UNDEFINED);
    let files = FILES
        .iter()
        .filter_map(|f| Some(((*f).to_owned(), text(&o, &file_key(f))?.into_bytes())))
        .collect();
    let exports = text(&o, EXPORTS)
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    BrowserBackend { files, exports }
}

/// Where the tree's journal stood in the app's store when it came to the
/// browser (`tt.since`), if it came from there.
const SINCE: &str = "tt.since";

/// Keep the tree the companion app sent (with its bookmark map and the
/// app's journal position), replacing whatever the browser kept before.
///
/// # Errors
///
/// When it cannot be written.
pub async fn keep_detached(
    mut tree: Tree,
    bookmarks: Option<String>,
    seq: u64,
) -> Result<(), String> {
    idle().await;
    remove_ours().await?;
    tree.journal_seq = Some(0);
    let json = serde_json::to_string(&tree).map_err(|e| e.to_string())?;
    let o = Object::new();
    let _ = Reflect::set(&o, &TREE.into(), &json.into());
    let _ = Reflect::set(&o, &SINCE.into(), &JsValue::from_f64(seq as f64));
    if let Some(b) = bookmarks {
        let _ = Reflect::set(&o, &file_key("bookmarks.json").into(), &b.into());
    }
    call("storage.local.set", &Array::of1(&o))
        .await
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// See [`keep_detached`].
pub async fn detached_at() -> Option<u64> {
    let o = get(&[SINCE.to_owned()]).await.ok()?;
    let v = Reflect::get(&o, &SINCE.into()).ok()?.as_f64()?;
    (v >= 0.0 && v.fract() == 0.0).then_some(v as u64)
}

/// Until every queued write is done.
pub async fn idle() {
    while WRITING.with(Cell::get) || !QUEUE.with(|q| q.borrow().is_empty()) {
        flush();
        crate::sleep(20).await;
    }
}

/// Remove every `tt.*` key now.
async fn remove_ours() -> Result<(), String> {
    let all = call("storage.local.get", &Array::of1(&JsValue::NULL))
        .await
        .map_err(|e| format!("{e:?}"))?;
    let keys: Array = Object::keys(all.unchecked_ref::<Object>())
        .iter()
        .filter(|k| k.as_string().is_some_and(|k| k.starts_with("tt.")))
        .collect();
    call("storage.local.remove", &Array::of1(&keys))
        .await
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// The bookmark map saved by the mirror (`bookmarks.json`), if any.
pub async fn bookmark_map() -> Option<String> {
    let key = file_key("bookmarks.json");
    let o = get(std::slice::from_ref(&key)).await.ok()?;
    text(&o, &key)
}

/// Remove everything kept for running without the companion app (once
/// the app has the tree).
pub async fn clear_all() {
    let all = match call("storage.local.get", &Array::of1(&JsValue::NULL)).await {
        Ok(o) => o,
        Err(e) => {
            web_sys::console::error_2(&"[treetabs] cannot list the storage".into(), &e);
            return;
        }
    };
    let keys: Vec<String> = Object::keys(all.unchecked_ref::<Object>())
        .iter()
        .filter_map(|k| k.as_string())
        .filter(|k| k.starts_with("tt."))
        .collect();
    queue_remove(keys);
    flush();
}

/// The hub's [`Backend`] in the browser.
pub struct BrowserBackend {
    files: HashMap<String, Vec<u8>>,
    /// Copies, newest first.
    exports: Vec<BackupInfo>,
}

impl BrowserBackend {
    fn save_index(&mut self) {
        self.exports.sort_by_key(|b| std::cmp::Reverse(b.at));
        match serde_json::to_string(&self.exports) {
            Ok(j) => queue_set(EXPORTS.into(), j),
            Err(e) => log::error!("cannot save the list of copies: {e}"),
        }
    }

    fn put_copy(&mut self, tree: &Tree, name: &str, kind: BackupKind, now: i64) -> io::Result<()> {
        let json = serde_json::to_string(tree).map_err(io::Error::other)?;
        self.exports.retain(|b| b.name != name);
        self.exports.push(BackupInfo {
            kind,
            name: name.to_owned(),
            at: now,
            bytes: json.len() as u64,
        });
        queue_set(copy_key(name), json);
        Ok(())
    }
}

impl Backend for BrowserBackend {
    type Store = MemStore;

    fn read(&self, name: &str) -> Option<Vec<u8>> {
        self.files.get(name).cloned()
    }

    fn write(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.files.insert(name.to_owned(), data.to_vec());
        queue_set(file_key(name), String::from_utf8_lossy(data).into_owned());
        Ok(())
    }

    /// Nothing to show: the UI says "in this browser" for this kind.
    fn location(&self) -> String {
        String::new()
    }

    fn kind(&self) -> StoreKind {
        StoreKind::Browser
    }

    /// Browser storage holds every copy in full: fewer than in a folder.
    fn default_daily_exports(&self) -> usize {
        7
    }

    fn backups(&self) -> Vec<BackupInfo> {
        self.exports.clone()
    }

    /// Read from storage, then sent to the page that asked.
    fn read_backup(&mut self, client: u64, name: &str) -> Option<Result<Tree, String>> {
        if !self.exports.iter().any(|b| b.name == name) {
            return Some(Err(format!("no backup named {name}")));
        }
        let name = name.to_owned();
        spawn_local(async move {
            let tree = match get(&[copy_key(&name)]).await {
                Ok(o) => match text(&o, &copy_key(&name)) {
                    Some(j) => treetabs_core::load_json_str(&j)
                        .map(|l| l.tree)
                        .map_err(|e| e.to_string()),
                    None => Err(format!("no backup named {name}")),
                },
                Err(e) => Err(format!("{e:?}")),
            };
            let msg = match tree {
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
            post_viewer(client, &msg);
        });
        None
    }

    fn export(
        &mut self,
        tree: &Tree,
        keep: usize,
        now: i64,
        when: ExportWhen,
    ) -> io::Result<Option<String>> {
        if keep == 0 && when == ExportWhen::Daily {
            return Ok(None);
        }
        let day = treetabs_core::backups::local_time(now)[..10].to_owned();
        let name = format!("treetabs-{day}.json");
        if when == ExportWhen::Daily && self.exports.iter().any(|b| b.name == name) {
            return Ok(None);
        }
        self.put_copy(tree, &name, BackupKind::Daily, now)?;
        // Keep the newest `keep` daily copies (names sort by date).
        let mut daily: Vec<String> = self
            .exports
            .iter()
            .filter(|b| b.kind == BackupKind::Daily)
            .map(|b| b.name.clone())
            .collect();
        daily.sort_unstable_by(|a, b| b.cmp(a));
        let old: Vec<String> = daily.into_iter().skip(keep.max(1)).collect();
        self.exports.retain(|b| !old.contains(&b.name));
        queue_remove(old.iter().map(|n| copy_key(n)).collect());
        self.save_index();
        Ok(Some(name))
    }

    fn save_backup(&mut self, tree: &Tree, name: &str) -> io::Result<String> {
        self.put_copy(
            tree,
            name,
            BackupKind::Snapshot,
            treetabs_core::ops::now_ms(),
        )?;
        self.save_index();
        Ok(name.to_owned())
    }
}
