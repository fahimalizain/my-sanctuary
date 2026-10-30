//! API keys: opaque bearer credentials for the public `/api/*` surface.
//!
//! A key is `sk_` followed by 32 random bytes in unpadded base64url (so 46
//! characters in total). Only the lowercase SHA-256 hex of the whole secret is
//! stored ([`hash_api_key`]) — the plaintext never touches the database, so a
//! dumped table cannot be replayed. The first 11 characters (`sk_` + 8) are
//! kept as [`ApiKey::key_prefix`] so the Settings UI can identify a key after
//! the one-time reveal.
//!
//! Expiry is chosen as a civil date ([`validate_expires_on`]) but stored as the
//! RFC 3339 instant that ends that day in the user's zone
//! ([`expiry_instant_rfc3339`]) — midnight at the start of the *next* local
//! day, so the key works through the whole chosen day and DST is handled by
//! chrono-tz rather than by the caller.
//!
//! This module is deliberately free of I/O: randomness comes from
//! `getrandom`, "now" is always supplied by the caller (never `SystemTime`,
//! which is unreliable on wasm), and persistence goes through [`ApiKeyRepo`].

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::TimeZone;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::repo::RepoError;
use crate::session::SessionUser;
use crate::time::{parse_iana_tz, rfc3339_to_unix_secs, unix_secs_to_rfc3339};

// ──────────────────────────────────────────
// Constants
// ──────────────────────────────────────────

/// Scheme prefix of every API key secret.
pub const API_KEY_PREFIX: &str = "sk_";

/// Random bytes behind the secret: 256 bits of OS randomness.
const API_KEY_ENTROPY_BYTES: usize = 32;

/// Characters kept for display: `sk_` + 8 base64url characters.
pub const API_KEY_DISPLAY_PREFIX_LEN: usize = 11;

/// Longest accepted key name, in characters (after trimming).
pub const MAX_API_KEY_NAME_LEN: usize = 64;

/// Most living keys one user may hold; checked before every create.
pub const MAX_ACTIVE_API_KEYS: i64 = 20;

/// How stale `last_used_at` must be before a request rewrites it. One write
/// per key per hour keeps the auth path from turning every call into a write.
pub const LAST_USED_TOUCH_SECS: i64 = 3600;

/// Hex digits for [`hash_api_key`] (no extra crate for a two-line encoder).
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

// ──────────────────────────────────────────
// Errors
// ──────────────────────────────────────────

/// Errors produced by the API-key primitives and services.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ApiKeyError {
    /// 400 — the name is empty, too long, or contains control characters.
    #[error("{0}")]
    InvalidName(String),
    /// 400 — `expires_on` is not a strict `YYYY-MM-DD` civil date.
    #[error("expiry must be a YYYY-MM-DD date")]
    InvalidExpiry,
    /// 400 — `expires_on` is before the user's today.
    #[error("expiry is in the past")]
    PastExpiry,
    /// 409 — the user already holds [`MAX_ACTIVE_API_KEYS`] living keys.
    #[error("too many active API keys")]
    TooManyKeys,
    /// 500 — the OS/Web Crypto randomness source failed.
    #[error("failed to obtain randomness for the API key")]
    Entropy,
    /// 500 — the database failed.
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

// ──────────────────────────────────────────
// Generation and hashing
// ──────────────────────────────────────────

/// A freshly generated key: the one-time plaintext plus what gets persisted.
///
/// `secret` is returned to the user exactly once and is never stored; only
/// `hash` (and `key_prefix` for display) reach the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedApiKey {
    /// The plaintext credential (`sk_…`) — show once, never log.
    pub secret: String,
    /// Lowercase SHA-256 hex of `secret`; the stored lookup value.
    pub hash: String,
    /// First [`API_KEY_DISPLAY_PREFIX_LEN`] characters, safe to display.
    pub key_prefix: String,
}

