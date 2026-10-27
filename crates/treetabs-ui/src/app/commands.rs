//! Commands from menus, buttons and keys; undo and redo.

use super::rows::is_unloaded;
use super::{App, Command, EditKind, NewItem, Place, Scope};
use crate::i18n::{self, tr, trf};
use treetabs_core::protocol::ClientMsg;
use treetabs_core::{Node, NodeId, NodeKind, Op, Position};

impl App {
    pub(super) fn undo(&mut self) {
        if let Some(step) = self.undo.pop() {
            let step = self.retarget(step);
            self.edit(step, EditKind::Undo);
        }
    }

    pub(super) fn redo(&mut self) {
        if let Some(step) = self.redo.pop() {
            let step = self.retarget(step);
            self.edit(step, EditKind::Redo);
        }
    }

    /// Things may have changed since an undo step was recorded (the browser
    /// closed a window, …). Put nodes whose old parent is gone at the end of
    /// the tree instead of failing.
    pub(super) fn retarget(&self, step: Vec<Op>) -> Vec<Op> {
        let mut sim = self.tree.clone();
        let mut out = Vec::with_capacity(step.len());
        for mut op in step {
            if let Op::Restore { parent, index, .. }
            | Op::Insert { parent, index, .. }
            | Op::Move { parent, index, .. } = &mut op
            {
                let len = sim.root.find(*parent).map(|p| p.children.len());
                match len {
                    None => {
                        *parent = sim.root.id;
                        *index = sim.root.children.len();
                    }
                    Some(len) if *index > len => *index = len,
                    _ => {}
                }
            }
            let _ = sim.apply(&op);
            out.push(op);
        }
        out
    }

    /// Ids without the ones inside other listed nodes (and without the root).
    pub(super) fn top_level(&self, ids: &[NodeId]) -> Vec<NodeId> {
        ids.iter()
            .copied()
            .filter(|&id| id != self.tree.root.id)
            .filter(|&id| {
                !ids.iter()
                    .any(|&o| o != id && self.tree.root.find(o).is_some_and(|n| n.contains(id)))
            })
            .collect()
    }

    /// Run `cmd`, asking first if it would delete a big branch.
    pub(super) fn request(&mut self, cmd: Command) {
        if let Command::MoveStore(dir) = &cmd {
            self.confirm = Some((trf("Move the tree to {dir}?", &[("dir", dir)]), cmd));
            return;
        }
        if let Command::EmptyTrash = cmd {
            let n = self.tree.trash.len();
            self.confirm = Some((
                trf(
                    "Empty the trash? {n} item(s) are removed for good.",
                    &[("n", &n)],
                ),
                cmd,
            ));
            return;
        }
        if let Command::Trash {
            ids,
            scope: Scope::Branch,
        } = &cmd
        {
            let n: usize = self
                .top_level(ids)
                .iter()
                .filter_map(|id| self.tree.root.find(*id))
                .map(Node::count)
                .sum();
            if n > self.settings.confirm_branch_over.max(1) {
                self.confirm = Some((trf("Move {n} nodes to the trash?", &[("n", &n)]), cmd));
                return;
            }
        }
        self.command(cmd);
    }

