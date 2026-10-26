use super::*;

fn tab(id: i64, win: i64, index: usize, url: &str) -> BrowserTab {
    BrowserTab {
        id,
        window_id: win,
        index,
        url: Some(url.into()),
        pending_url: None,
        title: Some(url.trim_start_matches("https://").into()),
        fav_icon_url: None,
        pinned: false,
        opener_tab_id: None,
        group_id: None,
        incognito: false,
        discarded: false,
    }
}

fn opened_by(mut t: BrowserTab, opener: i64) -> BrowserTab {
    t.opener_tab_id = Some(opener);
    t
}

fn window(id: i64, tabs: Vec<BrowserTab>) -> BrowserWindow {
    BrowserWindow {
        id,
        kind: Some("normal".into()),
        state: Some("normal".into()),
        left: Some(0),
        top: Some(0),
        width: Some(800),
        height: Some(600),
        incognito: false,
        tabs,
    }
}

/// Compact text picture of the tree: one line per node, indented.
fn shape(t: &Tree) -> String {
    let mut out = String::new();
    t.root.walk(&mut |n, d| {
        if d == 0 {
            return;
        }
        let label = match n.kind {
            NodeKind::Window | NodeKind::SavedWindow => String::new(),
            _ => n.url.clone().unwrap_or_default().trim_start_matches("https://").to_owned(),
        };
        out.push_str(&format!("{}{} {}\n", "  ".repeat(d - 1), n.kind, label).replace(" \n", "\n"));
    });
    out
}

fn ev(e: &mut Engine, t: &mut Tree, event: BrowserEvent) {
    e.handle(t, &event).unwrap();
}

#[test]
fn reconcile_builds_tree_with_opener_nesting() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    let live = vec![window(
        1,
        vec![tab(10, 1, 0, "https://a"), opened_by(tab(11, 1, 1, "https://a1"), 10), tab(12, 1, 2, "https://b")],
    )];
    e.reconcile(&mut t, &live).unwrap();
    assert_eq!(shape(&t), "window\n  tab a\n    tab a1\n  tab b\n");
    // Second reconcile with the same state changes nothing.
    assert_eq!(e.reconcile(&mut t, &live).unwrap(), 0);
}

#[test]
fn restart_matches_by_url_not_id() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(
        &mut t,
        &[
            window(1, vec![tab(10, 1, 0, "https://a"), tab(11, 1, 1, "https://b")]),
            window(2, vec![tab(20, 2, 0, "https://x")]),
        ],
    )
    .unwrap();
    // Give the first window a custom title: it must survive the restart.
    let w1 = t.root.children[0].id;
    let mut f = t.root.children[0].fields();
    f.custom_title = Some("Work".into());
    t.apply(&Op::Update { id: w1, fields: Box::new(f) }).unwrap();

    // Browser restarted: new ids (window 1 now has the old id 2!), window
    // "x" gone, and a tab "c" opened in between.
    e.reconcile(
        &mut t,
        &[window(2, vec![tab(5, 2, 0, "https://a"), tab(6, 2, 1, "https://b"), tab(7, 2, 2, "https://c")])],
    )
    .unwrap();
    assert_eq!(
        shape(&t),
        "window\n  tab a\n  tab b\n  tab c\nsaved_window\n  saved_tab x\n"
    );
    let w = &t.root.children[0];
    assert_eq!(w.id, w1);
    assert_eq!(w.custom_title.as_deref(), Some("Work"));
    assert_eq!(w.data["id"], json!(2));
    assert_eq!(w.children[0].data["id"], json!(5));
    assert!(t.root.children[1].data.contains_key("crashDetectedDate"));
}

#[test]
fn close_tab_trashes_and_promotes_children() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a")])]).unwrap();
    ev(&mut e, &mut t, BrowserEvent::TabCreated(opened_by(tab(11, 1, 1, "https://a1"), 10)));
    ev(&mut e, &mut t, BrowserEvent::TabCreated(opened_by(tab(12, 1, 2, "https://a2"), 10)));
    assert_eq!(shape(&t), "window\n  tab a\n    tab a1\n    tab a2\n");
    ev(&mut e, &mut t, BrowserEvent::TabRemoved { tab_id: 10, window_id: 1, window_closing: false });
    assert_eq!(shape(&t), "window\n  tab a1\n  tab a2\n");
    assert_eq!(t.trash.len(), 1);
    assert_eq!(t.trash[0].node.url.as_deref(), Some("https://a"));
}