/// Mints a new key from [`API_KEY_ENTROPY_BYTES`] bytes of OS randomness.
pub fn generate_api_key() -> Result<GeneratedApiKey, ApiKeyError> {
    let mut raw = [0u8; API_KEY_ENTROPY_BYTES];
    getrandom::getrandom(&mut raw).map_err(|_| ApiKeyError::Entropy)?;
    let secret = format!("{API_KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(raw));
    // The secret is `sk_` + base64url, so byte 11 is a character boundary.
    let key_prefix = secret[..API_KEY_DISPLAY_PREFIX_LEN].to_string();
    Ok(GeneratedApiKey {
        hash: hash_api_key(&secret),
        key_prefix,
        secret,
    })
}

/// Lowercase SHA-256 hex of a key secret — the only form ever persisted.
///
/// Hex (rather than base64) keeps `key_hash` diffable and its shape assertable
/// without decoding; length is always 64.
pub fn hash_api_key(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        hex.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    hex
}

/// Whether `secret` has the shape [`generate_api_key`] produces: the `sk_`
/// prefix and exactly [`API_KEY_ENTROPY_BYTES`] decodable bytes.
///
/// A shape check only — authentication still hashes the value and looks it up.
pub fn is_well_formed_api_key(secret: &str) -> bool {
    let Some(encoded) = secret.strip_prefix(API_KEY_PREFIX) else {
        return false;
    };
    URL_SAFE_NO_PAD
        .decode(encoded)
        .map(|raw| raw.len() == API_KEY_ENTROPY_BYTES)
        .unwrap_or(false)
}

// ──────────────────────────────────────────
// Authorization header parsing
// ──────────────────────────────────────────

/// Classification of an `Authorization` request header.
///
/// A missing header is [`Absent`](Self::Absent) (fall back to the session
/// cookie); a present-but-unusable one is [`Malformed`](Self::Malformed) —
/// never silently treated as absent, so a bad key cannot downgrade a request
/// into cookie auth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationHeader<'a> {
    /// No `Authorization` header at all.
    Absent,
    /// Present but not a usable `Bearer <token>` header.
    Malformed,
    /// `Bearer <token>`; the token itself is not validated here.
    Bearer(&'a str),
}

/// Parses an `Authorization` header value.
///
/// The scheme is matched case-insensitively, surrounding whitespace is
/// trimmed, and exactly one space must separate scheme from token. The token
/// must be non-empty and contain no whitespace. Its *shape* is not checked —
/// that is [`is_well_formed_api_key`]'s job at the auth call site.
pub fn parse_authorization_header(header: Option<&str>) -> AuthorizationHeader<'_> {
    let Some(header) = header else {
        return AuthorizationHeader::Absent;
    };
    let header = header.trim();
    if header.is_empty() {
        return AuthorizationHeader::Malformed;
    }
    let Some((scheme, token)) = header.split_once(' ') else {
        return AuthorizationHeader::Malformed;
    };
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.is_empty()
        || token.chars().any(char::is_whitespace)
    {
        return AuthorizationHeader::Malformed;
    }
    AuthorizationHeader::Bearer(token)
}

// ──────────────────────────────────────────
// Validation
// ──────────────────────────────────────────

/// Trims and validates a key name, returning the stored form.
///
/// Empty (after trim), longer than [`MAX_API_KEY_NAME_LEN`] characters, and
/// names containing control characters are rejected.
pub fn validate_api_key_name(raw: &str) -> Result<String, ApiKeyError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiKeyError::InvalidName("name is required".to_string()));
    }
    if name.chars().count() > MAX_API_KEY_NAME_LEN {
        return Err(ApiKeyError::InvalidName(format!(
            "name must be at most {MAX_API_KEY_NAME_LEN} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiKeyError::InvalidName(
            "name must not contain control characters".to_string(),
        ));
    }
    Ok(name.to_string())
}

/// Validates an expiry civil date against the user's civil "today".
///
/// Strict `YYYY-MM-DD` (no `2026-1-2`, no real-but-unwritten days); today
/// itself is fine — the key then lives through the rest of today.
pub fn validate_expires_on(raw: &str, today: &str) -> Result<(), ApiKeyError> {
    if crate::digest::parse_civil_date(raw).is_none() {
        return Err(ApiKeyError::InvalidExpiry);
    }
    // Both sides are strict `YYYY-MM-DD`, so the string compare is the date
    // compare (fixed-width, zero-padded).
    if raw < today {
        return Err(ApiKeyError::PastExpiry);
    }
    Ok(())
}

/// The authoritative expiry instant for a chosen civil date.
///
/// Returns the RFC 3339 UTC instant of local midnight *starting the day after*
/// `expires_on` in `zone`, so the key stays valid through the whole chosen
/// day. Unknown/empty zones fall back to UTC. A fall-back fold resolves to the
/// earliest instant (same `.single().or_else(earliest)` rule as the digest's
/// local-midnight walk); a midnight swallowed by a spring-forward gap has no
/// instant and is reported as [`ApiKeyError::InvalidExpiry`].
pub fn expiry_instant_rfc3339(expires_on: &str, zone: &str) -> Result<String, ApiKeyError> {
    let day = crate::digest::parse_civil_date(expires_on).ok_or(ApiKeyError::InvalidExpiry)?;
    let next_day = day
        .checked_add_days(chrono::Days::new(1))
        .ok_or(ApiKeyError::InvalidExpiry)?;
    let midnight = next_day
        .and_hms_opt(0, 0, 0)
        .ok_or(ApiKeyError::InvalidExpiry)?;

    let tz = parse_iana_tz(zone).unwrap_or(Tz::UTC);
    let resolved = tz.from_local_datetime(&midnight);
    let instant = resolved
        .single()
        .or_else(|| resolved.earliest())
        .ok_or(ApiKeyError::InvalidExpiry)?;
    Ok(unix_secs_to_rfc3339(instant.timestamp()))
}

