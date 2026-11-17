//! PBKDF2-HMAC-SHA256 password hashing for standalone users (ADR 0012).
//!
//! Pure Rust (`pbkdf2` + `sha2`) so it compiles for `wasm32-unknown-unknown`
//! and unit-tests natively. WebCrypto is deliberately not used: Workers caps
//! PBKDF2 at 100,000 iterations, below the 210,000 this module uses.
//!
//! Stored format (one TEXT column):
//!
//! ```text
//! pbkdf2-sha256$<iterations>$<salt_b64>$<hash_b64>
//! ```
//!
//! Standard base64 (padded) for both blobs. The iteration count travels with
//! the hash, so it can be raised later without invalidating stored rows.
//! Verification is a constant-time compare ([`subtle`]) and any malformed
//! string verifies as `false` rather than erroring — a corrupt row must not
//! crash a login, and the caller already treats "no match" as 401.
//!
//! Salts come from the existing `getrandom` 0.2 dependency (OS randomness
//! natively, Web Crypto on wasm). `uuid` is deliberately not added: a second
//! `getrandom` version would be pulled in for no benefit.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

/// Algorithm identifier at the head of every stored hash.
pub const PASSWORD_ALGORITHM: &str = "pbkdf2-sha256";

/// Production work factor. High enough to be expensive offline, low enough to
/// stay comfortably inside a Worker's CPU budget.
pub const PASSWORD_ITERATIONS: u32 = 210_000;

/// Random salt length in bytes.
pub const PASSWORD_SALT_LEN: usize = 16;

/// Derived key length in bytes (SHA-256 output).
pub const PASSWORD_KEY_LEN: usize = 32;

/// Lowest accepted iteration count when parsing a stored hash.
pub const MIN_PASSWORD_ITERATIONS: u32 = 1;

/// Highest accepted iteration count when parsing a stored hash. Bounds the CPU
/// a corrupt/hostile row can force a verifier to burn.
pub const MAX_PASSWORD_ITERATIONS: u32 = 1_000_000;

/// Errors produced while hashing a password.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PasswordError {
    /// 500 — the OS/Web Crypto randomness source failed.
    #[error("failed to obtain randomness for the password salt")]
    Entropy,
    /// A caller-supplied work factor outside
    /// [`MIN_PASSWORD_ITERATIONS`]`..=`[`MAX_PASSWORD_ITERATIONS`].
    #[error("password hash iterations must be between {MIN_PASSWORD_ITERATIONS} and {MAX_PASSWORD_ITERATIONS}")]
    InvalidIterations,
}

/// Hashes `password` with the production work factor
/// ([`PASSWORD_ITERATIONS`]) and a fresh random salt.
pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    hash_password_with_iterations(password, PASSWORD_ITERATIONS)
}

/// Hashes `password` with an explicit work factor — the test path (a few
/// thousand iterations keeps the suite fast) and the future upgrade path.
///
/// Each call draws a fresh [`PASSWORD_SALT_LEN`]-byte salt, so hashing the same
/// password twice yields two different strings.
pub fn hash_password_with_iterations(
    password: &str,
    iterations: u32,
) -> Result<String, PasswordError> {
    if !(MIN_PASSWORD_ITERATIONS..=MAX_PASSWORD_ITERATIONS).contains(&iterations) {
        return Err(PasswordError::InvalidIterations);
    }
    let mut salt = [0u8; PASSWORD_SALT_LEN];
    getrandom::getrandom(&mut salt).map_err(|_| PasswordError::Entropy)?;
    let mut key = [0u8; PASSWORD_KEY_LEN];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut key);
    Ok(format!(
        "{PASSWORD_ALGORITHM}${iterations}${}${}",
        STANDARD.encode(salt),
        STANDARD.encode(key)
    ))
}

/// Verifies `password` against a stored hash string.
///
/// Returns `false` for a wrong password **and** for any malformed stored value
/// (wrong algorithm, missing fields, bad base64, out-of-range iterations,
/// wrong key length). Never panics and never leaks how close a guess was.
pub fn verify_password(stored: &str, password: &str) -> bool {
    let Some((iterations, salt, expected)) = parse_stored_hash(stored) else {
        return false;
    };
    let mut actual = [0u8; PASSWORD_KEY_LEN];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut actual);
    actual.ct_eq(expected.as_slice()).into()
}

