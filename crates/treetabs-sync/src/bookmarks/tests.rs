//! The mirror against a fake Chrome bookmark store.

use super::*;
use std::collections::VecDeque;

/// Minimal `chrome.bookmarks` with Chrome's index rules and events.
struct FakeChrome {
    items: HashMap<String, Item>,
    next: u64,
    events: VecDeque<(String, Vec<Value>)>,
}

impl FakeChrome {
    fn new() -> Self {
        let mut items = HashMap::new();
        for (id, parent, title) in [
            ("0", "", ""),
            ("1", "0", "Bookmarks bar"),
            ("2", "0", "Other bookmarks"),
        ] {
            items.insert(
                id.into(),
                Item {
                    parent: parent.into(),
                    title: title.into(),
                    url: None,
                    children: vec![],
                },
            );
        }
        items.get_mut("0").unwrap().children = vec!["1".into(), "2".into()];
        FakeChrome {
            items,
            next: 100,
            events: VecDeque::new(),
        }
    }

    fn node_json(&self, id: &str) -> Value {
        let it = &self.items[id];
        let mut v = json!({"id": id, "parentId": it.parent, "title": it.title});
        if let Some(u) = &it.url {
            v["url"] = json!(u);
        } else {
            v["children"] = Value::Array(it.children.iter().map(|c| self.node_json(c)).collect());
        }
        if id == "2" {
            v["folderType"] = json!("other");
        }
        v
    }

    fn index_of(&self, id: &str) -> usize {
        let p = &self.items[id].parent;
        self.items[p].children.iter().position(|c| c == id).unwrap()
    }

    fn create(&mut self, spec: &Value) -> Value {
        self.next += 1;
        let id = self.next.to_string();
        let parent = spec["parentId"].as_str().unwrap().to_owned();
        let len = self.items[&parent].children.len();
        let index = spec
            .get("index")
            .and_then(Value::as_u64)
            .map_or(len, |i| usize::try_from(i).unwrap_or(len).min(len));
        self.items.insert(
            id.clone(),
            Item {
                parent: parent.clone(),
                title: spec["title"].as_str().unwrap_or("").into(),
                url: spec.get("url").and_then(Value::as_str).map(str::to_owned),
                children: vec![],
            },
        );
        self.items
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(index, id.clone());
        let node = self.node_json(&id);
        let mut ev = node.clone();
        ev["index"] = json!(index);
        self.events
            .push_back(("bookmarks.onCreated".into(), vec![json!(id), ev]));
        let mut r = node;
        r["index"] = json!(index);
        r
    }

    fn mv(&mut self, id: &str, dest: &Value) {
        let old_parent = self.items[id].parent.clone();
        let old_index = self.index_of(id);
        let parent = dest["parentId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or(old_parent.clone());
        let mut index = dest
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok());
        if old_parent == parent
            && let Some(i) = index
        {
            if i == old_index || i == old_index + 1 {
                return; // no-op in Chrome
            }
            if i > old_index {
                index = Some(i - 1);
            }
        }
        self.items
            .get_mut(&old_parent)
            .unwrap()
            .children
            .remove(old_index);
        let len = self.items[&parent].children.len();
        let i = index.unwrap_or(len).min(len);
        self.items
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(i, id.to_owned());
        self.items.get_mut(id).unwrap().parent = parent.clone();
        self.events.push_back((
            "bookmarks.onMoved".into(),
            vec![json!(id), json!({"parentId": parent, "index": i, "oldParentId": old_parent, "oldIndex": old_index})],
        ));
    }

    fn update(&mut self, id: &str, change: &Value) {
        let it = self.items.get_mut(id).unwrap();
        if let Some(t) = change.get("title").and_then(Value::as_str) {
            it.title = t.into();
        }
        if let Some(u) = change.get("url").and_then(Value::as_str) {
            it.url = Some(u.into());
        }
        let mut info = json!({"title": it.title});
        if let Some(u) = &it.url {
            info["url"] = json!(u);
        }
        self.events
            .push_back(("bookmarks.onChanged".into(), vec![json!(id), info]));
    }

    fn remove(&mut self, id: &str) {
        let parent = self.items[id].parent.clone();
        let index = self.index_of(id);
        self.items.get_mut(&parent).unwrap().children.remove(index);
        fn drop_all(items: &mut HashMap<String, Item>, id: &str) {
            if let Some(it) = items.remove(id) {
                for c in it.children {
                    drop_all(items, &c);
                }
            }
        }
        drop_all(&mut self.items, id);
        self.events.push_back((
            "bookmarks.onRemoved".into(),
            vec![json!(id), json!({"parentId": parent, "index": index})],
        ));
    }

    fn exec(&mut self, api: &str, args: &[Value]) -> Result<Value, String> {
        Ok(match api {
            "bookmarks.getTree" => json!([self.node_json("0")]),
            "bookmarks.create" => self.create(&args[0]),
            "bookmarks.move" => {
                self.mv(args[0].as_str().unwrap(), &args[1]);
                Value::Null
            }
            "bookmarks.update" => {
                self.update(args[0].as_str().unwrap(), &args[1]);
                Value::Null
            }
            "bookmarks.remove" | "bookmarks.removeTree" => {
                let id = args[0].as_str().unwrap();
                if !self.items.contains_key(id) {
                    return Err("Can't find bookmark for id.".into());
                }
                self.remove(id);
                Value::Null
            }
            other => return Err(format!("unsupported {other}")),
        })
    }