/// Whether an authenticated request should rewrite `last_used_at`.
///
/// True when the stamp is missing, unparseable, or more than
/// [`LAST_USED_TOUCH_SECS`] old — the write throttle for the bearer path.
pub fn should_touch_last_used(last_used_at: Option<&str>, now_unix: i64) -> bool {
    match last_used_at.and_then(rfc3339_to_unix_secs) {
        Some(last_used) => now_unix - last_used > LAST_USED_TOUCH_SECS,
        None => true,
    }
}

// ──────────────────────────────────────────
// Rows and DTOs
// ──────────────────────────────────────────

/// One `api_keys` row.
///
/// `key_hash` is the stored SHA-256 hex; it must never reach a response — use
/// [`ApiKeyView`] for that.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub key_hash: String,
    pub key_prefix: String,
    /// The civil date the user picked (`YYYY-MM-DD`); display only.
    pub expires_on: Option<String>,
    /// Authoritative RFC 3339 UTC expiry; `None` = never.
    pub expires_at: Option<String>,
    /// RFC 3339 UTC of the last authenticated call; `None` = never used.
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert input for [`ApiKeyRepo::insert`].
///
/// The caller mints `id` and both timestamps (the Worker uses `worker::Date`,
/// tests use fixed strings); `last_used_at`/`deleted_at` start NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApiKey {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub key_hash: String,
    pub key_prefix: String,
    pub expires_on: Option<String>,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The identity behind an authenticated key.
///
/// Carries the key's own id (for the `last_used_at` touch) and the owning
/// user, shaped exactly like a session identity so existing handlers can reuse
/// the user without a second read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyOwner {
    pub key_id: String,
    pub last_used_at: Option<String>,
    pub user: SessionUser,
}

/// Row projection of [`API_KEY_AUTHENTICATE_SQL`].
///
/// Every column is aliased in the query (`key_id`, `key_last_used_at`,
/// `user_id`, `user_email`, `user_name`, `user_picture`) because both joined
/// tables carry `id`/`created_at`/`updated_at`/`deleted_at`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiKeyOwnerRow {
    /// `api_keys.id AS key_id`.
    pub key_id: String,
    /// `api_keys.last_used_at AS key_last_used_at`.
    pub key_last_used_at: Option<String>,
    /// `users.id AS user_id`.
    pub user_id: String,
    /// `users.email AS user_email`.
    pub user_email: String,
    /// `users.name AS user_name`.
    pub user_name: String,
    /// `COALESCE(users.picture, '') AS user_picture` — never NULL.
    pub user_picture: String,
}

impl From<ApiKeyOwnerRow> for ApiKeyOwner {
    fn from(row: ApiKeyOwnerRow) -> Self {
        ApiKeyOwner {
            key_id: row.key_id,
            last_used_at: row.key_last_used_at,
            user: SessionUser {
                id: row.user_id,
                email: row.user_email,
                name: row.user_name,
                picture: row.user_picture,
            },
        }
    }
}

/// Row projection of [`API_KEY_COUNT_ACTIVE_SQL`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActiveKeyCountRow {
    pub count: i64,
}

/// Response projection of one key — never includes `key_hash` or the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiKeyView {
    pub id: String,
    pub name: String,
    pub key_prefix: String,
    pub expires_on: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
}

impl From<&ApiKey> for ApiKeyView {
    fn from(key: &ApiKey) -> Self {
        ApiKeyView {
            id: key.id.clone(),
            name: key.name.clone(),
            key_prefix: key.key_prefix.clone(),
            expires_on: key.expires_on.clone(),
            expires_at: key.expires_at.clone(),
            last_used_at: key.last_used_at.clone(),
            created_at: key.created_at.clone(),
        }
    }
}

impl From<ApiKey> for ApiKeyView {
    fn from(key: ApiKey) -> Self {
        ApiKeyView {
            id: key.id,
            name: key.name,
            key_prefix: key.key_prefix,
            expires_on: key.expires_on,
            expires_at: key.expires_at,
            last_used_at: key.last_used_at,
            created_at: key.created_at,
        }
    }
}

