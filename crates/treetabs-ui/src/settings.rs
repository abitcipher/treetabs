//! Look and feel of the tree, saved per device (see [`crate::Transport`]).

use crate::i18n::{self, Lang, tr};
use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// What the side panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PanelTab {
    #[default]
    Details,
    Trash,
    Backups,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Scales the whole interface (like browser zoom).
    pub ui_scale: f32,
    /// Text size in points, before scaling.
    pub font_size: f32,
    /// Size of the node icons relative to the text.
    pub icon_scale: f32,
    /// Extra space between rows, in points.
    pub row_spacing: f32,
    pub theme: Theme,
    /// 0: the theme's own colors; 1: text and icons at full black/white.
    pub contrast: f32,
    /// Size of the rename/close/delete buttons on rows, relative to text.
    pub row_button_scale: f32,
    /// Interface language; None follows the system.
    pub language: Option<Lang>,
    /// Show rename / close / delete buttons on the row under the pointer.
    pub hover_buttons: bool,
    /// Site icons (favicons) instead of the generic tab icon.
    pub favicons: bool,
    /// Open tabs and windows in bold.
    pub bold_open: bool,
    /// Renamed nodes (custom title) in italics.
    pub italic_renamed: bool,
    /// Saved (closed) tabs and windows in dimmer text.
    pub dim_saved: bool,
    /// Ask before deleting a whole branch with more than this many nodes.
    pub confirm_branch_over: usize,
    /// Side panel shown (the user's choice; never opened automatically).
    #[serde(alias = "details_open")]
    pub panel_open: bool,
    pub panel_tab: PanelTab,
    /// Width of the side panel at the side, height when at the bottom.
    pub details_width: f32,
    pub details_height: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            ui_scale: 1.0,
            font_size: 14.0,
            icon_scale: 1.0,
            row_spacing: 2.0,
            theme: Theme::System,
            language: None,
            contrast: 0.5,
            row_button_scale: 1.0,
            hover_buttons: true,
            favicons: true,
            bold_open: true,
            italic_renamed: false,
            dim_saved: false,
            confirm_branch_over: 20,
            panel_open: true,
            panel_tab: PanelTab::Details,
            details_width: 320.0,
            details_height: 240.0,
        }
    }
}

/// Text size of the settings window. It doesn't follow the text size
/// setting, so the controls stay put while that slider moves.
const DIALOG_TEXT: f32 = 14.0;
/// A new interface scale waits this long after the last wheel or key step.
const SCALE_DELAY: f64 = 0.6;

/// The settings window's own text styles (see [`DIALOG_TEXT`]).
pub fn dialog_style(style: &mut egui::Style) {
    use egui::{FontFamily, FontId, TextStyle};
    let s = DIALOG_TEXT;
    style.text_styles = [
        (TextStyle::Small, FontId::new(s * 0.8, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(s, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(s, FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(s * 1.25, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(s * 0.9, FontFamily::Monospace)),
    ]
    .into();
}

/// Mouse wheel over `resp` moves `value` by `step` per notch (up = more).
/// The wheel is taken from the scroll area underneath. Returns true on change.
pub fn wheel<T: egui::emath::Numeric>(ui: &egui::Ui, resp: &egui::Response, value: &mut T, step: f64, range: std::ops::RangeInclusive<f64>) -> bool {
    if !resp.hovered() {
        return false;
    }
    let notches: f32 = ui.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { unit, delta, modifiers, .. } if !modifiers.command => Some(match unit {
                    egui::MouseWheelUnit::Line => delta.y + delta.x,
                    // A mouse notch is 50-120 px; a touchpad sends a few px at a time.
                    egui::MouseWheelUnit::Point => {
                        let d = delta.y + delta.x;
                        if d.abs() >= 40.0 { d.signum() } else { d / 40.0 }
                    }
                    egui::MouseWheelUnit::Page => (delta.y + delta.x) * 5.0,
                }),
                _ => None,
            })
            .sum()
    });
    ui.ctx().input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    if notches == 0.0 {
        return false;
    }
    // Touchpads send small deltas: add them up until they make a step.
    let id = resp.id.with("wheel");
    let acc = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0) + notches;
    let steps = acc.trunc();
    ui.data_mut(|d| d.insert_temp(id, acc - steps));
    if steps == 0.0 {
        return false;
    }
    let old = value.to_f64();
    let new = ((old / step).round() + f64::from(steps)) * step;
    let new = new.clamp(*range.start(), *range.end());
    *value = T::from_f64(new);
    new != old
}

