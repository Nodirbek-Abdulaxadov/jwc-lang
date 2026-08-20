//! Pure hash / HMAC helpers shared by the interpreter and the native AOT
//! prelude (`native_prelude_crypto.rs.in`). All functions are sync, take and
//! return owned `String`s, and emit lowercase hex.
//!
//! The `Digest` trait is shared across `sha2`, `sha1`, and `md-5` — they all
//! depend on the `digest` crate, and `use sha2::Digest` brings the
//! `update`/`finalize` methods into scope for every hasher below.

use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

pub fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    to_hex(&h.finalize())
}

/// The same over raw bytes. An archive is not text, and going through a
/// lossy UTF-8 conversion to reuse the string form would hash something
/// other than what was uploaded.
pub fn sha256_hex_bytes(input: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(input);
    to_hex(&h.finalize())
}

pub fn sha1_hex(input: &str) -> String {
    let mut h = Sha1::new();
    h.update(input.as_bytes());
    to_hex(&h.finalize())
}

pub fn md5_hex(input: &str) -> String {
    let mut h = Md5::new();
    h.update(input.as_bytes());
    to_hex(&h.finalize())
}

pub fn hmac_sha256_hex(key: &str, msg: &str) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(msg.as_bytes());
    to_hex(&mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn sha1_known_vector() {
        assert_eq!(sha1_hex("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn md5_known_vector() {
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn hmac_sha256_rfc4231_case2() {
        // RFC 4231 Test Case 2.
        assert_eq!(
            hmac_sha256_hex("Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