    /// Outline of our folder.
    fn outline(&self) -> String {
        let other = &self.items["2"];
        let Some(root) = other
            .children
            .iter()
            .find(|c| self.items[*c].title == "treetabs")
        else {
            return "<no folder>".into();
        };
        let mut out = String::new();
        fn go(f: &FakeChrome, id: &str, d: usize, out: &mut String) {
            for c in &f.items[id].children {
                let it = &f.items[c];
                match &it.url {
                    Some(u) => out.push_str(&format!("{}{} <{u}>\n", "  ".repeat(d), it.title)),
                    None => out.push_str(&format!("{}[{}]\n", "  ".repeat(d), it.title)),
                }
                go(f, c, d + 1, out);
            }
        }
        go(self, root, 0, &mut out);
        out
    }
}

/// Run calls until the mirror is idle. `echo_first`: deliver events before
/// the call result (Chrome does either).
fn settle(
    m: &mut Mirror,
    chrome: &mut FakeChrome,
    tree: &mut Tree,
    calls: Vec<BookmarkCall>,
    echo_first: bool,
) {
    let mut queue: VecDeque<BookmarkCall> = calls.into();
    for _ in 0..1_000_000 {
        // Events from edits made directly in "Chrome" (by the user).
        while let Some((name, args)) = chrome.events.pop_front() {
            let (ops, more) = m.browser_event(&name, &args, tree);
            for op in ops {
                tree.apply(&op).unwrap();
            }
            queue.extend(more);
        }
        if let Some(c) = queue.pop_front() {
            let r = chrome.exec(&c.api, &c.args);
            let deliver_events =
                |m: &mut Mirror,
                 chrome: &mut FakeChrome,
                 tree: &mut Tree,
                 queue: &mut VecDeque<BookmarkCall>| {
                    while let Some((name, args)) = chrome.events.pop_front() {
                        let (ops, more) = m.browser_event(&name, &args, tree);
                        for op in ops {
                            tree.apply(&op).unwrap();
                        }
                        queue.extend(more);
                    }
                };
            if echo_first {
                deliver_events(m, chrome, tree, &mut queue);
            }
            let (ok, err) = match r {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(e)),
            };
            let (ops, more) = m.call_result(c.pending, ok, err, tree);
            for op in ops {
                tree.apply(&op).unwrap();
            }
            queue.extend(more);
            if !echo_first {
                deliver_events(m, chrome, tree, &mut queue);
            }
            continue;
        }
        let more = m.tick(tree);
        if more.is_empty() {
            return;
        }
        queue.extend(more);
    }
    panic!("mirror did not settle");
}

fn saved_tab(title: &str, url: &str) -> Node {
    let mut n = Node::new(NodeKind::SavedTab);
    n.title = Some(title.into());
    n.url = Some(url.into());
    n
}

fn sample() -> Tree {
    let mut t = Tree::default();
    let mut w = Node::new(NodeKind::SavedWindow);
    w.custom_title = Some("Work".into());
    let mut a = saved_tab("A", "https://a/");
    a.children.push(saved_tab("A1", "https://a1/"));
    w.children = vec![a, saved_tab("B", "https://b/")];
    let mut note = Node::new(NodeKind::Note);
    note.note = Some("remember".into());
    t.root.children = vec![w, saved_tab("C", "https://c/"), note];
    t
}

fn mirror() -> Mirror {
    Mirror {
        enabled: true,
        min_diff_interval: Some(std::time::Duration::ZERO),
        ..Default::default()
    }
}

const SAMPLE_OUTLINE: &str = "[Work]\n  [A]\n    A <https://a/>\n    A1 <https://a1/>\n  B <https://b/>\nC <https://c/>\n[remember]\n";

#[test]
fn initial_sync_builds_folder() {
    for echo_first in [false, true] {
        let mut t = sample();
        let mut chrome = FakeChrome::new();
        let mut m = mirror();
        let calls = m.start(&t);
        settle(&mut m, &mut chrome, &mut t, calls, echo_first);
        assert_eq!(chrome.outline(), SAMPLE_OUTLINE, "echo_first={echo_first}");
        // Nothing to do on the next round, and the tree is untouched.
        assert!(m.tick(&t).is_empty());
        assert_eq!(t.count(), sample().count());
    }
}