impl Settings {
    pub fn parse(s: &str) -> Settings {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Row height for the tree view.
    pub fn row_height(&self) -> f32 {
        self.font_size.max(self.font_size * self.icon_scale) + 4.0 + self.row_spacing
    }

    pub fn icon_size(&self) -> f32 {
        self.font_size * self.icon_scale
    }

    /// Push the settings into egui.
    pub fn apply(&self, ctx: &egui::Context) {
        i18n::set(self.language.unwrap_or_else(i18n::system));
        ctx.set_zoom_factor(self.ui_scale.clamp(0.5, 3.0));
        ctx.set_theme(match self.theme {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        });
        let size = self.font_size.clamp(8.0, 32.0);
        let k = self.contrast.clamp(0.0, 1.0);
        ctx.all_styles_mut(|style| {
            // From the theme's own colors each time, so changes don't add up.
            let dark = style.visuals.dark_mode;
            style.visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
            let target = if dark { egui::Color32::WHITE } else { egui::Color32::BLACK };
            let w = &mut style.visuals.widgets;
            for ws in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
                ws.fg_stroke.color = ws.fg_stroke.color.lerp_to_gamma(target, k);
            }
            // Weak (faded) text: less faded with more contrast.
            style.visuals.weak_text_alpha = 0.6 + 0.3 * k;
            use egui::{FontFamily, FontId, TextStyle};
            style.text_styles = [
                (TextStyle::Small, FontId::new(size * 0.75, FontFamily::Proportional)),
                (TextStyle::Body, FontId::new(size, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(size, FontFamily::Proportional)),
                (TextStyle::Heading, FontId::new(size * 1.4, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(size * 0.9, FontFamily::Monospace)),
            ]
            .into();
        });
    }

    /// The settings window body. Returns true when something changed.
    ///
    /// The interface scale is applied when the slider is let go (or shortly
    /// after the last wheel step): applied while dragging, it would move
    /// the slider away from under the pointer.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        // Right to left: labels right of their controls, headings at the
        // right. (No right-aligned layout around the grids: egui's Grid
        // can't run in one.) Labels stay on one line: egui's own wrapping
        // would put a right-to-left line's words in the wrong order.
        if i18n::is_rtl() {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        }
        self.ui_inner(ui)
    }

    fn ui_inner(&mut self, ui: &mut egui::Ui) -> bool {
        let rtl = i18n::is_rtl();
        let heading = |ui: &mut egui::Ui, text: &str| {
            if rtl {
                let size = egui::vec2(ui.available_width(), ui.text_style_height(&egui::TextStyle::Heading));
                ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), |ui| ui.heading(text));
            } else {
                ui.heading(text);
            }
        };
        // One grid row: label, then the control (mirrored when right to left).
        macro_rules! row {
            ($ui:expr, $label:expr, $body:block) => {{
                if rtl {
                    $body
                    $ui.label($label);
                } else {
                    $ui.label($label);
                    $body
                }
                $ui.end_row();
            }};
        }
        let before = self.clone();
        let pct = |v: f64, _| format!("{:.0}%", v * 100.0);
        heading(ui, tr("Size"));
        egui::Grid::new("settings-size").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            row!(ui, tr("Interface scale"), {
                self.scale_slider(ui);
            });
            row!(ui, tr("Text size"), {
                slider(ui, &mut self.font_size, 9.0..=28.0, 0.5, |s| s.suffix(" pt"));
            });
            row!(ui, tr("Icon size"), {
                slider(ui, &mut self.icon_scale, 0.6..=2.0, 0.05, |s| s.custom_formatter(pct));
            });
            row!(ui, tr("Row spacing"), {
                slider(ui, &mut self.row_spacing, 0.0..=12.0, 0.5, |s| s.suffix(" pt"));
            });
            row!(ui, tr("Row button size"), {
                slider(ui, &mut self.row_button_scale, 0.6..=2.5, 0.05, |s| s.custom_formatter(pct));
            });
        });
        ui.add_space(6.0);
        heading(ui, tr("Look"));
        egui::Grid::new("settings-look").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            row!(ui, tr("Language"), {
                let shown = self.language.map_or_else(|| format!("{} ({})", tr("System"), i18n::shown(i18n::system().name())), |l| i18n::shown(l.name()));
                egui::ComboBox::from_id_salt("language").selected_text(shown).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.language, None, tr("System"));
                    for l in Lang::ALL {
                        ui.selectable_value(&mut self.language, Some(l), i18n::shown(l.name()));
                    }
                });
            });
            row!(ui, tr("Theme"), {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.theme, Theme::System, tr("System"));
                    ui.selectable_value(&mut self.theme, Theme::Light, tr("Light"));
                    ui.selectable_value(&mut self.theme, Theme::Dark, tr("Dark"));
                });
            });
            row!(ui, tr("Contrast"), {
                slider(ui, &mut self.contrast, 0.0..=1.0, 0.05, |s| s.custom_formatter(pct));
            });
            row!(ui, tr("Icons"), {
                ui.checkbox(&mut self.favicons, tr("Site icons (saved tabs faded)"));
            });
            row!(ui, tr("Text styles"), {
                self.styles(ui);
            });
            row!(ui, tr("Row buttons"), {
                ui.checkbox(&mut self.hover_buttons, tr("rename, close, delete on hover"));
            });
        });
        ui.add_space(6.0);
        heading(ui, tr("Deleting"));
        let dir = if rtl { egui::Layout::right_to_left(egui::Align::Center) } else { egui::Layout::left_to_right(egui::Align::Center) };
        ui.with_layout(dir, |ui| {
            ui.label(tr("Ask before deleting a branch of more than"));
            let r = ui.add(egui::DragValue::new(&mut self.confirm_branch_over).range(0..=100_000).suffix(format!(" {}", tr("nodes"))));
            wheel(ui, &r, &mut self.confirm_branch_over, 1.0, 0.0..=100_000.0);
        });
        ui.separator();
        if ui.button(tr("Reset to defaults")).clicked() {
            let (open, tab, w, h) = (self.panel_open, self.panel_tab, self.details_width, self.details_height);
            *self = Settings { panel_open: open, panel_tab: tab, details_width: w, details_height: h, ..Settings::default() };
        }
        *self != before
    }

    /// Text style switches for tree rows.
    pub fn styles(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.checkbox(&mut self.bold_open, tr("Open tabs and windows in bold"));
            ui.checkbox(&mut self.dim_saved, tr("Saved tabs and windows dimmed"));
            ui.checkbox(&mut self.italic_renamed, tr("Renamed items in italics"));
        });
    }

    fn scale_slider(&mut self, ui: &mut egui::Ui) {
        // The value being chosen, and when it last changed.
        let id = ui.id().with("scale-draft");
        let (mut draft, mut changed_at) = ui.data(|d| d.get_temp::<(f32, f64)>(id)).unwrap_or((self.ui_scale, 0.0));
        let now = ui.input(|i| i.time);
        let range = 0.5..=2.5;
        let r = ui.add(
            egui::Slider::new(&mut draft, range.clone())
                .step_by(0.05)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        );
        if r.changed() || wheel(ui, &r, &mut draft, 0.05, 0.5..=2.5) {
            changed_at = now;
        }
        let held = r.is_pointer_button_down_on() || r.dragged();
        if draft != self.ui_scale {
            if !held && (r.drag_stopped() || now - changed_at >= SCALE_DELAY) {
                self.ui_scale = draft;
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(SCALE_DELAY));
            }
        }
        ui.data_mut(|d| d.insert_temp(id, (draft, changed_at)));
    }
}

/// A slider that also follows the mouse wheel.
fn slider(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, step: f64, f: impl FnOnce(egui::Slider) -> egui::Slider) {
    let (lo, hi) = (f64::from(*range.start()), f64::from(*range.end()));
    let r = ui.add(f(egui::Slider::new(value, range).step_by(step)));
    wheel(ui, &r, value, step, lo..=hi);
}
