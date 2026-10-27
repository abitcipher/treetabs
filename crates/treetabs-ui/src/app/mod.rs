//! Tree window: a live view of the server's tree.
//!
//! The GUI keeps a replica of the tree, fed by the server's snapshot and op
//! stream. Edits are sent as ops; the tree only changes when the server
//! echoes them back, so the GUI, the browser and other viewers always agree.

mod commands;
mod data;
mod frame;
mod layout;
mod panels;
#[cfg(test)]
mod perf;
mod rows;
mod toolbar;
mod tree;
mod windows;

use crate::Transport;
use crate::i18n::{self, tr, trf};
use crate::search;
use crate::settings::Settings;
use eframe::egui;
use egui_ltreeview::TreeViewState;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use treetabs_core::protocol::{ClientMsg, ServerMsg};
use treetabs_core::{Node, NodeId, Op, Tree};

const UNDO_LIMIT: usize = 200;

/// What an edit request was for, to file its inverse correctly.
#[derive(Clone, Copy)]
enum EditKind {
    Normal,
    Undo,
    Redo,
    /// Not undoable (expand/collapse).
    Quiet,
}

/// What a command on a node applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The node itself (for deleting: its children move up into its place).
    Node,
    /// The node and everything below it.
    Branch,
}

/// A new item made from the context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewItem {
    Note,
    Group,
}

/// Where a new item goes, relative to the node it was made from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    After,
    Inside,
}

/// Requests from inside the tree (menus, hover buttons), handled after drawing.
#[derive(Debug, Clone)]
enum Command {
    Open(NodeId),
    /// Close in the browser (nodes stay as saved).
    Close {
        ids: Vec<NodeId>,
        scope: Scope,
    },
    /// Move to the trash.
    Trash {
        ids: Vec<NodeId>,
        scope: Scope,
    },
    /// Unload open tabs (free memory, keep them open).
    Unload {
        ids: Vec<NodeId>,
        scope: Scope,
    },
    StartRename(NodeId),
    Rename(NodeId, String),
    CancelRename,
    Add {
        item: NewItem,
        at: NodeId,
        place: Place,
    },
    /// Expand a node and everything below it.
    ExpandAll(NodeId),
    /// Collapse a node and everything below it.
    CollapseAll(NodeId),
    Copy(String),
    /// Remove everything in the trash for good (asks first).
    EmptyTrash,
    /// Copy the store to this folder and use it from now on (asks first).
    MoveStore(String),
}

/// The details panel's text fields, kept while the user types; committed
/// on Enter or when the field loses focus.
#[derive(Debug, Clone)]
struct EditBuffer {
    id: NodeId,
    custom_title: String,
    note: String,
}

/// Tabs of the settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Interface,
    Storage,
    About,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            SettingsTab::Interface => tr("Interface"),
            SettingsTab::Storage => tr("Storage"),
            SettingsTab::About => tr("About"),
        }
    }
}

/// Inline rename in progress: node, text, focus requested yet.
type Renaming = Option<(NodeId, String, bool)>;

/// What tree rows need while being drawn.
struct Rows<'a> {
    cmds: &'a RefCell<Vec<Command>>,
    renaming: &'a RefCell<Renaming>,
    settings: &'a Settings,
    favicon: &'a dyn Fn(&Node) -> Option<String>,
    /// Labels for windows by profile id, when the tree has more than one.
    profiles: Option<&'a std::collections::BTreeMap<String, String>>,
    /// Top of the next tree row (screen y), and the height of each row.
    next_row: std::cell::Cell<f32>,
    row_step: f32,
    /// Rows outside this y range get no label, icon or menu: they are far
    /// off screen, and building those for every row of a big tree is what
    /// costs the most.
    near: egui::Rangef,
}

pub struct App {
    conn: Box<dyn Transport>,
    tree: Tree,
    seq: u64,
    browsers: usize,
    status: String,
    state: TreeViewState<NodeId>,
    selected: Vec<NodeId>,
    search: String,
    search_filter: search::Filter,
    search_cache: Option<search::Results>,
    /// Backup copies on the server, once asked for; and where they are.
    backups: Option<(Vec<treetabs_core::backups::BackupInfo>, String)>,
    /// Readable names of browser profiles, by profile id.
    profile_names: std::collections::BTreeMap<String, String>,
    /// Show only the windows of this profile (None: all).
    profile_filter: Option<String>,
    /// Tree version and connection the window title was made for.
    title_for: Option<(u64, bool)>,
    /// Store option: trash items older than this are removed (0 = never).
    trash_days: u32,
    next_req: u64,
    pending: HashMap<u64, EditKind>,
    undo: Vec<Vec<Op>>,
    redo: Vec<Vec<Op>>,
    /// Text fields of the details panel, committed on Enter / focus loss.
    edit_buf: Option<EditBuffer>,
    show_settings: bool,
    settings: Settings,
    settings_applied: bool,
    renaming: Renaming,
    /// A command waiting for "are you sure?".
    confirm: Option<(String, Command)>,
    /// A file to import once the tree has arrived (desktop app argument).
    pending_import: Option<PathBuf>,
    export_format: treetabs_core::export::Format,
    /// Where the server keeps the tree, and the folder typed to move it to.
    store_dir: String,
    move_to: String,
    /// Tab shown in the settings window.
    settings_tab: SettingsTab,
    /// Ids of the tree views: shortcuts work while they have focus.
    tree_ids: [egui::Id; 2],
    /// Collapse changes sent but not yet echoed (don't send them twice).
    collapsing: std::collections::HashSet<NodeId>,
}

