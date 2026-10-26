//! Right-to-left text for egui.
//!
//! egui shapes text, so Arabic letters join and every run of Arabic-font
//! characters comes out right to left. But it places those runs left to
//! right in reading order, without the Unicode bidi rules, so a sentence's
//! words (and any Latin or digits in it) end up in the wrong order.
//!
//! [`visual`] fixes that: it puts the text in on-screen order with the bidi
//! algorithm, then turns each Arabic run back into reading order, because
//! egui reverses those runs itself when it shapes them.

use eframe::egui;
use unicode_bidi::{BidiInfo, Level};

/// Characters drawn from a right-to-left font (Arabic and Hebrew scripts).
pub(crate) fn is_rtl_char(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x08FF      // Hebrew, Arabic, Syriac, Thaana, NKo, …, Arabic Extended
        | 0xFB1D..=0xFDFF    // Hebrew and Arabic presentation forms A
        | 0xFE70..=0xFEFF    // Arabic presentation forms B
    )
}

/// Whether `text` has anything right-to-left in it.
pub fn has_rtl(text: &str) -> bool {
    text.chars().any(is_rtl_char)
}

/// `text` arranged so that egui shows it in correct bidi order. `rtl`:
/// the paragraph direction is right to left (otherwise it follows the first
/// strong character). Lines (split at `\n`) are reordered one by one.
pub fn visual(text: &str, rtl: bool) -> String {
    if !rtl && !has_rtl(text) {
        return text.to_owned();
    }
    text.split('\n').map(|line| visual_line(line, rtl)).collect::<Vec<_>>().join("\n")
}

fn visual_line(line: &str, rtl: bool) -> String {
    if line.is_empty() {
        return String::new();
    }
    let level = rtl.then(Level::rtl);
    let info = BidiInfo::new(line, level);
    let Some(para) = info.paragraphs.first() else { return line.to_owned() };
    // Runs in on-screen order; right-to-left runs reversed, with their
    // brackets mirrored (egui draws "(" as "(" whatever the direction).
    let (levels, runs) = info.visual_runs(para, para.range.clone());
    let mut shown: Vec<char> = Vec::with_capacity(line.len());
    for run in runs {
        let text = &line[run.clone()];
        if levels[run.start].is_rtl() {
            shown.extend(text.chars().rev().map(mirror));
        } else {
            shown.extend(text.chars());
        }
    }
    // Each run of RTL-font characters back to reading order: egui reverses
    // those itself when it shapes them.
    let chars = shown;
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if is_rtl_char(chars[i]) {
            let start = i;
            // Combining marks (harakat) belong to the run too.
            while i < chars.len() && (is_rtl_char(chars[i]) || is_mark(chars[i])) {
                i += 1;
            }
            out.extend(chars[start..i].iter().rev());
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// The mirror image of a bracket, for right-to-left runs.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        c => c,
    }
}

fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06ED | 0x200C | 0x200D)
}

/// A wrapped paragraph that may be right to left: wrapped at `width` in
/// reading order first (so lines keep their order), then each line
/// reordered. Right-aligned when `rtl`.
pub fn paragraph(ui: &mut egui::Ui, text: &str, rtl: bool, weak: bool) {
    if !rtl && !has_rtl(text) {
        let t = egui::RichText::new(text);
        ui.add(egui::Label::new(if weak { t.weak() } else { t }).wrap());
        return;
    }
    let width = ui.available_width().max(40.0);
    let color = if weak { ui.visuals().weak_text_color() } else { ui.visuals().text_color() };
    let font = egui::TextStyle::Body.resolve(ui.style());
    let lines = wrap_lines(ui, text, &font, width);
    let shown: Vec<String> = lines.iter().map(|l| visual_line(l, rtl)).collect();
    let layout = if rtl { egui::Layout::top_down(egui::Align::Max) } else { egui::Layout::top_down(egui::Align::Min) };
    ui.with_layout(layout, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for l in shown {
            ui.add(egui::Label::new(egui::RichText::new(l).font(font.clone()).color(color)).extend());
        }
    });
}

