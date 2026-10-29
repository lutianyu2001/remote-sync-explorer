//! Passwords obscured the way rclone.conf keeps them (`rclone obscure`): AES-256-CTR
//! under a key published in rclone's source, with a random IV in front, in unpadded
//! URL-safe base64. Obscuring is not encryption; it only keeps passwords from being
//! read at a glance.

use std::io::Read as _;

use aes::Aes256;
use aes::cipher::{BlockEncrypt, KeyInit};

/// rclone's `fs/config/obscure.cryptKey`.
const KEY: [u8; 32] = [
    0x9c, 0x93, 0x5b, 0x48, 0x73, 0x0a, 0x55, 0x4d, 0x6b, 0xfd, 0x7c, 0x63, 0xc8, 0x86, 0xa9, 0x2b,
    0xd3, 0x90, 0x19, 0x8e, 0xb8, 0x12, 0x8a, 0xfb, 0xf4, 0xde, 0x16, 0x2b, 0x8b, 0x95, 0xf6, 0x38,
];

/// `clear` obscured like `rclone obscure` does, with a fresh random IV.
pub fn obscure(clear: &str) -> std::io::Result<String> {
    let mut data = vec![0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut data)?;
    let text = crypt(&data, clear.as_bytes());
    data.extend(text);
    Ok(base64_url_encode(&data))
}

/// The clear text of an obscured value, like `rclone reveal`, but only from text exactly as
/// `rclone obscure` writes it (see `base64_url_decode`).
///
/// # Errors
/// When the value is not something `rclone obscure` produced.
pub fn reveal(obscured: &str) -> Result<String, String> {
    let data = base64_url_decode(obscured).ok_or("not obscured: bad base64")?;
    if data.len() < 16 {
        return Err("not obscured: too short".to_owned());
    }
    let (iv, text) = data.split_at(16);
    String::from_utf8(crypt(iv, text)).map_err(|_| "not obscured: not UTF-8".to_owned())
}

/// AES-256-CTR from the 16-byte `iv`; encrypting and decrypting are the same.
fn crypt(iv: &[u8], text: &[u8]) -> Vec<u8> {
    let cipher = Aes256::new(&KEY.into());
    let mut counter = iv.iter().fold(0u128, |n, &b| (n << 8) | u128::from(b));
    let mut out = Vec::with_capacity(text.len());
    for chunk in text.chunks(16) {
        let mut block = counter.to_be_bytes().into();
        cipher.encrypt_block(&mut block);
        out.extend(chunk.iter().zip(block.iter()).map(|(c, k)| c ^ k));
        counter = counter.wrapping_add(1);
    }
    out
}

/// Unpadded URL-safe base64 (Go's `base64.RawURLEncoding`).
pub(crate) fn base64_url_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n =
            chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

/// Unpadded URL-safe base64, read more strictly than by Go's `base64.RawURLEncoding`, which
/// rclone uses: only the one encoding of some bytes is taken, so the bits left over at the end
/// must be zero and a line break is refused, where Go lets both through. `rclone obscure`
/// never writes either.
pub(crate) fn base64_url_decode(s: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(v);
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push(u8::try_from((bits >> n) & 0xff).ok()?);
        }
    }
    // Leftover bits must be zero padding of fewer than 6 bits.
    (n < 6 && bits & ((1 << n) - 1) == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveals_what_rclone_obscured() {
        // Produced by rclone's core/obscure.
        assert_eq!(reveal("3T9PgUymd9w-jZk2CoHnCElf").unwrap(), "pw");
        assert_eq!(reveal("dlGK7HGgNL9oJ5cxgg8FeI5Ne_Yjjg").unwrap(), "secret");
    }

    #[test]
    fn obscures_like_rclone() {
        let a = obscure("pw").unwrap();
        assert_eq!(a.len(), "3T9PgUymd9w-jZk2CoHnCElf".len());
        assert_eq!(reveal(&a).unwrap(), "pw");
        assert_ne!(obscure("pw").unwrap(), a, "a fresh IV each time");
        for clear in ["", "x", "日本語 with spaces", &"long".repeat(20)] {
            assert_eq!(reveal(&obscure(clear).unwrap()).unwrap(), clear);
        }
        assert_eq!(base64_url_encode(b"\xfb\xff"), "-_8");
    }

    #[test]
    fn base64_round_trips_every_short_input() {
        let mut inputs = vec![vec![]];
        inputs.extend((0..=255u8).map(|a| vec![a]));
        inputs.extend((0..=u16::MAX).map(|n| n.to_be_bytes().to_vec()));
        for data in inputs {
            assert_eq!(base64_url_decode(&base64_url_encode(&data)).as_ref(), Some(&data));
        }
        // Whatever decodes is the one encoding of its bytes, and nothing panics.
        let alphabet = ['A', 'B', 'Q', 'g', 'w', '-', '_', '=', '+', 'é'];
        let mut texts = vec![String::new()];
        for len in 1..=4 {
            let start = texts.len() - alphabet.len().pow(len - 1);
            let longer: Vec<String> = texts[start..]
                .iter()
                .flat_map(|t| alphabet.iter().map(move |c| format!("{t}{c}")))
                .collect();
            texts.extend(longer);
        }
        for text in texts {
            if let Some(data) = base64_url_decode(&text) {
                assert_eq!(base64_url_encode(&data), text);
            }
        }
    }

    #[test]
    fn decodes_only_the_text_rclone_writes() {
        assert_eq!(base64_url_decode("QQ"), Some(b"A".to_vec()));
        // Go's RawURLEncoding, which rclone reveal uses, reads each of these as "A".
        for text in ["QR", "QQ\n", "Q\rQ"] {
            assert_eq!(base64_url_decode(text), None, "{text:?}");
        }
        assert_eq!(base64_url_decode("Q"), None);
    }

    #[test]
    fn rejects_other_text() {
        assert!(reveal("plain password").is_err());
        assert!(reveal("c2hvcnQ").is_err());
    }
}