#[test]
fn window_close_saves_everything() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a"), tab(11, 1, 1, "https://b")])]).unwrap();
    ev(&mut e, &mut t, BrowserEvent::TabRemoved { tab_id: 10, window_id: 1, window_closing: true });
    ev(&mut e, &mut t, BrowserEvent::TabRemoved { tab_id: 11, window_id: 1, window_closing: true });
    ev(&mut e, &mut t, BrowserEvent::WindowRemoved(1));
    assert_eq!(shape(&t), "saved_window\n  saved_tab a\n  saved_tab b\n");
    assert!(t.trash.is_empty());
    assert!(t.root.children[0].data.contains_key("closeDate"));
}

#[test]
fn window_close_reported_as_tab_closes_is_recovered() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a"), tab(11, 1, 1, "https://b")])]).unwrap();
    // No isWindowClosing (seen on Linux), then the window goes away.
    ev(&mut e, &mut t, BrowserEvent::TabRemoved { tab_id: 10, window_id: 1, window_closing: false });
    ev(&mut e, &mut t, BrowserEvent::TabRemoved { tab_id: 11, window_id: 1, window_closing: false });
    ev(&mut e, &mut t, BrowserEvent::WindowRemoved(1));
    assert_eq!(shape(&t), "saved_window\n  saved_tab a\n  saved_tab b\n");
    assert!(t.trash.is_empty());
}

#[test]
fn moves_within_and_between_windows() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(
        &mut t,
        &[
            window(1, vec![tab(10, 1, 0, "https://a"), tab(11, 1, 1, "https://b"), tab(12, 1, 2, "https://c")]),
            window(2, vec![tab(20, 2, 0, "https://x")]),
        ],
    )
    .unwrap();
    ev(&mut e, &mut t, BrowserEvent::TabMoved { tab_id: 12, window_id: 1, to: 0 });
    assert_eq!(shape(&t), "window\n  tab c\n  tab a\n  tab b\nwindow\n  tab x\n");
    ev(&mut e, &mut t, BrowserEvent::TabAttached { tab_id: 10, window_id: 2, position: 1 });
    assert_eq!(shape(&t), "window\n  tab c\n  tab b\nwindow\n  tab x\n  tab a\n");
    // A no-op move changes nothing.
    assert_eq!(e.handle(&mut t, &BrowserEvent::TabMoved { tab_id: 12, window_id: 1, to: 0 }).unwrap(), 0);
}

#[test]
fn updates_are_skipped_when_nothing_changed() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a")])]).unwrap();
    assert_eq!(e.handle(&mut t, &BrowserEvent::TabUpdated(tab(10, 1, 0, "https://a"))).unwrap(), 0);
    assert_eq!(e.handle(&mut t, &BrowserEvent::TabUpdated(tab(10, 1, 0, "https://a2"))).unwrap(), 1);
    assert_eq!(shape(&t), "window\n  tab a2\n");
}

#[test]
fn decodes_chrome_events() {
    let e = BrowserEvent::from_chrome("tabs.onRemoved", &[json!(5), json!({"windowId": 2, "isWindowClosing": true})])
        .unwrap()
        .unwrap();
    assert_eq!(e, BrowserEvent::TabRemoved { tab_id: 5, window_id: 2, window_closing: true });
    let e = BrowserEvent::from_chrome(
        "tabs.onUpdated",
        &[json!(5), json!({"title": "T"}), json!({"id": 5, "windowId": 2, "index": 0, "url": "https://a", "title": "T"})],
    )
    .unwrap()
    .unwrap();
    assert!(matches!(e, BrowserEvent::TabUpdated(t) if t.title.as_deref() == Some("T")));
    assert!(BrowserEvent::from_chrome("tabs.onZoomChange", &[]).unwrap().is_none());
}

#[test]
fn link_tab_created_without_url_is_nested() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a")])]).unwrap();
    // What Chrome really sends for a target=_blank link: empty url.
    let mut child = opened_by(tab(11, 1, 1, ""), 10);
    child.url = Some(String::new());
    ev(&mut e, &mut t, BrowserEvent::TabCreated(child));
    ev(&mut e, &mut t, BrowserEvent::TabUpdated(opened_by(tab(11, 1, 1, "https://a-child"), 10)));
    assert_eq!(shape(&t), "window\n  tab a\n    tab a-child\n");
    // A new-tab page opened from a tab stays a sibling.
    ev(&mut e, &mut t, BrowserEvent::TabCreated(opened_by(tab(12, 1, 2, "chrome://newtab/"), 10)));
    assert_eq!(shape(&t), "window\n  tab a\n    tab a-child\n  tab chrome://newtab/\n");
}

#[test]
fn tab_closed_right_before_a_reported_window_close_stays_trashed() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a"), tab(11, 1, 1, "https://b")])]).unwrap();
    ev(&mut e, &mut t, BrowserEvent::TabRemoved { tab_id: 11, window_id: 1, window_closing: false });
    ev(&mut e, &mut t, BrowserEvent::TabRemoved { tab_id: 10, window_id: 1, window_closing: true });
    ev(&mut e, &mut t, BrowserEvent::WindowRemoved(1));
    assert_eq!(shape(&t), "saved_window\n  saved_tab a\n");
    assert_eq!(t.trash.len(), 1);
}