/// Response body for `GET /api/keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiKeysResponse {
    pub keys: Vec<ApiKeyView>,
}

/// Response body for `POST /api/keys` — the only time `secret` is returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateApiKeyResponse {
    pub key: ApiKeyView,
    pub secret: String,
}

/// Response body for `DELETE /api/keys/:id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeleteApiKeyResponse {
    pub success: bool,
}

/// Request body for `POST /api/keys`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CreateApiKeyInput {
    pub name: String,
    /// Optional `YYYY-MM-DD` expiry; absent/null = never expires.
    #[serde(default)]
    pub expires_on: Option<String>,
}

// ──────────────────────────────────────────
// SQL
// ──────────────────────────────────────────

/// Every living key of one user, newest first.
pub const API_KEY_LIST_BY_USER_ID_SQL: &str = "SELECT * FROM api_keys WHERE user_id = ? AND deleted_at IS NULL ORDER BY created_at DESC, id DESC";

/// How many living keys the user holds — the [`MAX_ACTIVE_API_KEYS`] guard.
pub const API_KEY_COUNT_ACTIVE_SQL: &str =
    "SELECT COUNT(*) AS count FROM api_keys WHERE user_id = ? AND deleted_at IS NULL";

/// Binds: id, user_id, name, key_hash, key_prefix, expires_on, expires_at,
/// created_at, updated_at (9).
///
/// No `RETURNING`: the caller constructs the row from the same values
/// (`last_used_at`/`deleted_at` start NULL), mirroring
/// `SqliteRuleRepo::insert_rule`.
pub const API_KEY_INSERT_SQL: &str = "
    INSERT INTO api_keys (
      id, user_id, name, key_hash, key_prefix, expires_on, expires_at,
      created_at, updated_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
";

/// Binds: deleted_at, updated_at, id, user_id (4).
///
/// Scoped to the owner so one user cannot revoke another's key; the
/// `deleted_at IS NULL` guard makes a repeat revoke change zero rows.
pub const API_KEY_SOFT_DELETE_SQL: &str = "
    UPDATE api_keys SET deleted_at = ?, updated_at = ?
    WHERE id = ? AND user_id = ? AND deleted_at IS NULL
";

/// Binds: key_hash, now_rfc3339 (2).
///
/// Every joined column is aliased: both tables have `id`, `created_at`,
/// `updated_at`, and `deleted_at`, so a bare `SELECT *` would be ambiguous.
/// A revoked key (`k.deleted_at`) or a soft-deleted owner (`u.deleted_at`)
/// never authenticates, and `expires_at` is exclusive against now.
pub const API_KEY_AUTHENTICATE_SQL: &str = "
    SELECT k.id AS key_id, k.last_used_at AS key_last_used_at,
           u.id AS user_id, u.email AS user_email, u.name AS user_name,
           COALESCE(u.picture, '') AS user_picture
    FROM api_keys k
    JOIN users u ON u.id = k.user_id
    WHERE k.key_hash = ?
      AND k.deleted_at IS NULL
      AND u.deleted_at IS NULL
      AND (k.expires_at IS NULL OR k.expires_at > ?)
    LIMIT 1
";

/// Binds: now_rfc3339, id, cutoff_rfc3339 (3).
///
/// The `last_used_at <= cutoff` guard is the write throttle: a request within
/// [`LAST_USED_TOUCH_SECS`] of the stored stamp leaves it untouched, so
/// concurrent callers cannot move it backwards or turn auth into a write per
/// request.
pub const API_KEY_MARK_USED_SQL: &str = "
    UPDATE api_keys SET last_used_at = ?
    WHERE id = ? AND deleted_at IS NULL AND (last_used_at IS NULL OR last_used_at <= ?)
";

// ──────────────────────────────────────────
// Repository trait
// ──────────────────────────────────────────

/// Persistence for `api_keys` rows.
///
/// `#[async_trait(?Send)]`: the Worker's D1 futures are `!Send` on wasm; the
/// `Send + Sync` supertraits still hold. Timestamps are always supplied by the
/// caller — this layer never reads the clock.
#[async_trait(?Send)]
pub trait ApiKeyRepo: Send + Sync {
    /// Every living key of `user_id`, newest first.
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<ApiKey>, RepoError>;
    /// Count of living keys, for the [`MAX_ACTIVE_API_KEYS`] guard.
    async fn count_active(&self, user_id: &str) -> Result<i64, RepoError>;
    /// Inserts the row and returns it with `last_used_at`/`deleted_at` NULL.
    async fn insert(&self, key: NewApiKey) -> Result<ApiKey, RepoError>;
    /// Soft-deletes `id` when it belongs to `user_id`; true when a living row
    /// changed (false = unknown id, wrong owner, or already revoked).
    async fn soft_delete(
        &self,
        id: &str,
        user_id: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError>;
    /// Resolves a key hash to its owner for a non-expired, non-revoked key.
    async fn authenticate(
        &self,
        key_hash: &str,
        now_rfc3339: &str,
    ) -> Result<Option<ApiKeyOwner>, RepoError>;
    /// Stamps `last_used_at = now_rfc3339` unless it already moved past
    /// `cutoff_rfc3339` (the throttle).
    async fn mark_used(
        &self,
        id: &str,
        cutoff_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> ApiKey {
        ApiKey {
            id: "k-1".to_string(),
            user_id: "u-1".to_string(),
            name: "Laptop".to_string(),
            key_hash: hash_api_key("sk_secret-value"),
            key_prefix: "sk_abcd1234".to_string(),
            expires_on: Some("2026-11-12".to_string()),
            expires_at: Some("2026-11-13T00:00:00Z".to_string()),
            last_used_at: Some("2026-10-01T09:00:00Z".to_string()),
            created_at: "2026-10-01T08:00:00Z".to_string(),
            updated_at: "2026-10-01T08:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    fn unix(stamp: &str) -> i64 {
        rfc3339_to_unix_secs(stamp).expect("valid fixture stamp")
    }

    // ── generation and hashing ──────────────────────────────────────────

    #[test]
    fn generate_api_key_has_prefix_shape_and_matching_hash() {
        let generated = generate_api_key().expect("entropy");
        assert!(generated.secret.starts_with(API_KEY_PREFIX));
        // 46 = 3 + ceil(32 bytes / 3) * 4.
        assert_eq!(generated.secret.len(), 46);
        assert_eq!(generated.key_prefix.len(), API_KEY_DISPLAY_PREFIX_LEN);
        assert_eq!(
            generated.key_prefix,
            generated.secret[..API_KEY_DISPLAY_PREFIX_LEN]
        );
        assert!(generated.key_prefix.starts_with(API_KEY_PREFIX));
        assert_eq!(generated.hash, hash_api_key(&generated.secret));
        assert!(is_well_formed_api_key(&generated.secret));
        assert_eq!(generated.hash.len(), 64);
        assert!(
            generated
                .hash
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "hash must be lowercase hex: {}",
            generated.hash
        );
    }

    #[test]
    fn generated_keys_are_unique() {
        let first = generate_api_key().expect("entropy");
        let second = generate_api_key().expect("entropy");
        assert_ne!(first.secret, second.secret);
        assert_ne!(first.hash, second.hash);
        assert_ne!(first.key_prefix, second.key_prefix);
    }

    #[test]
    fn hash_api_key_is_deterministic_sha256_hex() {
        // SHA-256("abc") — a fixed vector, so a backend change is caught.
        assert_eq!(
            hash_api_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash_api_key("abc"), hash_api_key("abc"));
        assert_ne!(hash_api_key("abc"), hash_api_key("abd"));
        assert_eq!(hash_api_key("").len(), 64);
    }

    #[test]
    fn is_well_formed_accepts_generated_and_rejects_other_shapes() {
        let generated = generate_api_key().expect("entropy");
        assert!(is_well_formed_api_key(&generated.secret));

        let raw16 = URL_SAFE_NO_PAD.encode([0u8; 16]);
        let raw33 = URL_SAFE_NO_PAD.encode([0u8; 33]);
        let padded = URL_SAFE_NO_PAD.encode([0u8; 32]);
        for bad in [
            String::new(),
            "sk_".to_string(),
            format!("pk_{}", raw33),
            format!("sk{}", raw33),
            format!("sk_{raw16}"),
            format!("sk_{raw33}"),
            format!("sk_{padded}="),
            "sk_!!!".to_string(),
            "sk_  ".to_string(),
        ] {
            assert!(
                !is_well_formed_api_key(&bad),
                "{bad:?} must not be well formed"
            );
        }
    }

    // ── bearer parsing ──────────────────────────────────────────────────

    #[test]
    fn absent_header_is_absent() {
        assert_eq!(
            parse_authorization_header(None),
            AuthorizationHeader::Absent
        );
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_and_trimmed() {
        for header in [
            "Bearer abc123",
            "bearer abc123",
            "BEARER abc123",
            "BeArEr abc123",
            "  Bearer abc123  ",
            "\tBearer abc123\t",
        ] {
            assert_eq!(
                parse_authorization_header(Some(header)),
                AuthorizationHeader::Bearer("abc123"),
                "{header:?}"
            );
        }
    }

    #[test]
    fn malformed_headers_are_rejected() {
        for header in [
            "",
            "   ",
            "Bearer",
            "Bearer ",
            " Basic abc123",
            "Basic abc123",
            "Token abc123",
            // Exactly one space: the token may not carry whitespace.
            "Bearer  abc123",
            "Bearer abc 123",
            "Bearer\tabc123",
            "Bearer\tabc 123",
        ] {
            assert_eq!(
                parse_authorization_header(Some(header)),
                AuthorizationHeader::Malformed,
                "{header:?}"
            );
        }
    }

    #[test]
    fn bearer_does_not_validate_the_token_shape() {
        assert_eq!(
            parse_authorization_header(Some("Bearer not-a-key")),
            AuthorizationHeader::Bearer("not-a-key")
        );
        assert_eq!(
            parse_authorization_header(Some("Bearer sk_!!!")),
            AuthorizationHeader::Bearer("sk_!!!")
        );
    }

    // ── name validation ─────────────────────────────────────────────────

    #[test]
    fn validate_name_trims_and_accepts() {
        assert_eq!(
            validate_api_key_name("  Laptop  ").expect("valid"),
            "Laptop"
        );
        let longest = "n".repeat(MAX_API_KEY_NAME_LEN);
        assert_eq!(validate_api_key_name(&longest).expect("valid"), longest);
        // Multi-byte names count characters, not bytes.
        let unicode = "é".repeat(MAX_API_KEY_NAME_LEN);
        assert_eq!(validate_api_key_name(&unicode).expect("valid"), unicode);
    }

    #[test]
    fn validate_name_rejects_empty_long_and_control() {
        for bad in ["", "   ", "\t\n"] {
            assert!(
                matches!(validate_api_key_name(bad), Err(ApiKeyError::InvalidName(_))),
                "{bad:?}"
            );
        }
        let too_long = "n".repeat(MAX_API_KEY_NAME_LEN + 1);
        assert!(matches!(
            validate_api_key_name(&too_long),
            Err(ApiKeyError::InvalidName(_))
        ));
        for bad in ["lap\ttop", "lap\ntop", "lap\u{7}top", "lap\u{0}top"] {
            assert!(
                matches!(validate_api_key_name(bad), Err(ApiKeyError::InvalidName(_))),
                "{bad:?}"
            );
        }
    }

    // ── expiry validation ───────────────────────────────────────────────

    #[test]
    fn validate_expires_on_accepts_today_and_future() {
        assert!(validate_expires_on("2026-11-12", "2026-11-12").is_ok());
        assert!(validate_expires_on("2026-11-13", "2026-11-12").is_ok());
        assert!(validate_expires_on("2027-01-01", "2026-12-31").is_ok());
    }

    #[test]
    fn validate_expires_on_rejects_past() {
        assert_eq!(
            validate_expires_on("2026-11-11", "2026-11-12"),
            Err(ApiKeyError::PastExpiry)
        );
        assert_eq!(
            validate_expires_on("2025-12-31", "2026-01-01"),
            Err(ApiKeyError::PastExpiry)
        );
    }

    #[test]
    fn validate_expires_on_rejects_non_strict_dates() {
        for bad in [
            "",
            "2026-11-12T00:00:00Z",
            "2026-1-2",
            "2026-02-31",
            "2026-13-01",
            "12-11-2026",
            "not-a-date",
            "2026-11-12 ",
        ] {
            assert_eq!(
                validate_expires_on(bad, "2020-01-01"),
                Err(ApiKeyError::InvalidExpiry),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn expiry_instant_is_the_next_local_midnight() {
        // The chosen day is inclusive: 2026-11-12 in Kolkata ends at
        // 2026-11-13T00:00+05:30 == 2026-11-12T18:30:00Z.
        assert_eq!(
            expiry_instant_rfc3339("2026-11-12", "Asia/Kolkata").expect("instant"),
            "2026-11-12T18:30:00Z"
        );
        assert_eq!(
            expiry_instant_rfc3339("2026-11-12", "UTC").expect("instant"),
            "2026-11-13T00:00:00Z"
        );
        // Unknown/empty zones fall back to UTC.
        assert_eq!(
            expiry_instant_rfc3339("2026-11-12", "").expect("instant"),
            "2026-11-13T00:00:00Z"
        );
        assert_eq!(
            expiry_instant_rfc3339("2026-11-12", "bogus").expect("instant"),
            "2026-11-13T00:00:00Z"
        );
    }

    #[test]
    fn expiry_instant_handles_month_and_leap_boundaries() {
        assert_eq!(
            expiry_instant_rfc3339("2028-02-28", "UTC").expect("instant"),
            "2028-02-29T00:00:00Z"
        );
        assert_eq!(
            expiry_instant_rfc3339("2028-02-29", "UTC").expect("instant"),
            "2028-03-01T00:00:00Z"
        );
        assert_eq!(
            expiry_instant_rfc3339("2026-12-31", "UTC").expect("instant"),
            "2027-01-01T00:00:00Z"
        );
    }

    #[test]
    fn expiry_instant_rejects_a_bad_date() {
        assert_eq!(
            expiry_instant_rfc3339("2026-02-31", "UTC"),
            Err(ApiKeyError::InvalidExpiry)
        );
        assert_eq!(
            expiry_instant_rfc3339("not-a-date", "UTC"),
            Err(ApiKeyError::InvalidExpiry)
        );
    }

    // ── last-used throttle ──────────────────────────────────────────────

    #[test]
    fn should_touch_when_missing_or_unparseable() {
        let now = unix("2026-10-01T12:00:00Z");
        assert!(should_touch_last_used(None, now));
        assert!(should_touch_last_used(Some(""), now));
        assert!(should_touch_last_used(Some("yesterday"), now));
        // A date-only string is not RFC 3339 — treated as unparseable.
        assert!(should_touch_last_used(Some("2026-10-01"), now));
    }

    #[test]
    fn should_touch_only_when_older_than_the_throttle() {
        let now = unix("2026-10-01T12:00:00Z");
        // 3601 seconds old → touch.
        assert!(should_touch_last_used(Some("2026-10-01T10:59:59Z"), now));
        // Exactly 3600 seconds old → not older than the throttle.
        assert!(!should_touch_last_used(Some("2026-10-01T11:00:00Z"), now));
        assert!(!should_touch_last_used(Some("2026-10-01T11:59:00Z"), now));
        // Clock drift making the stamp "future" must not loop writes.
        assert!(!should_touch_last_used(Some("2026-10-01T12:30:00Z"), now));
    }

    // ── response projections ────────────────────────────────────────────

    #[test]
    fn view_from_reference_drops_hash_and_timestamps_it_does_not_need() {
        let key = sample_key();
        let view = ApiKeyView::from(&key);
        assert_eq!(view.id, key.id);
        assert_eq!(view.name, key.name);
        assert_eq!(view.key_prefix, key.key_prefix);
        assert_eq!(view.expires_on, key.expires_on);
        assert_eq!(view.expires_at, key.expires_at);
        assert_eq!(view.last_used_at, key.last_used_at);
        assert_eq!(view.created_at, key.created_at);

        let json = serde_json::to_string(&view).expect("json");
        assert!(!json.contains("key_hash"), "{json}");
        assert!(!json.contains(&key.key_hash), "{json}");
        assert!(!json.contains("user_id"), "{json}");
        assert!(!json.contains("deleted_at"), "{json}");
    }

    #[test]
    fn view_from_owned_matches_view_from_reference() {
        let key = sample_key();
        assert_eq!(ApiKeyView::from(&key), ApiKeyView::from(key));
    }

    #[test]
    fn response_envelopes_serialize() {
        let view = ApiKeyView::from(&sample_key());
        assert_eq!(
            serde_json::to_string(&ApiKeysResponse {
                keys: vec![view.clone()]
            })
            .expect("json"),
            format!(
                r#"{{"keys":[{}]}}"#,
                serde_json::to_string(&view).expect("json")
            )
        );
        let created = serde_json::to_value(&CreateApiKeyResponse {
            key: view,
            secret: "sk_secret".to_string(),
        })
        .expect("json");
        assert_eq!(created["secret"], "sk_secret");
        assert!(created["key"].get("key_hash").is_none());
        assert_eq!(
            serde_json::to_string(&DeleteApiKeyResponse { success: true }).expect("json"),
            r#"{"success":true}"#
        );
    }

    #[test]
    fn create_input_defaults_expires_on_to_none() {
        let input: CreateApiKeyInput =
            serde_json::from_str(r#"{"name":"Laptop"}"#).expect("deserialize");
        assert_eq!(input.name, "Laptop");
        assert_eq!(input.expires_on, None);

        let input: CreateApiKeyInput =
            serde_json::from_str(r#"{"name":"Laptop","expires_on":"2026-11-12"}"#)
                .expect("deserialize");
        assert_eq!(input.expires_on.as_deref(), Some("2026-11-12"));
    }

    #[test]
    fn owner_row_maps_onto_the_session_identity() {
        let owner = ApiKeyOwner::from(ApiKeyOwnerRow {
            key_id: "k-1".to_string(),
            key_last_used_at: Some("2026-10-01T09:00:00Z".to_string()),
            user_id: "u-1".to_string(),
            user_email: "user@example.com".to_string(),
            user_name: "Test User".to_string(),
            user_picture: String::new(),
        });
        assert_eq!(owner.key_id, "k-1");
        assert_eq!(owner.last_used_at.as_deref(), Some("2026-10-01T09:00:00Z"));
        assert_eq!(
            owner.user,
            SessionUser {
                id: "u-1".to_string(),
                email: "user@example.com".to_string(),
                name: "Test User".to_string(),
                picture: String::new(),
            }
        );
    }

    // ── SQL constants ───────────────────────────────────────────────────

    #[test]
    fn list_sql_filters_living_rows_newest_first() {
        assert!(API_KEY_LIST_BY_USER_ID_SQL.contains("user_id = ?"));
        assert!(API_KEY_LIST_BY_USER_ID_SQL.contains("deleted_at IS NULL"));
        assert!(
            API_KEY_LIST_BY_USER_ID_SQL.ends_with("ORDER BY created_at DESC, id DESC"),
            "{API_KEY_LIST_BY_USER_ID_SQL}"
        );
        assert_eq!(API_KEY_LIST_BY_USER_ID_SQL.matches('?').count(), 1);
    }

    #[test]
    fn count_sql_counts_only_living_rows() {
        assert!(API_KEY_COUNT_ACTIVE_SQL.contains("AS count"));
        assert!(API_KEY_COUNT_ACTIVE_SQL.contains("user_id = ?"));
        assert!(API_KEY_COUNT_ACTIVE_SQL.contains("deleted_at IS NULL"));
        assert_eq!(API_KEY_COUNT_ACTIVE_SQL.matches('?').count(), 1);
    }

    #[test]
    fn insert_sql_binds_every_writable_column() {
        assert_eq!(API_KEY_INSERT_SQL.matches('?').count(), 9);
        assert!(API_KEY_INSERT_SQL.contains("key_hash"));
        assert!(API_KEY_INSERT_SQL.contains("key_prefix"));
        // Caller-constructed row: nothing is read back from the database.
        assert!(!API_KEY_INSERT_SQL.contains("RETURNING"));
        assert!(!API_KEY_INSERT_SQL.contains("last_used_at"));
        assert!(!API_KEY_INSERT_SQL.contains("deleted_at"));
    }

    #[test]
    fn soft_delete_sql_is_scoped_soft_and_idempotent() {
        let sql = API_KEY_SOFT_DELETE_SQL.trim();
        assert!(
            sql.starts_with("UPDATE api_keys SET deleted_at = ?"),
            "{sql}"
        );
        assert!(!sql.contains("DELETE FROM"), "{sql}");
        assert!(sql.contains("id = ? AND user_id = ?"), "{sql}");
        assert!(sql.ends_with("deleted_at IS NULL"), "{sql}");
        assert_eq!(sql.matches('?').count(), 4);
    }

    #[test]
    fn authenticate_sql_aliases_every_joined_column() {
        let sql = API_KEY_AUTHENTICATE_SQL.trim();
        assert!(!sql.contains("SELECT *"), "{sql}");
        for alias in [
            "AS key_id",
            "AS key_last_used_at",
            "AS user_id",
            "AS user_email",
            "AS user_name",
            "AS user_picture",
        ] {
            assert!(sql.contains(alias), "{alias}");
        }
        assert!(sql.contains("COALESCE(u.picture, '')"), "{sql}");
        assert!(sql.contains("JOIN users u ON u.id = k.user_id"), "{sql}");
        assert!(sql.contains("k.deleted_at IS NULL"), "{sql}");
        assert!(sql.contains("u.deleted_at IS NULL"), "{sql}");
        assert!(
            sql.contains("k.expires_at IS NULL OR k.expires_at > ?"),
            "{sql}"
        );
        assert!(sql.ends_with("LIMIT 1"), "{sql}");
        assert_eq!(sql.matches('?').count(), 2);
    }

    #[test]
    fn mark_used_sql_is_throttled_and_skips_revoked_rows() {
        assert!(API_KEY_MARK_USED_SQL.contains("SET last_used_at = ?"));
        assert!(API_KEY_MARK_USED_SQL.contains("deleted_at IS NULL"));
        assert!(API_KEY_MARK_USED_SQL.contains("last_used_at IS NULL OR last_used_at <= ?"));
        assert_eq!(API_KEY_MARK_USED_SQL.matches('?').count(), 3);
    }
}
