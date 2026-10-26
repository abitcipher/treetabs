use serde_json::json;
use std::time::Duration;
use treetabs_core::{NodeKind, Tree};
use treetabs_sync::{ClientMsg, Role, ServerMsg, connect};

fn tmpdir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("treetabs-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn recv(rx: &std::sync::mpsc::Receiver<ServerMsg>) -> ServerMsg {
    rx.recv_timeout(Duration::from_secs(5)).expect("server message")
}

/// Viewer replica: apply everything the server sends until `until` holds.
fn follow(rx: &std::sync::mpsc::Receiver<ServerMsg>, tree: &mut Tree, until: impl Fn(&Tree) -> bool) {
    while !until(tree) {
        match recv(rx) {
            ServerMsg::Snapshot { tree: t, .. } => *tree = *t,
            ServerMsg::Applied { ops, .. } => {
                for op in &ops {
                    tree.apply(op).expect("replica apply");
                }
            }
            _ => {}
        }
    }
}

#[test]
fn browser_events_reach_viewer_and_survive_restart() {
    let dir = tmpdir("flow");
    {
        let browser = connect(&dir, Role::Browser, "test-browser", || {}).unwrap();
        assert!(browser.is_server());
        assert!(matches!(recv(&browser.rx), ServerMsg::Resync));
        browser
            .send(&ClientMsg::BrowserSnapshot {
                browser: Some("test-profile".into()),
                label: Some("Test".into()),
                windows: vec![json!({"id": 1, "type": "normal", "tabs": [
                    {"id": 10, "windowId": 1, "index": 0, "url": "https://a", "title": "A"}
                ]})],
            })
            .unwrap();

        let viewer = connect(&dir, Role::Viewer, "test-viewer", || {}).unwrap();
        assert!(!viewer.is_server());
        let mut replica = Tree::default();
        follow(&viewer.rx, &mut replica, |t| t.count() == 3);

        browser
            .send(&ClientMsg::BrowserEvent {
                name: "tabs.onCreated".into(),
                args: vec![json!({"id": 11, "windowId": 1, "index": 1, "url": "https://b", "openerTabId": 10})],
            })
            .unwrap();
        follow(&viewer.rx, &mut replica, |t| t.count() == 4);
        let win = &replica.root.children[0];
        assert_eq!(win.kind, NodeKind::Window);
        assert_eq!(win.children[0].children[0].url.as_deref(), Some("https://b"));

        // Viewer edit: rename the window, get the inverse back.
        let mut f = win.fields();
        f.custom_title = Some("Work".into());
        viewer
            .send(&ClientMsg::Edit { req: 7, ops: vec![treetabs_core::Op::Update { id: win.id, fields: Box::new(f) }] })
            .unwrap();
        loop {
            match recv(&viewer.rx) {
                ServerMsg::EditResult { req, inverse, err } => {
                    assert_eq!(req, 7);
                    assert!(err.is_none());
                    assert_eq!(inverse.len(), 1);
                    break;
                }
                ServerMsg::Applied { ops, .. } => ops.iter().for_each(|op| {
                    replica.apply(op).unwrap();
                }),
                _ => {}
            }
        }
        // The browser closes the window.
        for (name, args) in [
            ("tabs.onRemoved", vec![json!(11), json!({"windowId": 1, "isWindowClosing": true})]),
            ("tabs.onRemoved", vec![json!(10), json!({"windowId": 1, "isWindowClosing": true})]),
            ("windows.onRemoved", vec![json!(1)]),
        ] {
            browser.send(&ClientMsg::BrowserEvent { name: name.into(), args }).unwrap();
        }
        follow(&viewer.rx, &mut replica, |t| t.root.children[0].kind == NodeKind::SavedWindow);
        drop(viewer);
        drop(browser); // stops the in-process server, writes the snapshot
    }
    // Everything is on disk.
    let store = treetabs_core::Store::open(&dir).unwrap();
    let w = &store.tree().root.children[0];
    assert_eq!(w.kind, NodeKind::SavedWindow);
    assert_eq!(w.custom_title.as_deref(), Some("Work"));
    assert_eq!(w.children[0].kind, NodeKind::SavedTab);
    assert_eq!(w.children[0].children[0].kind, NodeKind::SavedTab);
    drop(store);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn client_takes_over_when_server_process_leaves() {
    let dir = tmpdir("takeover");
    let first = connect(&dir, Role::Viewer, "first", || {}).unwrap();
    assert!(first.is_server());
    let second = connect(&dir, Role::Viewer, "second", || {}).unwrap();
    assert!(!second.is_server());
    drop(first); // server stops
    // The second client sees the connection end...
    while second.rx.recv_timeout(Duration::from_secs(5)).is_ok() {}
    // ...and a new connect becomes the server.
    let again = connect(&dir, Role::Viewer, "again", || {}).unwrap();
    assert!(again.is_server());
    drop(again);
    drop(second);
    std::fs::remove_dir_all(&dir).unwrap();
}
