//! Small layout helpers shared by the app modules.

use crate::bidi::{Direction, Tone};
use crate::i18n::{self};
use eframe::egui::{self};

/// A row of widgets; right to left in a right-to-left interface.
pub(super) fn line<R>(
    ui: &mut egui::Ui,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
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
pub(super) fn far() -> egui::Layout {
    if i18n::is_rtl() {
        egui::Layout::left_to_right(egui::Align::Center)
    } else {
        egui::Layout::right_to_left(egui::Align::Center)
    }
}

/// A wrapped paragraph of translated text (`text` in reading order).
pub(super) fn para(ui: &mut egui::Ui, text: &str, tone: Tone) {
    crate::bidi::paragraph(ui, text, Direction::of_interface(), tone);
}

/// "YYYY-MM-DD HH:MM", local time.
pub(super) fn format_time(ms: i64) -> String {
    treetabs_core::backups::local_time(ms)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "rounded to one decimal for display"
)]
pub(super) fn format_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1 << 20)),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}
