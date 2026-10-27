//! Tree rows: text, icons, hover buttons and the right-click menu.

use super::{Command, NewItem, Place, Rows, Scope};
use crate::i18n::{self, tr, trf};
use crate::search;
use crate::settings::Settings;
use eframe::egui::{self, Color32, RichText};
use egui_ltreeview::{NodeBuilder, TreeViewBuilder};
use std::cell::RefCell;
use treetabs_core::{Node, NodeId, NodeKind, Tree};

pub(super) fn kind_style(n: &Node) -> (&'static str, Option<Color32>) {
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
/// Text styles of a row, as chosen in the settings.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LabelStyle {
    /// Open tab or window.
    pub(super) bold: bool,
    /// Saved tab or window.
    pub(super) dim: bool,
    /// Renamed (has a custom title).
    pub(super) italic: bool,
}

impl LabelStyle {
    pub(super) fn of(n: &Node, s: &Settings) -> Self {
        let open = matches!(n.kind, NodeKind::Tab | NodeKind::Window);
        let saved = matches!(n.kind, NodeKind::SavedTab | NodeKind::SavedWindow);
        LabelStyle {
            bold: open && s.bold_open,
            dim: saved && s.dim_saved,
            italic: n.custom_title.is_some() && s.italic_renamed,
        }
    }
}

pub(super) fn label_text(n: &Node, s: &Settings) -> RichText {
    let mut t = RichText::new(i18n::shown_title(n));
    let style = LabelStyle::of(n, s);
    if style.bold {
        t = t.strong();
    }
    if style.dim {
        t = t.weak();
    }
    if style.italic {
        t = t.italics();
    }
    t
}

/// Browser profile a top-level window belongs to.
pub(super) fn profile_of(n: &Node) -> Option<&str> {
    matches!(
        n.kind,
        NodeKind::Window | NodeKind::SavedWindow | NodeKind::WaitingWindow
    )
    .then(|| n.data.get("browser")?.as_str())
    .flatten()
}

/// Labels for the profiles that have windows in the tree: the browser's
/// name, plus a short id when two profiles share a name.
pub(super) fn profile_labels(
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
pub(super) fn is_unloaded(n: &Node) -> bool {
    n.kind == NodeKind::Tab && n.data.get("discarded").is_some_and(|v| v == true)
}

pub(super) fn has_open(n: &Node) -> bool {
    n.find_by(|m| matches!(m.kind, NodeKind::Tab | NodeKind::Window))
        .is_some()
}

/// The right-click menu of a node.
pub(super) fn node_menu(ui: &mut egui::Ui, n: &Node, cmds: &RefCell<Vec<Command>>) {
    if i18n::is_rtl() {
        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Max), |ui| {
            node_menu_items(ui, n, cmds)
        });
    } else {
        node_menu_items(ui, n, cmds);
    }
}

/// One entry of the right-click menu.
struct MenuEntry {
    label: String,
    tip: Option<&'static str>,
    command: Command,
}

impl MenuEntry {
    fn new(label: impl Into<String>, command: Command) -> Self {
        MenuEntry {
            label: label.into(),
            tip: None,
            command,
        }
    }

    fn tip(mut self, tip: &'static str) -> Self {
        self.tip = Some(tip);
        self
    }
}

/// The menu's sections, each a list of entries; only the entries that
/// apply to `n` are there.
fn menu_sections(n: &Node) -> [Vec<MenuEntry>; 3] {
    let id = n.id;
    let one = || vec![id];
    let is_open = matches!(n.kind, NodeKind::Tab | NodeKind::Window);
    let kids = !n.children.is_empty();
    let loaded = |m: &Node| m.kind == NodeKind::Tab && !is_unloaded(m);

    let mut open = Vec::new();
    if n.url.is_some() || kids {
        let label = if is_open {
            tr("Focus in browser")
        } else {
            tr("Open in browser")
        };
        open.push(MenuEntry::new(label, Command::Open(id)));
    }
    open.push(MenuEntry::new(
        format!("{}    F2", tr("Rename…")),
        Command::StartRename(id),
    ));

    let mut change = Vec::new();
    if is_open {
        change.push(MenuEntry::new(
            tr("Close (keep saved)"),
            Command::Close {
                ids: one(),
                scope: Scope::Node,
            },
        ));
    }
    if kids && has_open(n) {
        change.push(MenuEntry::new(
            tr("Close branch (keep saved)"),
            Command::Close {
                ids: one(),
                scope: Scope::Branch,
            },
        ));
    }
    if loaded(n) {
        change.push(
            MenuEntry::new(
                tr("Unload (free memory)"),
                Command::Unload {
                    ids: one(),
                    scope: Scope::Node,
                },
            )
            .tip(tr("The tab stays open and loads again when you go to it")),
        );
    }
    if kids && n.find_by(|m| loaded(m)).is_some() {
        change.push(MenuEntry::new(
            tr("Unload branch"),
            Command::Unload {
                ids: one(),
                scope: Scope::Branch,
            },
        ));
    }
    if kids {
        change.push(MenuEntry::new(
            tr("Delete, keep children"),
            Command::Trash {
                ids: one(),
                scope: Scope::Node,
            },
        ));
    }
    let delete = if kids {
        tr("Delete branch")
    } else {
        tr("Delete")
    };
    change.push(MenuEntry::new(
        format!("{delete}    Del"),
        Command::Trash {
            ids: one(),
            scope: Scope::Branch,
        },
    ));

    let mut copy = Vec::new();
    if let Some(u) = &n.url {
        copy.push(MenuEntry::new(tr("Copy URL"), Command::Copy(u.clone())));
    }
    copy.push(MenuEntry::new(
        tr("Copy title"),
        Command::Copy(i18n::title(n)),
    ));
    [open, change, copy]
}