/// Parses `pbkdf2-sha256$<iterations>$<salt_b64>$<hash_b64>` into its parts.
/// The hash must decode to exactly [`PASSWORD_KEY_LEN`] bytes and the salt must
/// be non-empty.
fn parse_stored_hash(stored: &str) -> Option<(u32, Vec<u8>, [u8; PASSWORD_KEY_LEN])> {
    let mut parts = stored.split('$');
    let algorithm = parts.next()?;
    let iterations = parts.next()?;
    let salt_b64 = parts.next()?;
    let hash_b64 = parts.next()?;
    if parts.next().is_some() || algorithm != PASSWORD_ALGORITHM {
        return None;
    }
    let iterations: u32 = iterations.parse().ok()?;
    if !(MIN_PASSWORD_ITERATIONS..=MAX_PASSWORD_ITERATIONS).contains(&iterations) {
        return None;
    }
    let salt = STANDARD.decode(salt_b64).ok()?;
    if salt.is_empty() {
        return None;
    }
    let hash = STANDARD.decode(hash_b64).ok()?;
    let expected: [u8; PASSWORD_KEY_LEN] = hash.try_into().ok()?;
    Some((iterations, salt, expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fast work factor for tests; the format still carries it.
    const TEST_ITERATIONS: u32 = 1_000;

    #[test]
    fn roundtrip_accepts_the_correct_password() {
        let stored = hash_password_with_iterations("correct horse battery staple", TEST_ITERATIONS)
            .expect("hash");
        assert!(verify_password(&stored, "correct horse battery staple"));
    }

    #[test]
    fn wrong_password_is_rejected() {
        let stored = hash_password_with_iterations("secret", TEST_ITERATIONS).expect("hash");
        for wrong in ["Secret", "secret ", " secret", "secre", "", "secret2"] {
            assert!(!verify_password(&stored, wrong), "{wrong:?}");
        }
    }

    #[test]
    fn tampered_hash_is_rejected() {
        let stored = hash_password_with_iterations("secret", TEST_ITERATIONS).expect("hash");
        let (head, hash) = stored.rsplit_once('$').expect("hash segment");
        // Flip the first character to another base64 character.
        let mut chars: Vec<char> = hash.chars().collect();
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        let tampered = format!("{head}${}", chars.into_iter().collect::<String>());
        assert!(!verify_password(&tampered, "secret"));

        // Truncating the hash (wrong key length) must also fail closed.
        let truncated = format!("{head}${}", &hash[..hash.len() - 4]);
        assert!(!verify_password(&truncated, "secret"));

        // And the salt is committed to: changing it invalidates the hash.
        let mut parts: Vec<&str> = stored.split('$').collect();
        let other = hash_password_with_iterations("secret", TEST_ITERATIONS).expect("hash");
        parts[2] = other.split('$').nth(2).expect("salt");
        assert!(!verify_password(&parts.join("$"), "secret"));
    }

    #[test]
    fn unicode_passwords_roundtrip() {
        for password in ["pässwörd", "🔐🔐🔐", "パスワード", "naïve café"] {
            let stored = hash_password_with_iterations(password, TEST_ITERATIONS).expect("hash");
            assert!(verify_password(&stored, password), "{password:?}");
            assert!(
                !verify_password(&stored, &format!("{password}x")),
                "{password:?}"
            );
        }
    }

    #[test]
    fn stored_format_carries_the_iteration_count() {
        let stored = hash_password_with_iterations("secret", 2_000).expect("hash");
        let parts: Vec<&str> = stored.split('$').collect();
        assert_eq!(parts.len(), 4, "{stored}");
        assert_eq!(parts[0], PASSWORD_ALGORITHM);
        assert_eq!(parts[1], "2000");
        assert_eq!(
            STANDARD.decode(parts[2]).expect("salt b64").len(),
            PASSWORD_SALT_LEN
        );
        assert_eq!(
            STANDARD.decode(parts[3]).expect("hash b64").len(),
            PASSWORD_KEY_LEN
        );
        // Verification honors the stored iteration count, not the constant.
        assert!(verify_password(&stored, "secret"));
    }

    #[test]
    fn production_factor_uses_210k_iterations() {
        assert_eq!(PASSWORD_ITERATIONS, 210_000);
        let stored = hash_password("secret").expect("hash");
        assert!(stored.starts_with("pbkdf2-sha256$210000$"), "{stored}");
        assert!(verify_password(&stored, "secret"));
        assert!(!verify_password(&stored, "Secret"));
    }

    #[test]
    fn two_hashes_of_the_same_password_differ() {
        let a = hash_password_with_iterations("secret", TEST_ITERATIONS).expect("hash");
        let b = hash_password_with_iterations("secret", TEST_ITERATIONS).expect("hash");
        assert_ne!(a, b, "a fresh random salt must make every hash unique");
        assert!(verify_password(&a, "secret") && verify_password(&b, "secret"));
    }

    #[test]
    fn malformed_stored_hashes_fail_closed() {
        let valid = hash_password_with_iterations("secret", TEST_ITERATIONS).expect("hash");
        let valid_salt = valid.split('$').nth(2).expect("salt");
        let valid_hash = valid.split('$').nth(3).expect("hash");
        for bad in [
            String::new(),
            "secret".to_string(),
            "pbkdf2-sha256".to_string(),
            "pbkdf2-sha256$1000".to_string(),
            "pbkdf2-sha256$1000$salt".to_string(),
            format!("bcrypt$1000${valid_salt}${valid_hash}"),
            format!("pbkdf2-sha256$zero${valid_salt}${valid_hash}"),
            format!("pbkdf2-sha256$0${valid_salt}${valid_hash}"),
            format!("pbkdf2-sha256$1000001${valid_salt}${valid_hash}"),
            format!("pbkdf2-sha256$1000$${valid_hash}"),
            format!("pbkdf2-sha256$1000$!!!${valid_hash}"),
            format!("pbkdf2-sha256$1000${valid_salt}$!!!"),
            format!("pbkdf2-sha256$1000${valid_salt}$"),
            format!("pbkdf2-sha256$1000${valid_salt}${valid_hash}$extra"),
            format!("PBKDF2-SHA256$1000${valid_salt}${valid_hash}"),
            format!("pbkdf2-sha256$1000${valid_salt}${valid_hash}AAAA"),
        ] {
            assert!(!verify_password(&bad, "secret"), "{bad:?} must not verify");
        }
    }

    #[test]
    fn iteration_bounds_are_validated_on_hash() {
        assert_eq!(
            hash_password_with_iterations("secret", 0),
            Err(PasswordError::InvalidIterations)
        );
        assert_eq!(
            hash_password_with_iterations("secret", MAX_PASSWORD_ITERATIONS + 1),
            Err(PasswordError::InvalidIterations)
        );
        assert!(hash_password_with_iterations("secret", MIN_PASSWORD_ITERATIONS).is_ok());
    }
}
