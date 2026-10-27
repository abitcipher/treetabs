//! Side panel tabs: Details and Trash.

use super::layout::{line, para};
use super::rows::{is_unloaded, kind_style};
use super::{App, Command, EditBuffer, EditKind, Scope};
use crate::bidi::{Direction, Tone};
use crate::i18n::{self, tr, trf, trl};
use eframe::egui::{self, RichText};
use treetabs_core::protocol::ClientMsg;
use treetabs_core::{Node, NodeKind, Op};

/// Title, URL and child counts, as label/value rows (mirrored when the
/// interface is right to left).
fn details_grid(ui: &mut egui::Ui, node: &Node, children: usize, below: usize) {
    let rtl = i18n::is_rtl();
    let pair = |ui: &mut egui::Ui, label: &str, value: &mut dyn FnMut(&mut egui::Ui)| {
        if rtl {
            value(ui);
            ui.label(label);
        } else {
            ui.label(label);
            value(ui);
        }
        ui.end_row();
    };
    egui::Grid::new("details")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            if let Some(t) = &node.title {
                pair(ui, tr("Title"), &mut |ui| {
                    crate::bidi::paragraph(ui, t, Direction::LeftToRight, Tone::Normal)
                });
            }
            if let Some(u) = &node.url {
                // Wrapped text (a hyperlink can't wrap and would widen the panel).
                pair(ui, tr("URL"), &mut |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(u).color(ui.visuals().hyperlink_color))
                            .wrap()
                            .selectable(true),
                    );
                });
            }
            let count = trf(
                "{n} (whole branch {all})",
                &[("n", &children), ("all", &below)],
            );
            pair(ui, tr("Children"), &mut |ui| {
                ui.label(&count);
            });
        });
}

impl App {
    pub(super) fn details(&mut self, ui: &mut egui::Ui) {
        let Some(&id) = self.selected.last() else {
            ui.weak(tr("Nothing selected"));
            return;
        };
        if self.selected.len() > 1 {
            self.details_of_many(ui);
            return;
        }
        let Some(found) = self.tree.root.find(id) else {
            return;
        };
        // Only this node's own fields: no copy of its whole subtree.
        let (children, below) = (found.children.len(), found.count() - 1);
        let node = found.without_children();
        ui.heading(if is_unloaded(&node) {
            format!("{} · {}", i18n::kind_name(&node.kind), tr("unloaded"))
        } else {
            i18n::kind_name(&node.kind).to_owned()
        });
        details_grid(ui, &node, children, below);
        ui.horizontal_wrapped(|ui| {
            let open = matches!(node.kind, NodeKind::Tab | NodeKind::Window);
            if ui
                .button(if open {
                    tr("Focus in browser")
                } else {
                    tr("Open in browser")
                })
                .clicked()
            {
                self.command(Command::Open(id));
            }
            if open && ui.button(tr("Close (keep saved)")).clicked() {
                self.command(Command::Close {
                    ids: vec![id],
                    scope: Scope::Node,
                });
            }
        });
        ui.separator();
        self.details_editor(ui, &node);
        ui.separator();
        ui.collapsing(tr("Raw data"), |ui| {
            let text = serde_json::to_string_pretty(&node).unwrap_or_default();
            ui.add(egui::Label::new(RichText::new(text).monospace()).wrap());
        });
    }

    /// Several nodes selected: what can be done to all of them.
    fn details_of_many(&mut self, ui: &mut egui::Ui) {
        ui.label(trf("{n} nodes selected", &[("n", &self.selected.len())]));
        ui.horizontal_wrapped(|ui| {
            let ids = self.selected.clone();
            if ui.button(tr("Close all")).clicked() {
                self.request(Command::Close {
                    ids: ids.clone(),
                    scope: Scope::Branch,
                });
            }
            if ui.button(tr("Move to trash")).clicked() {
                self.request(Command::Trash {
                    ids,
                    scope: Scope::Branch,
                });
            }
        });
    }

    /// Custom title (and the text of a note), committed on Enter or when
    /// the field loses focus.
    fn details_editor(&mut self, ui: &mut egui::Ui, node: &Node) {
        let id = node.id;
        let mut buf = match self.edit_buf.take() {
            Some(b) if b.id == id => b,
            _ => EditBuffer {
                id,
                custom_title: node.custom_title.clone().unwrap_or_default(),
                note: node.note.clone().unwrap_or_default(),
            },
        };
        let is_note = node.kind == NodeKind::Note;
        ui.label(tr("Custom title"));
        let mut commit = crate::bidi::line_edit(
            ui,
            egui::Id::new("custom-title"),
            &mut buf.custom_title,
            |e| e,
        )
        .lost_focus();
        if is_note {
            ui.label(tr("Note"));
            commit |= ui.text_edit_multiline(&mut buf.note).lost_focus();
        }
        let changed = buf.custom_title != node.custom_title.clone().unwrap_or_default()
            || (is_note && Some(&buf.note) != node.note.as_ref());
        if commit && changed {
            let mut f = node.fields();
            f.custom_title = (!buf.custom_title.is_empty()).then(|| buf.custom_title.clone());
            if is_note {
                f.note = Some(buf.note.clone());
            }
            self.edit(
                vec![Op::Update {
                    id,
                    fields: Box::new(f),
                }],
                EditKind::Normal,
            );
        }
        self.edit_buf = Some(buf);
    }

    pub(super) fn trash_panel(&mut self, ui: &mut egui::Ui) {
        para(
            ui,
            trl("Newest first. Restore puts a node back where it was."),
            Tone::Weak,
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Remove items older than"));
            let mut days = self.trash_days;
            let name = |d: u32| {
                if d == 0 {
                    tr("never").to_owned()
                } else {
                    trf("{n} days", &[("n", &d)])
                }
            };
            egui::ComboBox::from_id_salt("trash-days")
                .selected_text(name(days))
                .show_ui(ui, |ui| {
                    for d in [0, 7, 30, 90, 365] {
                        ui.selectable_value(&mut days, d, name(d));
                    }
                });
            if days != self.trash_days {
                self.trash_days = days;
                self.send(ClientMsg::SetTrashDays { days });
            }
            if ui
                .add_enabled(
                    !self.tree.trash.is_empty(),
                    egui::Button::new(tr("Empty trash…")),
                )
                .clicked()
            {
                self.request(Command::EmptyTrash);
            }
        });
        ui.separator();
        // Only the list scrolls, both ways: long titles stay on one line
        // and the controls above stay in place.
        let mut restore = None;
        egui::ScrollArea::both()
            .id_salt("trash-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                for item in self.tree.trash.iter().rev().take(500) {
                    line(ui, |ui| {
                        if ui.small_button(tr("Restore")).clicked() {
                            restore = Some((item.node.id, item.parent, item.index));
                        }
                        let (icon, _) = kind_style(&item.node);
                        let extra = if item.node.children.is_empty() {
                            String::new()
                        } else {
                            format!(" (+{})", item.node.count() - 1)
                        };
                        ui.label(format!("{icon} {}{extra}", i18n::shown_title(&item.node)));
                    });
                }
            });
        if let Some((id, parent, index)) = restore {
            // The old parent may be gone; fall back to the end of the tree.
            let (parent, index) = match self.tree.root.find(parent) {
                Some(p) if index <= p.children.len() => (parent, index),
                Some(p) => (parent, p.children.len()),
                None => (self.tree.root.id, self.tree.root.children.len()),
            };
            self.edit(vec![Op::Restore { id, parent, index }], EditKind::Normal);
        }
    }
}
