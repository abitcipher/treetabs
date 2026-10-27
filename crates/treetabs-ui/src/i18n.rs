//! Interface languages. The English text is the key; each other language
//! has a JSON file in `i18n/` mapping English text to its translation.
//! Texts with values use named placeholders: `"Move {n} nodes to the trash?"`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use treetabs_core::{Node, NodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Lang {
    #[default]
    En,
    Fr,
    Uk,
    Es,
    Pt,
    Fi,
    De,
    Zh,
    Ru,
    /// Modern Standard Arabic.
    Ar,
    /// Egyptian Arabic (only the texts that differ; the rest is [`Lang::Ar`]).
    ArEg,
    /// Levantine Arabic.
    ArLev,
    /// Maghrebi Arabic.
    ArMag,
    /// Gulf (Arabian Peninsula) Arabic.
    ArGulf,
    /// Iraqi (Mesopotamian) Arabic.
    ArIq,
}

const COUNT: usize = 15;

impl Lang {
    /// In the order shown in the settings.
    pub const ALL: [Lang; COUNT] = [
        Lang::En,
        Lang::Fr,
        Lang::Uk,
        Lang::Es,
        Lang::Pt,
        Lang::Fi,
        Lang::De,
        Lang::Zh,
        Lang::Ru,
        Lang::Ar,
        Lang::ArEg,
        Lang::ArLev,
        Lang::ArMag,
        Lang::ArGulf,
        Lang::ArIq,
    ];

    /// The language's own name.
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Fr => "Français",
            Lang::Uk => "Українська",
            Lang::Es => "Español",
            Lang::Pt => "Português",
            Lang::Fi => "Suomi",
            Lang::De => "Deutsch",
            Lang::Zh => "中文",
            Lang::Ru => "Русский",
            Lang::Ar => "العربية",
            Lang::ArEg => "العربية المصرية",
            Lang::ArLev => "العربية الشامية",
            Lang::ArMag => "العربية المغاربية",
            Lang::ArGulf => "العربية الخليجية",
            Lang::ArIq => "العربية العراقية",
        }
    }

    /// File name in `i18n/` (also the locale code it is chosen for).
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Fr => "fr",
            Lang::Uk => "uk",
            Lang::Es => "es",
            Lang::Pt => "pt",
            Lang::Fi => "fi",
            Lang::De => "de",
            Lang::Zh => "zh",
            Lang::Ru => "ru",
            Lang::Ar => "ar",
            Lang::ArEg => "ar-eg",
            Lang::ArLev => "ar-lev",
            Lang::ArMag => "ar-mag",
            Lang::ArGulf => "ar-gulf",
            Lang::ArIq => "ar-iq",
        }
    }

    /// Where missing texts come from before English.
    fn parent(self) -> Option<Lang> {
        matches!(
            self,
            Lang::ArEg | Lang::ArLev | Lang::ArMag | Lang::ArGulf | Lang::ArIq
        )
        .then_some(Lang::Ar)
    }

    /// Written right to left.
    pub fn is_rtl(self) -> bool {
        self == Lang::Ar || self.parent() == Some(Lang::Ar)
    }

    /// From a locale such as `de_DE.UTF-8`, `pt-BR` or `ar_EG`; English
    /// otherwise.
    pub fn from_locale(locale: &str) -> Lang {
        let mut parts = locale.split(['_', '-', '.', '@']);
        let code = parts.next().unwrap_or("").to_lowercase();
        let region = parts.next().unwrap_or("").to_uppercase();
        if code == "ar" {
            return match region.as_str() {
                "EG" | "SD" => Lang::ArEg,
                "SY" | "LB" | "JO" | "PS" => Lang::ArLev,
                "MA" | "DZ" | "TN" | "LY" | "MR" => Lang::ArMag,
                "SA" | "AE" | "KW" | "QA" | "BH" | "OM" | "YE" => Lang::ArGulf,
                "IQ" => Lang::ArIq,
                _ => Lang::Ar,
            };
        }
        Lang::ALL
            .into_iter()
            .find(|l| l.code() == code)
            .unwrap_or(Lang::En)
    }

    fn source(self) -> &'static str {
        match self {
            Lang::En => "{}",
            Lang::Fr => include_str!("../i18n/fr.json"),
            Lang::Uk => include_str!("../i18n/uk.json"),
            Lang::Es => include_str!("../i18n/es.json"),
            Lang::Pt => include_str!("../i18n/pt.json"),
            Lang::Fi => include_str!("../i18n/fi.json"),
            Lang::De => include_str!("../i18n/de.json"),
            Lang::Zh => include_str!("../i18n/zh.json"),
            Lang::Ru => include_str!("../i18n/ru.json"),
            Lang::Ar => include_str!("../i18n/ar.json"),
            Lang::ArEg => include_str!("../i18n/ar-eg.json"),
            Lang::ArLev => include_str!("../i18n/ar-lev.json"),
            Lang::ArMag => include_str!("../i18n/ar-mag.json"),
            Lang::ArGulf => include_str!("../i18n/ar-gulf.json"),
            Lang::ArIq => include_str!("../i18n/ar-iq.json"),
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Add Chinese and Arabic fonts after the default ones (which have
/// neither): a subset of Noto Sans CJK SC and Noto Sans Arabic, see
/// `fonts/LICENSE-OFL.txt`. Arabic letters join (egui shapes text), but
/// egui does not reorder mixed right-to-left text yet.
pub fn install_fonts(ctx: &egui::Context) {
    use eframe::egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    use eframe::egui::{FontData, FontFamily};
    let data = FontData::from_static(include_bytes!("../fonts/NotoSansSC-subset.otf"));
    let families = [FontFamily::Proportional, FontFamily::Monospace]
        .map(|family| InsertFontFamily {
            family,
            priority: FontPriority::Lowest,
        })
        .to_vec();
    ctx.add_font(FontInsert::new("noto-sans-sc", data, families.clone()));
    let arabic = FontData::from_static(include_bytes!("../fonts/NotoSansArabic-Regular.ttf"));
    ctx.add_font(FontInsert::new("noto-sans-arabic", arabic, families));
}

/// The system's language: the browser's on the web, `LC_ALL` /
/// `LC_MESSAGES` / `LANG` on the desktop.
pub fn system() -> Lang {
    #[cfg(target_arch = "wasm32")]
    {
        let nav = js_sys::Reflect::get(&js_sys::global(), &"navigator".into()).ok();
        let lang = nav
            .and_then(|n| js_sys::Reflect::get(&n, &"language".into()).ok())
            .and_then(|l| l.as_string());
        Lang::from_locale(&lang.unwrap_or_default())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let var = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()));
        Lang::from_locale(&var.unwrap_or_default())
    }
}

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn current() -> Lang {
    Lang::ALL[CURRENT.load(Ordering::Relaxed) as usize]
}