/// Break `text` into lines no wider than `width`, at spaces where possible.
fn wrap_lines(ui: &egui::Ui, text: &str, font: &egui::FontId, width: f32) -> Vec<String> {
    let measure = |s: &str| ui.painter().layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE).size().x;
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
            if !line.is_empty() && measure(&candidate) > width {
                lines.push(std::mem::take(&mut line));
                line = word.to_owned();
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

/// Direction of the paragraph: right to left in a right-to-left interface,
/// otherwise from the first strong letter.
fn base_rtl(text: &str, ui_rtl: bool) -> bool {
    ui_rtl || matches!(unicode_bidi::get_base_direction(text), unicode_bidi::Direction::Rtl)
}

/// Where the text cursor goes on screen, for each logical position
/// `0..=chars` of `text` laid out (by [`visual`]) as `galley` at `origin`.
fn caret_xs(galley: &egui::Galley, origin: egui::Pos2, text: &str, ui_rtl: bool) -> Vec<f32> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return vec![origin.x];
    }
    let info = BidiInfo::new(text, ui_rtl.then(Level::rtl));
    let levels: Vec<Level> = text.char_indices().map(|(b, _)| info.levels[b]).collect();
    // order[k]: the logical character shown k-th from the left.
    let order = BidiInfo::reorder_visual(&levels);
    // The drawn glyphs, left to right (zero-width ones are placeholders).
    let mut glyphs: Vec<(f32, f32, char)> = galley
        .rows
        .iter()
        .flat_map(|r| r.glyphs.iter().filter(|g| g.advance_width > 0.0).map(|g| (g.pos.x, g.advance_width, g.chr)))
        .collect();
    glyphs.sort_by(|a, b| a.0.total_cmp(&b.0));
    // A box for each logical character. Marks and ligature parts (lam-alef)
    // have no glyph of their own: they share the one before.
    let mut boxes = vec![(0.0f32, 0.0f32); n];
    let mut g = 0;
    let mut last = (0.0, 0.0);
    for &i in &order {
        let want = if levels[i].is_rtl() { mirror(chars[i]) } else { chars[i] };
        if g < glyphs.len() && glyphs[g].2 == want {
            last = (glyphs[g].0, glyphs[g].0 + glyphs[g].1);
            g += 1;
        }
        boxes[i] = last;
    }
    (0..=n)
        .map(|p| {
            let x = if p == 0 {
                // Before the first character: at its start side.
                let (l, r) = boxes[0];
                if levels[0].is_rtl() { r } else { l }
            } else {
                // After character p-1, on its end side.
                let (l, r) = boxes[p - 1];
                if levels[p - 1].is_rtl() { l } else { r }
            };
            origin.x + x
        })
        .collect()
}

