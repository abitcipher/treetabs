//! Search over the tree: words, filters, duplicates, and match highlighting.

use eframe::egui::{self, Color32, text::LayoutJob};
use std::collections::HashMap;
use crate::i18n::tr;
use treetabs_core::{Node, NodeKind};

/// Stop collecting after this many hits.
pub const LIMIT: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    /// Open tabs and windows.
    Open,
    /// Saved (closed) tabs and windows.
    Saved,
    /// Tabs whose URL appears more than once.
    Duplicates,
}

impl Filter {
    pub const ALL: [Filter; 4] = [Filter::All, Filter::Open, Filter::Saved, Filter::Duplicates];

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => tr("All"),
            Filter::Open => tr("Open"),
            Filter::Saved => tr("Saved"),
            Filter::Duplicates => tr("Duplicates"),
        }
    }

    fn keeps(self, n: &Node) -> bool {
        match self {
            Filter::All | Filter::Duplicates => true,
            Filter::Open => matches!(n.kind, NodeKind::Tab | NodeKind::Window),
            Filter::Saved => matches!(
                n.kind,
                NodeKind::SavedTab | NodeKind::SavedWindow | NodeKind::WaitingTab | NodeKind::WaitingWindow | NodeKind::AttachWaitingTab
            ),
        }
    }
}

/// One result: where the node is (child indexes from the root) and the
/// titles of its parents.
pub struct Hit {
    pub at: Vec<usize>,
    pub parents: String,
}

/// Results for one query, kept until the query or the tree changes.
pub struct Results {
    pub key: (String, Filter, u64),
    pub hits: Vec<Hit>,
    pub truncated: bool,
    pub words: Vec<String>,
}

/// The words of a query, lowercased; all must match.
pub fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// The same page for duplicate detection: no fragment, no trailing slash.
pub fn normalize_url(u: &str) -> &str {
    let u = u.split_once('#').map_or(u, |(a, _)| a);
    u.strip_suffix('/').unwrap_or(u)
}

fn matches(n: &Node, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let mut text = n.display_title().to_lowercase();
    for extra in [n.title.as_deref(), n.url.as_deref(), n.note.as_deref()].into_iter().flatten() {
        text.push('\n');
        text.push_str(&extra.to_lowercase());
    }
    words.iter().all(|w| text.contains(w.as_str()))
}

pub fn run(root: &Node, query: &str, filter: Filter, version: u64) -> Results {
    let words = words(query);
    let dup_counts = (filter == Filter::Duplicates).then(|| {
        let mut c: HashMap<&str, usize> = HashMap::new();
        root.walk(&mut |n, _| {
            if let Some(u) = n.url.as_deref() {
                *c.entry(normalize_url(u)).or_default() += 1;
            }
        });
        c
    });
    let mut hits = Vec::new();
    let mut truncated = false;
    let mut at = Vec::new();
    let mut parents: Vec<String> = Vec::new();
    #[allow(clippy::too_many_arguments)]
    fn go(
        n: &Node,
        words: &[String],
        filter: Filter,
        dups: Option<&HashMap<&str, usize>>,
        at: &mut Vec<usize>,
        parents: &mut Vec<String>,
        hits: &mut Vec<Hit>,
        truncated: &mut bool,
    ) {
        for (i, c) in n.children.iter().enumerate() {
            if hits.len() >= LIMIT {
                *truncated = true;
                return;
            }
            at.push(i);
            let dup_ok = dups.is_none_or(|d| c.url.as_deref().is_some_and(|u| d.get(normalize_url(u)).copied().unwrap_or(0) > 1));
            if filter.keeps(c) && dup_ok && matches(c, words) {
                hits.push(Hit { at: at.clone(), parents: breadcrumbs(parents) });
            }
            if !c.children.is_empty() {
                parents.push(crate::i18n::title(c));
                go(c, words, filter, dups, at, parents, hits, truncated);
                parents.pop();
            }
            at.pop();
        }
    }
    go(root, &words, filter, dup_counts.as_ref(), &mut at, &mut parents, &mut hits, &mut truncated);
    if filter == Filter::Duplicates {
        // Copies next to each other.
        let url = |h: &Hit| resolve(root, &h.at).and_then(|n| n.url.as_deref()).map(normalize_url).unwrap_or("").to_owned();
        hits.sort_by_cached_key(url);
    }
    Results { key: (query.to_owned(), filter, version), hits, truncated, words }
}

/// For duplicate results: every copy but one per page. The one kept is the
/// first open tab, or the first copy if none is open.
pub fn extra_copies(root: &Node, hits: &[Hit]) -> Vec<treetabs_core::NodeId> {
    let mut groups: Vec<(&str, Vec<&Node>)> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for h in hits {
        let Some(n) = resolve(root, &h.at) else { continue };
        let Some(u) = n.url.as_deref() else { continue };
        let u = normalize_url(u);
        let i = *index.entry(u).or_insert_with(|| {
            groups.push((u, Vec::new()));
            groups.len() - 1
        });
        groups[i].1.push(n);
    }
    let mut out = Vec::new();
    for (_, copies) in groups {
        let keep = copies.iter().position(|n| n.kind == NodeKind::Tab).unwrap_or(0);
        out.extend(copies.iter().enumerate().filter(|(i, _)| *i != keep).map(|(_, n)| n.id));
    }
    out
}

/// "Window › docs", shortened to the last three parents.
fn breadcrumbs(parents: &[String]) -> String {
    let short = |s: &String| {
        if s.chars().count() > 30 { format!("{}…", s.chars().take(29).collect::<String>()) } else { s.clone() }
    };
    let skip = parents.len().saturating_sub(3);
    let mut out: Vec<String> = parents[skip..].iter().map(short).collect();
    if skip > 0 {
        out.insert(0, "…".into());
    }
    out.join(" › ")
}

