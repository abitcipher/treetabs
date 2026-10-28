//! Settings > Support (donations) and Settings > Credits (the projects
//! treetabs is built on).

use super::App;
use super::layout::para;
use crate::bidi::Tone;
use crate::i18n::{tr, trl};
use eframe::egui::{self, Color32, RichText};

/// PayPal's donation page for treetabs; `assets/donate-qr.png` holds the
/// same address (with `&source=qr`).
const DONATE_URL: &str = "https://www.paypal.com/donate/?hosted_button_id=TQTXU7ZB92CCL";

const MIT_APACHE: &str = "MIT / Apache-2.0";

/// The key projects in Settings > Credits, most visible first: name,
/// address, what treetabs uses it for, and the license its package states.
fn credits() -> [(&'static str, &'static str, &'static str, &'static str); 16] {
    [
        (
            "Rust",
            "https://www.rust-lang.org",
            tr("the programming language"),
            MIT_APACHE,
        ),
        (
            "egui, eframe",
            "https://github.com/emilk/egui",
            tr("the interface"),
            MIT_APACHE,
        ),
        (
            "egui_ltreeview",
            "https://github.com/LennysLounge/egui_ltreeview",
            tr("the tree view"),
            "MIT",
        ),
        (
            "wasm-bindgen",
            "https://github.com/wasm-bindgen/wasm-bindgen",
            tr("Rust in the browser"),
            MIT_APACHE,
        ),
        (
            "oxichrome",
            "https://github.com/0xsouravm/oxichrome",
            tr("the browser extension"),
            "MIT",
        ),
        (
            "Binaryen",
            "https://github.com/WebAssembly/binaryen",
            tr("smaller downloads"),
            "Apache-2.0",
        ),
        (
            "serde, serde_json",
            "https://serde.rs",
            tr("saving and reading data"),
            MIT_APACHE,
        ),
        (
            "chrono",
            "https://github.com/chronotope/chrono",
            tr("dates and times"),
            MIT_APACHE,
        ),
        (
            "unicode-bidi",
            "https://github.com/servo/unicode-bidi",
            tr("right-to-left text"),
            MIT_APACHE,
        ),
        (
            "image",
            "https://github.com/image-rs/image",
            tr("site icons"),
            MIT_APACHE,
        ),
        (
            "snap",
            "https://github.com/BurntSushi/rust-snappy",
            tr("reading browser databases"),
            "BSD-3-Clause",
        ),
        (
            "clap",
            "https://github.com/clap-rs/clap",
            tr("the command line"),
            MIT_APACHE,
        ),
        (
            "winit",
            "https://github.com/rust-windowing/winit",
            tr("the desktop window"),
            "Apache-2.0",
        ),
        (
            "rfd",
            "https://github.com/PolyMeilex/rfd",
            tr("file dialogs"),
            "MIT",
        ),
        (
            "Noto Sans CJK SC, Noto Sans Arabic",
            "https://notofonts.github.io",
            tr("Chinese and Arabic letters"),
            "SIL OFL 1.1",
        ),
        (
            "Playwright",
            "https://playwright.dev",
            tr("browser tests"),
            "Apache-2.0",
        ),
    ]
}

impl App {
    /// Settings > Support: how to donate. Nothing here is required, and
    /// nothing depends on it.
    pub(super) fn support_ui(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(340.0);
        para(
            ui,
            trl(
                "treetabs is free, with no ads and no paid features. If it is useful to you, a donation helps keep it going.",
            ),
            Tone::Normal,
        );
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            let donate = egui::Button::new(RichText::new(tr("Donate with PayPal")).strong());
            if ui.add(donate).on_hover_text(DONATE_URL).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(DONATE_URL));
            }
            ui.add_space(8.0);
            // On a white square in both themes, with a margin: phones read
            // dark codes on light.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(184.0, 184.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 6.0, Color32::WHITE);
            egui::Image::new(egui::include_image!("../../assets/donate-qr.png"))
                .paint_at(ui, rect.shrink(16.0));
            ui.add_space(2.0);
        });
        para(
            ui,
            trl("Or scan the code with your phone's camera."),
            Tone::Weak,
        );
        ui.add_space(8.0);
        para(
            ui,
            trl(
                "Donations are voluntary and unlock nothing: every feature is free for everyone. Thank you!",
            ),
            Tone::Weak,
        );
    }

    /// Settings > Credits: the projects treetabs is built on.
    pub(super) fn credits_ui(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(340.0);
        para(
            ui,
            trl("treetabs is built on these free projects. Thank you to everyone who makes them."),
            Tone::Normal,
        );
        ui.add_space(6.0);
        egui::Grid::new("credits")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for (name, url, purpose, license) in credits() {
                    ui.hyperlink_to(name, url).on_hover_text(url);
                    ui.label(purpose);
                    ui.weak(license);
                    ui.end_row();
                }
            });
        ui.add_space(6.0);
        para(
            ui,
            trl(
                "And the many smaller libraries these use. Each package's license is in its source.",
            ),
            Tone::Weak,
        );
    }
}