#[test]
fn second_link_from_same_opener_is_a_sibling() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://docs")])]).unwrap();
    let mut c1 = opened_by(tab(11, 1, 1, ""), 10);
    c1.url = Some(String::new());
    ev(&mut e, &mut t, BrowserEvent::TabCreated(c1));
    ev(&mut e, &mut t, BrowserEvent::TabUpdated(opened_by(tab(11, 1, 1, "https://c1"), 10)));
    let mut c2 = opened_by(tab(12, 1, 2, ""), 10);
    c2.url = Some(String::new());
    ev(&mut e, &mut t, BrowserEvent::TabCreated(c2));
    ev(&mut e, &mut t, BrowserEvent::TabUpdated(opened_by(tab(12, 1, 2, "https://c2"), 10)));
    assert_eq!(shape(&t), "window\n  tab docs\n    tab c1\n    tab c2\n");
}

#[test]
fn own_tree_window_is_not_tracked() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    let own = format!("{OWN_PAGES}tree.html");
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a")]), window(2, vec![tab(20, 2, 0, &own)])]).unwrap();
    assert_eq!(shape(&t), "window\n  tab a\n");
    ev(&mut e, &mut t, BrowserEvent::WindowCreated(window(3, vec![])));
    ev(&mut e, &mut t, BrowserEvent::TabCreated(tab(30, 3, 0, &own)));
    assert_eq!(shape(&t), "window\n  tab a\n");
    ev(&mut e, &mut t, BrowserEvent::TabRemoved { tab_id: 30, window_id: 3, window_closing: true });
    ev(&mut e, &mut t, BrowserEvent::WindowRemoved(3));
    assert_eq!(shape(&t), "window\n  tab a\n");
    // A normal new window appears with its first tab.
    ev(&mut e, &mut t, BrowserEvent::WindowCreated(window(4, vec![])));
    ev(&mut e, &mut t, BrowserEvent::TabCreated(tab(40, 4, 0, "https://b")));
    assert_eq!(shape(&t), "window\n  tab a\nwindow\n  tab b\n");
}

#[test]
fn own_pages_whatever_the_id() {
    assert!(is_own_page(&format!("{OWN_PAGES}tree.html")));
    assert!(is_own_page("chrome-extension://abcdefghijklmnopabcdefghijklmnop/tree.html"));
    assert!(is_own_page("moz-extension://0e7f3a52-4d1b/tree.html"));
    assert!(!is_own_page("https://example.com/tree.html"));
    assert!(!is_own_page("chrome-extension://abcdefghijklmnopabcdefghijklmnop/options.html"));
}

#[test]
fn firefox_tree_window_goes_once_its_url_is_known() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a")])]).unwrap();
    // Firefox: the popup's tab is about:blank first, then the add-on page.
    ev(&mut e, &mut t, BrowserEvent::WindowCreated(window(2, vec![])));
    ev(&mut e, &mut t, BrowserEvent::TabCreated(tab(20, 2, 0, "about:blank")));
    assert_eq!(shape(&t), "window\n  tab a\nwindow\n  tab about:blank\n");
    ev(&mut e, &mut t, BrowserEvent::TabUpdated(tab(20, 2, 0, "moz-extension://0e7f3a52-4d1b/tree.html")));
    assert_eq!(shape(&t), "window\n  tab a\n");
    // ...also as a tab in a normal window.
    ev(&mut e, &mut t, BrowserEvent::TabCreated(tab(11, 1, 1, "about:blank")));
    ev(&mut e, &mut t, BrowserEvent::TabUpdated(tab(11, 1, 1, "moz-extension://0e7f3a52-4d1b/tree.html")));
    assert_eq!(shape(&t), "window\n  tab a\n");
}

#[test]
fn two_browser_profiles_do_not_touch_each_others_windows() {
    let mut t = Tree::default();
    let mut e = Engine::new();
    e.browser = Some("A".into());
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a")])]).unwrap();
    e.browser = Some("B".into());
    // Same window and tab ids in the other profile.
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://b")])]).unwrap();
    assert_eq!(shape(&t), "window\n  tab a\nwindow\n  tab b\n");
    ev(&mut e, &mut t, BrowserEvent::TabUpdated(tab(10, 1, 0, "https://b2")));
    assert_eq!(shape(&t), "window\n  tab a\nwindow\n  tab b2\n");
    e.browser = Some("A".into());
    e.reconcile(&mut t, &[window(1, vec![tab(10, 1, 0, "https://a")])]).unwrap();
    assert_eq!(shape(&t), "window\n  tab a\nwindow\n  tab b2\n");
}