    pub(super) fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Open(id) => self.send(ClientMsg::Activate { id }),
            Command::MoveStore(dir) => {
                self.status = trf("Moving the tree to {dir}…", &[("dir", &dir)]);
                self.send(ClientMsg::MoveStore { dir });
            }
            Command::EmptyTrash => {
                // First a copy that still has them (messages stay in order).
                self.send(ClientMsg::ExportNow);
                self.edit(vec![Op::PurgeTrash { before: i64::MAX }], EditKind::Quiet);
                self.status = tr("Trash emptied").into();
            }
            Command::Close { ids, scope } => self.close_in_browser(&ids, scope),
            Command::Unload { ids, scope } => self.unload(&ids, scope),
            Command::Trash { ids, scope } => self.trash(&ids, scope),
            Command::StartRename(id) => self.start_rename(id),
            Command::CancelRename => self.renaming = None,
            Command::Rename(id, text) => self.rename(id, &text),
            Command::Add { item, at, place } => self.add_item(item, at, place),
            Command::ExpandAll(id) => self.set_openness_below(id, true),
            Command::CollapseAll(id) => self.set_openness_below(id, false),
            Command::Copy(_) => {} // handled where the egui context is
        }
    }

    /// Ids of the nodes in `ids` (and below them, for a branch) that match
    /// `want`.
    fn matching(&self, ids: &[NodeId], scope: Scope, want: impl Fn(&Node) -> bool) -> Vec<NodeId> {
        let mut out = Vec::new();
        for n in ids.iter().filter_map(|id| self.tree.root.find(*id)) {
            match scope {
                Scope::Branch => n.walk(&mut |m, _| {
                    if want(m) {
                        out.push(m.id);
                    }
                }),
                Scope::Node if want(n) => out.push(n.id),
                Scope::Node => {}
            }
        }
        out
    }

    /// Close open tabs and windows in the browser; the nodes stay as saved.
    fn close_in_browser(&mut self, ids: &[NodeId], scope: Scope) {
        let open = self.matching(ids, scope, |m| {
            matches!(m.kind, NodeKind::Tab | NodeKind::Window)
        });
        if open.is_empty() {
            self.status = tr("Nothing open to close").into();
        } else {
            self.status = trf(
                "Closing {n} tab(s)/window(s), kept as saved",
                &[("n", &open.len())],
            );
            self.send(ClientMsg::CloseInBrowser { ids: open });
        }
    }

    /// Unload loaded tabs: they stay open and load again when shown.
    fn unload(&mut self, ids: &[NodeId], scope: Scope) {
        let tabs = self.matching(ids, scope, |m| m.kind == NodeKind::Tab && !is_unloaded(m));
        if tabs.is_empty() {
            self.status = tr("Nothing loaded to unload").into();
        } else {
            self.status = trf(
                "Unloading {n} tab(s); they stay open and load again when shown",
                &[("n", &tabs.len())],
            );
            self.send(ClientMsg::Unload { ids: tabs });
        }
    }

    /// Move to the trash. For [`Scope::Node`] the children first move up
    /// into the node's place.
    fn trash(&mut self, ids: &[NodeId], scope: Scope) {
        let top = self.top_level(ids);
        let at = treetabs_core::ops::now_ms();
        let n = top.len();
        let ops = match scope {
            Scope::Branch => top.into_iter().map(|id| Op::Trash { id, at }).collect(),
            Scope::Node => self.trash_keeping_children(top, at),
        };
        self.edit(ops, EditKind::Normal);
        self.selected.clear();
        self.status = match scope {
            Scope::Branch => trf(
                "Moved {n} branch(es) to the trash (Ctrl+Z to undo)",
                &[("n", &n)],
            ),
            Scope::Node => trf(
                "Moved {n} node(s) to the trash (Ctrl+Z to undo)",
                &[("n", &n)],
            ),
        };
    }

    /// Ops that move each node's children up into its place, then trash
    /// the node. Planned on a copy, so later ids see earlier moves.
    fn trash_keeping_children(&self, top: Vec<NodeId>, at: i64) -> Vec<Op> {
        let mut sim = self.tree.clone();
        let mut ops = Vec::new();
        for id in top {
            let Some((p, i)) = sim.root.locate(id) else {
                continue;
            };
            let kids: Vec<NodeId> = sim
                .root
                .find(id)
                .map(|n| n.children.iter().map(|c| c.id).collect())
                .unwrap_or_default();
            let mut step: Vec<Op> = kids
                .into_iter()
                .enumerate()
                .map(|(k, c)| Op::Move {
                    id: c,
                    parent: p,
                    index: i + 1 + k,
                })
                .collect();
            step.push(Op::Trash { id, at });
            for op in &step {
                // Same ops the server will apply; an error here only means
                // the server will refuse it too.
                let _ = sim.apply(op);
            }
            ops.extend(step);
        }
        ops
    }

    fn start_rename(&mut self, id: NodeId) {
        if let Some(n) = self.tree.root.find(id) {
            let text = match n.kind {
                NodeKind::Note => n.note.clone().unwrap_or_default(),
                _ => i18n::title(n),
            };
            self.renaming = Some((id, text, false));
        }
    }

    /// A note gets the text; anything else a custom title (or its own
    /// title back when the text is empty or the same as the page's).
    fn rename(&mut self, id: NodeId, text: &str) {
        self.renaming = None;
        let Some(n) = self.tree.root.find(id) else {
            return;
        };
        let mut f = n.fields();
        let text = text.trim().to_owned();
        if n.kind == NodeKind::Note {
            f.note = Some(text);
        } else if text.is_empty() || Some(&text) == n.title.as_ref() {
            f.custom_title = None;
        } else {
            f.custom_title = Some(text);
        }
        if f != n.fields() {
            self.edit(
                vec![Op::Update {
                    id,
                    fields: Box::new(f),
                }],
                EditKind::Normal,
            );
        }
    }

    /// A new note or group next to or inside `at`, selected and ready to
    /// be named.
    fn add_item(&mut self, item: NewItem, at: NodeId, place: Place) {
        let (mut n, label) = match item {
            NewItem::Note => (Node::new(NodeKind::Note), tr("New note")),
            NewItem::Group => (Node::new(NodeKind::Group), tr("New group")),
        };
        match item {
            NewItem::Note => n.note = Some(label.into()),
            NewItem::Group => n.custom_title = Some(label.into()),
        }
        let id = n.id;
        let (parent, pos) = match place {
            Place::Inside => (at, Position::LastChild),
            Place::After => {
                let Some((parent, _)) = self.tree.root.locate(at) else {
                    return;
                };
                (parent, Position::After(at))
            }
        };
        let Ok(index) = self.tree.resolve(parent, pos, None) else {
            return;
        };
        self.edit(
            vec![Op::Insert {
                parent,
                index,
                node: Box::new(n),
            }],
            EditKind::Normal,
        );
        if place == Place::Inside {
            self.state.set_openness(at, true);
        }
        self.selected = vec![id];
        self.state.set_one_selected(id);
        self.renaming = Some((id, label.into(), false));
    }

    /// Expand or collapse `id` and every node below it that has children.
    fn set_openness_below(&mut self, id: NodeId, open: bool) {
        let Some(n) = self.tree.root.find(id) else {
            return;
        };
        let mut dirs = Vec::new();
        n.walk(&mut |m, _| {
            if !m.children.is_empty() {
                dirs.push(m.id);
            }
        });
        for d in dirs {
            self.state.set_openness(d, open);
        }
    }
}
