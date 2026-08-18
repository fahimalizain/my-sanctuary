//! Google OAuth login: consent URL, state generation, and the code exchange.
//!
//! Everything here is pure Rust and unit-testable: the HTTP calls go through
//! the [`HttpClient`] trait (implemented in the Worker with `worker::Fetch`)
//! and persistence goes through the repo traits (implemented in the Worker
//! with D1). Timestamps are derived from a caller-supplied `now_unix` — never
//! `SystemTime`.

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::config::OAuthConfig;
use crate::models::{NewToken, NewUser};
use crate::repo::{RepoError, TokenRepo, UserRepo};
use crate::session::SessionUser;
use crate::time::unix_secs_to_rfc3339;

pub const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/auth";
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const GOOGLE_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";

/// OAuth scopes requested on the consent screen (space-separated in the URL).
pub const OAUTH_SCOPES: [&str; 4] = [
    "openid",
    "email",
    "profile",
    "https://www.googleapis.com/auth/calendar",
];

/// Errors produced by an HTTP call in [`HttpClient`].
#[derive(Debug, Clone, Error)]
pub enum HttpError {
    #[error("http request failed: {0}")]
    Message(String),
}

/// Minimal HTTP surface needed for OAuth and the calendar API. The Worker
/// implements this with `worker::Fetch` (see `apps/worker/src/http.rs`); tests
/// use a fake.
///
/// `#[async_trait(?Send)]` because `worker::Fetch` futures are `!Send` on wasm.
#[async_trait(?Send)]
pub trait HttpClient: Send + Sync {
    /// POSTs an `application/x-www-form-urlencoded` body and returns the raw
    /// response body. Non-2xx responses are errors.
    async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError>;
    /// GETs a URL with an `Authorization: Bearer …` header and returns the raw
    /// response body. Non-2xx responses are errors.
    async fn get_bearer(&self, url: &str, access_token: &str) -> Result<Vec<u8>, HttpError>;
    /// Like [`get_bearer`](Self::get_bearer) but returns the status alongside
    /// the body and treats non-2xx as data: the calendar sync needs to inspect
    /// `410 Gone` (stale sync token → full resync) and `404` (calendar does not
    /// support events.list → disable sync).
    async fn get_bearer_raw(
        &self,
        url: &str,
        access_token: &str,
    ) -> Result<(u16, Vec<u8>), HttpError>;
    /// POSTs a JSON body with `Content-Type: application/json` and an
    /// `Authorization: Bearer …` header. Returns `(status, body)` so the
    /// caller can distinguish 2xx (created event) from errors.
    async fn post_json(
        &self,
        url: &str,
        access_token: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), HttpError>;
    /// PATCHes a JSON body with the same headers as
    /// [`post_json`](Self::post_json) (Google `events.patch` for the task
    /// timer). Returns `(status, body)`.
    async fn patch_json(
        &self,
        url: &str,
        access_token: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), HttpError>;
}

