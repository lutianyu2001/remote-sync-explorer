//! Names and messages drawn in cells: cutting and wrapping keep to the width, lose no
//! text and always end; what cannot be seen is shown as a mark, and nothing else changes.
#![no_main]

use std::borrow::Cow;

use libfuzzer_sys::fuzz_target;
use rext::fuzz::wrap;
use rext::ui::text::{
    ellipsize_middle, ellipsize_name, fit, fit_right, format_eta, human_size, shown, take_width,
    take_width_end, width,
};
use unicode_segmentation::UnicodeSegmentation as _;

fuzz_target!(|input: (&str, u8, i64, f64)| {
    let (s, w, bytes, secs) = input;
    let w = usize::from(w);
    assert!(width(take_width(s, w)) <= w);
    assert!(width(take_width_end(s, w)) <= w);
    assert!(s.starts_with(take_width(s, w)) && s.ends_with(take_width_end(s, w)));
    assert_eq!(width(&fit(s, w)), w);
    assert_eq!(width(&fit_right(s, w)), w);
    assert!(width(&ellipsize_middle(s, w)) <= w);
    assert!(width(&ellipsize_name(s, w)) <= w);
    let lines = wrap(s, w);
    for line in &lines {
        // Wider only when it holds a single visible character, wider than the whole line;
        // characters of no width may come along with it.
        let visible = line.graphemes(true).filter(|g| width(g) > 0).count();
        assert!(width(line) <= w || visible == 1, "{line:?} is wider than {w}");
    }
    let kept: String = lines.concat().chars().filter(|c| *c != ' ').collect();
    let given: String = s.chars().filter(|c| !matches!(c, ' ' | '\n')).collect();
    assert_eq!(kept, given, "wrap lost text");
    let _ = (human_size(bytes), format_eta(secs));
    let marked = shown(s);
    assert_eq!(marked.chars().count(), s.chars().count());
    for (c, m) in s.chars().zip(marked.chars()) {
        // An oracle of its own: what must be marked, and what never may be.
        let unseen = c.is_control()
            || matches!(
                c,
                '\u{61c}' | '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{2060}' | '\u{feff}'
            )
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        let plain = c.is_alphanumeric() || c.is_ascii_graphic() || matches!(c, ' ' | '\u{200d}');
        if unseen {
            assert_eq!(m, '\u{fffd}', "{c:?} was not marked");
        } else if plain {
            assert_eq!(m, c, "{c:?} was changed");
        } else {
            assert!(m == c || m == '\u{fffd}', "{c:?} became {m:?}");
        }
    }
    if let Cow::Owned(_) = marked {
        assert_ne!(marked, s, "copied without a change");
    }
});