pub(super) fn node_menu_items(ui: &mut egui::Ui, n: &Node, cmds: &RefCell<Vec<Command>>) {
    let push = |ui: &mut egui::Ui, c: Command| {
        cmds.borrow_mut().push(c);
        ui.close();
    };
    let entries = |ui: &mut egui::Ui, list: Vec<MenuEntry>| {
        for e in list {
            let mut r = ui.button(e.label);
            if let Some(tip) = e.tip {
                r = r.on_hover_text(tip);
            }
            if r.clicked() {
                push(ui, e.command);
            }
        }
    };
    let [open, change, copy] = menu_sections(n);
    entries(ui, open);
    ui.separator();
    entries(ui, change);
    ui.separator();
    ui.menu_button(tr("Add"), |ui| {
        for (item, place, label) in [
            (NewItem::Note, Place::After, tr("Note after")),
            (NewItem::Note, Place::Inside, tr("Note inside")),
            (NewItem::Group, Place::After, tr("Group after")),
            (NewItem::Group, Place::Inside, tr("Group inside")),
        ] {
            if ui.button(label).clicked() {
                push(
                    ui,
                    Command::Add {
                        item,
                        at: n.id,
                        place,
                    },
                );
            }
        }
    });
    if !n.children.is_empty() {
        if ui.button(tr("Expand all below")).clicked() {
            push(ui, Command::ExpandAll(n.id));
        }
        if ui.button(tr("Collapse all below")).clicked() {
            push(ui, Command::CollapseAll(n.id));
        }
    }
    ui.separator();
    entries(ui, copy);
}

/// The text part of a row: label, inline rename field, hover buttons.
/// A search result's context for its row.
pub(super) struct Found<'a> {
    pub(super) words: &'a [String],
    pub(super) parents: &'a str,
}

pub(super) fn row_label(ui: &mut egui::Ui, n: &Node, rows: &Rows<'_>, found: Option<&Found<'_>>) {
    if rename_field(ui, n.id, rows) {
        return;
    }
    match found {
        None => plain_label(ui, n, rows),
        Some(f) => search_label(ui, n, rows, f),
    }
    if rows.settings.hover_buttons {
        hover_buttons(ui, n, rows);
    }
}

/// The text field while the row is being renamed; true if it was shown.
fn rename_field(ui: &mut egui::Ui, id: NodeId, rows: &Rows<'_>) -> bool {
    let mut r = rows.renaming.borrow_mut();
    let Some((_, text, focused)) = r.as_mut().filter(|(rid, ..)| *rid == id) else {
        return false;
    };
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
    true
}

/// A tree row: the title, and the browser's name on windows when the tree
/// has more than one profile.
fn plain_label(ui: &mut egui::Ui, n: &Node, rows: &Rows<'_>) {
    ui.add(egui::Label::new(label_text(n, rows.settings)).selectable(false));
    if let Some(labels) = rows.profiles
        && let Some(l) = profile_of(n).and_then(|b| labels.get(b))
    {
        ui.add(egui::Label::new(RichText::new(l).small().weak()).selectable(false));
    }
}

/// A search result: the tree row's look with the matches highlighted, and
/// where the node is.
fn search_label(ui: &mut egui::Ui, n: &Node, rows: &Rows<'_>, f: &Found<'_>) {
    let mut format = egui::TextFormat::simple(
        egui::TextStyle::Body.resolve(ui.style()),
        ui.visuals().text_color(),
    );
    let style = LabelStyle::of(n, rows.settings);
    if style.dim {
        format.color = ui.visuals().weak_text_color();
    } else if style.bold {
        format.color = ui.visuals().strong_text_color();
    }
    format.italics = style.italic;
    let job = search::highlighted(ui, &i18n::shown_title(n), f.words, format);
    ui.add(egui::Label::new(job).selectable(false));
    if !f.parents.is_empty() {
        let path = RichText::new(trf("in {path}", &[("path", &f.parents)]))
            .small()
            .weak();
        ui.add(egui::Label::new(path).selectable(false));
    }
}

/// Rename, close and delete, under the pointer: pinned just inside the right
/// edge of the visible tree, over the text (on their own background), so
/// long rows don't push them out of view.
fn hover_buttons(ui: &mut egui::Ui, n: &Node, rows: &Rows<'_>) {
    let row = ui.max_rect();
    let clip = ui.clip_rect();
    let hovered = ui
        .ctx()
        .pointer_hover_pos()
        .is_some_and(|p| row.y_range().contains(p.y) && clip.contains(p));
    if !hovered {
        return;
    }
    let id = n.id;
    let leaf = n.children.is_empty();
    let mut buttons: Vec<(&str, &str, Command)> =
        vec![("✏", tr("Rename (F2)"), Command::StartRename(id))];
    if has_open(n) {
        let tip = if leaf {
            tr("Close, keep saved")
        } else {
            tr("Close branch, keep saved")
        };
        buttons.push((
            "✖",
            tip,
            Command::Close {
                ids: vec![id],
                scope: Scope::Branch,
            },
        ));
    }
    let tip = if leaf {
        tr("Delete")
    } else {
        tr("Delete branch")
    };
    buttons.push((
        "🗑",
        tip,
        Command::Trash {
            ids: vec![id],
            scope: Scope::Branch,
        },
    ));

    let size = ui.text_style_height(&egui::TextStyle::Body)
        * 0.85
        * rows.settings.row_button_scale.clamp(0.6, 2.5);
    let gap = 4.0;
    #[expect(clippy::cast_precision_loss, reason = "at most three buttons")]
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
pub(super) fn node_icon(ui: &mut egui::Ui, n: &Node, rows: &Rows<'_>) {
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
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "base and k keep it within 0..=255"
        )]
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

pub(super) fn add_node(
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