/// Errors produced by the code-exchange/login orchestration.
///
/// Never carries tokens or secrets in its messages.
#[derive(Debug, Clone, Error)]
pub enum OAuthError {
    #[error("http request failed: {0}")]
    Http(#[from] HttpError),
    #[error("invalid oauth response: {0}")]
    InvalidResponse(String),
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

/// Builds Google's consent-screen URL for a fresh login.
///
/// Query parameters mirror the old Go `AuthCodeURL(state, AccessTypeOffline,
/// ApprovalForce)`: `access_type=offline` earns a refresh token and
/// `prompt=consent` forces a fresh consent so the refresh token is re-issued.
pub fn authorization_url(oauth: &OAuthConfig, state: &str) -> String {
    let mut url = Url::parse(GOOGLE_AUTH_URL).expect("static auth URL is valid");
    url.query_pairs_mut()
        .append_pair("client_id", &oauth.client_id)
        .append_pair("redirect_uri", &oauth.redirect_url)
        .append_pair("response_type", "code")
        .append_pair("scope", &OAUTH_SCOPES.join(" "))
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    url.to_string()
}

/// Generates a 32-byte random OAuth state value, base64url-encoded without
/// padding (same shape as Go's `base64.URLEncoding` output minus padding).
pub fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    // getrandom uses OS randomness natively and Web Crypto on wasm; failure is
    // practically impossible (session.rs maps the same source to an error).
    let _ = getrandom::getrandom(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Google's token-endpoint response.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
    #[serde(default)]
    token_type: String,
    scope: Option<String>,
}

/// Google's `/oauth2/v2/userinfo` response.
#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    id: String,
    email: String,
    name: String,
    picture: Option<String>,
}

/// Exchanges an authorization `code` for tokens, fetches the Google profile,
/// and persists both the user (upsert by `google_id`) and the tokens.
///
/// Returns the session identity — the *DB* user id, not the Google id — so
/// the caller can seal a session cookie. `now_unix` (Unix seconds from
/// `worker::Date::now()`) seeds the token `expiry`.
pub async fn exchange_and_login(
    http: &dyn HttpClient,
    users: &dyn UserRepo,
    tokens: &dyn TokenRepo,
    oauth: &OAuthConfig,
    code: &str,
    now_unix: i64,
) -> Result<SessionUser, OAuthError> {
    let form: [(&str, &str); 5] = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", &oauth.client_id),
        ("client_secret", &oauth.client_secret),
        ("redirect_uri", &oauth.redirect_url),
    ];
    let token_body = http.post_form(GOOGLE_TOKEN_URL, &form).await?;
    let token: TokenResponse = serde_json::from_slice(&token_body)
        .map_err(|err| OAuthError::InvalidResponse(format!("token response: {err}")))?;
    if token.access_token.is_empty() {
        return Err(OAuthError::InvalidResponse("token response missing access_token".into()));
    }

    let user_body = http
        .get_bearer(GOOGLE_USERINFO_URL, &token.access_token)
        .await?;
    let info: UserInfoResponse = serde_json::from_slice(&user_body)
        .map_err(|err| OAuthError::InvalidResponse(format!("userinfo response: {err}")))?;
    if info.id.is_empty() {
        return Err(OAuthError::InvalidResponse("userinfo response missing id".into()));
    }

    let saved = users
        .upsert_by_google_id(NewUser {
            google_id: info.id,
            email: info.email,
            name: info.name,
            picture: info.picture,
        })
        .await?;

    let expiry = unix_secs_to_rfc3339(now_unix + token.expires_in);
    // Google defaults `token_type` to "Bearer" when absent.
    let token_type = if token.token_type.is_empty() {
        "Bearer".to_string()
    } else {
        token.token_type
    };
    tokens
        .upsert(NewToken {
            user_id: saved.id.clone(),
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expiry,
            token_type,
            scope: token.scope,
        })
        .await?;

