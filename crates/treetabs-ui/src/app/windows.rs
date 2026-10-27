//! Settings (Interface, Storage, About) and the confirmation dialog.

use super::layout::{far, line, para};
use super::{App, Command, SettingsTab};
use crate::bidi::Tone;
use crate::i18n::{self, tr, trf, trl};
use eframe::egui::{self, RichText};
use treetabs_core::protocol::StoreKind;

impl App {
    /// Settings: a window with its own fixed text size and no title bar
    /// (a title would grow with the text size and push the controls down).
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
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
                    for tab in [
                        SettingsTab::Interface,
                        SettingsTab::Storage,
                        SettingsTab::About,
                    ] {
                        ui.selectable_value(&mut self.settings_tab, tab, tab.label());
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([true, true])
                    .show(ui, |ui| match self.settings_tab {
                        SettingsTab::Storage => self.storage_ui(ui),
                        SettingsTab::About => self.about_ui(ui),
                        SettingsTab::Interface => {
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
    pub(super) fn storage_ui(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(340.0);
        ui.label(tr("The tree is stored in"));
        if self.store_kind == StoreKind::Browser {
            ui.strong(tr("In this browser's own storage"));
            para(
                ui,
                trl(
                    "Without the companion app the tree is kept in the browser. Installing the app moves it to a folder on your computer.",
                ),
                Tone::Weak,
            );
            self.companion_switch_ui(ui, true);
            return;
        }
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
                Tone::Weak,
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
            Tone::Weak,
        );
        self.companion_switch_ui(ui, false);
    }

    /// The button that turns the companion app on (`on`) or off, in the
    /// extension's tree window only.
    fn companion_switch_ui(&mut self, ui: &mut egui::Ui, on: bool) {
        if !self.conn.companion_switch() {
            return;
        }
        ui.add_space(8.0);
        ui.separator();
        ui.label(tr("Companion app"));
        para(
            ui,
            trl(
                "The companion app keeps the tree in a folder on your computer, shared with the desktop window and other browsers.",
            ),
            Tone::Weak,
        );
        let label = if on {
            tr("Use the companion app…")
        } else {
            tr("Stop using the companion app…")
        };
        if ui.button(label).clicked() {
            self.request(Command::UseCompanionApp(on));
        }
    }

    /// Settings > About.
    pub(super) fn about_ui(&mut self, ui: &mut egui::Ui) {
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
            Tone::Normal,
        );
        ui.add_space(4.0);
        para(
            ui,
            trl("Your data stays on your computer. Nothing is sent anywhere."),
            Tone::Normal,
        );
        ui.add_space(4.0);
        ui.label(tr("The tree is stored in"));
        if self.store_kind == StoreKind::Browser {
            ui.strong(tr("In this browser's own storage"));
        } else {
            ui.add(egui::Label::new(RichText::new(&self.store_dir).monospace()).selectable(true));
        }
        ui.add_space(4.0);
        para(
            ui,
            trl(
                "Free software (MIT license). Fonts: Noto Sans CJK SC and Noto Sans Arabic (SIL Open Font License).",
            ),
            Tone::Weak,
        );
    }

    pub(super) fn confirm_window(&mut self, ctx: &egui::Context) {
        let Some((text, cmd)) = &self.confirm else {
            return;
        };
        let text = text.clone();
        let (note, button) = match cmd {
            Command::EmptyTrash => (
                trl("A copy that still has them is saved to Backups first."),
                tr("Empty trash"),
            ),
            Command::UseCompanionApp(true) => (
                trl(
                    "Install the companion app first. The tree kept in the browser moves into the app's folder, and the extension restarts.",
                ),
                tr("Use the app"),
            ),
            Command::UseCompanionApp(false) => (
                trl(
                    "The tree comes along into the browser, and the extension restarts. The app's folder stays as it is, so you can switch back later.",
                ),
                tr("Stop using the app"),
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
                para(ui, &text, Tone::Normal);
                para(ui, note, Tone::Weak);
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
