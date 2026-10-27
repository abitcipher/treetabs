//! The top bar: undo/redo, search with its filters, panel tabs, settings button.

use super::App;
use super::layout::{far, line};
use super::rows::profile_labels;
use crate::i18n::tr;
use crate::search;
use crate::settings::PanelTab;
use eframe::egui::{self, RichText};

impl App {
    /// Top bar. Left: frequent actions (undo, redo, search). Right, in a
    /// different style: the side panel tabs and settings, which open views
    /// rather than change the tree.
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui, narrow: bool) {
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

    pub(super) fn search_box(&mut self, ui: &mut egui::Ui, width: f32) {
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

    pub(super) fn history_buttons(&mut self, ui: &mut egui::Ui) {
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
    pub(super) fn view_buttons(&mut self, ui: &mut egui::Ui, narrow: bool) {
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
    pub(super) fn show_panel(&mut self, tab: Option<PanelTab>) {
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
    pub(super) fn panel_header(&mut self, ui: &mut egui::Ui) {
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
}
