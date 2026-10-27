//! The hub on its own, with an in-memory store and backend: what the
//! extension runs when there is no companion app.

use std::collections::HashMap;
use treetabs_core::backups::{BackupInfo, BackupKind};
use treetabs_core::memstore::MemStore;
use treetabs_core::protocol::StoreKind;
use treetabs_core::{Node, NodeKind, Tree};
use treetabs_sync::hub::{Backend, ExportWhen, Hub};
use treetabs_sync::{ClientMsg, Role, ServerMsg};

#[derive(Default)]
struct MemBackend {
    files: HashMap<String, Vec<u8>>,
    copies: HashMap<String, Tree>,
}

impl Backend for MemBackend {
    type Store = MemStore;
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        self.files.get(name).cloned()
    }
    fn write(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.files.insert(name.into(), data.to_vec());
        Ok(())
    }
    fn location(&self) -> String {
        String::new()
    }
    fn kind(&self) -> StoreKind {
        StoreKind::Browser
    }
    fn backups(&self) -> Vec<BackupInfo> {
        self.copies
            .keys()
            .map(|name| BackupInfo {
                kind: BackupKind::Snapshot,
                name: name.clone(),
                at: 0,
                bytes: 0,
            })
            .collect()
    }
    fn read_backup(&mut self, _client: u64, name: &str) -> Option<Result<Tree, String>> {
        Some(
            self.copies
                .get(name)
                .cloned()
                .ok_or_else(|| "missing".into()),
        )
    }
    fn export(
        &mut self,
        _: &Tree,
        _: usize,
        _: i64,
        _: ExportWhen,
    ) -> std::io::Result<Option<String>> {
        Ok(None)
    }
    fn save_backup(&mut self, tree: &Tree, name: &str) -> std::io::Result<String> {
        self.copies.insert(name.into(), tree.clone());
        Ok(name.into())
    }
}

const BROWSER: u64 = 1;
const VIEWER: u64 = 2;

fn hub() -> Hub<MemBackend> {
    let mut h = Hub::new(MemStore::open(None).unwrap(), MemBackend::default());
    for (id, role) in [(BROWSER, Role::Browser), (VIEWER, Role::Viewer)] {
        h.connected(id);
        h.message(
            id,
            ClientMsg::Hello {
                role,
                name: "test".into(),
            },
        );
    }
    h.take_out();
    h
}

fn kept_tree() -> Tree {
    let mut t = Tree::default();
    let mut w = Node::new(NodeKind::SavedWindow);
    let mut tab = Node::new(NodeKind::SavedTab);
    tab.url = Some("https://kept/".into());
    w.children.push(tab);
    t.root.children.push(w);
    t
}

fn adopt(h: &mut Hub<MemBackend>) -> Vec<(u64, ServerMsg)> {
    h.message(
        BROWSER,
        ClientMsg::AdoptTree {
            tree: Box::new(kept_tree()),
            bookmarks: None,
            since: None,
        },
    );
    h.take_out()
}

#[test]
fn a_new_store_takes_the_browsers_tree() {
    let mut h = hub();
    let out = adopt(&mut h);
    assert!(
        out.iter()
            .any(|(id, m)| *id == BROWSER && matches!(m, ServerMsg::Adopted { backup: None }))
    );
    assert!(
        out.iter()
            .any(|(id, m)| *id == VIEWER && matches!(m, ServerMsg::Snapshot { .. })),
        "viewers get the new tree"
    );
    assert_eq!(h.tree().root.count(), 3);
    assert!(h.backend.copies.is_empty());
}

#[test]
fn a_used_store_keeps_the_browsers_tree_as_a_backup() {
    let mut h = hub();
    let root = h.tree().root.id;
    h.message(
        VIEWER,
        ClientMsg::Edit {
            req: 1,
            ops: vec![treetabs_core::Op::Insert {
                parent: root,
                index: 0,
                node: Box::new(Node::new(NodeKind::Note)),
            }],
        },
    );
    h.take_out();
    let out = adopt(&mut h);
    let backup = out.iter().find_map(|(id, m)| match m {
        ServerMsg::Adopted { backup } if *id == BROWSER => backup.clone(),
        _ => None,
    });
    let name = backup.expect("saved as a backup");
    assert_eq!(
        h.tree().root.children.len(),
        1,
        "the store's tree is unchanged"
    );
    assert_eq!(h.backend.copies[&name].root.count(), 3);
}

#[test]
fn moving_a_browser_store_is_refused() {
    let mut h = hub();
    h.message(
        VIEWER,
        ClientMsg::MoveStore {
            dir: "/tmp/x".into(),
        },
    );
    let out = h.take_out();
    assert!(!h.stopping());
    assert!(out.iter().any(|(_, m)| matches!(m, ServerMsg::Status { message, .. } if message.contains("cannot be moved"))));
}

/// The browser takes the tree along, then brings it back.
fn detach(h: &mut Hub<MemBackend>) -> (u64, Tree) {
    h.message(VIEWER, ClientMsg::UseCompanionApp { on: false });
    h.take_out()
        .into_iter()
        .find_map(|(id, m)| match m {
            ServerMsg::Detached { seq, tree, .. } if id == VIEWER => Some((seq, *tree)),
            _ => None,
        })
        .expect("the tree is sent along")
}

