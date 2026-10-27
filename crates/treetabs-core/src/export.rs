//! Exports to formats other programs read: a bookmarks HTML file (every
//! browser imports it) and a Markdown outline.

use crate::{Node, NodeKind, Tree};

/// Export formats offered to users.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// treetabs portable JSON: everything, can be imported back.
    Treetabs,
    /// Tabs Outliner's own format: can be imported into Tabs Outliner.
    TabsOutliner,
    /// Netscape bookmarks HTML: windows and groups become folders.
    BookmarksHtml,
    /// A nested Markdown list of links.
    Markdown,
}

impl Format {
    pub const ALL: [Format; 4] = [
        Format::Treetabs,
        Format::TabsOutliner,
        Format::BookmarksHtml,
        Format::Markdown,
    ];

    /// File name for an export made on `date` (`YYYY-MM-DD`).
    pub fn file_name(self, date: &str) -> String {
        match self {
            Format::Treetabs => format!("treetabs-{date}.json"),
            Format::TabsOutliner => format!("treetabs-{date}-tabs-outliner.json"),
            Format::BookmarksHtml => format!("treetabs-{date}-bookmarks.html"),
            Format::Markdown => format!("treetabs-{date}.md"),
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Treetabs | Format::TabsOutliner => "application/json",
            Format::BookmarksHtml => "text/html",
            Format::Markdown => "text/markdown",
        }
    }

    /// The tree in this format.
    pub fn render(self, tree: &Tree) -> String {
        match self {
            Format::Treetabs => {
                crate::to_portable_json(tree, crate::JsonStyle::Pretty).unwrap_or_default()
            }
            Format::TabsOutliner => {
                serde_json::to_string(&crate::tabs_outliner::to_value(tree)).unwrap_or_default()
            }
            Format::BookmarksHtml => bookmarks_html(tree),
            Format::Markdown => markdown(tree),
        }
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn is_folder(n: &Node) -> bool {
    n.kind.is_window() || n.kind == NodeKind::Group || (n.url.is_none() && !n.children.is_empty())
}

/// Netscape bookmark file. Windows and groups are folders; a tab with
/// children becomes a folder holding the tab, then its children (as in the
/// bookmark mirror). Notes and separators are left out.
pub fn bookmarks_html(tree: &Tree) -> String {
    fn go(n: &Node, depth: usize, out: &mut String) {
        let pad = "    ".repeat(depth);
        let title = escape(&n.display_title());
        if is_folder(n) {
            out.push_str(&format!("{pad}<DT><H3>{title}</H3>\n{pad}<DL><p>\n"));
            for c in &n.children {
                go(c, depth + 1, out);
            }
            out.push_str(&format!("{pad}</DL><p>\n"));
        } else if let Some(url) = &n.url {
            let link = format!("<DT><A HREF=\"{}\">{title}</A>\n", escape(url));
            if n.children.is_empty() {
                out.push_str(&format!("{pad}{link}"));
            } else {
                out.push_str(&format!(
                    "{pad}<DT><H3>{title}</H3>\n{pad}<DL><p>\n{pad}    {link}"
                ));
                for c in &n.children {
                    go(c, depth + 1, out);
                }
                out.push_str(&format!("{pad}</DL><p>\n"));
            }
        } else {
            for c in &n.children {
                go(c, depth, out);
            }
        }
    }
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n<TITLE>treetabs</TITLE>\n<H1>treetabs</H1>\n<DL><p>\n",
    );
    for c in &tree.root.children {
        go(c, 1, &mut out);
    }
    out.push_str("</DL><p>\n");
    out
}

/// Nested Markdown list: links for tabs, bold for windows and groups,
/// notes as text.
pub fn markdown(tree: &Tree) -> String {
    fn md(s: &str) -> String {
        s.replace('[', "\\[").replace(']', "\\]")
    }
    fn go(n: &Node, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        let title = md(&n.display_title());
        let line = match (&n.kind, &n.url) {
            (NodeKind::Separator, _) => "---".to_owned(),
            (NodeKind::Note, _) => title,
            (_, Some(url)) => format!("[{title}]({})", url.replace(' ', "%20").replace(')', "%29")),
            _ => format!("**{title}**"),
        };
        out.push_str(&format!("{pad}- {line}\n"));
        for c in &n.children {
            go(c, depth + 1, out);
        }
    }
    let mut out = String::from("# treetabs\n\n");
    for c in &tree.root.children {
        go(c, 0, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Tree {
        let mut t = Tree::default();
        let mut w = Node::new(NodeKind::SavedWindow);
        w.custom_title = Some("Work <1>".into());
        let mut a = Node::new(NodeKind::SavedTab);
        a.title = Some("Docs & more".into());
        a.url = Some("https://a.example/".into());
        let mut b = Node::new(NodeKind::SavedTab);
        b.title = Some("Child".into());
        b.url = Some("https://b.example/".into());
        a.children.push(b);
        let mut note = Node::new(NodeKind::Note);
        note.note = Some("remember".into());
        w.children.push(a);
        w.children.push(note);
        t.root.children.push(w);
        t
    }

    #[test]
    fn html_has_folders_links_and_escapes() {
        let h = bookmarks_html(&tree());
        assert!(h.starts_with("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
        assert!(h.contains("<H3>Work &lt;1&gt;</H3>"));
        assert!(h.contains("<A HREF=\"https://a.example/\">Docs &amp; more</A>"));
        assert!(h.contains("<A HREF=\"https://b.example/\">Child</A>"));
        assert!(!h.contains("remember"), "notes are not bookmarks");
    }

    #[test]
    fn markdown_nests() {
        let m = markdown(&tree());
        assert!(m.contains("- **Work <1>**\n  - [Docs & more](https://a.example/)\n    - [Child](https://b.example/)\n  - remember\n"), "{m}");
    }

    #[test]
    fn json_formats_load_back() {
        for f in [Format::Treetabs, Format::TabsOutliner] {
            let text = f.render(&tree());
            let back = crate::load_json_str(&text).expect("loads").tree;
            assert_eq!(back.count(), tree().count(), "{f:?}");
        }
    }
}