/// A one-line text field that also shows right-to-left text correctly:
/// the text is drawn in bidi order, the cursor is drawn where it belongs,
/// clicks put it under the pointer, and the arrow keys move it visually
/// (← to the left) in right-to-left text. Otherwise a plain `TextEdit`.
pub fn line_edit(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    configure: impl FnOnce(egui::TextEdit<'_>) -> egui::TextEdit<'_>,
) -> egui::Response {
    let ui_rtl = crate::i18n::is_rtl();
    if !ui_rtl && !has_rtl(text) {
        return configure(egui::TextEdit::singleline(text).id(id)).show(ui).response.response;
    }
    let focused = ui.memory(|m| m.has_focus(id));
    if focused && base_rtl(text, ui_rtl) {
        // Logical order runs right to left here: swap the arrows.
        ui.ctx().input_mut(|i| {
            for e in &mut i.events {
                if let egui::Event::Key { key, .. } = e {
                    *key = match *key {
                        egui::Key::ArrowLeft => egui::Key::ArrowRight,
                        egui::Key::ArrowRight => egui::Key::ArrowLeft,
                        k => k,
                    };
                }
            }
        });
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().widgets.inactive.text_color();
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        let job = egui::text::LayoutJob::simple_singleline(visual(buf.as_str(), ui_rtl), font.clone(), color);
        ui.painter().layout_job(job)
    };
    let cursor_style = ui.visuals().text_cursor.clone();
    let out = ui
        .scope(|ui| {
            // egui would put its cursor by the reordered text: ours instead.
            ui.visuals_mut().text_cursor.stroke.color = egui::Color32::TRANSPARENT;
            // Right-to-left text starts at the right edge, as in browsers.
            let align = if base_rtl(text, ui_rtl) { egui::Align::Max } else { egui::Align::Min };
            configure(egui::TextEdit::singleline(text).id(id).layouter(&mut layouter).horizontal_align(align)).show(ui)
        })
        .inner;
    let response = out.response.response.clone();
    if !response.has_focus() {
        return response;
    }
    let xs = caret_xs(&out.galley, out.galley_pos, text, ui_rtl);
    // A press puts the cursor at the nearest position under the pointer.
    let pressed = ui.input(|i| i.pointer.primary_pressed());
    if pressed
        && response.contains_pointer()
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        let nearest = (0..xs.len()).min_by(|&a, &b| (xs[a] - p.x).abs().total_cmp(&(xs[b] - p.x).abs())).unwrap_or(0);
        let mut state = out.state.clone();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(nearest))));
        state.store(ui.ctx(), id);
        ui.ctx().request_repaint();
    }
    if let Some(range) = egui::TextEdit::load_state(ui.ctx(), id).and_then(|s| s.cursor.char_range()) {
        let p = range.primary.index.0.min(xs.len() - 1);
        let x = xs[p];
        let row_h = out.galley.rows.first().map_or(font.size, |r| r.height());
        let top = out.galley_pos.y;
        let time = ui.input(|i| i.time);
        let period = f64::from(cursor_style.on_duration + cursor_style.off_duration);
        let on = !cursor_style.blink || (time % period) < f64::from(cursor_style.on_duration);
        if on {
            let stroke = egui::Stroke::new(cursor_style.stroke.width.max(1.5), ui.visuals().text_color());
            ui.painter().with_clip_rect(out.text_clip_rect.expand(2.0)).line_segment([egui::pos2(x, top), egui::pos2(x, top + row_h)], stroke);
        }
        if cursor_style.blink {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(cursor_style.on_duration.min(cursor_style.off_duration).max(0.1)));
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Characters of `text` in the order egui puts them on screen.
    fn egui_visual(text: &str) -> String {
        let ctx = egui::Context::default();
        crate::i18n::install_fonts(&ctx);
        let mut out = String::new();
        let mut full = ctx.run_ui(Default::default(), |ui| {
            let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::proportional(14.0), egui::Color32::BLACK);
            // Zero-width glyphs are cursor placeholders, not drawn.
            let mut glyphs: Vec<(f32, char)> =
                galley.rows.iter().flat_map(|r| r.glyphs.iter().filter(|g| g.advance_width > 0.0).map(|g| (g.pos.x, g.chr))).collect();
            glyphs.sort_by(|a, b| a.0.total_cmp(&b.0));
            out = glyphs.into_iter().map(|g| g.1).collect();
        });
        full.textures_delta.clear();
        out
    }

    /// The correct on-screen order (Unicode bidi algorithm).
    /// Brackets mirrored where the bidi level is right to left, as a
    /// renderer should.
    fn bidi_visual(text: &str, rtl: bool) -> String {
        let info = BidiInfo::new(text, rtl.then(Level::rtl));
        let mirrored: String = text.char_indices().map(|(i, c)| if info.levels[i].is_rtl() { mirror(c) } else { c }).collect();
        let info = BidiInfo::new(&mirrored, rtl.then(Level::rtl));
        let para = &info.paragraphs[0];
        info.reorder_line(para, para.range.clone()).into_owned()
    }

    // No lam-alef pairs here: they become one glyph, which this check can't
    // compare character by character.
    const SAMPLES: &[(&str, bool)] = &[
        ("مرحبا بالعالم", false),
        ("نافذة جديدة: 3 تبويب", false),
        ("اضغط Ctrl+Z للتراجع", false),
        ("العربية — Arabic mixed 123", false),
        ("Hello مرحبا world", false),
        ("نقل 12 عنصر في السلة؟", true),
        ("URL", true),
        ("{n} عنصر", true),
    ];

    #[test]
    fn egui_shows_text_in_bidi_order() {
        for &(s, rtl) in SAMPLES {
            assert_eq!(egui_visual(&visual(s, rtl)), bidi_visual(s, rtl), "{s:?}");
        }
    }

    #[test]
    fn brackets_face_the_right_way() {
        // Left to right on screen: the closing bracket (drawn "("), the
        // words, the opening one (drawn ")"), then "إغلاق" at the right
        // (its lam-alef is one glyph, hence one letter less).
        let shown = egui_visual(&visual("إغلاق (مع الإبقاء)", true));
        assert_eq!(shown, "(ءاقبلا عم) قلغإ");
    }

    /// Cursor positions of `text` as a field would show it.
    fn carets(text: &str, ui_rtl: bool) -> Vec<f32> {
        let ctx = egui::Context::default();
        crate::i18n::install_fonts(&ctx);
        let mut xs = Vec::new();
        let mut full = ctx.run_ui(Default::default(), |ui| {
            let galley = ui.painter().layout_no_wrap(visual(text, ui_rtl), egui::FontId::proportional(14.0), egui::Color32::BLACK);
            xs = caret_xs(&galley, egui::Pos2::ZERO, text, ui_rtl);
        });
        full.textures_delta.clear();
        xs
    }

    #[test]
    fn cursor_moves_right_to_left_in_arabic() {
        let xs = carets("سلام عليكم", false);
        // Each next position is further left; the end is at the far left.
        assert!(xs.windows(2).all(|w| w[1] <= w[0]), "{xs:?}");
        assert!(xs[0] > xs[xs.len() - 1]);
    }

    #[test]
    fn cursor_in_mixed_text_jumps_into_the_latin_part() {
        // "مرحبا abc": after the Arabic word and the space the cursor is at
        // the left of the Arabic, where the Latin begins.
        let xs = carets("مرحبا abc", false);
        assert!(xs[0] > xs[5], "start right of the word: {xs:?}");
        // In the Latin part positions go left to right again.
        assert!(xs[7] < xs[8] && xs[8] < xs[9], "{xs:?}");
    }

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(visual("Hello world 123", false), "Hello world 123");
        assert!(!has_rtl("Привет 中文"));
        assert!(has_rtl("x مرحبا"));
    }
}