type Table = HashMap<&'static str, &'static str>;

fn tables() -> &'static [Table; COUNT] {
    static T: OnceLock<[Table; COUNT]> = OnceLock::new();
    T.get_or_init(|| {
        Lang::ALL.map(|l| {
            // A broken file means English for that language, not an error.
            let map: HashMap<String, String> =
                serde_json::from_str(l.source()).unwrap_or_else(|e| {
                    log::warn!("bad translation file {}: {e}", l.code());
                    HashMap::new()
                });
            map.into_iter()
                .map(|(k, v)| (&*k.leak(), &*v.leak()))
                .collect()
        })
    })
}

/// The text in `lang`, in reading order: the language's own file, then
/// its parent (dialect → Arabic), then English.
fn lookup(lang: Lang, text: &'static str) -> &'static str {
    if lang == Lang::En {
        return text;
    }
    let t = tables();
    t[lang as usize]
        .get(text)
        .or_else(|| lang.parent().and_then(|p| t[p as usize].get(text)))
        .copied()
        .unwrap_or(text)
}

/// Whether the interface is right to left now.
pub fn is_rtl() -> bool {
    current().is_rtl()
}

/// Text ready for drawing: in a right-to-left interface, or when it has
/// right-to-left letters, put in the order egui shows correctly (see
/// `bidi`). Only for display: copy and edit the original text.
pub fn shown(text: &str) -> String {
    crate::bidi::visual(text, is_rtl())
}

/// `text` in the current language, ready for drawing (English if there is
/// no translation).
pub fn tr(text: &'static str) -> &'static str {
    let lang = current();
    let s = lookup(lang, text);
    if !lang.is_rtl() {
        return s;
    }
    // Reordered once per text, then kept.
    static SHOWN: OnceLock<std::sync::Mutex<HashMap<(u8, &'static str), &'static str>>> =
        OnceLock::new();
    let cache = SHOWN.get_or_init(Default::default);
    let mut cache = cache.lock().expect("no panics while held");
    cache
        .entry((lang as u8, text))
        .or_insert_with(|| crate::bidi::visual(s, true).leak())
}

/// `text` in the current language in reading order, for widgets that
/// arrange right-to-left text themselves (wrapped paragraphs).
pub fn trl(text: &'static str) -> &'static str {
    lookup(current(), text)
}

/// [`tr`] with `{name}` placeholders filled in, ready for drawing.
pub fn trf(text: &'static str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut s = lookup(current(), text).to_owned();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), &v.to_string());
    }
    shown(&s)
}

/// A node's title in reading order: generic titles ("Window") translated.
/// For copying, editing and searching; draw [`shown_title`].
pub fn title(n: &Node) -> String {
    if n.custom_title.as_deref().is_some_and(|t| !t.is_empty()) {
        return n.display_title();
    }
    match &n.kind {
        k if k.is_window() => lookup(current(), "Window").to_owned(),
        NodeKind::Session => lookup(current(), "Session").to_owned(),
        _ => n.display_title(),
    }
}

/// [`title`] ready for drawing. Titles keep their own direction (first
/// strong letter), whatever the interface language.
pub fn shown_title(n: &Node) -> String {
    crate::bidi::visual(&title(n), false)
}

/// Name of a node kind, for the details panel.
pub fn kind_name(k: &NodeKind) -> &'static str {
    match k {
        NodeKind::Window => tr("Window"),
        NodeKind::SavedWindow => tr("Saved window"),
        NodeKind::WaitingWindow => tr("Waiting window"),
        NodeKind::Tab => tr("Tab"),
        NodeKind::SavedTab => tr("Saved tab"),
        NodeKind::WaitingTab | NodeKind::AttachWaitingTab => tr("Waiting tab"),
        NodeKind::Group => tr("Group"),
        NodeKind::Note => tr("Note"),
        NodeKind::Separator => tr("Separator"),
        NodeKind::Session => tr("Session"),
        NodeKind::Other(_) => tr("Unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every literal passed to `tr(`/`trf(` in the UI sources.
    fn keys() -> BTreeSet<String> {
        let sources = [
            include_str!("app/mod.rs"),
            include_str!("app/commands.rs"),
            include_str!("app/data.rs"),
            include_str!("app/frame.rs"),
            include_str!("app/layout.rs"),
            include_str!("app/panels.rs"),
            include_str!("app/rows.rs"),
            include_str!("app/toolbar.rs"),
            include_str!("app/tree.rs"),
            include_str!("app/windows.rs"),
            include_str!("settings.rs"),
            include_str!("search.rs"),
            // Not this test's own code.
            include_str!("i18n.rs")
                .split("#[cfg(test)]")
                .next()
                .unwrap_or(""),
            include_str!("../../treetabs-gui/src/main.rs"),
            include_str!("../../../extension/ui/src/lib.rs"),
        ];
        let mut out = BTreeSet::new();
        for src in sources {
            // Calls may be split over lines by rustfmt: `trf(\n    "text"`.
            for call in ["tr(", "trf(", "trl("] {
                let mut rest = src;
                while let Some(i) = rest.find(call) {
                    // Only whole names (not `ctr(`, `attr(` …).
                    let whole = i == 0
                        || !rest.as_bytes()[i - 1].is_ascii_alphanumeric()
                            && rest.as_bytes()[i - 1] != b'_';
                    rest = &rest[i + call.len()..];
                    let after = rest.trim_start();
                    if !whole || !after.starts_with('"') {
                        continue;
                    }
                    rest = &after[1..];
                    // Up to the closing quote, honouring \" escapes.
                    let mut key = String::new();
                    let mut chars = rest.chars();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => {
                                if let Some(n) = chars.next() {
                                    key.push(n);
                                }
                            }
                            '"' => break,
                            c => key.push(c),
                        }
                    }
                    out.insert(key);
                }
            }
        }
        // The calls in this test's own text are not keys.
        out.remove(")");
        out.retain(|k| !k.is_empty());
        out
    }

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            rest = &rest[i + 1..];
            if let Some(j) = rest.find('}') {
                out.insert(rest[..j].to_owned());
                rest = &rest[j..];
            }
        }
        out
    }

    #[test]
    fn every_text_is_translated_in_every_language() {
        let keys = keys();
        assert!(keys.len() > 80, "found only {} keys", keys.len());
        let mut problems = Vec::new();
        for lang in Lang::ALL.into_iter().skip(1) {
            let map: HashMap<String, String> =
                serde_json::from_str(lang.source()).expect("valid json");
            for k in &keys {
                match map.get(k) {
                    // Dialects only hold what differs from their parent.
                    None if lang.parent().is_some() => {}
                    None => problems.push(format!("{}: missing {k:?}", lang.code())),
                    Some(v) if placeholders(v) != placeholders(k) => {
                        problems.push(format!("{}: placeholders differ in {k:?}", lang.code()))
                    }
                    Some(v) if v.trim().is_empty() => {
                        problems.push(format!("{}: empty {k:?}", lang.code()))
                    }
                    _ => {}
                }
            }
            for k in map.keys() {
                if !keys.contains(k) {
                    problems.push(format!("{}: unused {k:?}", lang.code()));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "{} problems:\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    #[test]
    fn locale_names() {
        assert_eq!(Lang::from_locale("de_DE.UTF-8"), Lang::De);
        assert_eq!(Lang::from_locale("pt-BR"), Lang::Pt);
        assert_eq!(Lang::from_locale("zh-CN"), Lang::Zh);
        assert_eq!(Lang::from_locale("C"), Lang::En);
        assert_eq!(Lang::from_locale("ar_EG.UTF-8"), Lang::ArEg);
        assert_eq!(Lang::from_locale("ar-LB"), Lang::ArLev);
        assert_eq!(Lang::from_locale("ar"), Lang::Ar);
    }

    #[test]
    fn dialects_fall_back_to_arabic_then_english() {
        assert_eq!(lookup(Lang::ArEg, "Nothing selected"), "مفيش حاجة متحددة");
        assert_eq!(lookup(Lang::ArEg, "Settings"), "الإعدادات");
        assert_eq!(lookup(Lang::Ar, "no such text"), "no such text");
    }
}
