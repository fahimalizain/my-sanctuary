//! Google access-token refresh.
//!
//! Mirrors the old Go `TokenRefresher` (`token_refresher.go`): load the stored
//! token for the user; if it expires more than 5 minutes out, return it as-is;
//! otherwise POST `grant_type=refresh_token` to Google's token endpoint and
//! persist the refreshed access token. A `refresh_token` missing from the
//! response never blanks the stored one — the SQL upsert `COALESCE`s it, and
//! we pass `None` here.
//!
//! Pure Rust and unit-testable: HTTP goes through [`HttpClient`], persistence
//! through [`TokenRepo`], and "now" comes from the caller — never `SystemTime`.

use serde::Deserialize;
use thiserror::Error;

use crate::config::OAuthConfig;
use crate::models::NewToken;
use crate::oauth::{HttpClient, HttpError, GOOGLE_TOKEN_URL};
use crate::repo::{RepoError, TokenRepo};
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};

/// Tokens expiring within this many seconds are refreshed (5 minutes, same as
/// the old Go `TokenRefresher`).
pub const REFRESH_SKEW_SECS: i64 = 5 * 60;

/// A usable Google access token — enough for bearer API calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleAccess {
    pub access_token: String,
    pub token_type: String,
}

/// Errors produced by [`refresh_if_needed`]. The Worker maps every variant to
/// `401 {"error":"unauthorized"}` (a user whose token cannot be refreshed is
/// effectively logged out of the calendar API).
#[derive(Debug, Clone, Error)]
pub enum TokenError {
    #[error("no token stored for this user")]
    NoToken,
    #[error("stored token has no refresh token")]
    NoRefreshToken,
    #[error("stored token is invalid: {0}")]
    InvalidStored(String),
    #[error("http request failed: {0}")]
    Http(#[from] HttpError),
    #[error("invalid refresh response: {0}")]
    InvalidResponse(String),
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

/// Google's token-endpoint response to a refresh grant.
#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    token_type: String,
}

/// Returns a valid access token for `user_id`, refreshing it first when the
/// stored one expires within `REFRESH_SKEW_SECS` seconds (or is already
/// expired). The refreshed token is persisted via [`TokenRepo::upsert`].
pub async fn refresh_if_needed(
    http: &dyn HttpClient,
    tokens: &dyn TokenRepo,
    oauth: &OAuthConfig,
    user_id: &str,
    now_unix: i64,
) -> Result<GoogleAccess, TokenError> {
    let Some(stored) = tokens.get_by_user_id(user_id).await? else {
        return Err(TokenError::NoToken);
    };

    let expiry_unix = rfc3339_to_unix_secs(&stored.expiry)
        .ok_or_else(|| TokenError::InvalidStored(format!("expiry {:#?}", stored.expiry)))?;
    if expiry_unix > now_unix + REFRESH_SKEW_SECS {
        return Ok(GoogleAccess {
            access_token: stored.access_token,
            token_type: stored.token_type,
        });
    }

    let refresh_token = stored
        .refresh_token
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or(TokenError::NoRefreshToken)?;
    let form: [(&str, &str); 4] = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", &oauth.client_id),
        ("client_secret", &oauth.client_secret),
    ];
    let body = http.post_form(GOOGLE_TOKEN_URL, &form).await?;
    let response: RefreshResponse = serde_json::from_slice(&body)
        .map_err(|err| TokenError::InvalidResponse(err.to_string()))?;
    if response.access_token.is_empty() {
        return Err(TokenError::InvalidResponse("missing access_token".into()));
    }

    // Google defaults `token_type` to "Bearer" when absent.
    let token_type = if response.token_type.is_empty() {
        "Bearer".to_string()
    } else {
        response.token_type.clone()
    };
    tokens
        .upsert(NewToken {
            user_id: user_id.to_string(),
            access_token: response.access_token.clone(),
            // Never wipe a stored refresh token: Google only re-issues it on
            // first consent, so refresh responses usually omit it.
            refresh_token: None,
            expiry: unix_secs_to_rfc3339(now_unix + response.expires_in),
            token_type: token_type.clone(),
            scope: None,
        })
        .await?;

