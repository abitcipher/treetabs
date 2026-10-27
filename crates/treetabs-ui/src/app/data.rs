//! The Data tab: import, export and backups.

use super::layout::{format_size, format_time, line, para};
use super::{App, EditKind};
use crate::bidi::Tone;
use crate::i18n::{tr, trf, trl};
use eframe::egui::{self, RichText};
use treetabs_core::backups::{BackupInfo, BackupKind};
use treetabs_core::protocol::ClientMsg;
use treetabs_core::{Node, NodeKind, Op};

impl App {
    /// Import a picked file: treetabs or Tabs Outliner JSON, added at the
    /// end as a group.
    pub(super) fn import_file(&mut self, name: &str, data: &[u8]) {
        let text = String::from_utf8_lossy(data);
        match treetabs_core::load_json_str(&text) {
            Ok(loaded) => self.add_copy(
                loaded.tree.root,
                &trf("Imported from {name}", &[("name", &name)]),
                loaded.warnings.len(),
            ),
            Err(e) => {
                self.status = trf(
                    "Cannot import {name}: {error}",
                    &[("name", &name), ("error", &e)],
                )
            }
        }
    }

    pub(super) fn export_tree(&mut self) {
        let f = self.export_format;
        let date =
            treetabs_core::backups::local_time(treetabs_core::ops::now_ms())[..10].to_owned();
        let name = f.file_name(&date);
        let data = f.render(&self.tree);
        self.status = match self.conn.save_file(&name, f.mime(), data.as_bytes()) {
            Ok(Some(where_)) => trf("Exported {name}", &[("name", &where_)]),
            Ok(None) => String::new(),
            Err(e) => trf("Export failed: {error}", &[("error", &e)]),
        };
    }

    /// Add a copied tree at the end, as a group of saved tabs (an undoable
    /// edit: nothing existing is replaced).
    pub(super) fn add_copy(&mut self, mut root: Node, title: &str, warnings: usize) {
        let mut group = Node::new(NodeKind::Group);
        group.custom_title = Some(title.to_owned());
        root.reassign_ids();
        root.make_saved();
        group.children = root.children;
        let n = group.count() - 1;
        let index = self.tree.root.children.len();
        self.edit(
            vec![Op::Insert {
                parent: self.tree.root.id,
                index,
                node: Box::new(group),
            }],
            EditKind::Normal,
        );
        self.status = if warnings > 0 {
            trf(
                "{title}: {n} nodes added at the end ({warnings} warnings)",
                &[("title", &title), ("n", &n), ("warnings", &warnings)],
            )
        } else {
            trf(
                "{title}: {n} nodes added at the end",
                &[("title", &title), ("n", &n)],
            )
        };
    }

    /// The Data tab: export, import, backups.
    pub(super) fn backups_panel(&mut self, ui: &mut egui::Ui) {
        self.export_section(ui);
        ui.add_space(6.0);
        self.import_section(ui);
        ui.separator();
        self.backups_section(ui);
    }

    fn export_section(&mut self, ui: &mut egui::Ui) {
        use treetabs_core::export::Format;
        ui.strong(tr("Export"));
        let name = |f: Format| match f {
            Format::Treetabs => tr("treetabs JSON (complete)"),
            Format::TabsOutliner => tr("Tabs Outliner JSON"),
            Format::BookmarksHtml => tr("Bookmarks HTML (for any browser)"),
            Format::Markdown => tr("Markdown list"),
        };
        // One per line: side by side they would widen the panel.
        egui::ComboBox::from_id_salt("export-format")
            .selected_text(name(self.export_format))
            .show_ui(ui, |ui| {
                for f in Format::ALL {
                    ui.selectable_value(&mut self.export_format, f, name(f));
                }
            });
        if ui.button(tr("Export…")).clicked() {
            self.export_tree();
        }
    }

    fn import_section(&mut self, ui: &mut egui::Ui) {
        ui.strong(tr("Import"));
        para(
            ui,
            trl(
                "A treetabs or Tabs Outliner JSON file. It is added at the end as a group of saved tabs; nothing is replaced, and Ctrl+Z takes it out again.",
            ),
            Tone::Weak,
        );
        if ui.button(tr("Import file…")).clicked() {
            self.conn.open_file();
        }
    }

    /// Daily copies and snapshots; asked from the server the first time.
    fn backups_section(&mut self, ui: &mut egui::Ui) {
        ui.strong(tr("Backups"));
        if self.backups.is_none() {
            self.backups = Some((Vec::new(), String::new()));
            self.send(ClientMsg::ListBackups);
        }
        para(
            ui,
            trl(
                "A copy of the tree is saved every day. \"Add to tree\" puts a copy back at the end as a group of saved tabs; nothing is replaced, and Ctrl+Z takes it out again.",
            ),
            Tone::Weak,
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button(format!("💾 {}", tr("Save a copy now"))).clicked() {
                self.send(ClientMsg::ExportNow);
            }
            if ui.button(format!("⟳ {}", tr("Refresh"))).clicked() {
                self.send(ClientMsg::ListBackups);
            }
        });
        let Some((list, dir)) = &self.backups else {
            return;
        };
        let read = backups_list(ui, list, dir);
        if let Some(name) = read {
            self.status = trf("Reading {name}…", &[("name", &name)]);
            self.send(ClientMsg::ReadBackup { name });
        }
    }
}

/// The copies by kind, newest first; returns the one to add to the tree.
fn backups_list(ui: &mut egui::Ui, list: &[BackupInfo], dir: &str) -> Option<String> {
    if !dir.is_empty() {
        ui.add(
            egui::Label::new(
                RichText::new(trf("Folder: {dir}", &[("dir", &dir)]))
                    .small()
                    .weak(),
            )
            .wrap(),
        );
    }
    ui.separator();
    if list.is_empty() {
        ui.weak(tr("No copies yet."));
    }
    let mut read = None;
    for (kind, heading) in [
        (BackupKind::Daily, tr("Daily copies")),
        (BackupKind::Snapshot, tr("Automatic snapshots")),
    ] {
        let items: Vec<&BackupInfo> = list.iter().filter(|b| b.kind == kind).collect();
        if items.is_empty() {
            continue;
        }
        ui.strong(heading);
        for b in items.into_iter().take(100) {
            line(ui, |ui| {
                if ui.small_button(tr("Add to tree")).clicked() {
                    read = Some(b.name.clone());
                }
                ui.label(format_time(b.at));
                ui.weak(format_size(b.bytes));
            })
            .response
            .on_hover_text(&b.name);
        }
        ui.add_space(6.0);
    }
    read
}
