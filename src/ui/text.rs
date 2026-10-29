//! Width-aware text helpers and number formatting.

use std::borrow::Cow;

use chrono::{DateTime, FixedOffset, Local};
use lazyui_ratatui::text::grapheme_width;
// Measured the way ratatui draws text (per grapheme), as the windows and fields are.
pub use lazyui_ratatui::text::{take_width, width};
use unicode_segmentation::UnicodeSegmentation;

/// `s` as it can be shown: control characters, which would be dropped, and the invisible
/// marks that reorder text on some terminals, appear as `�`. Names and paths from a
/// server go through this, so what is shown is what is there.
pub fn shown(s: &str) -> Cow<'_, str> {
    if s.contains(hidden) {
        Cow::Owned(s.chars().map(|c| if hidden(c) { '\u{fffd}' } else { c }).collect())
    } else {
        Cow::Borrowed(s)
    }
}

/// Characters that do not show as themselves: controls, and the formatting characters
/// without a glyph, among them the bidirectional marks and zero-width spaces. The two
/// joiners (U+200C, U+200D) stay, since emoji and some scripts are written with them.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}'
                | '\u{600}'..='\u{605}'
                | '\u{61c}'
                | '\u{6dd}'
                | '\u{70f}'
                | '\u{890}'..='\u{891}'
                | '\u{8e2}'
                | '\u{180e}'
                | '\u{200b}'
                | '\u{200e}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{110bd}'
                | '\u{110cd}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0001}'
                | '\u{e0020}'..='\u{e007f}'
        )
}

/// The longest suffix of `s` that fits in `w` cells.
pub fn take_width_end(s: &str, w: usize) -> &str {
    let mut used = 0;
    for (i, g) in s.grapheme_indices(true).rev() {
        let gw = grapheme_width(g);
        if used + gw > w {
            return &s[i + g.len()..];
        }
        used += gw;
    }
    s
}

/// Pads with spaces (or cuts) to exactly `w` cells.
pub fn fit(s: &str, w: usize) -> String {
    let t = take_width(s, w);
    format!("{t}{}", " ".repeat(w.saturating_sub(width(t))))
}

pub fn fit_right(s: &str, w: usize) -> String {
    let t = take_width_end(s, w);
    format!("{}{t}", " ".repeat(w.saturating_sub(width(t))))
}

/// Shortens `s` to `w` cells by replacing its middle with `…`.
pub fn ellipsize_middle(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_owned();
    }
    if w == 0 {
        return String::new();
    }
    let tail_w = (w - 1) / 2;
    let head_w = w - 1 - tail_w;
    format!("{}…{}", take_width(s, head_w), take_width_end(s, tail_w))
}

/// Like `ellipsize_middle` but keeps the whole extension (`.tar.gz`) when it fits.
pub fn ellipsize_name(name: &str, w: usize) -> String {
    if width(name) <= w {
        return name.to_owned();
    }
    let ext = extension(name);
    let ext_w = width(ext);
    let tail_w = ((w.saturating_sub(1)) * 2 / 5).max(ext_w);
    if w < 2 || tail_w + 2 > w {
        return ellipsize_middle(name, w);
    }
    let head_w = w - 1 - tail_w;
    format!("{}…{}", take_width(name, head_w), take_width_end(name, tail_w))
}

/// `.gz`, `.tar.gz`, or empty. Leading dots (dot files) do not count.
fn extension(name: &str) -> &str {
    let body = name.trim_start_matches('.');
    let offset = name.len() - body.len();
    let Some(last) = body.rfind('.') else { return "" };
    let before = &body[..last];
    let start = match before.rfind('.') {
        Some(prev) if body[prev + 1..last].eq_ignore_ascii_case("tar") => prev,
        _ => last,
    };
    &name[offset + start..]
}

/// `512 B`, `4.1 KB`, `12 MB`, `1.5 GB` (powers of 1024).
#[allow(clippy::cast_precision_loss)]
pub fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 0 {
        return "?".to_owned();
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.0} {}", UNITS[unit])
    }
}