    Ok(GoogleAccess {
        access_token: response.access_token,
        token_type,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::models::GoogleOAuthToken;

    fn oauth() -> OAuthConfig {
        OAuthConfig {
            client_id: "client-id.apps.googleusercontent.com".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_url: "http://localhost:5173/auth/google/callback".to_string(),
        }
    }

    /// A token row with the given expiry; `refresh_token` optional.
    fn stored_token(expiry: &str, refresh_token: Option<&str>) -> GoogleOAuthToken {
        GoogleOAuthToken {
            id: "tok-1".to_string(),
            user_id: "db-user-1".to_string(),
            access_token: "at-stale".to_string(),
            refresh_token: refresh_token.map(|value| value.to_string()),
            expiry: expiry.to_string(),
            token_type: "Bearer".to_string(),
            scope: Some("calendar".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    /// Fake HTTP: a configurable refresh response (or failure).
    struct FakeHttp {
        refresh_body: Result<Vec<u8>, HttpError>,
        forms: Mutex<Vec<(String, Vec<(String, String)>)>>,
    }

    impl FakeHttp {
        fn ok(body: &str) -> Self {
            Self {
                refresh_body: Ok(body.as_bytes().to_vec()),
                forms: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl HttpClient for FakeHttp {
        async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
            self.forms.lock().unwrap().push((
                url.to_string(),
                form.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
            ));
            self.refresh_body.clone()
        }

        async fn get_bearer(&self, _url: &str, _token: &str) -> Result<Vec<u8>, HttpError> {
            Ok(Vec::new())
        }

        async fn get_bearer_raw(
            &self,
            _url: &str,
            _token: &str,
        ) -> Result<(u16, Vec<u8>), HttpError> {
            Ok((200, Vec::new()))
        }

        async fn post_json(
            &self,
            _url: &str,
            _token: &str,
            _body: &[u8],
        ) -> Result<(u16, Vec<u8>), HttpError> {
            Ok((200, Vec::new()))
        }

        async fn patch_json(
            &self,
            _url: &str,
            _token: &str,
            _body: &[u8],
        ) -> Result<(u16, Vec<u8>), HttpError> {
            Ok((200, Vec::new()))
        }
    }

    /// Fake token repo: returns the configured row; records upserts.
    struct FakeTokenRepo {
        stored: Option<GoogleOAuthToken>,
        upserted: Mutex<Option<NewToken>>,
        fail: Mutex<Option<RepoError>>,
    }

    #[async_trait::async_trait(?Send)]
    impl TokenRepo for FakeTokenRepo {
        async fn get_by_user_id(
            &self,
            _user_id: &str,
        ) -> Result<Option<GoogleOAuthToken>, RepoError> {
            Ok(self.stored.clone())
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

    fn fresh_expiry() -> String {
        // now = 1_700_000_000 (2023-11-14T22:13:20Z); expiry = now + 6 min.
        "2023-11-14T22:19:20Z".to_string()
    }

    #[test]
    fn fresh_token_is_returned_without_http_call() {
        let http = FakeHttp::ok(r#"{"access_token":"should-not-be-used","expires_in":3600}"#);
        let tokens = FakeTokenRepo {
            stored: Some(stored_token(&fresh_expiry(), Some("rt-1"))),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };

        let access = pollster::block_on(refresh_if_needed(
            &http, &tokens, &oauth(), "db-user-1", 1_700_000_000,
        ))
        .unwrap();

        assert_eq!(access.access_token, "at-stale");
        assert_eq!(access.token_type, "Bearer");
        assert!(http.forms.lock().unwrap().is_empty(), "no refresh POST");
        assert!(tokens.upserted.lock().unwrap().is_none(), "no token upsert");
    }

    #[test]
    fn expired_token_is_refreshed_and_persisted() {
        let http = FakeHttp::ok(
            r#"{"access_token":"at-new","expires_in":3599,"token_type":"Bearer","scope":"calendar"}"#,
        );
        let tokens = FakeTokenRepo {
            stored: Some(stored_token("2023-11-14T22:00:00Z", Some("rt-1"))), // already expired
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };

        let access = pollster::block_on(refresh_if_needed(
            &http, &tokens, &oauth(), "db-user-1", 1_700_000_000,
        ))
        .unwrap();

        assert_eq!(access.access_token, "at-new");
        assert_eq!(access.token_type, "Bearer");

        // Refresh POST hit the token endpoint with the refresh grant.
        let (url, form) = http.forms.lock().unwrap().first().unwrap().clone();
        assert_eq!(url, GOOGLE_TOKEN_URL);
        let form: std::collections::HashMap<String, String> = form.into_iter().collect();
        assert_eq!(form.get("grant_type").map(String::as_str), Some("refresh_token"));
        assert_eq!(form.get("refresh_token").map(String::as_str), Some("rt-1"));
        assert_eq!(
            form.get("client_id").map(String::as_str),
            Some("client-id.apps.googleusercontent.com")
        );
        assert_eq!(form.get("client_secret").map(String::as_str), Some("client-secret"));

        // Persisted with the new token and a fresh expiry (now + 3599).
        let upserted = tokens.upserted.lock().unwrap().clone().unwrap();
        assert_eq!(upserted.user_id, "db-user-1");
        assert_eq!(upserted.access_token, "at-new");
        assert_eq!(upserted.expiry, "2023-11-14T23:13:19Z");
        assert_eq!(upserted.token_type, "Bearer");
        // Refresh responses rarely include a refresh_token; the SQL COALESCE
        // protects the stored one (asserted on the SQL constant in repo.rs).
        assert!(upserted.refresh_token.is_none());
    }

    #[test]
    fn empty_token_type_defaults_to_bearer() {
        let http = FakeHttp::ok(r#"{"access_token":"at-new","expires_in":3600,"token_type":""}"#);
        let tokens = FakeTokenRepo {
            stored: Some(stored_token("2023-11-14T22:00:00Z", Some("rt-1"))),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let access = pollster::block_on(refresh_if_needed(
            &http, &tokens, &oauth(), "db-user-1", 1_700_000_000,
        ))
        .unwrap();
        assert_eq!(access.token_type, "Bearer");
        assert_eq!(tokens.upserted.lock().unwrap().as_ref().unwrap().token_type, "Bearer");
    }

    #[test]
    fn missing_token_is_an_error() {
        let http = FakeHttp::ok(r#"{"access_token":"x","expires_in":3600}"#);
        let tokens = FakeTokenRepo {
            stored: None,
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let err = pollster::block_on(refresh_if_needed(
            &http, &tokens, &oauth(), "db-user-1", 1_700_000_000,
        ))
        .unwrap_err();
        assert!(matches!(err, TokenError::NoToken), "got {err:?}");
    }

    #[test]
    fn missing_refresh_token_is_an_error() {
        let http = FakeHttp::ok(r#"{"access_token":"x","expires_in":3600}"#);
        let tokens = FakeTokenRepo {
            stored: Some(stored_token("2023-11-14T22:00:00Z", None)),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let err = pollster::block_on(refresh_if_needed(
            &http, &tokens, &oauth(), "db-user-1", 1_700_000_000,
        ))
        .unwrap_err();
        assert!(matches!(err, TokenError::NoRefreshToken), "got {err:?}");
        assert!(http.forms.lock().unwrap().is_empty(), "no POST without refresh token");
    }

    #[test]
    fn http_failure_propagates() {
        let http = FakeHttp {
            refresh_body: Err(HttpError::Message("connection refused".into())),
            forms: Mutex::new(Vec::new()),
        };
        let tokens = FakeTokenRepo {
            stored: Some(stored_token("2023-11-14T22:00:00Z", Some("rt-1"))),
            upserted: Mutex::new(None),
            fail: Mutex::new(None),
        };
        let err = pollster::block_on(refresh_if_needed(
            &http, &tokens, &oauth(), "db-user-1", 1_700_000_000,
        ))
        .unwrap_err();
        assert!(matches!(err, TokenError::Http(_)), "got {err:?}");
        assert!(tokens.upserted.lock().unwrap().is_none());
    }

    #[test]
    fn repo_error_propagates() {
        let http = FakeHttp::ok(r#"{"access_token":"x","expires_in":3600}"#);
        let tokens = FakeTokenRepo {
            stored: Some(stored_token("2023-11-14T22:00:00Z", Some("rt-1"))),
            upserted: Mutex::new(None),
            fail: Mutex::new(Some(RepoError::Backend("d1 down".into()))),
        };
        let err = pollster::block_on(refresh_if_needed(
            &http, &tokens, &oauth(), "db-user-1", 1_700_000_000,
        ))
        .unwrap_err();
        assert!(matches!(err, TokenError::Repo(_)), "got {err:?}");
    }
}
