//! Tabs Outliner session format.
//!
//! The same layout is used by the JSON export and by the
//! `currentSessionSnapshot` record in the extension's IndexedDB:
//!
//! ```text
//! [
//!   {"type": 2000, "node": {"type": "session", "data": {...}}},   // header
//!   [2001, {node}, [0]],          // node + its index path from the root
//!   [2001, {node}, [0, 0]],
//!   ...
//!   {"type": 11111, "time": 1627533622824}                       // footer
//! ]
//! ```
//!
//! Entries come in pre-order. A node without `type` is a saved tab.
//!
//! The importer never drops a node. If a path doesn't fit, the node is still
//! attached (to the deepest parent that exists) and a warning is recorded.

use crate::model::{Node, NodeKind, Tree};
use serde_json::{Map, Value, json};

const HEADER: i64 = 2000;
const NODE: i64 = 2001;
const FOOTER: i64 = 11111;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("expected a JSON array of entries")]
    NotAnArray,
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Problems that did not stop the import. Each one names the entry index.
#[derive(Debug, Clone, Default)]
pub struct Warnings(pub Vec<String>);

impl Warnings {
    fn push(&mut self, s: impl Into<String>) {
        self.0.push(s.into());
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Parse Tabs Outliner JSON text.
pub fn parse_str(s: &str) -> Result<(Tree, Warnings), ImportError> {
    let v: Value = serde_json::from_str(s)?;
    from_value(&v)
}

/// Build a tree from an already parsed entry array.
pub fn from_value(v: &Value) -> Result<(Tree, Warnings), ImportError> {
    let entries = v.as_array().ok_or(ImportError::NotAnArray)?;
    let mut warn = Warnings::default();
    let mut tree = Tree::default();

    // stack[d] is the open node at depth d; stack[0] is the session root.
    let mut stack: Vec<Node> = vec![std::mem::replace(&mut tree.root, Node::new(NodeKind::Session))];

    fn close_to(stack: &mut Vec<Node>, depth: usize) {
        while stack.len() > depth {
            let n = stack.pop().expect("stack not empty");
            stack.last_mut().expect("root stays").children.push(n);
        }
    }

    for (i, e) in entries.iter().enumerate() {
        match e {
            Value::Object(o) => match o.get("type").and_then(Value::as_i64) {
                Some(HEADER) => {
                    if let Some(node) = o.get("node").and_then(Value::as_object) {
                        let mut data = node
                            .get("data")
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default();
                        tree.tree_id = data.remove("treeId").and_then(|v| match v {
                            Value::String(s) => Some(s),
                            Value::Null => None,
                            other => Some(other.to_string()),
                        });
                        tree.session = data;
                    }
                }
                Some(FOOTER) => tree.saved_at = o.get("time").and_then(as_i64_lossy),
                other => warn.push(format!("entry {i}: unknown object entry type {other:?}")),
            },
            Value::Array(a) => {
                if a.first().and_then(Value::as_i64) != Some(NODE) {
                    warn.push(format!("entry {i}: unknown array entry, kept as note"));
                    let mut n = Node::new(NodeKind::Other("unknown".into()));
                    n.extra.insert("raw".into(), e.clone());
                    close_to(&mut stack, 1);
                    stack.push(n);
                    continue;
                }
                let raw = a.get(1).and_then(Value::as_object);
                let node = match raw {
                    Some(raw) => node_from_raw(raw),
                    None => {
                        warn.push(format!("entry {i}: node is not an object"));
                        Node::new(NodeKind::Other("invalid".into()))
                    }
                };
                let path: Vec<usize> = a
                    .get(2)
                    .and_then(Value::as_array)
                    .map(|p| p.iter().filter_map(|x| x.as_u64().map(|x| x as usize)).collect())
                    .unwrap_or_default();
                if path.is_empty() {
                    warn.push(format!("entry {i}: missing path, attached to root"));
                }
                let mut depth = path.len().max(1);
                if depth > stack.len() {
                    warn.push(format!(
                        "entry {i}: path {path:?} skips a level, attached to the deepest parent"
                    ));
                    depth = stack.len();
                }
                close_to(&mut stack, depth);
                let parent = stack.last().expect("root stays");
                if let Some(&idx) = path.last()
                    && idx != parent.children.len()
                {
                    warn.push(format!(
                        "entry {i}: path {path:?} expected child index {}, got {idx}; kept in file order",
                        parent.children.len()
                    ));
                }
                stack.push(node);
            }
            _ => warn.push(format!("entry {i}: unexpected value, skipped")),
        }
    }
    close_to(&mut stack, 1);
    tree.root = stack.pop().expect("root");
    Ok((tree, warn))
}

fn as_i64_lossy(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

fn take_string(m: &mut Map<String, Value>, k: &str) -> Option<String> {
    match m.remove(k)? {
        Value::String(s) => Some(s),
        Value::Null => None,
        // Keep odd values instead of losing them.
        other => {
            m.insert(k.to_owned(), other);
            None
        }
    }
}

fn node_from_raw(raw: &Map<String, Value>) -> Node {
    let mut raw = raw.clone();
    let kind = NodeKind::from_tabs_outliner(raw.get("type").and_then(Value::as_str));
    if raw.get("type").is_some_and(Value::is_string) || raw.get("type").is_some_and(Value::is_null) {
        raw.remove("type");
    }
    let mut n = Node::new(kind);
    n.collapsed = match raw.remove("colapsed") {
        Some(Value::Bool(b)) => b,
        Some(other) => {
            n.extra.insert("colapsed".into(), other);
            false
        }
        None => false,
    };
    match raw.remove("data") {
        Some(Value::Object(mut d)) => {
            if n.kind == NodeKind::Note {
                n.note = take_string(&mut d, "note");
            }
            n.title = take_string(&mut d, "title");
            n.url = take_string(&mut d, "url");
            n.favicon = take_string(&mut d, "favIconUrl");
            n.data = d;
        }
        Some(Value::Null) | None => {}
        Some(other) => {
            n.extra.insert("data".into(), other);
        }
    }
    match raw.remove("marks") {
        Some(Value::Object(mut m)) => {
            n.custom_title = take_string(&mut m, "customTitle");
            n.custom_favicon = take_string(&mut m, "customFavicon");
            // `relicons: []` is on nearly every node; leave it out when empty.
            if m.get("relicons").and_then(Value::as_array).is_some_and(Vec::is_empty) {
                m.remove("relicons");
            }
            n.marks = m;
        }
        Some(Value::Null) | None => {}
        Some(other) => {
            n.extra.insert("marks".into(), other);
        }
    }
    n.extra.extend(raw);
    n
}

fn node_to_raw(n: &Node) -> Value {
    let mut o = Map::new();
    if let Some(t) = n.kind.to_tabs_outliner() {
        o.insert("type".into(), json!(t));
    }
    let mut marks = Map::new();
    marks.insert("relicons".into(), json!([]));
    marks.extend(n.marks.clone());
    if let Some(t) = &n.custom_title {
        marks.insert("customTitle".into(), json!(t));
    }
    if let Some(f) = &n.custom_favicon {
        marks.insert("customFavicon".into(), json!(f));
    }
    o.insert("marks".into(), Value::Object(marks));
    if n.collapsed {
        o.insert("colapsed".into(), json!(true));
    }
    let mut d = n.data.clone();
    if let Some(v) = &n.title {
        d.insert("title".into(), json!(v));
    }
    if let Some(v) = &n.url {
        d.insert("url".into(), json!(v));
    }
    if let Some(v) = &n.favicon {
        d.insert("favIconUrl".into(), json!(v));
    }
    if let Some(v) = &n.note {
        d.insert("note".into(), json!(v));
    }
    o.insert("data".into(), Value::Object(d));
    for (k, v) in &n.extra {
        o.insert(k.clone(), v.clone());
    }
    Value::Object(o)
}

/// Write a tree back as a Tabs Outliner entry array.
pub fn to_value(tree: &Tree) -> Value {
    let mut out = Vec::with_capacity(tree.count() + 2);
    let mut sd = Map::new();
    sd.insert("treeId".into(), json!(tree.tree_id));
    sd.extend(tree.session.clone());
    out.push(json!({"type": HEADER, "node": {"type": "session", "data": sd}}));

    fn go(n: &Node, path: &mut Vec<usize>, out: &mut Vec<Value>) {
        for (i, c) in n.children.iter().enumerate() {
            path.push(i);
            out.push(json!([NODE, node_to_raw(c), path]));
            go(c, path, out);
            path.pop();
        }
    }
    go(&tree.root, &mut Vec::new(), &mut out);
    if let Some(t) = tree.saved_at {
        out.push(json!({"type": FOOTER, "time": t}));
    }
    Value::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
      {"type":2000,"node":{"type":"session","data":{"treeId":"1.5","nextDId":1}}},
      [2001,{"type":"savedwin","marks":{"relicons":[],"customTitle":"Work"},"colapsed":true,"data":{"rect":"0_0_10_10"}},[0]],
      [2001,{"marks":{"relicons":[]},"data":{"title":"A","url":"https://a"}},[0,0]],
      [2001,{"marks":{"relicons":[]},"data":{"title":"A1","url":"https://a1"}},[0,0,0]],
      [2001,{"marks":{"relicons":[]},"data":{"title":"B","url":"https://b"}},[0,1]],
      [2001,{"type":"textnote","marks":{"relicons":[]},"data":{"note":"hello"}},[1]],
      {"type":11111,"time":1627533622824}
    ]"#;

    #[test]
    fn parses_tree_shape() {
        let (t, w) = parse_str(SAMPLE).unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(t.tree_id.as_deref(), Some("1.5"));
        assert_eq!(t.saved_at, Some(1627533622824));
        assert_eq!(t.count(), 6);
        let win = &t.root.children[0];
        assert_eq!(win.kind, NodeKind::SavedWindow);
        assert!(win.collapsed);
        assert_eq!(win.custom_title.as_deref(), Some("Work"));
        assert_eq!(win.children[0].children[0].title.as_deref(), Some("A1"));
        assert_eq!(t.root.children[1].note.as_deref(), Some("hello"));
    }

    #[test]
    fn roundtrip_is_stable() {
        let (t, _) = parse_str(SAMPLE).unwrap();
        let v = to_value(&t);
        let (t2, w) = from_value(&v).unwrap();
        assert!(w.is_empty());
        // Ids are assigned fresh on import; compare everything else.
        fn strip(v: &mut Value) {
            match v {
                Value::Object(o) => {
                    o.remove("id");
                    o.values_mut().for_each(strip);
                }
                Value::Array(a) => a.iter_mut().for_each(strip),
                _ => {}
            }
        }
        let (mut a, mut b) = (serde_json::to_value(&t).unwrap(), serde_json::to_value(&t2).unwrap());
        strip(&mut a);
        strip(&mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn bad_paths_keep_nodes() {
        let s = r#"[
          [2001,{"data":{"title":"A"}},[0]],
          [2001,{"data":{"title":"deep"}},[0,0,0,0]],
          [2001,{"data":{"title":"nopath"}}]
        ]"#;
        let (t, w) = parse_str(s).unwrap();
        assert_eq!(t.count(), 4);
        assert_eq!(w.0.len(), 2);
    }
}