    Ok(SessionUser {
        id: saved.id,
        email: saved.email,
        name: saved.name,
        picture: saved.picture.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::models::{User, GoogleOAuthToken};

    fn oauth() -> OAuthConfig {
        OAuthConfig {
            client_id: "client-id.apps.googleusercontent.com".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_url: "http://localhost:5173/auth/google/callback".to_string(),
        }
    }

    fn token_json(refresh: Option<&str>) -> String {
        let mut json = r#"{"access_token":"at-123","#.to_string();
        if let Some(value) = refresh {
            json.push_str(&format!(r#""refresh_token":"{value}","#));
        }
        json.push_str(
            r#""expires_in":3599,"token_type":"Bearer","scope":"openid email profile https://www.googleapis.com/auth/calendar"}"#,
        );
        json
    }

    const USERINFO_JSON: &str = r#"{"id":"google-42","email":"ada@example.com","name":"Ada Lovelace","picture":"https://example.com/ada.png","verified_email":true}"#;

    /// Fake HTTP: canned token/userinfo bodies and a failure switch.
    struct FakeHttp {
        token_body: Result<Vec<u8>, HttpError>,
        userinfo_body: Result<Vec<u8>, HttpError>,
        posts: Mutex<Vec<(String, Vec<(String, String)>)>>,
        gets: Mutex<Vec<(String, String)>>,
    }

    impl FakeHttp {
        fn ok(token: &str, userinfo: &str) -> Self {
            Self {
                token_body: Ok(token.as_bytes().to_vec()),
                userinfo_body: Ok(userinfo.as_bytes().to_vec()),
                posts: Mutex::new(Vec::new()),
                gets: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait(?Send)]
    impl HttpClient for FakeHttp {
        async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
            self.posts.lock().unwrap().push((
                url.to_string(),
                form.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
            ));
            self.token_body.clone()
        }

        async fn get_bearer(&self, url: &str, access_token: &str) -> Result<Vec<u8>, HttpError> {
            self.gets
                .lock()
                .unwrap()
                .push((url.to_string(), access_token.to_string()));
            self.userinfo_body.clone()
        }

        async fn get_bearer_raw(
            &self,
            url: &str,
            access_token: &str,
        ) -> Result<(u16, Vec<u8>), HttpError> {
            // OAuth tests never exercise the raw variant; mirror get_bearer.
            self.gets
                .lock()
                .unwrap()
                .push((url.to_string(), access_token.to_string()));
            Ok((200, self.userinfo_body.clone().unwrap_or_default()))
        }

        async fn post_json(
            &self,
            _url: &str,
            _access_token: &str,
            _body: &[u8],
        ) -> Result<(u16, Vec<u8>), HttpError> {
            // OAuth tests never POST JSON.
            Ok((200, Vec::new()))
        }

        async fn patch_json(
            &self,
            _url: &str,
            _access_token: &str,
            _body: &[u8],
        ) -> Result<(u16, Vec<u8>), HttpError> {
            // OAuth tests never PATCH JSON.
            Ok((200, Vec::new()))
        }
    }

    /// Fake repo: records inputs, returns a canned stored user.
    struct FakeUserRepo {
        stored: User,
        upserted: Mutex<Option<NewUser>>,
        fail: Mutex<Option<RepoError>>,
    }

    #[async_trait(?Send)]
    impl UserRepo for FakeUserRepo {
        async fn get_by_id(&self, _id: &str) -> Result<Option<User>, RepoError> {
            Ok(Some(self.stored.clone()))
        }
        async fn get_by_google_id(&self, _google_id: &str) -> Result<Option<User>, RepoError> {
            Ok(Some(self.stored.clone()))
        }
        async fn upsert_by_google_id(&self, user: NewUser) -> Result<User, RepoError> {
            if let Some(err) = self.fail.lock().unwrap().as_ref() {
                return Err(err.clone());
            }
            *self.upserted.lock().unwrap() = Some(user);
            Ok(self.stored.clone())
        }
    }

    struct FakeTokenRepo {
        upserted: Mutex<Option<NewToken>>,
        fail: Mutex<Option<RepoError>>,
    }

    #[async_trait(?Send)]
    impl TokenRepo for FakeTokenRepo {
        async fn get_by_user_id(
            &self,
            _user_id: &str,
        ) -> Result<Option<GoogleOAuthToken>, RepoError> {
            Ok(None)
        }
        async fn upsert(&self, token: NewToken) -> Result<(), RepoError> {
            if let Some(err) = self.fail.lock().unwrap().as_ref() {
                return Err(err.clone());
            }
            *self.upserted.lock().unwrap() = Some(token);
            Ok(())
        }
        async fn delete(&self, _user_id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }
    }

    fn stored_user() -> User {
        User {
            id: "db-user-1".to_string(),
            google_id: "google-42".to_string(),
            email: "ada@example.com".to_string(),
            name: "Ada Lovelace".to_string(),
            picture: Some("https://example.com/ada.png".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    #[test]
    fn authorization_url_contains_every_required_parameter() {
        let url = authorization_url(&oauth(), "state-abc");
        let parsed = Url::parse(&url).unwrap();
        assert_eq!(parsed.scheme(), "https");
        assert_eq!(parsed.host_str(), Some("accounts.google.com"));
        assert_eq!(parsed.path(), "/o/oauth2/auth");

        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

        assert_eq!(get("client_id").as_deref(), Some("client-id.apps.googleusercontent.com"));
        assert_eq!(
            get("redirect_uri").as_deref(),
            Some("http://localhost:5173/auth/google/callback")
        );
        assert_eq!(get("response_type").as_deref(), Some("code"));
        assert_eq!(get("state").as_deref(), Some("state-abc"));
        assert_eq!(get("access_type").as_deref(), Some("offline"));
        assert_eq!(get("prompt").as_deref(), Some("consent"));
        assert_eq!(
            get("scope").as_deref(),
            Some("openid email profile https://www.googleapis.com/auth/calendar")
        );
    }

    #[test]
    fn generate_state_is_32_random_bytes_base64url_unpadded() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a, b, "state must be random");
        assert_eq!(a.len(), 43, "32 bytes base64url without padding is 43 chars");
        assert!(!a.contains('+') && !a.contains('/') && !a.contains('='), "{a}");
        // Decodes back to exactly 32 bytes.
        let decoded = URL_SAFE_NO_PAD.decode(&a).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn exchange_and_login_persists_user_and_token_and_returns_db_user() {
        let http = FakeHttp::ok(&token_json(Some("rt-1")), USERINFO_JSON);
        let users = FakeUserRepo {
            stored: stored_user(),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let tokens = FakeTokenRepo {
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };

        let now = 1_700_000_000;
        let session = pollster::block_on(exchange_and_login(
            &http, &users, &tokens, &oauth(), "auth-code", now,
        ))
        .unwrap();

        // Session identity comes from the DB row, not from Google.
        assert_eq!(session.id, "db-user-1");
        assert_eq!(session.email, "ada@example.com");
        assert_eq!(session.name, "Ada Lovelace");
        assert_eq!(session.picture, "https://example.com/ada.png");

        // Token exchange hit the right URL with the right form.
        let (token_url, form) = http.posts.lock().unwrap().first().unwrap().clone();
        assert_eq!(token_url, GOOGLE_TOKEN_URL);
        let form: std::collections::HashMap<String, String> = form.into_iter().collect();
        assert_eq!(form.get("grant_type").map(String::as_str), Some("authorization_code"));
        assert_eq!(form.get("code").map(String::as_str), Some("auth-code"));
        assert_eq!(
            form.get("client_id").map(String::as_str),
            Some("client-id.apps.googleusercontent.com")
        );
        assert_eq!(form.get("client_secret").map(String::as_str), Some("client-secret"));
        assert_eq!(
            form.get("redirect_uri").map(String::as_str),
            Some("http://localhost:5173/auth/google/callback")
        );

        // Userinfo fetched with the access token as a bearer.
        let (userinfo_url, bearer) = http.gets.lock().unwrap().first().unwrap().clone();
        assert_eq!(userinfo_url, GOOGLE_USERINFO_URL);
        assert_eq!(bearer, "at-123");

        // User upsert carried the Google identity.
        let new_user = users.upserted.lock().unwrap().clone().unwrap();
        assert_eq!(new_user.google_id, "google-42");
        assert_eq!(new_user.email, "ada@example.com");
        assert_eq!(new_user.name, "Ada Lovelace");
        assert_eq!(new_user.picture.as_deref(), Some("https://example.com/ada.png"));

        // Token upsert carried the DB user id and an RFC 3339 expiry.
        let new_token = tokens.upserted.lock().unwrap().clone().unwrap();
        assert_eq!(new_token.user_id, "db-user-1");
        assert_eq!(new_token.access_token, "at-123");
        assert_eq!(new_token.refresh_token.as_deref(), Some("rt-1"));
        assert_eq!(new_token.expiry, "2023-11-14T23:13:19Z"); // now + 3599
        assert_eq!(new_token.token_type, "Bearer");
        assert_eq!(
            new_token.scope.as_deref(),
            Some("openid email profile https://www.googleapis.com/auth/calendar")
        );
    }

    #[test]
    fn exchange_and_login_without_refresh_token_still_upserts() {
        let http = FakeHttp::ok(&token_json(None), USERINFO_JSON);
        let users = FakeUserRepo {
            stored: stored_user(),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let tokens = FakeTokenRepo {
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };

        let session = pollster::block_on(exchange_and_login(
            &http, &users, &tokens, &oauth(), "auth-code", 0,
        ))
        .unwrap();
        assert_eq!(session.id, "db-user-1");

        // refresh_token is None; the D1 upsert's COALESCE(NULLIF(...,''), …)
        // protects any previously stored value (asserted on the SQL constant
        // in repo.rs tests).
        let new_token = tokens.upserted.lock().unwrap().clone().unwrap();
        assert!(new_token.refresh_token.is_none());
        assert_eq!(new_token.access_token, "at-123");
    }

    #[test]
    fn exchange_and_login_http_error_fails() {
        let http = FakeHttp {
            token_body: Err(HttpError::Message("connection refused".into())),
            userinfo_body: Ok(USERINFO_JSON.as_bytes().to_vec()),
            posts: Mutex::new(Vec::new()),
            gets: Mutex::new(Vec::new()),
        };
        let users = FakeUserRepo {
            stored: stored_user(),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let tokens = FakeTokenRepo {
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let err = pollster::block_on(exchange_and_login(
            &http, &users, &tokens, &oauth(), "auth-code", 0,
        ))
        .unwrap_err();
        assert!(matches!(err, OAuthError::Http(_)), "got {err:?}");
    }

    #[test]
    fn exchange_and_login_missing_access_token_fails() {
        let http = FakeHttp::ok(
            r#"{"refresh_token":"rt","expires_in":3599,"token_type":"Bearer"}"#,
            USERINFO_JSON,
        );
        let users = FakeUserRepo {
            stored: stored_user(),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let tokens = FakeTokenRepo {
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let err = pollster::block_on(exchange_and_login(
            &http, &users, &tokens, &oauth(), "auth-code", 0,
        ))
        .unwrap_err();
        assert!(
            matches!(err, OAuthError::InvalidResponse(_)),
            "got {err:?}"
        );
        assert!(tokens.upserted.lock().unwrap().is_none(), "no token upsert on failure");
    }

    #[test]
    fn exchange_and_login_userinfo_http_error_fails() {
        let http = FakeHttp {
            token_body: Ok(token_json(None).into_bytes()),
            userinfo_body: Err(HttpError::Message("403 from google".into())),
            posts: Mutex::new(Vec::new()),
            gets: Mutex::new(Vec::new()),
        };
        let users = FakeUserRepo {
            stored: stored_user(),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let tokens = FakeTokenRepo {
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let err = pollster::block_on(exchange_and_login(
            &http, &users, &tokens, &oauth(), "auth-code", 0,
        ))
        .unwrap_err();
        assert!(matches!(err, OAuthError::Http(_)), "got {err:?}");
    }

    #[test]
    fn exchange_and_login_missing_userinfo_id_fails() {
        let http = FakeHttp::ok(
            &token_json(None),
            r#"{"email":"ada@example.com","name":"Ada Lovelace"}"#,
        );
        let users = FakeUserRepo {
            stored: stored_user(),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let tokens = FakeTokenRepo {
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let err = pollster::block_on(exchange_and_login(
            &http, &users, &tokens, &oauth(), "auth-code", 0,
        ))
        .unwrap_err();
        assert!(
            matches!(err, OAuthError::InvalidResponse(_)),
            "got {err:?}"
        );
        assert!(users.upserted.lock().unwrap().is_none(), "no user upsert on failure");
    }

    #[test]
    fn exchange_and_login_repo_error_propagates() {
        let http = FakeHttp::ok(&token_json(None), USERINFO_JSON);
        let users = FakeUserRepo {
            stored: stored_user(),
            upserted: Mutex::new(None),
            fail: Mutex::new(Some(RepoError::Backend("d1 down".into()))),
        };
        let tokens = FakeTokenRepo {
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let err = pollster::block_on(exchange_and_login(
            &http, &users, &tokens, &oauth(), "auth-code", 0,
        ))
        .unwrap_err();
        assert!(matches!(err, OAuthError::Repo(_)), "got {err:?}");
    }

    #[test]
    fn empty_token_type_defaults_to_bearer() {
        let json = r#"{"access_token":"at","expires_in":3599,"token_type":""}"#;
        let http = FakeHttp::ok(json, USERINFO_JSON);
        let users = FakeUserRepo {
            stored: stored_user(),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let tokens = FakeTokenRepo {
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        pollster::block_on(exchange_and_login(
            &http, &users, &tokens, &oauth(), "auth-code", 0,
        ))
        .unwrap();
        assert_eq!(tokens.upserted.lock().unwrap().as_ref().unwrap().token_type, "Bearer");
    }
}
