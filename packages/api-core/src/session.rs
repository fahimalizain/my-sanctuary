//! Sealed session cookies (AES-256-GCM).
//!
//! A session cookie carries identity only — `id`, `email`, `name`, `picture` —
//! never Google tokens. The token format is `base64url(nonce || ciphertext)`
//! (no padding), where the plaintext is JSON containing the user plus an `exp`
//! Unix timestamp 7 days in the future.
//!
//! Design notes:
//! - Key derivation: `SHA-256(secret)` produces the 32-byte AES-256 key. The
//!   secret lives in the Worker's `SESSION_SECRET` secret binding (>= 32 bytes,
//!   enforced by [`Config`](crate::Config)).
//! - The nonce is 12 random bytes from `getrandom` (OS randomness natively,
//!   Web Crypto on `wasm32-unknown-unknown`).
//! - `now` (Unix seconds) is passed in instead of reading `SystemTime`, which
//!   is unreliable on wasm. The Worker supplies `Date.now() / 1000`.
//! - Old gorilla/securecookie sessions are intentionally invalid: they fail
//!   unsealing and are treated as logged out.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Name of the sealed session cookie.
pub const SESSION_COOKIE_NAME: &str = "sanctuary-session";

/// Cookie lifetime in seconds (7 days), also used as the `Max-Age` attribute.
pub const SESSION_DURATION_SECS: i64 = 7 * 24 * 60 * 60;

/// Size of the AES-256-GCM nonce in bytes.
const NONCE_LEN: usize = 12;
/// Size of the AES-256-GCM authentication tag in bytes.
const TAG_LEN: usize = 16;

/// Identity carried by a session cookie.
///
/// Mirrors the frontend `User` type in `apps/web/lib/auth.tsx` field for field;
/// `picture` may be an empty string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUser {
    pub id: String,
    pub email: String,
    pub name: String,
    pub picture: String,
}

/// JSON payload sealed inside the cookie: the user identity plus an expiry.
#[derive(Debug, Serialize, Deserialize)]
struct SessionPayload {
    #[serde(flatten)]
    user: SessionUser,
    exp: i64,
}

/// Errors produced while sealing or unsealing a session cookie.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("session token is not valid base64url")]
    InvalidBase64,
    #[error("session token payload is malformed")]
    InvalidPayload,
    #[error("session token failed authentication (tampered or wrong secret)")]
    AuthenticationFailed,
    #[error("session token has expired")]
    Expired,
    #[error("failed to obtain randomness for the session nonce")]
    Entropy,
}

/// Response body for `GET /auth/me`.
///
/// Serializes to `{"user":null}` when logged out, `{"user":{...}}` when logged in.
#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub user: Option<SessionUser>,
}

/// Response body for `POST /auth/logout`.
#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub success: bool,
}

/// Derives the 32-byte AES-256 key from the shared secret (SHA-256).
fn derive_key(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// Seals a session user into an opaque cookie value: `base64url(nonce || ciphertext)`.
///
/// The cookie is valid for [`SESSION_DURATION_SECS`] seconds from `now`.
pub fn seal(secret: &str, user: &SessionUser, now: i64) -> Result<String, SessionError> {
    let cipher = Aes256Gcm::new_from_slice(&derive_key(secret))
        .expect("SHA-256 output is always a valid 32-byte AES key");

    let payload = SessionPayload {
        user: user.clone(),
        exp: now + SESSION_DURATION_SECS,
    };
    let plaintext =
        serde_json::to_vec(&payload).map_err(|_| SessionError::InvalidPayload)?;

    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|_| SessionError::Entropy)?;

    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
        .map_err(|_| SessionError::AuthenticationFailed)?;

    let mut token = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    token.extend_from_slice(&nonce);
    token.extend_from_slice(&ciphertext);
    Ok(URL_SAFE_NO_PAD.encode(token))
}

