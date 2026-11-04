//! Interface text in English and Simplified Chinese, from the Fluent files in `locales/`
//! (built into the binary).
//!
//! `tr!(id)` or `tr!(id, name = value, ...)` looks a message up in the language
//! `set` picked at startup; a message missing from it comes from English.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

pub use fluent_templates::fluent_bundle::FluentValue;
use fluent_templates::{LanguageIdentifier, Loader, langid, static_loader};

use crate::config::Language;

static_loader! {
    static LOCALES = {
        locales: "./locales",
        fallback_language: "en",
        // No Unicode isolation marks around arguments: terminals show them or count
        // them as cells.
        customise: |bundle| bundle.set_use_isolating(false),
    };
}

static EN: LanguageIdentifier = langid!("en");
static ZH_CN: LanguageIdentifier = langid!("zh-CN");

static CHINESE: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    /// Tests switch the language of their own thread, leaving the others in English.
    static THREAD_CHINESE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Sets the interface language; `Auto` follows the locale as gettext does ([`locale`]),
/// and without one the system's language. Call it before starting threads: it also sets
/// the C library's message language.
pub fn set(language: Language) {
    let chinese = match language {
        Language::Auto => locale(|name| std::env::var(name).ok())
            .or_else(sys_locale::get_locale)
            .is_some_and(|l| l.starts_with("zh")),
        Language::En => false,
        Language::ZhCn => true,
    };
    CHINESE.store(chinese, Ordering::Relaxed);
    set_system_messages(chinese);
}

/// The locale the environment names for messages, as gettext reads it: the first set of
/// `LC_ALL`, `LC_MESSAGES` and `LANG`, unless `LANGUAGE` lists one ahead of it; never
/// `LANGUAGE` when that is `C` or `POSIX`. None when none is set, which leaves the
/// system's language: sys-locale reads these variables on Linux, but on macOS only the
/// language of the system's settings.
fn locale(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty());
    let named = ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().find_map(set);
    // As glibc's gettext: `C.UTF-8` and the like still take `LANGUAGE`.
    if named.as_deref().is_some_and(|l| l == "C" || l == "POSIX") {
        return named;
    }
    let first =
        set("LANGUAGE").and_then(|l| l.split(':').find(|l| !l.is_empty()).map(str::to_owned));
    first.or(named)
}

/// Has glibc translate system errors (`No such file or directory`), which `io::Error`
/// shows, into the interface language. `build.rs` checks the declaration below against
/// libintl.h.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
fn set_system_messages(chinese: bool) {
    unsafe extern "C" {
        fn bind_textdomain_codeset(
            domain: *const libc::c_char,
            codeset: *const libc::c_char,
        ) -> *mut libc::c_char;
    }
    let locales: &[&std::ffi::CStr] =
        if chinese { &[c"zh_CN.UTF-8", c"zh_CN.utf8"] } else { &[c"C"] };
    // Rust leaves LC_CTYPE at "C" (ASCII); without this glibc turns Chinese into "?".
    // SAFETY: both strings are NUL-terminated literals. `set` runs before rse starts any
    // thread; the Go runtime of the embedded build has threads by then, but Go never reads
    // libc's locale or text domains.
    unsafe { bind_textdomain_codeset(c"libc".as_ptr(), c"UTF-8".as_ptr()) };
    for locale in locales {
        // SAFETY: a NUL-terminated literal. setlocale must not race other locale users: `set`
        // runs before rse starts any thread, and Go's threads never use libc's locale.
        if !unsafe { libc::setlocale(libc::LC_MESSAGES, locale.as_ptr()) }.is_null() {
            break;
        }
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn set_system_messages(_chinese: bool) {}

#[cfg(test)]
pub fn set_for_this_thread(language: Language) {
    THREAD_CHINESE.set(Some(language == Language::ZhCn));
}

fn current() -> &'static LanguageIdentifier {
    #[cfg(test)]
    if let Some(chinese) = THREAD_CHINESE.get() {
        return if chinese { &ZH_CN } else { &EN };
    }
    if CHINESE.load(Ordering::Relaxed) { &ZH_CN } else { &EN }
}

/// The message `id` with `args`; the id itself if no language has it.
pub fn text<'a>(
    id: &str,
    args: impl IntoIterator<Item = (&'static str, FluentValue<'a>)>,
) -> String {
    let args: HashMap<Cow<'static, str>, FluentValue> =
        args.into_iter().map(|(k, v)| (Cow::Borrowed(k), v)).collect();
    let found = if args.is_empty() {
        LOCALES.try_lookup(current(), id)
    } else {
        LOCALES.try_lookup_with_args(current(), id, &args)
    };
    found.unwrap_or_else(|| id.to_owned())
}