fn note_edit(h: &mut Hub<MemBackend>, req: u64) {
    let root = h.tree().root.id;
    h.message(
        VIEWER,
        ClientMsg::Edit {
            req,
            ops: vec![treetabs_core::Op::Insert {
                parent: root,
                index: 0,
                node: Box::new(Node::new(NodeKind::Note)),
            }],
        },
    );
    h.take_out();
}

fn bring_back(h: &mut Hub<MemBackend>, tree: Tree, since: u64) -> Option<String> {
    h.message(
        BROWSER,
        ClientMsg::AdoptTree {
            tree: Box::new(tree),
            bookmarks: None,
            since: Some(since),
        },
    );
    h.take_out()
        .into_iter()
        .find_map(|(id, m)| match m {
            ServerMsg::Adopted { backup } if id == BROWSER => Some(backup),
            _ => None,
        })
        .expect("answered")
}

#[test]
fn a_tree_that_left_comes_back_into_an_unchanged_store() {
    let mut h = hub();
    note_edit(&mut h, 1);
    let (seq, mut tree) = detach(&mut h);
    assert_eq!(seq, 1);
    // Changed while in the browser.
    tree.root.children.extend(kept_tree().root.children);
    assert_eq!(
        bring_back(&mut h, tree, seq),
        None,
        "replaces the store's tree"
    );
    assert_eq!(h.tree().root.children.len(), 2);
}

#[test]
fn a_tree_that_left_a_store_used_since_becomes_a_backup() {
    let mut h = hub();
    note_edit(&mut h, 1);
    let (seq, tree) = detach(&mut h);
    note_edit(&mut h, 2);
    let backup = bring_back(&mut h, tree, seq);
    assert!(backup.is_some(), "the store moved on: kept as a backup");
    assert_eq!(
        h.tree().root.children.len(),
        2,
        "the store's tree is unchanged"
    );
}

// --- Opening a saved tab from the tree, with Firefox's order of events:
// the new tab is reported as about:blank first, its address comes later.

fn ev(h: &mut Hub<MemBackend>, name: &str, args: serde_json::Value) {
    let args = args.as_array().cloned().unwrap();
    h.message(
        BROWSER,
        ClientMsg::BrowserEvent {
            name: name.into(),
            args,
        },
    );
}

/// A hub with one open window (id 10, one tab) holding a saved tab;
/// returns the saved tab's node id.
fn window_with_saved_tab(h: &mut Hub<MemBackend>) -> treetabs_core::NodeId {
    h.message(
        BROWSER,
        ClientMsg::BrowserSnapshot {
            windows: vec![serde_json::json!({"id": 10, "tabs": [{"id": 100, "windowId": 10, "index": 0, "url": "https://open/"}]})],
            browser: Some("b".into()),
            label: None,
        },
    );
    let window = h.tree().root.children[0].id;
    let mut saved = Node::new(NodeKind::SavedTab);
    saved.url = Some("https://saved/".into());
    let id = saved.id;
    h.message(
        VIEWER,
        ClientMsg::Edit {
            req: 1,
            ops: vec![treetabs_core::Op::Insert {
                parent: window,
                index: 1,
                node: Box::new(saved),
            }],
        },
    );
    h.take_out();
    id
}

/// The `chrome.tabs.create` call the hub sent, by request number.
fn create_call(out: &[(u64, ServerMsg)]) -> u64 {
    out.iter()
        .find_map(|(_, m)| match m {
            ServerMsg::Call { req, api, .. } if api == "tabs.create" => Some(*req),
            _ => None,
        })
        .expect("tabs.create sent")
}

fn nodes_with(h: &Hub<MemBackend>, url: &str) -> Vec<Node> {
    let mut out = Vec::new();
    h.tree().root.walk(&mut |n, _| {
        if n.url.as_deref() == Some(url) {
            out.push(n.clone());
        }
    });
    out
}

fn open_saved_tab_like_firefox(result_first: bool) {
    let mut h = hub();
    let saved = window_with_saved_tab(&mut h);
    h.message(VIEWER, ClientMsg::Activate { id: saved });
    let req = create_call(&h.take_out());
    let blank = serde_json::json!({"id": 101, "windowId": 10, "index": 1, "url": "about:blank"});
    let result = ClientMsg::CallResult {
        req,
        ok: Some(blank.clone()),
        err: None,
    };
    if result_first {
        h.message(BROWSER, result.clone());
    }
    ev(&mut h, "tabs.onCreated", serde_json::json!([blank]));
    if !result_first {
        h.message(BROWSER, result);
    }
    ev(
        &mut h,
        "tabs.onUpdated",
        serde_json::json!([101, {"url": "https://saved/"}, {"id": 101, "windowId": 10, "index": 1, "url": "https://saved/", "title": "Saved"}]),
    );
    h.take_out();
    let found = nodes_with(&h, "https://saved/");
    assert_eq!(found.len(), 1, "one node for the tab, not a second one");
    assert_eq!(found[0].id, saved, "the saved node itself is now the tab");
    assert_eq!(found[0].kind, NodeKind::Tab);
    assert!(nodes_with(&h, "about:blank").is_empty());
}

#[test]
fn opening_a_saved_tab_binds_it_when_the_tab_is_reported_first() {
    open_saved_tab_like_firefox(false);
}

#[test]
fn opening_a_saved_tab_binds_it_when_the_result_comes_first() {
    open_saved_tab_like_firefox(true);
}
