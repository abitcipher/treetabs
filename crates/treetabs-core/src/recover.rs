//! Find tabs that exist in older trees but are missing from the current one.

use crate::model::{Node, NodeKind, Tree};
use std::collections::HashSet;

/// Collect URLs of all tab nodes in `tree`.
pub fn tab_urls(tree: &Tree) -> HashSet<&str> {
    let mut set = HashSet::new();
    tree.root.walk(&mut |n, _| {
        if n.kind.is_tab()
            && let Some(u) = n.url.as_deref()
        {
            set.insert(u);
        }
    });
    set
}

/// Tabs (by URL) present in `older` trees but not in `current`.
///
/// Returns one saved window per older tree that contributed tabs, each
/// labelled with `label(index)`. Each URL is reported once, from the
/// newest tree that has it. Recovered tabs come back as saved tabs without
/// their children (children are checked on their own).
pub fn lost_tabs<'a>(
    current: &Tree,
    older: impl IntoIterator<Item = &'a Tree>,
    label: impl Fn(usize, &Tree) -> String,
) -> Vec<Node> {
    let mut seen: HashSet<String> = tab_urls(current).into_iter().map(str::to_owned).collect();
    let mut out = Vec::new();
    for (i, t) in older.into_iter().enumerate() {
        let mut win = Node::new(NodeKind::SavedWindow);
        win.custom_title = Some(label(i, t));
        t.root.walk(&mut |n, _| {
            if n.kind.is_tab()
                && let Some(u) = n.url.as_deref()
                && seen.insert(u.to_owned())
            {
                let mut c = n.clone();
                c.children.clear();
                c.kind = NodeKind::SavedTab;
                c.reassign_ids();
                win.children.push(c);
            }
        });
        if !win.children.is_empty() {
            out.push(win);
        }
    }
    out
}