pub fn resolve<'a>(root: &'a Node, at: &[usize]) -> Option<&'a Node> {
    let mut n = root;
    for &i in at {
        n = n.children.get(i)?;
    }
    Some(n)
}

/// `text` with the matching words highlighted.
pub fn highlighted(ui: &egui::Ui, text: &str, words: &[String], format: egui::TextFormat) -> LayoutJob {
    // Mark matches per char, comparing lowercased chars (byte offsets of a
    // lowercased string can differ from the original's).
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let lower: Vec<char> = chars.iter().map(|(_, c)| c.to_lowercase().next().unwrap_or(*c)).collect();
    let mut marked = vec![false; chars.len()];
    for w in words {
        let w: Vec<char> = w.chars().collect();
        if w.is_empty() || w.len() > lower.len() {
            continue;
        }
        for start in 0..=lower.len() - w.len() {
            if lower[start..start + w.len()] == w[..] {
                marked[start..start + w.len()].iter_mut().for_each(|m| *m = true);
            }
        }
    }
    // egui shapes each highlighted piece by itself: a piece boundary inside
    // an Arabic word would break its letters apart. Highlight such words
    // whole.
    let mut i = 0;
    while i < chars.len() {
        if crate::bidi::is_rtl_char(chars[i].1) {
            let start = i;
            while i < chars.len() && crate::bidi::is_rtl_char(chars[i].1) {
                i += 1;
            }
            if marked[start..i].iter().any(|&m| m) {
                marked[start..i].iter_mut().for_each(|m| *m = true);
            }
        } else {
            i += 1;
        }
    }
    let bg = if ui.visuals().dark_mode { Color32::from_rgb(110, 90, 20) } else { Color32::from_rgb(255, 225, 110) };
    let mut job = LayoutJob::default();
    let mut i = 0;
    while i < chars.len() {
        let m = marked[i];
        let mut j = i;
        while j < chars.len() && marked[j] == m {
            j += 1;
        }
        let (a, b) = (chars[i].0, chars.get(j).map_or(text.len(), |c| c.0));
        let mut f = format.clone();
        if m {
            f.background = bg;
        }
        job.append(&text[a..b], 0.0, f);
        i = j;
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, title: &str, url: Option<&str>, children: Vec<Node>) -> Node {
        let mut n = Node::new(kind);
        if n.kind == NodeKind::Window {
            n.custom_title = Some(title.into());
        } else {
            n.title = Some(title.into());
        }
        n.url = url.map(Into::into);
        n.children = children;
        n
    }

    fn tree() -> Node {
        node(
            NodeKind::Session,
            "",
            None,
            vec![
                node(
                    NodeKind::Window,
                    "Work",
                    None,
                    vec![
                        node(NodeKind::Tab, "Rust docs", Some("https://doc.rust-lang.org/"), vec![]),
                        node(NodeKind::SavedTab, "Rust Book", Some("https://doc.rust-lang.org/book#intro"), vec![]),
                    ],
                ),
                node(NodeKind::SavedTab, "Docs again", Some("https://doc.rust-lang.org"), vec![]),
            ],
        )
    }

    fn titles(root: &Node, r: &Results) -> Vec<String> {
        r.hits.iter().map(|h| resolve(root, &h.at).unwrap().display_title().to_owned()).collect()
    }

    #[test]
    fn all_words_must_match_anywhere() {
        let t = tree();
        let r = run(&t, "rust BOOK", Filter::All, 0);
        assert_eq!(titles(&t, &r), ["Rust Book"]);
        assert_eq!(r.hits[0].parents, "Work");
        // URL words count too.
        assert_eq!(titles(&t, &run(&t, "lang.org", Filter::All, 0)).len(), 3);
    }

    #[test]
    fn filters() {
        let t = tree();
        assert_eq!(titles(&t, &run(&t, "rust", Filter::Open, 0)), ["Rust docs"]);
        assert_eq!(titles(&t, &run(&t, "", Filter::Saved, 0)), ["Rust Book", "Docs again"]);
        // Trailing slash and fragment don't make a page different.
        let mut d = titles(&t, &run(&t, "", Filter::Duplicates, 0));
        d.sort();
        assert_eq!(d, ["Docs again", "Rust docs"]);
    }

    #[test]
    fn extra_copies_keep_the_open_tab() {
        let t = tree();
        let r = run(&t, "", Filter::Duplicates, 0);
        let extra = extra_copies(&t, &r.hits);
        assert_eq!(extra.len(), 1);
        let gone = t.find(extra[0]).unwrap();
        assert_eq!(gone.display_title(), "Docs again", "the open tab stays");
    }

    #[test]
    fn highlight_takes_whole_arabic_words() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let job = highlighted(ui, "مرحبا بالعالم", &words("بال"), egui::TextFormat::default());
            let marked: Vec<&str> =
                job.sections.iter().filter(|s| s.format.background != Color32::TRANSPARENT).map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0]).collect();
            assert_eq!(marked, ["بالعالم"]);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn highlight_handles_non_ascii() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let job = highlighted(ui, "Привет Мир", &words("мир"), egui::TextFormat::default());
            let marked: Vec<&str> =
                job.sections.iter().filter(|s| s.format.background != Color32::TRANSPARENT).map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0]).collect();
            assert_eq!(marked, ["Мир"]);
        });
        out.textures_delta.clear();
    }
}
