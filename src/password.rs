use anyhow::{anyhow, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Argon2, PasswordHash};

/// Hash a plaintext password with Argon2id using a fresh random salt.
/// Returns the standard PHC-format hash string (`$argon2id$v=19$m=...$...`).
pub fn hash_password(pwd: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hasher = Argon2::default();
    hasher
        .hash_password(pwd.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow!("hash_password failed: {e}"))
}

/// Verify a plaintext password against a previously stored PHC-format hash.
/// Returns `Ok(true)` on match, `Ok(false)` on mismatch. Returns `Err` only if
/// the stored hash is malformed.
pub fn verify_password(pwd: &str, stored_hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(stored_hash)
        .map_err(|e| anyhow!("verify_password: malformed stored hash: {e}"))?;
    Ok(Argon2::default()
        .verify_password(pwd.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_succeeds() {
        let h = hash_password("hunter2").unwrap();
        assert!(verify_password("hunter2", &h).unwrap());
    }

    #[test]
    fn verify_rejects_wrong_password() {
        let h = hash_password("hunter2").unwrap();
        assert!(!verify_password("wrong", &h).unwrap());
    }

    #[test]
    fn each_hash_is_different_due_to_random_salt() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn verify_errors_on_malformed_hash() {
        let err = verify_password("x", "not-a-real-phc-hash").unwrap_err().to_string();
        assert!(err.contains("malformed"));
    }
}