#[test]
fn tree_edits_reach_bookmarks() {
    let mut t = sample();
    let mut chrome = FakeChrome::new();
    let mut m = mirror();
    let calls = m.start(&t);
    settle(&mut m, &mut chrome, &mut t, calls, false);

    // Move C into the window before B, rename A, trash A1 (A loses its folder).
    let w = t.root.children[0].id;
    let c = t.root.children[1].id;
    let a = t.root.children[0].children[0].id;
    let a1 = t.root.children[0].children[0].children[0].id;
    let b = t.root.children[0].children[1].id;
    for op in t
        .plan_move(&[c], w, treetabs_core::Position::Before(b))
        .unwrap()
    {
        t.apply(&op).unwrap();
    }
    let mut f = t.root.find(a).unwrap().fields();
    f.custom_title = Some("Alpha".into());
    t.apply(&Op::Update {
        id: a,
        fields: Box::new(f),
    })
    .unwrap();
    t.apply(&Op::Trash { id: a1, at: 1 }).unwrap();
    m.tree_changed(&t);
    settle(&mut m, &mut chrome, &mut t, vec![], false);
    assert_eq!(
        chrome.outline(),
        "[Work]\n  Alpha <https://a/>\n  C <https://c/>\n  B <https://b/>\n[remember]\n"
    );
    // The tree was not changed by the echoes.
    assert_eq!(t.root.children.len(), 2);
}

#[test]
fn bookmark_edits_reach_tree() {
    let mut t = sample();
    let mut chrome = FakeChrome::new();
    let mut m = mirror();
    let calls = m.start(&t);
    settle(&mut m, &mut chrome, &mut t, calls, false);
    let id_of = |chrome: &FakeChrome, title: &str| {
        chrome
            .items
            .iter()
            .find(|(_, i)| i.title == title)
            .unwrap()
            .0
            .clone()
    };

    // User renames B, moves C into the window, deletes the note, adds a bookmark.
    let b = id_of(&chrome, "B");
    chrome.update(&b, &json!({"title": "Bee"}));
    let c = id_of(&chrome, "C");
    let work = id_of(&chrome, "Work");
    chrome.mv(&c, &json!({"parentId": work, "index": 0}));
    let note = id_of(&chrome, "remember");
    chrome.remove(&note);
    let root = id_of(&chrome, "treetabs");
    chrome.create(&json!({"parentId": root, "index": 1, "title": "New", "url": "https://new/"}));
    settle(&mut m, &mut chrome, &mut t, vec![], false);

    let w = &t.root.children[0];
    let titles: Vec<String> = w.children.iter().map(|n| n.display_title()).collect();
    assert_eq!(titles, ["C", "A", "Bee"]);
    assert_eq!(
        w.children[2].title.as_deref(),
        Some("B"),
        "page title kept, custom title set"
    );
    assert_eq!(t.root.children[1].url.as_deref(), Some("https://new/"));
    assert_eq!(t.root.children.len(), 2);
    assert_eq!(t.trash.len(), 1);
    // And the folder still matches the tree.
    assert_eq!(
        chrome.outline(),
        "[Work]\n  C <https://c/>\n  [A]\n    A <https://a/>\n    A1 <https://a1/>\n  Bee <https://b/>\nNew <https://new/>\n"
    );
}

#[test]
fn lost_mapping_is_recovered_without_duplicates() {
    let mut t = sample();
    let mut chrome = FakeChrome::new();
    let mut m = mirror();
    let calls = m.start(&t);
    settle(&mut m, &mut chrome, &mut t, calls, false);
    // New mirror without the mapping file.
    let mut m = mirror();
    let calls = m.start(&t);
    settle(&mut m, &mut chrome, &mut t, calls, false);
    assert_eq!(chrome.outline(), SAMPLE_OUTLINE);
    assert_eq!(t.count(), sample().count());
}

/// `cargo test -p treetabs-sync --release -- --ignored big_tree --nocapture`
#[test]
#[ignore = "slow: syncs 53,000 bookmarks; run with --release"]
fn big_tree_initial_sync() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tmp/data_example_json/1.json"
    );
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut t = treetabs_core::load_json_str(&text).unwrap().tree;
    let mut chrome = FakeChrome::new();
    let mut m = mirror();
    let start = Instant::now();
    let calls = m.start(&t);
    settle(&mut m, &mut chrome, &mut t, calls, false);
    let n = chrome.items.len();
    println!("synced {n} bookmarks in {:?}", start.elapsed());
    let start = Instant::now();
    m.dirty = true;
    assert!(m.tick(&t).is_empty());
    println!("no-op diff in {:?}", start.elapsed());
    assert!(n > 45_000);
}

#[test]
fn a_folder_of_another_tree_is_left_alone() {
    // The extension mirrored its own tree here, then handed it to an app
    // whose store already had a tree (so it became a backup).
    let mut kept = sample();
    let mut chrome = FakeChrome::new();
    let mut m = mirror();
    let calls = m.start(&kept);
    settle(&mut m, &mut chrome, &mut kept, calls, false);

    let mut t = Tree::default();
    t.root.children.push(saved_tab("Z", "https://z/"));
    let mut m = mirror();
    m.leave_folder();
    let calls = m.start(&t);
    settle(&mut m, &mut chrome, &mut t, calls, false);
    assert_eq!(chrome.outline(), "Z <https://z/>\n");
    assert_eq!(t.count(), 2, "nothing imported from the old folder");
    let left = chrome.items["2"]
        .children
        .iter()
        .filter(|c| chrome.items[*c].title == LEFT_TITLE)
        .count();
    assert_eq!(left, 1);
}
