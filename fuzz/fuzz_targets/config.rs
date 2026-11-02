//! config.toml as the user wrote it: reading never panics, what is read writes back to the
//! same settings, and key names never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rse::config::Config;
use rse::keymap::{Key, Keymap};

fuzz_target!(|text: &str| {
    for line in text.lines() {
        let _ = Key::parse(line);
    }
    let Ok(config) = Config::parse(text) else { return };
    let _ = Keymap::new(&config.keys);
    let written = toml::to_string_pretty(&config).expect("a config writes as TOML");
    assert_eq!(Config::parse(&written).expect("written config reads back"), config);
});
