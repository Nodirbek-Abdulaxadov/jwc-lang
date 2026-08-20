//! Keyset cursors (queries.md §9.3).
//!
//! A cursor is the ordering tuple of the last row on a page, signed. It is
//! **not** an offset: page 2 asks "the rows after these values", which
//! stays correct while rows are inserted and deleted under it.
//!
//! It is signed because it is a client-supplied predicate. Unsigned, a
//! caller could hand back any tuple and read rows the query's own `where`
//! was meant to keep from them — the cursor would be a second, unchecked
//! filter. A tampered one is a `BadRequest`, not a 500 and not a silent
//! empty page.
//!
//! Format: `v1.<base64url(payload)>.<base64url(hmac)>`, where the payload
//! is a JSON array of the key values as strings. The version prefix is
//! covered by the MAC, so a future format cannot be forged by claiming to
//! be this one.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

const VERSION: &str = "v1";

pub fn encode(secret: &str, keys: &[Option<String>]) -> String {
    let payload = serde_json::to_string(&keys).unwrap_or_else(|_| "[]".into());
    let body = format!("{VERSION}.{}", URL_SAFE_NO_PAD.encode(payload));
    let mac = crate::hash::hmac_sha256_hex(secret, &body);
    format!("{body}.{mac}")
}

/// The key values a cursor carries, or `None` when it is not ours.
///
/// Every failure is the same answer — a malformed cursor and a forged one
/// are the same event from the caller's side, and telling them apart is
/// information the caller has no use for.
pub fn decode(secret: &str, cursor: &str) -> Option<Vec<Option<String>>> {
    let (body, mac) = cursor.rsplit_once('.')?;
    let expected = crate::hash::hmac_sha256_hex(secret, body);
    if !constant_time_eq(mac.as_bytes(), expected.as_bytes()) {
        return None;
    }
    let (version, payload) = body.split_once('.')?;
    if version != VERSION {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Comparison that does not stop at the first differing byte. The MAC is
/// the only thing standing between a caller and an arbitrary predicate, so
/// how long the check takes must not depend on how much of it was right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let keys = vec![Some("2026-01-01T00:00:00Z".to_string()), Some("42".into())];
        let c = encode("s3cret", &keys);
        assert_eq!(decode("s3cret", &c), Some(keys));
    }

    #[test]
    fn a_null_key_survives() {
        let keys = vec![Some("a".to_string()), None];
        let c = encode("s3cret", &keys);
        assert_eq!(decode("s3cret", &c), Some(keys));
    }

    #[test]
    fn another_secret_does_not_verify() {
        let c = encode("s3cret", &[Some("1".into())]);
        assert_eq!(decode("other", &c), None);
    }

    #[test]
    fn a_tampered_payload_does_not_verify() {
        let c = encode("s3cret", &[Some("1".into())]);
        let (body, mac) = c.rsplit_once('.').unwrap();
        let forged = format!("{}.{}", body.replace("v1.", "v1.A"), mac);
        assert_eq!(decode("s3cret", &forged), None);
    }

    #[test]
    fn a_different_version_does_not_verify() {
        // Signed by us, but claiming a format we do not read. It must not
        // be accepted as this one.
        let payload = URL_SAFE_NO_PAD.encode("[\"1\"]");
        let body = format!("v2.{payload}");
        let mac = crate::hash::hmac_sha256_hex("s3cret", &body);
        assert_eq!(decode("s3cret", &format!("{body}.{mac}")), None);
    }

    #[test]
    fn junk_is_not_a_panic() {
        for c in ["", ".", "v1", "v1.", "v1..", "not-a-cursor", "a.b.c.d"] {
            assert_eq!(decode("s3cret", c), None, "{c}");
        }
    }
}
