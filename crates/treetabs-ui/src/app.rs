//! Tree window: a live view of the server's tree.
//!
//! The GUI keeps a replica of the tree, fed by the server's snapshot and op
//! stream. Edits are sent as ops; the tree only changes when the server
//! echoes them back, so the GUI, the browser and other viewers always agree.

use crate::Transport;
use crate::i18n::{self, tr, trf, trl};
use crate::search;
use crate::settings::{PanelTab, Settings};
use eframe::egui::{self, Color32, RichText};
use egui_ltreeview::{Action, DirPosition, NodeBuilder, TreeView, TreeViewBuilder, TreeViewState};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use treetabs_core::protocol::{ClientMsg, ServerMsg};
use treetabs_core::{Node, NodeId, NodeKind, Op, Position, Tree};

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

/// Requests from inside the tree (menus, hover buttons), handled after drawing.
#[derive(Clone)]
enum Command {
    Open(NodeId),
    /// Close in the browser (nodes stay as saved). `branch`: everything open
    /// below too.
    Close {
        ids: Vec<NodeId>,
        branch: bool,
    },
    /// Move to the trash. Without `branch`, children move up into the
    /// node's place.
    Trash {
        ids: Vec<NodeId>,
        branch: bool,
    },
    StartRename(NodeId),
    Rename(NodeId, String),
    CancelRename,
    AddNote {
        at: NodeId,
        inside: bool,
    },
    AddGroup {
        at: NodeId,
        inside: bool,
    },
    /// Expand or collapse a node and everything below it.
    Expand {
        id: NodeId,
        open: bool,
    },
    Copy(String),
    /// Remove everything in the trash for good (asks first).
    EmptyTrash,
    /// Copy the store to this folder and use it from now on (asks first).
    MoveStore(String),
    /// Unload open tabs (free memory, keep them open). `branch`: all below.
    Unload {
        ids: Vec<NodeId>,
        branch: bool,
    },
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
    edit_buf: Option<(NodeId, String, String)>,
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
    /// Tab of the settings window: 0 interface, 1 storage, 2 about.
    settings_tab: u8,
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
            settings_tab: 0,
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

    fn undo(&mut self) {
        if let Some(step) = self.undo.pop() {
            let step = self.retarget(step);
            self.edit(step, EditKind::Undo);
        }
    }

    fn redo(&mut self) {
        if let Some(step) = self.redo.pop() {
            let step = self.retarget(step);
            self.edit(step, EditKind::Redo);
        }
    }