impl App {
    pub fn new(conn: Box<dyn Transport>, import: Option<PathBuf>) -> Self {
        let settings = conn
            .load_settings()
            .map(|s| Settings::parse(&s))
            .unwrap_or_default();
        i18n::set(settings.language.unwrap_or_else(i18n::system));
        let status = conn.describe();
        App {
            conn,
            tree: Tree::default(),
            seq: 0,
            browsers: 0,
            status,
            state: TreeViewState::default(),
            selected: Vec::new(),
            search: String::new(),
            search_filter: search::Filter::All,
            search_cache: None,
            backups: None,
            trash_days: 0,
            title_for: None,
            profile_names: Default::default(),
            profile_filter: None,
            next_req: 1,
            pending: HashMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            edit_buf: None,
            show_settings: false,
            settings,
            settings_applied: false,
            renaming: None,
            confirm: None,
            pending_import: import,
            export_format: treetabs_core::export::Format::Treetabs,
            store_dir: String::new(),
            move_to: String::new(),
            settings_tab: SettingsTab::Interface,
            tree_ids: [egui::Id::NULL; 2],
            collapsing: Default::default(),
        }
    }

    fn reconnect(&mut self) {
        self.pending.clear();
        self.status = match self.conn.reconnect() {
            Ok(()) => self.conn.describe(),
            Err(e) => trf("Not connected: {error}", &[("error", &e)]),
        };
    }

    fn send(&mut self, msg: ClientMsg) {
        if let Err(e) = self.conn.send(&msg) {
            self.status = trf("Lost connection: {error}", &[("error", &e)]);
        }
    }

    fn edit(&mut self, ops: Vec<Op>, kind: EditKind) {
        if ops.is_empty() {
            return;
        }
        let req = self.next_req;
        self.next_req += 1;
        self.pending.insert(req, kind);
        self.send(ClientMsg::Edit { req, ops });
    }

    /// Process everything the server sent since the last frame.
    fn poll(&mut self) {
        let p = self.conn.poll();
        for m in p.msgs {
            self.server_msg(m);
        }
        if p.lost {
            // The process running the server quit: take over or reconnect.
            self.reconnect();
        }
    }

    /// Files picked for import, and the desktop app's import argument once
    /// the tree is here.
    fn poll_files(&mut self) {
        if let Some((name, data)) = self.conn.take_file() {
            self.import_file(&name, &data);
        }
        if self.seq > 0
            && let Some(path) = self.pending_import.take()
        {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            match treetabs_core::load(&path) {
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
    }

    fn server_msg(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Snapshot { seq, tree } => {
                log::info!("snapshot: {} nodes", tree.count());
                // Connected (again): older connection messages are stale.
                self.status = self.conn.describe();
                self.tree = *tree;
                // New nodes made here must not reuse the server's ids.
                self.tree.bump_ids();
                self.seq = seq;
                self.selected
                    .retain(|id| self.tree.root.find(*id).is_some());
            }
            ServerMsg::Applied { seq, ops } => {
                for op in &ops {
                    if let Err(e) = self.tree.apply(op) {
                        // Replica out of step: ask for a fresh copy.
                        log::warn!("replica out of sync ({e}), reconnecting");
                        self.reconnect();
                        return;
                    }
                }
                self.seq = seq;
                self.collapsing.clear();
            }
            ServerMsg::EditResult { req, inverse, err } => {
                let kind = self.pending.remove(&req).unwrap_or(EditKind::Quiet);
                if let Some(e) = err {
                    self.status = trf("Edit refused: {error}", &[("error", &e)]);
                    return;
                }
                match kind {
                    EditKind::Normal => {
                        self.undo.push(inverse);
                        if self.undo.len() > UNDO_LIMIT {
                            self.undo.remove(0);
                        }
                        self.redo.clear();
                    }
                    EditKind::Undo => self.redo.push(inverse),
                    EditKind::Redo => self.undo.push(inverse),
                    EditKind::Quiet => {}
                }
            }
            ServerMsg::StoreOptions { trash_days, dir } => {
                self.trash_days = trash_days;
                if self.move_to.is_empty() || self.move_to == self.store_dir {
                    self.move_to = dir.clone();
                }
                self.store_dir = dir;
            }
            ServerMsg::StoreMoved { dir } => {
                self.status = trf("Moved the tree to {dir}; reconnecting…", &[("dir", &dir)]);
                self.conn.store_moved(&dir);
                self.store_dir = dir.clone();
                self.move_to = dir;
            }
            ServerMsg::Profiles { names } => self.profile_names = names,
            ServerMsg::Backups { list, dir, message } => {
                self.backups = Some((list, dir));
                if let Some(m) = message {
                    self.status = m;
                }
            }
            ServerMsg::BackupTree { name, tree, err } => match (tree, err) {
                (Some(t), _) => {
                    self.add_copy(t.root, &trf("Restored from {name}", &[("name", &name)]), 0)
                }
                (None, e) => {
                    self.status = trf(
                        "Cannot read {name}: {error}",
                        &[("name", &name), ("error", &e.unwrap_or_default())],
                    )
                }
            },
            ServerMsg::Status { browsers, message } => {
                self.browsers = browsers;
                if !message.is_empty() {
                    self.status = message;
                }
            }
            ServerMsg::Call { .. } | ServerMsg::Resync => {}
        }
    }

    fn save_settings(&mut self) {
        let json = self.settings.to_json();
        self.conn.save_settings(&json);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}
