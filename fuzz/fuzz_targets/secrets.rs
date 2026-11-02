//! Obscured passwords, the remembered-password file and ssh's version banner: decoding
//! never panics and is the exact inverse of encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rse::fuzz::{base64_url_decode, base64_url_encode, openssh_version, password_entry, reveal};

fuzz_target!(|input: (&str, &[u8])| {
    let (text, data) = input;
    let _ = reveal(text);
    if let Some(bytes) = base64_url_decode(text) {
        assert_eq!(base64_url_encode(&bytes), text, "only one encoding decodes");
    }
    assert_eq!(base64_url_decode(&base64_url_encode(data)).as_deref(), Some(data));
    for line in text.lines() {
        if let Some((login, obscured)) = password_entry(line) {
            assert!(!login.is_empty() && !obscured.is_empty());
            assert!(
                !login.contains(char::is_whitespace) && !obscured.contains(char::is_whitespace)
            );
        }
    }
    let _ = openssh_version(text);
});