    /// Things may have changed since an undo step was recorded (the browser
    /// closed a window, …). Put nodes whose old parent is gone at the end of
    /// the tree instead of failing.
    fn retarget(&self, step: Vec<Op>) -> Vec<Op> {
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
    fn top_level(&self, ids: &[NodeId]) -> Vec<NodeId> {
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
    fn request(&mut self, cmd: Command) {
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
        if let Command::Trash { ids, branch: true } = &cmd {
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

    fn command(&mut self, cmd: Command) {
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
            Command::Close { ids, branch } => {
                let mut open = Vec::new();
                for id in &ids {
                    let Some(n) = self.tree.root.find(*id) else {
                        continue;
                    };
                    if branch {
                        n.walk(&mut |m, _| {
                            if matches!(m.kind, NodeKind::Tab | NodeKind::Window) {
                                open.push(m.id);
                            }
                        });
                    } else if matches!(n.kind, NodeKind::Tab | NodeKind::Window) {
                        open.push(n.id);
                    }
                }
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
            Command::Unload { ids, branch } => {
                let mut tabs = Vec::new();
                for id in &ids {
                    let Some(n) = self.tree.root.find(*id) else {
                        continue;
                    };
                    let mut add = |m: &Node| {
                        if m.kind == NodeKind::Tab && !is_unloaded(m) {
                            tabs.push(m.id);
                        }
                    };
                    if branch {
                        n.walk(&mut |m, _| add(m));
                    } else {
                        add(n);
                    }
                }
                self.status = if tabs.is_empty() {
                    tr("Nothing loaded to unload").into()
                } else {
                    trf(
                        "Unloading {n} tab(s); they stay open and load again when shown",
                        &[("n", &tabs.len())],
                    )
                };
                if !tabs.is_empty() {
                    self.send(ClientMsg::Unload { ids: tabs });
                }
            }
            Command::Trash { ids, branch } => {
                let top = self.top_level(&ids);
                let at = treetabs_core::ops::now_ms();
                let n = top.len();
                let ops = if branch {
                    top.into_iter().map(|id| Op::Trash { id, at }).collect()
                } else {
                    // Children move up into the node's place, then the node
                    // goes. Planned on a copy so later ids see earlier moves.
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
                            let _ = sim.apply(op);
                        }
                        ops.extend(step);
                    }
                    ops
                };
                self.edit(ops, EditKind::Normal);
                self.selected.clear();
                self.status = if branch {
                    trf(
                        "Moved {n} branch(es) to the trash (Ctrl+Z to undo)",
                        &[("n", &n)],
                    )
                } else {
                    trf(
                        "Moved {n} node(s) to the trash (Ctrl+Z to undo)",
                        &[("n", &n)],
                    )
                };
            }
            Command::StartRename(id) => {
                if let Some(n) = self.tree.root.find(id) {
                    let text = match n.kind {
                        NodeKind::Note => n.note.clone().unwrap_or_default(),
                        _ => i18n::title(n),
                    };
                    self.renaming = Some((id, text, false));
                }
            }
            Command::CancelRename => self.renaming = None,
            Command::Rename(id, text) => {
                self.renaming = None;
                let Some(n) = self.tree.root.find(id) else {
                    return;
                };
                let mut f = n.fields();
                let text = text.trim().to_owned();
                if n.kind == NodeKind::Note {
                    f.note = Some(text);
                } else if text.is_empty() || Some(&text) == n.title.as_ref() {
                    // Back to the page's own title.
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
            Command::AddNote { at, inside } | Command::AddGroup { at, inside } => {
                let note = matches!(cmd, Command::AddNote { .. });
                let mut n = Node::new(if note {
                    NodeKind::Note
                } else {
                    NodeKind::Group
                });
                if note {
                    n.note = Some(tr("New note").into());
                } else {
                    n.custom_title = Some(tr("New group").into());
                }
                let id = n.id;
                let (parent, pos) = if inside {
                    (at, Position::LastChild)
                } else {
                    let Some((parent, _)) = self.tree.root.locate(at) else {
                        return;
                    };
                    (parent, Position::After(at))
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
                if inside {
                    self.state.set_openness(at, true);
                }
                self.selected = vec![id];
                self.state.set_one_selected(id);
                // Name it right away.
                let label = if note {
                    tr("New note")
                } else {
                    tr("New group")
                };
                self.renaming = Some((id, label.into(), false));
            }
            Command::Expand { id, open } => {
                if let Some(n) = self.tree.root.find(id) {
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
            Command::Copy(_) => {} // handled where the egui context is
        }
    }

    fn handle_actions(&mut self, actions: Vec<Action<NodeId>>, ui: &mut egui::Ui) {
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
    fn sync_collapsed(&mut self, visible: &[(NodeId, bool)]) {
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

    /// Import a picked file: treetabs or Tabs Outliner JSON, added at the
    /// end as a group.
    fn import_file(&mut self, name: &str, data: &[u8]) {
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

    fn export_tree(&mut self) {
        let f = self.export_format;
        let date = treetabs_core::backups::local_time(treetabs_core::ops::now_ms() as u64)[..10]
            .to_owned();
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
    fn add_copy(&mut self, mut root: Node, title: &str, warnings: usize) {
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

    fn backups_panel(&mut self, ui: &mut egui::Ui) {
        use treetabs_core::export::Format;
        ui.strong(tr("Export"));
        // One per line: side by side they would widen the panel.
        {
            let name = |f: Format| match f {
                Format::Treetabs => tr("treetabs JSON (complete)"),
                Format::TabsOutliner => tr("Tabs Outliner JSON"),
                Format::BookmarksHtml => tr("Bookmarks HTML (for any browser)"),
                Format::Markdown => tr("Markdown list"),
            };
            egui::ComboBox::from_id_salt("export-format")
                .selected_text(name(self.export_format))
                .show_ui(ui, |ui| {
                    for f in Format::ALL {
                        ui.selectable_value(&mut self.export_format, f, name(f));
                    }
                });
        }
        if ui.button(tr("Export…")).clicked() {
            self.export_tree();
        }
        ui.add_space(6.0);
        ui.strong(tr("Import"));
        para(
            ui,
            trl(
                "A treetabs or Tabs Outliner JSON file. It is added at the end as a group of saved tabs; nothing is replaced, and Ctrl+Z takes it out again.",
            ),
            true,
        );
        if ui.button(tr("Import file…")).clicked() {
            self.conn.open_file();
        }
        ui.separator();
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
            true,
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button(format!("💾 {}", tr("Save a copy now"))).clicked() {
                self.send(ClientMsg::ExportNow);
            }
            if ui.button(format!("⟳ {}", tr("Refresh"))).clicked() {
                self.send(ClientMsg::ListBackups);
            }
        });
        let Some((list, dir)) = self.backups.clone() else {
            return;
        };
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
            (
                treetabs_core::backups::BackupKind::Daily,
                tr("Daily copies"),
            ),
            (
                treetabs_core::backups::BackupKind::Snapshot,
                tr("Automatic snapshots"),
            ),
        ] {
            let items: Vec<_> = list.iter().filter(|b| b.kind == kind).collect();
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
        if let Some(name) = read {
            self.status = trf("Reading {name}…", &[("name", &name)]);
            self.send(ClientMsg::ReadBackup { name });
        }
    }

    /// Top bar. Left: frequent actions (undo, redo, search). Right, in a
    /// different style: the side panel tabs and settings, which open views
    /// rather than change the tree.
    fn toolbar(&mut self, ui: &mut egui::Ui, narrow: bool) {
        // Wrap whole buttons onto the next line, never the text inside them.
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        line(ui, |ui| {
            self.history_buttons(ui);
            if narrow {
                ui.with_layout(far(), |ui| self.view_buttons(ui, narrow));
            } else {
                ui.separator();
                self.search_box(ui, 300.0);
                ui.with_layout(far(), |ui| self.view_buttons(ui, narrow));
            }
        });
        if narrow {
            line(ui, |ui| {
                let w = ui.available_width() - 36.0;
                self.search_box(ui, w);
            });
        }
    }

    fn search_box(&mut self, ui: &mut egui::Ui, width: f32) {
        let filtered = self.search_filter != search::Filter::All;
        let narrowed = filtered || self.profile_filter.is_some();
        let hint = format!("🔍 {}", tr("Search: words in title, URL, note"));
        let width = (width - if narrowed { 130.0 } else { 30.0 }).max(80.0);
        // Also right-to-left typing (Arabic): see bidi::line_edit.
        let r = crate::bidi::line_edit(ui, egui::Id::new("search-box"), &mut self.search, |e| {
            e.hint_text(hint).desired_width(width)
        });
        // Esc takes the focus away from a text field first: check both.
        if (r.has_focus() || r.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.search.clear();
            self.search_filter = search::Filter::All;
            self.profile_filter = None;
        }
        let profiles = profile_labels(&self.tree, &self.profile_names);
        let mut parts: Vec<String> = Vec::new();
        if filtered {
            parts.push(self.search_filter.label().to_owned());
        }
        if let Some(l) = self.profile_filter.as_ref().and_then(|p| profiles.get(p)) {
            parts.push(l.clone());
        }
        let label = if parts.is_empty() {
            "⏷".to_owned()
        } else {
            format!("⏷ {}", parts.join(", "))
        };
        ui.menu_button(label, |ui| {
            ui.label(tr("Show"));
            for f in search::Filter::ALL {
                if ui
                    .radio_value(&mut self.search_filter, f, f.label())
                    .clicked()
                {
                    ui.close();
                }
            }
            if profiles.len() > 1 {
                ui.separator();
                ui.label(tr("Browser profile"));
                if ui.radio(self.profile_filter.is_none(), tr("All")).clicked() {
                    self.profile_filter = None;
                    ui.close();
                }
                for (id, l) in &profiles {
                    if ui
                        .radio(self.profile_filter.as_ref() == Some(id), l)
                        .clicked()
                    {
                        self.profile_filter = Some(id.clone());
                        ui.close();
                    }
                }
            }
        })
        .response
        .on_hover_text(tr("Filter: all, open, saved, duplicates"));
        if (!self.search.is_empty() || narrowed)
            && ui
                .button("✖")
                .on_hover_text(tr("Clear the search and filters (Esc)"))
                .clicked()
        {
            self.search.clear();
            self.search_filter = search::Filter::All;
            self.profile_filter = None;
        }
    }

    fn history_buttons(&mut self, ui: &mut egui::Ui) {
        let size = ui.text_style_height(&egui::TextStyle::Button) * 1.15;
        let big = |t: &str| RichText::new(t).size(size);
        if ui
            .add_enabled(!self.undo.is_empty(), egui::Button::new(big("⟲")))
            .on_hover_text(tr("Undo (Ctrl+Z)"))
            .clicked()
        {
            self.undo();
        }
        if ui
            .add_enabled(!self.redo.is_empty(), egui::Button::new(big("⟳")))
            .on_hover_text(tr("Redo (Ctrl+Y)"))
            .clicked()
        {
            self.redo();
        }
    }

    /// Right-to-left: settings, then the panel tabs.
    fn view_buttons(&mut self, ui: &mut egui::Ui, narrow: bool) {
        if ui
            .add(
                egui::Button::new("⚙")
                    .selected(self.show_settings)
                    .frame(false),
            )
            .on_hover_text(tr("Settings"))
            .clicked()
        {
            self.show_settings = !self.show_settings;
        }
        ui.separator();
        let trash = self.tree.trash.len();
        let tabs = [
            (
                PanelTab::Trash,
                if narrow {
                    format!("🗑 {trash}")
                } else {
                    format!("🗑 {} {trash}", tr("Trash"))
                },
                tr("Trash: deleted nodes, restore them from here"),
            ),
            (
                PanelTab::Backups,
                if narrow {
                    "💾".to_owned()
                } else {
                    format!("💾 {}", tr("Data"))
                },
                tr("Import, export and backups of the tree"),
            ),
            (
                PanelTab::Details,
                if narrow {
                    "ℹ".to_owned()
                } else {
                    format!("ℹ {}", tr("Details"))
                },
                tr("Details of the selected node"),
            ),
        ];
        // Segmented control: one tab at a time; the active one hides the panel.
        let mut pick = None;
        egui::Frame::new()
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(4.0)
            .inner_margin(2.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (tab, label, tip) in tabs {
                    let active = self.settings.panel_open && self.settings.panel_tab == tab;
                    if ui
                        .add(egui::Button::selectable(active, label))
                        .on_hover_text(tip)
                        .clicked()
                    {
                        pick = Some(if active { None } else { Some(tab) });
                    }
                }
            });
        if let Some(tab) = pick {
            self.show_panel(tab);
        }
    }

    /// Open the side panel at `tab`, or close it.
    fn show_panel(&mut self, tab: Option<PanelTab>) {
        match tab {
            Some(t) => {
                self.settings.panel_open = true;
                self.settings.panel_tab = t;
            }
            None => self.settings.panel_open = false,
        }
        self.save_settings();
    }

    /// Title row of the side panel, with its close button.
    fn panel_header(&mut self, ui: &mut egui::Ui) {
        line(ui, |ui| {
            let title = match self.settings.panel_tab {
                PanelTab::Details => tr("Details").to_owned(),
                PanelTab::Trash => format!("{} ({})", tr("Trash"), self.tree.trash.len()),
                PanelTab::Backups => tr("Data").to_owned(),
            };
            ui.strong(title);
            ui.with_layout(far(), |ui| {
                if ui
                    .add(egui::Button::new("✖").frame(false))
                    .on_hover_text(tr("Hide the panel"))
                    .clicked()
                {
                    self.show_panel(None);
                }
            });
        });
        ui.separator();
    }

    fn save_settings(&mut self) {
        let json = self.settings.to_json();
        self.conn.save_settings(&json);
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(&id) = self.selected.last() else {
            ui.weak(tr("Nothing selected"));
            return;
        };
        if self.selected.len() > 1 {
            ui.label(format!("{} nodes selected", self.selected.len()));
            ui.horizontal_wrapped(|ui| {
                let ids = self.selected.clone();
                if ui.button(tr("Close all")).clicked() {
                    self.request(Command::Close {
                        ids: ids.clone(),
                        branch: true,
                    });
                }
                if ui.button(tr("Move to trash")).clicked() {
                    self.request(Command::Trash { ids, branch: true });
                }
            });
            return;
        }
        let Some(node) = self.tree.root.find(id) else {
            return;
        };
        let node = node.clone();
        ui.heading(if is_unloaded(&node) {
            format!("{} · {}", i18n::kind_name(&node.kind), tr("unloaded"))
        } else {
            i18n::kind_name(&node.kind).to_owned()
        });
        let rtl = i18n::is_rtl();
        // Label, then value (mirrored when right to left).
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
                        crate::bidi::paragraph(ui, t, false, false);
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
                    &[("n", &node.children.len()), ("all", &(node.count() - 1))],
                );
                pair(ui, tr("Children"), &mut |ui| {
                    ui.label(&count);
                });
            });
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
                    branch: false,
                });
            }
        });
        ui.separator();
        if self.edit_buf.as_ref().is_none_or(|b| b.0 != id) {
            self.edit_buf = Some((
                id,
                node.custom_title.clone().unwrap_or_default(),
                node.note.clone().unwrap_or_default(),
            ));
        }
        let (_, mut custom, mut note) = self.edit_buf.clone().expect("set above");
        ui.label(tr("Custom title"));
        let mut commit =
            crate::bidi::line_edit(ui, egui::Id::new("custom-title"), &mut custom, |e| e)
                .lost_focus();
        if node.kind == NodeKind::Note {
            ui.label(tr("Note"));
            commit |= ui.text_edit_multiline(&mut note).lost_focus();
        }
        self.edit_buf = Some((id, custom.clone(), note.clone()));
        let changed = custom != node.custom_title.clone().unwrap_or_default()
            || (node.kind == NodeKind::Note && Some(&note) != node.note.as_ref());
        if commit && changed {
            let mut f = node.fields();
            f.custom_title = (!custom.is_empty()).then_some(custom);
            if node.kind == NodeKind::Note {
                f.note = Some(note);
            }
            self.edit(
                vec![Op::Update {
                    id,
                    fields: Box::new(f),
                }],
                EditKind::Normal,
            );
        }
        ui.separator();
        ui.collapsing(tr("Raw data"), |ui| {
            let mut shown = node.clone();
            shown.children.clear();
            let text = serde_json::to_string_pretty(&shown).unwrap_or_default();
            ui.add(egui::Label::new(RichText::new(text).monospace()).wrap());
        });
    }

    fn trash_panel(&mut self, ui: &mut egui::Ui) {
        para(
            ui,
            trl("Newest first. Restore puts a node back where it was."),
            true,
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

    /// Settings: a window with its own fixed text size and no title bar
    /// (a title would grow with the text size and push the controls down).
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let screen = ctx.content_rect();
        let mut close = false;
        egui::Window::new(tr("Settings"))
            .title_bar(false)
            .resizable(false)
            // Pinned under the gear button, so it is in the same place at any scale.
            .anchor(
                if i18n::is_rtl() {
                    egui::Align2::LEFT_TOP
                } else {
                    egui::Align2::RIGHT_TOP
                },
                [if i18n::is_rtl() { 8.0 } else { -8.0 }, 36.0],
            )
            .max_width(screen.width() * 0.94)
            .max_height(screen.height() * 0.85)
            .show(ctx, |ui| {
                crate::settings::dialog_style(ui.style_mut());
                line(ui, |ui| {
                    ui.strong(tr("Settings"));
                    ui.with_layout(far(), |ui| {
                        close = ui
                            .add(egui::Button::new("✖").frame(false))
                            .on_hover_text(tr("Close (Esc)"))
                            .clicked();
                    });
                });
                ui.separator();
                line(ui, |ui| {
                    for (i, name) in [tr("Interface"), tr("Storage"), tr("About")]
                        .into_iter()
                        .enumerate()
                    {
                        ui.selectable_value(&mut self.settings_tab, i as u8, name);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([true, true])
                    .show(ui, |ui| match self.settings_tab {
                        1 => self.storage_ui(ui),
                        2 => self.about_ui(ui),
                        _ => {
                            if self.settings.ui(ui) {
                                self.settings.apply(ctx);
                                self.save_settings();
                            }
                        }
                    });
            });
        if close || (self.confirm.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
            self.show_settings = false;
        }
    }

    /// Settings > Storage: where the tree is, and moving it.
    fn storage_ui(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(340.0);
        ui.label(tr("The tree is stored in"));
        let dir = if self.store_dir.is_empty() {
            "…".to_owned()
        } else {
            self.store_dir.clone()
        };
        ui.add(egui::Label::new(RichText::new(&dir).monospace()).selectable(true));
        if self.store_dir.contains("/snap/") {
            para(
                ui,
                trl("A snap browser can only reach folders inside its own snap folder."),
                true,
            );
        }
        ui.add_space(6.0);
        ui.label(tr("Move to another folder"));
        line(ui, |ui| {
            crate::bidi::line_edit(ui, egui::Id::new("move-store"), &mut self.move_to, |e| {
                e.desired_width(260.0).font(egui::TextStyle::Monospace)
            });
            let target = self.move_to.trim().to_owned();
            let ok = !target.is_empty() && target != self.store_dir && self.conn.connected();
            if ui.add_enabled(ok, egui::Button::new(tr("Move…"))).clicked() {
                self.request(Command::MoveStore(target));
            }
        });
        para(
            ui,
            trl(
                "Everything in the current folder is copied to the new one; the old folder stays until you delete it. The companion program restarts.",
            ),
            true,
        );
    }

    /// Settings > About.
    fn about_ui(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(340.0);
        ui.vertical_centered(|ui| {
            ui.heading("treetabs");
            ui.label(trf("Version {v}", &[("v", &env!("CARGO_PKG_VERSION"))]));
        });
        ui.add_space(6.0);
        para(
            ui,
            trl(
                "treetabs keeps all your browser windows and tabs in one tree: the open ones and the ones you closed but want to keep. Every change is saved on your computer before it counts, a copy is made every day, and the tree is mirrored to your bookmarks.",
            ),
            false,
        );
        ui.add_space(4.0);
        para(
            ui,
            trl("Your data stays on your computer. Nothing is sent anywhere."),
            false,
        );
        ui.add_space(4.0);
        ui.label(tr("The tree is stored in"));
        ui.add(egui::Label::new(RichText::new(&self.store_dir).monospace()).selectable(true));
        ui.add_space(4.0);
        para(
            ui,
            trl(
                "Free software (MIT license). Fonts: Noto Sans CJK SC and Noto Sans Arabic (SIL Open Font License).",
            ),
            true,
        );
    }

    fn confirm_window(&mut self, ctx: &egui::Context) {
        let Some((text, cmd)) = &self.confirm else {
            return;
        };
        let text = text.clone();
        let (note, button) = match cmd {
            Command::EmptyTrash => (
                trl("A copy that still has them is saved to Backups first."),
                tr("Empty trash"),
            ),
            Command::MoveStore(_) => (
                trl(
                    "Everything in the current folder is copied to the new one; the old folder stays until you delete it. The companion program restarts.",
                ),
                tr("Move"),
            ),
            _ => (
                trl("Everything goes to the trash and can be restored."),
                tr("Move to trash"),
            ),
        };
        let mut answer = None;
        egui::Window::new(tr("Confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                para(ui, &text, false);
                para(ui, note, true);
                line(ui, |ui| {
                    if ui.button(button).clicked() {
                        answer = Some(true);
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => {
                if let Some((_, cmd)) = self.confirm.take() {
                    self.command(cmd);
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

/// A row of widgets; right to left in a right-to-left interface.
fn line<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let layout = if i18n::is_rtl() {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    // One row high, like `ui.horizontal` (a bare layout would take all the
    // height there is, and a panel would grow every frame).
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, layout, add)
}

/// Layout for the far end of a row (right side, or left when RTL).
fn far() -> egui::Layout {
    if i18n::is_rtl() {
        egui::Layout::left_to_right(egui::Align::Center)
    } else {
        egui::Layout::right_to_left(egui::Align::Center)
    }
}

/// A wrapped paragraph of translated text (`text` in reading order).
fn para(ui: &mut egui::Ui, text: &str, weak: bool) {
    crate::bidi::paragraph(ui, text, i18n::is_rtl(), weak);
}

/// "YYYY-MM-DD HH:MM", local time.
fn format_time(ms: u64) -> String {
    treetabs_core::backups::local_time(ms)
}

fn format_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1 << 20)),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

fn kind_style(n: &Node) -> (&'static str, Option<Color32>) {
    match n.kind {
        NodeKind::Window => ("▣", Some(Color32::from_rgb(90, 160, 255))),
        NodeKind::SavedWindow | NodeKind::WaitingWindow => ("□", None),
        NodeKind::Tab => ("○", Some(Color32::from_rgb(90, 200, 120))),
        NodeKind::SavedTab | NodeKind::WaitingTab | NodeKind::AttachWaitingTab => ("○", None),
        NodeKind::Group => ("📁", Some(Color32::from_rgb(220, 170, 60))),
        NodeKind::Note => ("📝", Some(Color32::from_rgb(200, 140, 220))),
        NodeKind::Separator => ("─", None),
        NodeKind::Session => ("◆", None),
        NodeKind::Other(_) => ("?", Some(Color32::RED)),
    }
}

/// Row text style as chosen in the settings: (bold, dim, italic).
fn label_style(n: &Node, s: &Settings) -> (bool, bool, bool) {
    let open = matches!(n.kind, NodeKind::Tab | NodeKind::Window);
    let saved = matches!(n.kind, NodeKind::SavedTab | NodeKind::SavedWindow);
    (
        open && s.bold_open,
        saved && s.dim_saved,
        n.custom_title.is_some() && s.italic_renamed,
    )
}

fn label_text(n: &Node, s: &Settings) -> RichText {
    let mut t = RichText::new(i18n::shown_title(n));
    let (bold, dim, italic) = label_style(n, s);
    if bold {
        t = t.strong();
    }
    if dim {
        t = t.weak();
    }
    if italic {
        t = t.italics();
    }
    t
}

/// Browser profile a top-level window belongs to.
fn profile_of(n: &Node) -> Option<&str> {
    matches!(
        n.kind,
        NodeKind::Window | NodeKind::SavedWindow | NodeKind::WaitingWindow
    )
    .then(|| n.data.get("browser")?.as_str())
    .flatten()
}

/// Labels for the profiles that have windows in the tree: the browser's
/// name, plus a short id when two profiles share a name.
fn profile_labels(
    tree: &Tree,
    names: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let ids: std::collections::BTreeSet<&str> =
        tree.root.children.iter().filter_map(profile_of).collect();
    let name = |id: &str| {
        names
            .get(id)
            .cloned()
            .unwrap_or_else(|| tr("Browser").to_owned())
    };
    ids.iter()
        .map(|&id| {
            let n = name(id);
            let shared = ids.iter().filter(|&&o| name(o) == n).count() > 1;
            let label = if shared {
                format!("{n} {}", id.chars().take(4).collect::<String>())
            } else {
                n
            };
            (id.to_owned(), label)
        })
        .collect()
}

/// An open tab the browser has unloaded (discarded) to save memory.
fn is_unloaded(n: &Node) -> bool {
    n.kind == NodeKind::Tab && n.data.get("discarded").is_some_and(|v| v == true)
}

fn has_open(n: &Node) -> bool {
    n.find_by(|m| matches!(m.kind, NodeKind::Tab | NodeKind::Window))
        .is_some()
}

/// The right-click menu of a node.
fn node_menu(ui: &mut egui::Ui, n: &Node, cmds: &RefCell<Vec<Command>>) {
    if i18n::is_rtl() {
        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Max), |ui| {
            node_menu_items(ui, n, cmds)
        });
    } else {
        node_menu_items(ui, n, cmds);
    }
}

fn node_menu_items(ui: &mut egui::Ui, n: &Node, cmds: &RefCell<Vec<Command>>) {
    let id = n.id;
    let is_open = matches!(n.kind, NodeKind::Tab | NodeKind::Window);
    let kids = !n.children.is_empty();
    let push = |ui: &mut egui::Ui, c: Command| {
        cmds.borrow_mut().push(c);
        ui.close();
    };
    if (n.url.is_some() || kids)
        && ui
            .button(if is_open {
                tr("Focus in browser")
            } else {
                tr("Open in browser")
            })
            .clicked()
    {
        push(ui, Command::Open(id));
    }
    if ui.button(format!("{}    F2", tr("Rename…"))).clicked() {
        push(ui, Command::StartRename(id));
    }
    ui.separator();
    if is_open && ui.button(tr("Close (keep saved)")).clicked() {
        push(
            ui,
            Command::Close {
                ids: vec![id],
                branch: false,
            },
        );
    }
    if kids && has_open(n) && ui.button(tr("Close branch (keep saved)")).clicked() {
        push(
            ui,
            Command::Close {
                ids: vec![id],
                branch: true,
            },
        );
    }
    let loaded = |m: &Node| m.kind == NodeKind::Tab && !is_unloaded(m);
    if loaded(n)
        && ui
            .button(tr("Unload (free memory)"))
            .on_hover_text(tr("The tab stays open and loads again when you go to it"))
            .clicked()
    {
        push(
            ui,
            Command::Unload {
                ids: vec![id],
                branch: false,
            },
        );
    }
    if kids && n.find_by(|m| loaded(m)).is_some() && ui.button(tr("Unload branch")).clicked() {
        push(
            ui,
            Command::Unload {
                ids: vec![id],
                branch: true,
            },
        );
    }
    if kids && ui.button(tr("Delete, keep children")).clicked() {
        push(
            ui,
            Command::Trash {
                ids: vec![id],
                branch: false,
            },
        );
    }
    if ui
        .button(format!(
            "{}    Del",
            if kids {
                tr("Delete branch")
            } else {
                tr("Delete")
            }
        ))
        .clicked()
    {
        push(
            ui,
            Command::Trash {
                ids: vec![id],
                branch: true,
            },
        );
    }
    ui.separator();
    ui.menu_button(tr("Add"), |ui| {
        if ui.button(tr("Note after")).clicked() {
            push(
                ui,
                Command::AddNote {
                    at: id,
                    inside: false,
                },
            );
        }
        if ui.button(tr("Note inside")).clicked() {
            push(
                ui,
                Command::AddNote {
                    at: id,
                    inside: true,
                },
            );
        }
        if ui.button(tr("Group after")).clicked() {
            push(
                ui,
                Command::AddGroup {
                    at: id,
                    inside: false,
                },
            );
        }
        if ui.button(tr("Group inside")).clicked() {
            push(
                ui,
                Command::AddGroup {
                    at: id,
                    inside: true,
                },
            );
        }
    });
    if kids {
        if ui.button(tr("Expand all below")).clicked() {
            push(ui, Command::Expand { id, open: true });
        }
        if ui.button(tr("Collapse all below")).clicked() {
            push(ui, Command::Expand { id, open: false });
        }
    }
    ui.separator();
    if let Some(u) = &n.url
        && ui.button(tr("Copy URL")).clicked()
    {
        push(ui, Command::Copy(u.clone()));
    }
    if ui.button(tr("Copy title")).clicked() {
        push(ui, Command::Copy(i18n::title(n)));
    }
}

/// The text part of a row: label, inline rename field, hover buttons.
/// A search result's context for its row.
struct Found<'a> {
    words: &'a [String],
    parents: &'a str,
}

fn row_label(ui: &mut egui::Ui, n: &Node, rows: &Rows<'_>, found: Option<&Found<'_>>) {
    let id = n.id;
    {
        let mut r = rows.renaming.borrow_mut();
        if let Some((rid, text, focused)) = r.as_mut()
            && *rid == id
        {
            let width = ui.available_width().max(240.0);
            let resp = crate::bidi::line_edit(ui, ui.id().with(("rename", id)), text, |e| {
                e.desired_width(width)
            });
            if !*focused {
                resp.request_focus();
                *focused = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                rows.cmds.borrow_mut().push(Command::CancelRename);
            } else if resp.lost_focus() {
                rows.cmds
                    .borrow_mut()
                    .push(Command::Rename(id, text.clone()));
            }
            return;
        }
    }
    match found {
        None => {
            ui.add(egui::Label::new(label_text(n, rows.settings)).selectable(false));
            if let Some(labels) = rows.profiles
                && let Some(l) = profile_of(n).and_then(|b| labels.get(b))
            {
                ui.add(egui::Label::new(RichText::new(l).small().weak()).selectable(false));
            }
        }
        Some(f) => {
            // Same look as the tree row, with the matches highlighted.
            let style = ui.style().clone();
            let mut format = egui::TextFormat::simple(
                egui::TextStyle::Body.resolve(&style),
                ui.visuals().text_color(),
            );
            let (bold, dim, italic) = label_style(n, rows.settings);
            if dim {
                format.color = ui.visuals().weak_text_color();
            } else if bold {
                format.color = ui.visuals().strong_text_color();
            }
            format.italics = italic;
            let job = search::highlighted(ui, &i18n::shown_title(n), f.words, format);
            ui.add(egui::Label::new(job).selectable(false));
            if !f.parents.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(trf("in {path}", &[("path", &f.parents)]))
                            .small()
                            .weak(),
                    )
                    .selectable(false),
                );
            }
        }
    }
    if !rows.settings.hover_buttons {
        return;
    }
    let row = ui.max_rect();
    let clip = ui.clip_rect();
    let hovered = ui
        .ctx()
        .pointer_hover_pos()
        .is_some_and(|p| row.y_range().contains(p.y) && clip.contains(p));
    if !hovered {
        return;
    }
    // Pinned just inside the right edge of the visible tree, over the text
    // (on its own background), so long rows don't push them out of view.
    let size = ui.text_style_height(&egui::TextStyle::Body)
        * 0.85
        * rows.settings.row_button_scale.clamp(0.6, 2.5);
    let mut buttons: Vec<(&str, &str, Command)> =
        vec![("✏", tr("Rename (F2)"), Command::StartRename(id))];
    if has_open(n) {
        let tip = if n.children.is_empty() {
            tr("Close, keep saved")
        } else {
            tr("Close branch, keep saved")
        };
        buttons.push((
            "✖",
            tip,
            Command::Close {
                ids: vec![id],
                branch: true,
            },
        ));
    }
    let tip = if n.children.is_empty() {
        tr("Delete")
    } else {
        tr("Delete branch")
    };
    buttons.push((
        "🗑",
        tip,
        Command::Trash {
            ids: vec![id],
            branch: true,
        },
    ));
    let gap = 4.0;
    let width = buttons.len() as f32 * (size + 2.0 * gap) + gap;
    let right = clip.right();
    let area = egui::Rect::from_min_max(
        egui::pos2((right - width).max(row.left()), row.top()),
        egui::pos2(right, row.bottom()),
    );
    let bg = ui.visuals().panel_fill;
    ui.painter().with_clip_rect(clip).rect_filled(area, 4.0, bg);
    let mut bar = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    bar.set_clip_rect(clip.intersect(area));
    bar.spacing_mut().item_spacing.x = gap;
    bar.add_space(gap);
    for (icon, tip, c) in buttons {
        let b = egui::Button::new(RichText::new(icon).size(size))
            .frame(false)
            .min_size(egui::vec2(size + gap, size));
        if bar.add(b).on_hover_text(tip).clicked() {
            rows.cmds.borrow_mut().push(c);
        }
    }
}

/// Site icon when there is one and it has loaded, else the kind glyph.
/// Only rows on screen load anything.
fn node_icon(ui: &mut egui::Ui, n: &Node, rows: &Rows<'_>) {
    let size = rows.settings.icon_size();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    if rows.settings.favicons
        && let Some(uri) = (rows.favicon)(n)
        && let Ok(egui::load::TexturePoll::Ready { texture }) = ui.ctx().try_load_texture(
            &uri,
            egui::TextureOptions::LINEAR,
            egui::SizeHint::default(),
        )
    {
        let open = matches!(n.kind, NodeKind::Tab | NodeKind::Window);
        // Faded for saved tabs, less so with more contrast.
        let k = rows.settings.contrast.clamp(0.0, 1.0);
        let alpha = |base: f32| (base + (255.0 - base) * k * 0.6) as u8;
        let tint = if is_unloaded(n) {
            Color32::from_white_alpha(alpha(170.0))
        } else if open {
            Color32::WHITE
        } else {
            Color32::from_white_alpha(alpha(100.0))
        };
        egui::Image::from_texture(texture)
            .tint(tint)
            .paint_at(ui, rect.shrink(size * 0.06));
        return;
    }
    let (icon, color) = kind_style(n);
    let color = color.unwrap_or_else(|| ui.visuals().weak_text_color());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(size),
        color,
    );
}

fn add_node(
    b: &mut TreeViewBuilder<'_, NodeId>,
    n: &Node,
    rows: &Rows<'_>,
    visible: &mut Vec<(NodeId, bool)>,
) {
    let has_children = !n.children.is_empty();
    let id = n.id;
    let openable = n.url.is_some() || has_children;
    // Every node is a directory so anything can be dropped onto anything,
    // like in Tabs Outliner. Empty ones get a blank closer.
    let top = rows.next_row.get();
    rows.next_row.set(top + rows.row_step);
    let mut nb = NodeBuilder::dir(id)
        .default_open(!n.collapsed)
        .activatable(openable)
        .height(rows.settings.row_height());
    if rows
        .near
        .intersects(egui::Rangef::new(top, top + rows.row_step))
    {
        nb = nb
            .icon(|ui| node_icon(ui, n, rows))
            .label_ui(|ui| row_label(ui, n, rows, None))
            .context_menu(|ui| node_menu(ui, n, rows.cmds));
        if !has_children {
            nb = nb.closer(|_ui, _state| {});
        }
    }
    let open = b.node(nb);
    if has_children {
        visible.push((id, n.collapsed));
    }
    // Skip closed branches entirely: keeps big trees fast.
    if open {
        for c in &n.children {
            add_node(b, c, rows, visible);
        }
    }
    b.close_dir();
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
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
        let server_here = self.conn.is_server();
        let connected = self.conn.connected();
        // Counting a big tree every frame costs: only when it changed.
        if self.title_for != Some((self.seq, connected)) {
            self.title_for = Some((self.seq, connected));
            let n = self.tree.count() - 1;
            let title = if connected {
                trf("treetabs — {n} nodes", &[("n", &n)])
            } else {
                trf("treetabs — {n} nodes (offline)", &[("n", &n)])
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }

        // Each key event carries the modifiers held when it happened. The
        // frame's own modifier state is at its end: if Ctrl was let go in
        // the same frame, Ctrl+Z would read as a plain Z.
        let (undo, redo, delete, delete_one, rename) = ctx.input(|i| {
            let pressed = |key: egui::Key, want: fn(&egui::Modifiers) -> bool| {
                i.events.iter().any(|e| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && want(modifiers)))
            };
            (
                pressed(egui::Key::Z, |m| m.command && !m.shift),
                pressed(egui::Key::Y, |m| m.command) || pressed(egui::Key::Z, |m| m.command && m.shift),
                pressed(egui::Key::Delete, |m| !m.shift),
                pressed(egui::Key::Delete, |m| m.shift),
                pressed(egui::Key::F2, |_| true),
            )
        });
        // Shortcuts are for the tree, not while typing in a text field.
        let typing = ctx
            .memory(|m| m.focused())
            .is_some_and(|f| !self.tree_ids.contains(&f));
        if !typing && self.confirm.is_none() {
            if undo {
                self.undo();
            }
            if redo {
                self.redo();
            }
            if (delete || delete_one) && !self.selected.is_empty() {
                let ids = self.selected.clone();
                self.request(Command::Trash {
                    ids,
                    branch: delete,
                });
            }
            if rename && let Some(&id) = self.selected.last() {
                self.command(Command::StartRename(id));
            }
        }

        let narrow = ui.available_width() < 700.0;
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui, narrow);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
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
        });
        // The panel never grows with its content: everything inside
        // scrolls. Its size is the user's, saved with the settings.
        let side = |app: &mut Self, ui: &mut egui::Ui| {
            app.panel_header(ui);
            match app.settings.panel_tab {
                // Scrolls its list itself, below its controls.
                PanelTab::Trash => app.trash_panel(ui),
                tab => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match tab {
                            PanelTab::Backups => app.backups_panel(ui),
                            _ => app.details(ui),
                        });
                }
            }
        };
        if self.settings.panel_open {
            let dragging = ctx.input(|i| i.pointer.any_down());
            if narrow {
                let r = egui::Panel::bottom("details-bottom")
                    .resizable(true)
                    .default_size(self.settings.details_height)
                    .min_size(80.0)
                    .show(ui, |ui| side(self, ui));
                let h = r.response.rect.height();
                if !dragging && (h - self.settings.details_height).abs() > 1.0 {
                    self.settings.details_height = h;
                    self.save_settings();
                }
            } else {
                let side_panel = if i18n::is_rtl() {
                    egui::Panel::left("details")
                } else {
                    egui::Panel::right("details")
                };
                let r = side_panel
                    .resizable(true)
                    .default_size(self.settings.details_width)
                    .min_size(160.0)
                    .max_size((ui.available_width() - 200.0).max(160.0))
                    .show(ui, |ui| side(self, ui));
                let w = r.response.rect.width();
                if !dragging && (w - self.settings.details_width).abs() > 1.0 {
                    self.settings.details_width = w;
                    self.save_settings();
                }
            }
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let cmds = RefCell::new(Vec::new());
            let renaming = RefCell::new(self.renaming.take());
            let mut visible = Vec::new();
            let row_height = self.settings.row_height();
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
                        next_row: std::cell::Cell::new(ui.cursor().top()),
                        row_step: self.settings.row_height() + ui.spacing().item_spacing.y,
                        near: egui::Rangef::new(clip.top() - margin, clip.bottom() + margin),
                    };
                    self.tree_ids = [ui.make_persistent_id("tree"), ui.make_persistent_id("search")];
                    let tree = &self.tree;
                    if self.search.trim().is_empty() && self.search_filter == search::Filter::All {
                        let visible = &mut visible;
                        let (_, actions) = TreeView::new(self.tree_ids[0])
                            .allow_multi_selection(true)
                            .default_node_height(Some(row_height))
                            .show_state(ui, &mut self.state, |b| {
                                for c in &tree.root.children {
                                    if let Some(p) = &self.profile_filter
                                        && profile_of(c).is_some_and(|b| b != p)
                                    {
                                        continue;
                                    }
                                    add_node(b, c, &rows, visible);
                                }
                            });
                        actions
                    } else {
                        let key = (self.search.clone(), self.search_filter, self.seq);
                        if self.search_cache.as_ref().is_none_or(|c| c.key != key) {
                            self.search_cache = Some(search::run(&tree.root, &self.search, self.search_filter, self.seq));
                        }
                        let res = self.search_cache.as_ref().expect("set above");
                        ui.horizontal_wrapped(|ui| {
                            let count = format!("{}{}", res.hits.len(), if res.truncated { "+" } else { "" });
                            ui.weak(trf("{n} match(es); double-click opens in the browser", &[("n", &count)]));
                            if self.search_filter == search::Filter::Duplicates && !res.hits.is_empty() {
                                let extra = search::extra_copies(&tree.root, &res.hits);
                                if ui
                                    .button(trf("Close {n} duplicate(s)", &[("n", &extra.len())]))
                                    .on_hover_text(tr("Keep one copy of each page (an open one if there is), move the others to the trash"))
                                    .clicked()
                                {
                                    cmds.borrow_mut().push(Command::Trash { ids: extra, branch: false });
                                }
                            }
                        });
                        let (_, actions) = TreeView::new(self.tree_ids[1])
                            .allow_drag_and_drop(false)
                            .allow_multi_selection(true)
                            .default_node_height(Some(row_height))
                            .show(ui, |b| {
                                for hit in &res.hits {
                                    let Some(n) = search::resolve(&tree.root, &hit.at) else { continue };
                                    if let Some(p) = &self.profile_filter
                                        && search::resolve(&tree.root, &hit.at[..1]).and_then(profile_of).is_some_and(|b| b != p)
                                    {
                                        continue;
                                    }
                                    let found = Found { words: &res.words, parents: &hit.parents };
                                    b.node(
                                        NodeBuilder::leaf(n.id)
                                            .icon(|ui| node_icon(ui, n, &rows))
                                            .label_ui(|ui| row_label(ui, n, &rows, Some(&found)))
                                            .context_menu(|ui| node_menu(ui, n, rows.cmds)),
                                    );
                                }
                            });
                        actions
                    }
                })
                .inner;
            self.renaming = renaming.into_inner();
            self.handle_actions(actions, ui);
            for c in cmds.into_inner() {
                match c {
                    Command::Copy(text) => {
                        ctx.copy_text(text);
                        self.status = tr("Copied").into();
                    }
                    other => self.request(other),
                }
            }
            self.sync_collapsed(&visible);
        });
        self.settings_window(&ctx);
        self.confirm_window(&ctx);
    }
}

