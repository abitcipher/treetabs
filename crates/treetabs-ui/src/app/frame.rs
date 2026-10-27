//! One frame of the window: shortcuts, bars, side panel and tree.

use super::layout::line;
use super::rows::{Found, add_node, node_icon, node_menu, profile_labels, profile_of, row_label};
use super::{App, Command, Rows, Scope};
use crate::i18n::{self, tr, trf};
use crate::search;
use crate::settings::PanelTab;
use eframe::egui::{self, Color32, RichText};
use egui_ltreeview::{Action, NodeBuilder, TreeView, TreeViewState};
use std::cell::{Cell, RefCell};
use treetabs_core::{Node, NodeId, Tree};

/// Below this width the side panel goes under the tree instead of beside it.
const NARROW_WIDTH: f32 = 700.0;

/// Tree shortcuts pressed this frame.
#[derive(Debug, Default, Clone, Copy)]
struct Shortcuts {
    undo: bool,
    redo: bool,
    /// Delete the selected branches.
    delete: bool,
    /// Delete the selected nodes only (children move up).
    delete_one: bool,
    rename: bool,
}

impl Shortcuts {
    /// Each key event carries the modifiers held when it happened. The
    /// frame's own modifier state is at its end: if Ctrl was let go in the
    /// same frame, Ctrl+Z would read as a plain Z.
    fn read(ctx: &egui::Context) -> Self {
        ctx.input(|i| {
            let pressed = |key: egui::Key, want: fn(&egui::Modifiers) -> bool| {
                i.events.iter().any(|e| {
                    matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && want(modifiers))
                })
            };
            Shortcuts {
                undo: pressed(egui::Key::Z, |m| m.command && !m.shift),
                redo: pressed(egui::Key::Y, |m| m.command) || pressed(egui::Key::Z, |m| m.command && m.shift),
                delete: pressed(egui::Key::Delete, |m| !m.shift),
                delete_one: pressed(egui::Key::Delete, |m| m.shift),
                rename: pressed(egui::Key::F2, |_| true),
            }
        })
    }
}

impl App {
    /// One frame of the whole window.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.poll();
        self.poll_files();
        let ctx = ui.ctx().clone();
        if !self.settings_applied {
            i18n::install_fonts(&ctx);
            self.settings.apply(&ctx);
            self.settings_applied = true;
        }
        self.update_title(&ctx);
        self.handle_shortcuts(&ctx);