/// Unseals a cookie value, failing on tampering, a wrong secret, garbage input,
/// or an expired session. Returns the identity carried by the cookie.
pub fn unseal(secret: &str, token: &str, now: i64) -> Result<SessionUser, SessionError> {
    let cipher = Aes256Gcm::new_from_slice(&derive_key(secret))
        .expect("SHA-256 output is always a valid 32-byte AES key");

    let raw = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| SessionError::InvalidBase64)?;
    if raw.len() < NONCE_LEN + TAG_LEN {
        return Err(SessionError::InvalidPayload);
    }
    let (nonce, ciphertext) = raw.split_at(NONCE_LEN);
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| SessionError::AuthenticationFailed)?;

    let payload: SessionPayload =
        serde_json::from_slice(&plaintext).map_err(|_| SessionError::InvalidPayload)?;
    if now >= payload.exp {
        return Err(SessionError::Expired);
    }
    Ok(payload.user)
}

/// Builds the `Set-Cookie` header value that establishes the session cookie.
///
/// `sealed` is the output of [`seal`]. The `Secure` attribute is included only
/// when `secure` is true (production; never in local development).
pub fn session_cookie_header(sealed: &str, secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE_NAME}={sealed}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_DURATION_SECS}{secure_attr}"
    )
}

/// Builds the `Set-Cookie` header value that expires the session cookie.
pub fn clear_session_cookie_header(secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("{SESSION_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure_attr}")
}

