//! The tree view: drag and drop, activation, collapsed state.

use super::{App, Command, EditKind};
use crate::i18n::trf;
use eframe::egui::{self};
use egui_ltreeview::{Action, DirPosition};
use treetabs_core::{NodeId, Op, Position};

impl App {
    pub(super) fn handle_actions(&mut self, actions: Vec<Action<NodeId>>, ui: &mut egui::Ui) {
        for a in actions {
            match a {
                Action::SetSelected(s) => {
                    self.selected = s;
                    self.edit_buf = None;
                }
                Action::Move(dnd) => {
                    let pos = match dnd.position {
                        DirPosition::First => Position::FirstChild,
                        DirPosition::Last => Position::LastChild,
                        DirPosition::Before(id) => Position::Before(id),
                        DirPosition::After(id) => Position::After(id),
                    };
                    match self.tree.plan_move(&dnd.source, dnd.target, pos) {
                        Ok(ops) => self.edit(ops, EditKind::Normal),
                        Err(e) => self.status = trf("Move refused: {error}", &[("error", &e)]),
                    }
                }
                Action::Drag(dnd) => {
                    if dnd.source.iter().any(|&s| {
                        self.tree
                            .root
                            .find(s)
                            .is_some_and(|n| n.contains(dnd.target))
                    }) {
                        dnd.remove_drop_marker(ui);
                    }
                }
                Action::Activate(act) => {
                    if self.renaming.is_none() {
                        for id in act.selected {
                            self.command(Command::Open(id));
                        }
                    }
                }
                Action::DragExternal(_) | Action::MoveExternal(_) => {}
            }
        }
    }

    /// Store expand/collapse changes of visible nodes in the tree.
    pub(super) fn sync_collapsed(&mut self, visible: &[(NodeId, bool)]) {
        let mut ops = Vec::new();
        for &(id, collapsed) in visible {
            if let Some(open) = self.state.is_open(&id)
                && open == collapsed
                && self.collapsing.insert(id)
                && let Some(n) = self.tree.root.find(id)
            {
                let mut f = n.fields();
                f.collapsed = !open;
                ops.push(Op::Update {
                    id,
                    fields: Box::new(f),
                });
            }
        }
        self.edit(ops, EditKind::Quiet);
    }
}