#[cfg(test)]
mod perf {
    //! Frame times with a big tree: `cargo test -p treetabs-ui --release perf -- --ignored --nocapture`
    use super::*;
    use crate::Poll;

    struct Fake(Option<Tree>);

    impl Transport for Fake {
        fn send(&mut self, _: &ClientMsg) -> Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Poll {
            let msgs = self
                .0
                .take()
                .map(|t| {
                    vec![ServerMsg::Snapshot {
                        seq: 1,
                        tree: Box::new(t),
                    }]
                })
                .unwrap_or_default();
            Poll { msgs, lost: false }
        }
        fn reconnect(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn connected(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }

    /// `windows` windows of `tabs` tabs, every 5th tab with 3 children.
    pub(super) fn big_tree(windows: usize, tabs: usize) -> Tree {
        let mut t = Tree::default();
        for w in 0..windows {
            let mut win = Node::new(NodeKind::SavedWindow);
            for i in 0..tabs {
                let mut tab = Node::new(NodeKind::SavedTab);
                tab.title = Some(format!("Page {w}-{i} about something"));
                tab.url = Some(format!("https://example{}.com/{w}/{i}", i % 50));
                if i % 5 == 0 {
                    for c in 0..3 {
                        let mut child = Node::new(NodeKind::SavedTab);
                        child.title = Some(format!("Child {c}"));
                        child.url = Some(format!("https://example.org/{w}/{i}/{c}"));
                        tab.children.push(child);
                    }
                }
                win.children.push(tab);
            }
            t.root.children.push(win);
        }
        t
    }

    fn frames(
        app: &mut App,
        ctx: &egui::Context,
        n: usize,
        input: impl Fn(usize) -> egui::RawInput,
    ) -> std::time::Duration {
        let start = std::time::Instant::now();
        for i in 0..n {
            let mut out = ctx.run_ui(input(i), |ui| app.show(ui));
            out.textures_delta.clear();
        }
        start.elapsed() / n as u32
    }

    #[test]
    #[ignore]
    fn big_tree_frame_time() {
        let tree = big_tree(100, 300); // 100 windows, 30,000 tabs + 18,000 children
        let n = tree.root.count();
        let ctx = egui::Context::default();
        let mut app = App::new(Box::new(Fake(Some(tree))), None);
        let screen = |_| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        frames(&mut app, &ctx, 3, screen);
        let idle = frames(&mut app, &ctx, 20, screen);
        let scroll = frames(&mut app, &ctx, 20, |_| egui::RawInput {
            events: vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -400.0),
                phase: egui::TouchPhase::Move,
                modifiers: Default::default(),
            }],
            ..screen(0)
        });
        app.search = "something 7".into();
        let search_first = frames(&mut app, &ctx, 1, screen);
        let search = frames(&mut app, &ctx, 20, screen);
        println!(
            "{n} nodes: idle {idle:?}/frame, scrolling {scroll:?}/frame, search first {search_first:?}, then {search:?}/frame"
        );
    }
}