/// `tr!(id)`, `tr!(id, count = n, name = "x")`: a message in the current language.
#[macro_export]
macro_rules! tr {
    ($id:expr) => {
        $crate::i18n::text($id, ::std::iter::empty())
    };
    ($id:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::text(
            $id,
            [$((stringify!($name), $crate::i18n::FluentValue::from($value))),+],
        )
    };
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    #[test]
    fn the_locale_is_read_as_gettext_reads_it() {
        fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
            let pairs: Vec<(String, String)> =
                pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
            move |name| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        }
        assert_eq!(locale(env(&[])), None);
        assert_eq!(locale(env(&[("LANG", "")])), None);
        assert_eq!(locale(env(&[("LANG", "zh_CN.UTF-8")])).as_deref(), Some("zh_CN.UTF-8"));
        let messages = env(&[("LANG", "zh_CN.UTF-8"), ("LC_MESSAGES", "en_US.UTF-8")]);
        assert_eq!(locale(messages).as_deref(), Some("en_US.UTF-8"));
        let all = env(&[("LC_ALL", "zh_TW.UTF-8"), ("LC_MESSAGES", "en_US.UTF-8")]);
        assert_eq!(locale(all).as_deref(), Some("zh_TW.UTF-8"));
        let listed = env(&[("LANGUAGE", ":zh_CN:en"), ("LANG", "en_US.UTF-8")]);
        assert_eq!(locale(listed).as_deref(), Some("zh_CN"));
        assert_eq!(locale(env(&[("LANGUAGE", "zh_CN")])).as_deref(), Some("zh_CN"));
        for c in ["C", "POSIX"] {
            let plain = env(&[("LANGUAGE", "zh_CN"), ("LANG", c)]);
            assert_eq!(locale(plain).as_deref(), Some(c), "{c}");
        }
        let utf8 = env(&[("LANGUAGE", "zh_CN:en"), ("LANG", "C.UTF-8")]);
        assert_eq!(locale(utf8).as_deref(), Some("zh_CN"));
    }

    fn ids(file: &str) -> BTreeSet<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales").join(file);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
        text.lines()
            .filter(|l| l.starts_with(|c: char| c.is_ascii_alphabetic()))
            .filter_map(|l| l.split_once('=').map(|(id, _)| id.trim().to_owned()))
            .collect()
    }

    /// Every `tr!("…")` in the sources, and the help text of every key binding.
    fn used() -> BTreeSet<String> {
        fn walk(dir: &Path, out: &mut BTreeSet<String>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    for part in text.split("tr!(").skip(1) {
                        let id =
                            part.trim_start().strip_prefix('"').and_then(|p| p.split_once('"'));
                        let valid = |id: &str| {
                            !id.is_empty()
                                && id.chars().all(|c| {
                                    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
                                })
                        };
                        if let Some((id, _)) = id.filter(|(id, _)| valid(id)) {
                            out.insert(id.to_owned());
                        }
                    }
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
        out.extend(crate::keymap::help_ids());
        out
    }

    #[test]
    fn both_languages_have_every_message_in_use() {
        let (en, zh) = (ids("en/rse.ftl"), ids("zh-CN/rse.ftl"));
        assert_eq!(en, zh, "the two files define different messages");
        let missing: Vec<_> = used().difference(&en).cloned().collect();
        assert!(missing.is_empty(), "not in the Fluent files: {missing:?}");
        let unused: Vec<_> = en.difference(&used()).cloned().collect();
        assert!(unused.is_empty(), "never used: {unused:?}");
    }

    #[test]
    fn arguments_and_plurals() {
        set_for_this_thread(Language::En);
        assert_eq!(tr!("items", count = 1), "1 item");
        assert_eq!(tr!("items", count = 3), "3 items");
        set_for_this_thread(Language::ZhCn);
        assert_eq!(tr!("items", count = 3), "3 项");
        assert_eq!(text("no-such-message", std::iter::empty()), "no-such-message");
    }
}