        let narrow = ui.available_width() < NARROW_WIDTH;
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui, narrow);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.settings.panel_open {
            self.side_panel(ui, narrow);
        }
        egui::CentralPanel::default().show(ui, |ui| self.tree_panel(ui));
        self.settings_window(&ctx);
        self.confirm_window(&ctx);
    }

    /// Window title with the node count; counting a big tree every frame
    /// costs, so only when the tree or the connection changed.
    fn update_title(&mut self, ctx: &egui::Context) {
        let connected = self.conn.connected();
        if self.title_for == Some((self.seq, connected)) {
            return;
        }
        self.title_for = Some((self.seq, connected));
        let n = self.tree.count() - 1;
        let title = if connected {
            trf("treetabs — {n} nodes", &[("n", &n)])
        } else {
            trf("treetabs — {n} nodes (offline)", &[("n", &n)])
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Undo, redo, delete and rename keys: for the tree, not while typing
    /// in a text field or while a dialog asks something.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let keys = Shortcuts::read(ctx);
        let typing = ctx
            .memory(|m| m.focused())
            .is_some_and(|f| !self.tree_ids.contains(&f));
        if typing || self.confirm.is_some() {
            return;
        }
        if keys.undo {
            self.undo();
        }
        if keys.redo {
            self.redo();
        }
        if (keys.delete || keys.delete_one) && !self.selected.is_empty() {
            let ids = self.selected.clone();
            self.request(Command::Trash {
                ids,
                scope: if keys.delete {
                    Scope::Branch
                } else {
                    Scope::Node
                },
            });
        }
        if keys.rename
            && let Some(&id) = self.selected.last()
        {
            self.command(Command::StartRename(id));
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let connected = self.conn.connected();
        let server_here = self.conn.is_server();
        line(ui, |ui| {
            let (dot, text) = match (connected, self.browsers) {
                (false, _) => (Color32::RED, tr("offline").to_owned()),
                (true, 0) => (Color32::GRAY, tr("no browser connected").to_owned()),
                (true, n) => (
                    Color32::from_rgb(90, 200, 120),
                    trf("{n} browser(s) connected", &[("n", &n)]),
                ),
            };
            ui.label(RichText::new("●").color(dot));
            ui.label(text);
            if server_here {
                ui.weak(format!("· {}", tr("store owner")));
            }
            ui.separator();
            ui.label(&self.status);
        });
    }

    /// The side panel: beside the tree, or under it in a narrow window.
    /// It never grows with its content (everything inside scrolls); its
    /// size is the user's, saved with the settings.
    fn side_panel(&mut self, ui: &mut egui::Ui, narrow: bool) {
        let dragging = ui.input(|i| i.pointer.any_down());
        if narrow {
            let r = egui::Panel::bottom("details-bottom")
                .resizable(true)
                .default_size(self.settings.details_height)
                .min_size(80.0)
                .show(ui, |ui| self.side_panel_contents(ui));
            let h = r.response.rect.height();
            if !dragging && (h - self.settings.details_height).abs() > 1.0 {
                self.settings.details_height = h;
                self.save_settings();
            }
        } else {
            let panel = if i18n::is_rtl() {
                egui::Panel::left("details")
            } else {
                egui::Panel::right("details")
            };
            let r = panel
                .resizable(true)
                .default_size(self.settings.details_width)
                .min_size(160.0)
                .max_size((ui.available_width() - 200.0).max(160.0))
                .show(ui, |ui| self.side_panel_contents(ui));
            let w = r.response.rect.width();
            if !dragging && (w - self.settings.details_width).abs() > 1.0 {
                self.settings.details_width = w;
                self.save_settings();
            }
        }
    }

    fn side_panel_contents(&mut self, ui: &mut egui::Ui) {
        self.panel_header(ui);
        match self.settings.panel_tab {
            // Scrolls its list itself, below its controls.
            PanelTab::Trash => self.trash_panel(ui),
            tab => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match tab {
                        PanelTab::Backups => self.backups_panel(ui),
                        _ => self.details(ui),
                    });
            }
        }
    }

    /// The tree, or the search results while searching or filtering; then
    /// what the user did with them.
    fn tree_panel(&mut self, ui: &mut egui::Ui) {
        let cmds = RefCell::new(Vec::new());
        let renaming = RefCell::new(self.renaming.take());
        let mut visible = Vec::new();
        let searching = !self.search.trim().is_empty() || self.search_filter != search::Filter::All;
        if searching {
            self.refresh_search();
        }
        let actions = egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let conn = &self.conn;
                let favicon = |n: &Node| conn.favicon_uri(n);
                let profiles = profile_labels(&self.tree, &self.profile_names);
                let clip = ui.clip_rect();
                let margin = clip.height().max(200.0);
                let rows = Rows {
                    cmds: &cmds,
                    renaming: &renaming,
                    settings: &self.settings,
                    favicon: &favicon,
                    profiles: (profiles.len() > 1).then_some(&profiles),
                    next_row: Cell::new(ui.cursor().top()),
                    row_step: self.settings.row_height() + ui.spacing().item_spacing.y,
                    near: egui::Rangef::new(clip.top() - margin, clip.bottom() + margin),
                };
                self.tree_ids = [
                    ui.make_persistent_id("tree"),
                    ui.make_persistent_id("search"),
                ];
                let filter = self.profile_filter.as_deref();
                match self.search_cache.as_ref().filter(|_| searching) {
                    Some(res) => {
                        search_results(ui, self.tree_ids[1], &self.tree, res, filter, &rows)
                    }
                    None => tree_view(
                        ui,
                        self.tree_ids[0],
                        &mut self.state,
                        &self.tree,
                        filter,
                        &rows,
                        &mut visible,
                    ),
                }
            })
            .inner;
        self.renaming = renaming.into_inner();
        self.handle_actions(actions, ui);
        for c in cmds.into_inner() {
            match c {
                Command::Copy(text) => {
                    ui.ctx().copy_text(text);
                    self.status = tr("Copied").into();
                }
                other => self.request(other),
            }
        }
        self.sync_collapsed(&visible);
    }

    /// Search again if the query, the filter or the tree changed.
    fn refresh_search(&mut self) {
        let key = (self.search.clone(), self.search_filter, self.seq);
        if self.search_cache.as_ref().is_none_or(|c| c.key != key) {
            self.search_cache = Some(search::run(
                &self.tree.root,
                &self.search,
                self.search_filter,
                self.seq,
            ));
        }
    }
}