/// `0:16`, `12:03`, `1:02:03`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn format_eta(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// `2026-10-22 02:32` in `tz`, or local time when `tz` is `None`.
pub fn format_time(t: &DateTime<FixedOffset>, tz: Option<FixedOffset>) -> String {
    match tz {
        Some(tz) => t.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string(),
        None => t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_cannot_be_seen_is_shown_as_a_mark() {
        assert!(matches!(shown("photos 2026.jpg"), Cow::Borrowed(_)));
        assert_eq!(shown("a\u{1b}[2Jb\tc"), "a\u{fffd}[2Jb\u{fffd}c");
        // "invoice-\u{202e}fdp.exe" reads as "invoice-exe.pdf" where it is obeyed.
        assert_eq!(shown("invoice-\u{202e}fdp.exe"), "invoice-\u{fffd}fdp.exe");
        assert_eq!(shown("\u{9b}31m\u{2066}"), "\u{fffd}31m\u{fffd}");
        assert_eq!(shown("中文 ñ é"), "中文 ñ é");
        // Zero-width characters would make "ab" of "a\u{200b}b"; emoji keep their joiner.
        assert_eq!(shown("a\u{200b}b\u{feff}"), "a\u{fffd}b\u{fffd}");
        assert_eq!(shown("👩\u{200d}💻"), "👩\u{200d}💻");
    }

    #[test]
    fn middle_ellipsis_keeps_both_ends() {
        assert_eq!(ellipsize_middle("abcdefghij", 10), "abcdefghij");
        assert_eq!(ellipsize_middle("abcdefghij", 7), "abc…hij");
        assert_eq!(width(&ellipsize_middle("abcdefghij", 6)), 6);
        assert_eq!(ellipsize_middle("abc", 0), "");
    }

    #[test]
    fn name_ellipsis_keeps_extension() {
        let e = ellipsize_name("very-long-file-name.tar.gz", 23);
        assert_eq!(width(&e), 23);
        assert!(e.ends_with(".tar.gz"), "{e}");
        assert!(e.starts_with("very-long-fil"), "{e}");
        let short = ellipsize_name("a-very-long-report-name-final.pdf", 12);
        assert_eq!(take_width_end(&short, 4), ".pdf");
        assert_eq!(ellipsize_name("short.txt", 12), "short.txt");
        assert_eq!(width(&ellipsize_name("x.averyveryverylongextension", 8)), 8);
    }

    #[test]
    fn extension_rules() {
        assert_eq!(extension("a.tar.gz"), ".tar.gz");
        assert_eq!(extension("a.b.gz"), ".gz");
        assert_eq!(extension(".bashrc"), "");
        assert_eq!(extension(".config.toml"), ".toml");
        assert_eq!(extension("Makefile"), "");
    }

    #[test]
    fn wide_characters_count_double() {
        assert_eq!(width("文件"), 4);
        assert_eq!(take_width("文件名", 5), "文件");
        assert_eq!(fit("文件", 5), "文件 ");
        assert_eq!(width(&ellipsize_middle("很长的中文文件名字", 9)), 9);
    }

    #[test]
    fn emoji_and_control_characters_match_the_drawn_width() {
        let name = "❤️ Paris with family and friends 2026 summer.jpg";
        for w in 1..40 {
            let e = ellipsize_name(name, w);
            assert!(width(&e) <= w, "{w}: {e}");
            assert_eq!(width(&fit(&e, w)), w);
            assert_eq!(width(&fit_right(&e, w)), w);
        }
        assert_eq!(width("❤️"), 2);
        assert_eq!(width("a\tb"), 2);
        assert_eq!(width(&fit("a\tb\u{1b}", 4)), 4);
        assert_eq!(take_width("❤️x", 1), "");
    }

    #[test]
    fn sizes_and_durations() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(4198), "4.1 KB");
        assert_eq!(human_size(12 * 1024 * 1024 + 5), "12 MB");
        assert_eq!(human_size(1_610_612_736), "1.5 GB");
        assert_eq!(human_size(-1), "?");
        assert_eq!(format_eta(16.4), "0:16");
        assert_eq!(format_eta(3723.0), "1:02:03");
    }

    #[test]
    fn time_in_given_zone() {
        let t = DateTime::parse_from_rfc3339("2026-10-22T02:32:59+08:00").unwrap();
        assert_eq!(format_time(&t, FixedOffset::east_opt(0)), "2026-10-21 18:32");
    }
}