/// Extracts the value of the named cookie from a raw `Cookie` request header.
///
/// Returns `None` when the header is absent, empty, or does not contain the
/// cookie. The match is exact on the cookie name; surrounding whitespace on
/// both names and values is ignored.
pub fn cookie_value_from_header<'a>(cookie_header: Option<&'a str>, name: &str) -> Option<&'a str> {
    let header = cookie_header?;
    for part in header.split(';') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        if key.trim() == name {
            return Some(value.trim());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef"; // 32 bytes
    const OTHER_SECRET: &str = "fedcba9876543210fedcba9876543210"; // 32 bytes

    fn user() -> SessionUser {
        SessionUser {
            id: "google-123".to_string(),
            email: "ada@example.com".to_string(),
            name: "Ada Lovelace".to_string(),
            picture: "https://example.com/ada.png".to_string(),
        }
    }

    #[test]
    fn seal_unseal_roundtrip() {
        let token = seal(SECRET, &user(), 0).unwrap();
        let recovered = unseal(SECRET, &token, 0).unwrap();
        assert_eq!(recovered, user());
    }

    #[test]
    fn roundtrip_token_valid_until_expiry() {
        let token = seal(SECRET, &user(), 100).unwrap();
        // exp = 100 + 604800; valid one second before expiry.
        assert_eq!(unseal(SECRET, &token, 100 + SESSION_DURATION_SECS - 1).unwrap(), user());
    }

    #[test]
    fn two_seals_produce_distinct_tokens() {
        let a = seal(SECRET, &user(), 0).unwrap();
        let b = seal(SECRET, &user(), 0).unwrap();
        assert_ne!(a, b, "random nonce must make every seal unique");
    }

    #[test]
    fn flipped_bit_fails() {
        let token = seal(SECRET, &user(), 0).unwrap();
        let mut mangled = token.into_bytes();
        let mid = mangled.len() / 2;
        // Swap within the base64url alphabet so decoding still succeeds but
        // decryption must fail.
        mangled[mid] = if mangled[mid] == b'a' { b'b' } else { b'a' };
        let err = unseal(SECRET, &String::from_utf8(mangled).unwrap(), 0).unwrap_err();
        assert_eq!(err, SessionError::AuthenticationFailed);
    }

    #[test]
    fn wrong_secret_fails() {
        let token = seal(SECRET, &user(), 0).unwrap();
        let err = unseal(OTHER_SECRET, &token, 0).unwrap_err();
        assert_eq!(err, SessionError::AuthenticationFailed);
    }

    #[test]
    fn expired_token_fails() {
        let token = seal(SECRET, &user(), 0).unwrap(); // exp = 604800
        assert_eq!(
            unseal(SECRET, &token, SESSION_DURATION_SECS).unwrap_err(),
            SessionError::Expired
        );
        assert_eq!(
            unseal(SECRET, &token, SESSION_DURATION_SECS + 1).unwrap_err(),
            SessionError::Expired
        );
    }

    #[test]
    fn garbage_tokens_fail() {
        assert_eq!(
            unseal(SECRET, "not-base64!!", 0).unwrap_err(),
            SessionError::InvalidBase64
        );
        // Valid base64url, but too short to hold nonce + tag.
        assert_eq!(
            unseal(SECRET, "YWJj", 0).unwrap_err(),
            SessionError::InvalidPayload
        );
    }

    #[test]
    fn cookie_value_from_header_finds_named_cookie() {
        let header = Some("theme=dark; sanctuary-session=abc123; lang=en");
        assert_eq!(
            cookie_value_from_header(header, SESSION_COOKIE_NAME),
            Some("abc123")
        );
    }

    #[test]
    fn cookie_value_from_header_ignores_whitespace() {
        let header = Some("theme=dark;  sanctuary-session = abc123  ; lang=en");
        assert_eq!(
            cookie_value_from_header(header, SESSION_COOKIE_NAME),
            Some("abc123")
        );
    }

    #[test]
    fn cookie_value_from_header_requires_exact_name() {
        let header = Some("sanctuary-session-extra=xyz; sanctuary-session=abc");
        assert_eq!(cookie_value_from_header(header, SESSION_COOKIE_NAME), Some("abc"));
        assert_eq!(
            cookie_value_from_header(Some("sanctuary-session=abc"), "other-cookie"),
            None
        );
    }

    #[test]
    fn cookie_value_from_header_missing_or_empty() {
        assert_eq!(cookie_value_from_header(None, SESSION_COOKIE_NAME), None);
        assert_eq!(cookie_value_from_header(Some(""), SESSION_COOKIE_NAME), None);
        assert_eq!(
            cookie_value_from_header(Some("theme=dark"), SESSION_COOKIE_NAME),
            None
        );
        // Present but empty value (e.g. a cleared gorilla cookie).
        assert_eq!(
            cookie_value_from_header(Some("sanctuary-session="), SESSION_COOKIE_NAME),
            Some("")
        );
    }

    #[test]
    fn session_cookie_header_has_all_attributes() {
        let header = session_cookie_header("sealed-token", false);
        assert!(header.starts_with("sanctuary-session=sealed-token"), "{header}");
        assert!(header.contains("Path=/"), "{header}");
        assert!(header.contains("HttpOnly"), "{header}");
        assert!(header.contains("SameSite=Lax"), "{header}");
        assert!(header.contains("Max-Age=604800"), "{header}");
        assert!(!header.contains("Secure"), "{header}");
    }

    #[test]
    fn session_cookie_header_secure_flag() {
        assert!(session_cookie_header("sealed-token", true).contains("Secure"));
    }

    #[test]
    fn clear_session_cookie_header_expires() {
        let header = clear_session_cookie_header(false);
        assert!(header.starts_with("sanctuary-session="), "{header}");
        assert!(header.contains("Path=/"), "{header}");
        assert!(header.contains("HttpOnly"), "{header}");
        assert!(header.contains("SameSite=Lax"), "{header}");
        assert!(header.contains("Max-Age=0"), "{header}");
        assert!(!header.contains("Secure"), "{header}");
        assert!(clear_session_cookie_header(true).contains("Secure"));
    }

    #[test]
    fn me_response_none_serializes_to_null_user() {
        let json = serde_json::to_string(&MeResponse { user: None }).unwrap();
        assert_eq!(json, r#"{"user":null}"#);
    }

    #[test]
    fn me_response_some_serializes_full_user() {
        let json = serde_json::to_string(&MeResponse { user: Some(user()) }).unwrap();
        assert_eq!(
            json,
            r#"{"user":{"id":"google-123","email":"ada@example.com","name":"Ada Lovelace","picture":"https://example.com/ada.png"}}"#
        );
    }

    #[test]
    fn logout_response_serializes_to_json() {
        assert_eq!(
            serde_json::to_string(&LogoutResponse { success: true }).unwrap(),
            r#"{"success":true}"#
        );
    }
}