/// Whether a top-level node is hidden by the browser profile filter.
fn filtered_out(top: Option<&Node>, profile_filter: Option<&str>) -> bool {
    match profile_filter {
        Some(p) => top.and_then(profile_of).is_some_and(|b| b != p),
        None => false,
    }
}

/// The whole tree. `visible` collects the expandable nodes drawn, with
/// their stored collapsed state (see `App::sync_collapsed`).
fn tree_view(
    ui: &mut egui::Ui,
    id: egui::Id,
    state: &mut TreeViewState<NodeId>,
    tree: &Tree,
    profile_filter: Option<&str>,
    rows: &Rows<'_>,
    visible: &mut Vec<(NodeId, bool)>,
) -> Vec<Action<NodeId>> {
    let (_, actions) = TreeView::new(id)
        .allow_multi_selection(true)
        .default_node_height(Some(rows.settings.row_height()))
        .show_state(ui, state, |b| {
            for c in tree
                .root
                .children
                .iter()
                .filter(|c| !filtered_out(Some(c), profile_filter))
            {
                add_node(b, c, rows, visible);
            }
        });
    actions
}

/// Search results as a flat list, with the match count and, for the
/// duplicates filter, a button that closes the extra copies.
fn search_results(
    ui: &mut egui::Ui,
    id: egui::Id,
    tree: &Tree,
    res: &search::Results,
    profile_filter: Option<&str>,
    rows: &Rows<'_>,
) -> Vec<Action<NodeId>> {
    ui.horizontal_wrapped(|ui| {
        let count = format!("{}{}", res.hits.len(), if res.truncated { "+" } else { "" });
        ui.weak(trf("{n} match(es); double-click opens in the browser", &[("n", &count)]));
        if res.key.1 == search::Filter::Duplicates && !res.hits.is_empty() {
            let extra = search::extra_copies(&tree.root, &res.hits);
            if ui
                .button(trf("Close {n} duplicate(s)", &[("n", &extra.len())]))
                .on_hover_text(tr("Keep one copy of each page (an open one if there is), move the others to the trash"))
                .clicked()
            {
                rows.cmds.borrow_mut().push(Command::Trash { ids: extra, scope: Scope::Node });
            }
        }
    });
    let (_, actions) = TreeView::new(id)
        .allow_drag_and_drop(false)
        .allow_multi_selection(true)
        .default_node_height(Some(rows.settings.row_height()))
        .show(ui, |b| {
            for hit in &res.hits {
                let Some(n) = search::resolve(&tree.root, &hit.at) else {
                    continue;
                };
                if filtered_out(search::resolve(&tree.root, &hit.at[..1]), profile_filter) {
                    continue;
                }
                let found = Found {
                    words: &res.words,
                    parents: &hit.parents,
                };
                b.node(
                    NodeBuilder::leaf(n.id)
                        .icon(|ui| node_icon(ui, n, rows))
                        .label_ui(|ui| row_label(ui, n, rows, Some(&found)))
                        .context_menu(|ui| node_menu(ui, n, rows.cmds)),
                );
            }
        });
    actions
}
