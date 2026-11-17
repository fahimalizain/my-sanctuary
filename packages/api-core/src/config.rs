use serde::Deserialize;
use thiserror::Error;
use url::Url;

/// Minimum length (in bytes) required for `SESSION_SECRET`.
pub const MIN_SESSION_SECRET_LEN: usize = 32;

/// Frontend origin used for CORS when `FRONTEND_URL` is not configured.
/// Matches the Vite dev server used by `nx serve web`.
pub const DEFAULT_FRONTEND_URL: &str = "http://localhost:5173";

/// Google OAuth client credentials, parsed from the `GOOGLE_CREDENTIALS_JSON`
/// secret (the "OAuth 2.0 Client ID" JSON downloaded from Google Cloud Console).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

/// Runtime configuration for the Worker's API surface.
///
/// Loaded once per fetch from the Worker environment. `oauth` is `None` when
/// `GOOGLE_CREDENTIALS_JSON` is absent, in which case the OAuth routes report
/// "oauth not configured" while `/health`, `/version` and logged-out `/auth/*`
/// keep working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Symmetric key used to seal/unseal session cookies (>= 32 bytes).
    pub session_secret: String,
    /// Allowed cross-origin frontend origin for `Access-Control-Allow-Origin`.
    pub frontend_url: String,
    /// Whether session cookies are flagged `Secure` (true only in production).
    pub secure_cookie: bool,
    /// Google OAuth client credentials; `None` when `GOOGLE_CREDENTIALS_JSON`
    /// is not set (or empty).
    pub oauth: Option<OAuthConfig>,
    /// Full HTTPS callback URL for Google watch notifications
    /// (`WATCH_CALLBACK_URL`, e.g.
    /// `https://my-sanctuary.fahimalizain.com/api/calendar/notifications`);
    /// `None` when unset or empty, in which case watch I/O is skipped. Stored
    /// raw — the public-HTTPS check happens at use time (see
    /// [`crate::calendar::is_public_https_callback`]).
    pub watch_callback_url: Option<String>,
    /// Whether standalone username/password auth is allowed at all
    /// (`ALLOW_STANDALONE_AUTH == "true"`, ADR 0012). Default off.
    pub allow_standalone_auth: bool,
    /// IANA zone used when a standalone registration omits one
    /// (`STANDALONE_TIME_ZONE`); `UTC` when missing/empty.
    pub standalone_default_time_zone: String,
}

/// Time zone used for standalone registrations that do not supply one.
pub const DEFAULT_STANDALONE_TIME_ZONE: &str = "UTC";

/// Errors produced while loading [`Config`] from the environment.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("SESSION_SECRET is not set")]
    MissingSecret,
    #[error("SESSION_SECRET must be at least {MIN_SESSION_SECRET_LEN} bytes")]
    SecretTooShort,
    #[error("GOOGLE_CREDENTIALS_JSON is not valid JSON: {0}")]
    InvalidOAuthJson(String),
    #[error("GOOGLE_CREDENTIALS_JSON is missing client_id or client_secret")]
    MissingOAuthCredentials,
}

/// Raw shape of a Google credentials JSON file.
///
/// OAuth client files usually put the fields under `web`, but the same fields
/// may appear at the top level (e.g. for desktop/installed apps).
#[derive(Debug, Default, Deserialize)]
struct CredentialsFile {
    web: Option<WebCredentials>,
    client_id: Option<String>,
    client_secret: Option<String>,
    redirect_uris: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct WebCredentials {
    client_id: Option<String>,
    client_secret: Option<String>,
    redirect_uris: Option<Vec<String>>,
}

impl OAuthConfig {
    /// Parses a Google credentials JSON document (see [`CredentialsFile`]).
    ///
    /// `web.*` fields win over top-level ones when the `web` object exists,
    /// mirroring the old Go loader. The chosen `redirect_url` is the first
    /// `redirect_uris` entry whose hostname matches `frontend_url`'s hostname,
    /// else the first URI, else `{frontend_url}/auth/google/callback`.
    pub fn from_credentials_json(json: &str, frontend_url: &str) -> Result<Self, ConfigError> {
        let file: CredentialsFile =
            serde_json::from_str(json).map_err(|err| ConfigError::InvalidOAuthJson(err.to_string()))?;

        let (client_id, client_secret, redirect_uris) = match file.web {
            Some(web) => (web.client_id, web.client_secret, web.redirect_uris),
            None => (file.client_id, file.client_secret, file.redirect_uris),
        };
        let client_id = client_id.unwrap_or_default();
        let client_secret = client_secret.unwrap_or_default();
        if client_id.is_empty() || client_secret.is_empty() {
            return Err(ConfigError::MissingOAuthCredentials);
        }

        let redirect_url = pick_redirect_url(redirect_uris.as_deref(), frontend_url);
        Ok(Self {
            client_id,
            client_secret,
            redirect_url,
        })
    }
}

/// Selects the OAuth redirect URI: first entry whose hostname matches the
/// frontend hostname, else the first URI, else `{frontend_url}/auth/google/callback`.
fn pick_redirect_url(redirect_uris: Option<&[String]>, frontend_url: &str) -> String {
    let frontend_host = hostname_from_url(frontend_url);
    if let Some(uris) = redirect_uris {
        if let Some(uri) = uris
            .iter()
            .find(|uri| hostname_from_url(uri) == frontend_host)
        {
            return uri.clone();
        }
        if let Some(first) = uris.first() {
            return first.clone();
        }
    }
    format!("{frontend_url}/auth/google/callback")
}

/// Lowercased hostname of a URL, or `None` when the string is not a valid URL.
/// Ports are ignored: `http://localhost:5173` and `http://localhost:9999`
/// both match hostname `localhost` (same as Go's `url.URL.Hostname()`).
fn hostname_from_url(raw: &str) -> Option<String> {
    Url::parse(raw)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
}

impl Config {
    /// Loads configuration from an environment lookup function.
    ///
    /// - `SESSION_SECRET` is required and must be at least 32 bytes.
    /// - `FRONTEND_URL` defaults to `http://localhost:5173` when missing or empty.
    /// - `SECURE_COOKIE` is true only when its value is exactly `"true"`.
    /// - `GOOGLE_CREDENTIALS_JSON` is optional; when present it must parse to
    ///   valid OAuth credentials or `from_env` fails. An absent or empty value
    ///   leaves `oauth: None`.
    /// - `WATCH_CALLBACK_URL` is optional and stored raw (no validation —
    ///   missing or empty yields `watch_callback_url: None`).
    /// - `ALLOW_STANDALONE_AUTH` enables standalone auth only when its value
    ///   is exactly `"true"` (ADR 0012); default off.
    /// - `STANDALONE_TIME_ZONE` defaults to `UTC` when missing or empty.
    pub fn from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let session_secret = getenv("SESSION_SECRET").ok_or(ConfigError::MissingSecret)?;
        if session_secret.len() < MIN_SESSION_SECRET_LEN {
            return Err(ConfigError::SecretTooShort);
        }
        let frontend_url = getenv("FRONTEND_URL")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_FRONTEND_URL.to_string());
        let secure_cookie = getenv("SECURE_COOKIE").as_deref() == Some("true");
        let oauth = match getenv("GOOGLE_CREDENTIALS_JSON") {
            Some(json) if !json.trim().is_empty() => {
                Some(OAuthConfig::from_credentials_json(&json, &frontend_url)?)
            }
            _ => None,
        };
        let watch_callback_url = getenv("WATCH_CALLBACK_URL").filter(|value| !value.is_empty());
        let allow_standalone_auth = getenv("ALLOW_STANDALONE_AUTH").as_deref() == Some("true");
        let standalone_default_time_zone = getenv("STANDALONE_TIME_ZONE")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_STANDALONE_TIME_ZONE.to_string());
        Ok(Self {
            session_secret,
            frontend_url,
            secure_cookie,
            oauth,
            watch_callback_url,
            allow_standalone_auth,
            standalone_default_time_zone,
        })
    }

    /// Whether standalone username/password auth may serve this request
    /// (ADR 0012).
    ///
    /// All of the following must hold, otherwise `false` (fail closed):
    /// - [`Config::allow_standalone_auth`] (`ALLOW_STANDALONE_AUTH=true`);
    /// - [`Config::secure_cookie`] is `false` (production sessions are secure);
    /// - the configured `frontend_url` host is non-public;
    /// - `request_host` is present, parseable, and non-public.
    ///
    /// Non-public = `localhost`, `*.localhost`, `*.local`, `*.lan`, loopback
    /// (127/8, `::1`), RFC 1918 (10/8, 172.16/12, 192.168/16), or link-local
    /// (169.254/16, `fe80::/10`). Ports and IPv6 brackets are stripped. There
    /// is deliberately no allowlist: a public hostname is always rejected.
    pub fn standalone_enabled_for(&self, request_host: Option<&str>) -> bool {
        if !self.allow_standalone_auth || self.secure_cookie {
            return false;
        }
        let Some(frontend_host) = hostname_from_url(&self.frontend_url) else {
            return false;
        };
        if !is_non_public_host(&frontend_host) {
            return false;
        }
        match request_host {
            Some(raw) => is_non_public_host(raw),
            None => false,
        }
    }
}

/// Strips a port and IPv6 brackets, lowercases, and drops a trailing dot.
/// Returns `None` for an empty/invalid host. Bare IPv6 (`::1`, `fe80::1`) is
/// accepted without brackets.
fn normalize_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let host = if let Some(rest) = trimmed.strip_prefix('[') {
        // Bracketed IPv6, optionally followed by `:port`.
        let (inner, _) = rest.split_once(']')?;
        inner
    } else if trimmed.matches(':').count() == 1 {
        // `host:port` (a single colon cannot be IPv6).
        trimmed.split_once(':').map_or(trimmed, |(host, _)| host)
    } else {
        // No colon (hostname/IPv4) or a bare IPv6 literal.
        trimmed
    };
    let host = host.trim().trim_end_matches('.');
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Whether `raw_host` (possibly `host:port` / `[v6]:port`) is a non-public
/// development host. Unparseable input fails closed (`false`).
fn is_non_public_host(raw_host: &str) -> bool {
    let Some(host) = normalize_host(raw_host) else {
        return false;
    };
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".lan")
    {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xffc0) == 0xfe80,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef"; // 32 bytes
    const WEB_JSON: &str = r#"{
        "web": {
            "client_id": "web-client.apps.googleusercontent.com",
            "client_secret": "web-secret",
            "redirect_uris": ["http://localhost:5173/auth/google/callback"]
        }
    }"#;
    const TOP_LEVEL_JSON: &str = r#"{
        "client_id": "top-level-client.apps.googleusercontent.com",
        "client_secret": "top-level-secret",
        "redirect_uris": ["https://sanctuary.example.com/auth/google/callback"]
    }"#;

    fn lookup<'a>(entries: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
    }

    #[test]
    fn loads_full_config() {
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("FRONTEND_URL", "https://sanctuary.example.com"),
            ("SECURE_COOKIE", "true"),
            ("GOOGLE_CREDENTIALS_JSON", WEB_JSON),
        ]))
        .unwrap();
        assert_eq!(config.session_secret, SECRET);
        assert_eq!(config.frontend_url, "https://sanctuary.example.com");
        assert!(config.secure_cookie);
        let oauth = config.oauth.unwrap();
        assert_eq!(oauth.client_id, "web-client.apps.googleusercontent.com");
        assert_eq!(oauth.client_secret, "web-secret");
        assert_eq!(oauth.redirect_url, "http://localhost:5173/auth/google/callback");
    }

    #[test]
    fn missing_secret_is_an_error() {
        let err = Config::from_env(lookup(&[])).unwrap_err();
        assert_eq!(err, ConfigError::MissingSecret);
    }

    #[test]
    fn short_secret_is_an_error() {
        let err = Config::from_env(lookup(&[("SESSION_SECRET", "way-too-short")])).unwrap_err();
        assert_eq!(err, ConfigError::SecretTooShort);
    }

    #[test]
    fn missing_frontend_url_defaults_to_localhost() {
        let config = Config::from_env(lookup(&[("SESSION_SECRET", SECRET)])).unwrap();
        assert_eq!(config.frontend_url, DEFAULT_FRONTEND_URL);
    }

    #[test]
    fn empty_frontend_url_defaults_to_localhost() {
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("FRONTEND_URL", ""),
        ]))
        .unwrap();
        assert_eq!(config.frontend_url, DEFAULT_FRONTEND_URL);
    }

    #[test]
    fn secure_cookie_is_true_only_for_exact_true() {
        for value in ["false", "TRUE", "1", "yes"] {
            let config =
                Config::from_env(lookup(&[("SESSION_SECRET", SECRET), ("SECURE_COOKIE", value)]))
                    .unwrap();
            assert!(!config.secure_cookie, "SECURE_COOKIE={value:?} must be false");
        }
        let config =
            Config::from_env(lookup(&[("SESSION_SECRET", SECRET), ("SECURE_COOKIE", "true")]))
                .unwrap();
        assert!(config.secure_cookie);
    }

    #[test]
    fn missing_secure_cookie_defaults_to_false() {
        let config = Config::from_env(lookup(&[("SESSION_SECRET", SECRET)])).unwrap();
        assert!(!config.secure_cookie);
    }

    #[test]
    fn missing_google_credentials_yields_no_oauth() {
        let config = Config::from_env(lookup(&[("SESSION_SECRET", SECRET)])).unwrap();
        assert!(config.oauth.is_none(), "no GOOGLE_CREDENTIALS_JSON means no oauth");
    }

    #[test]
    fn empty_google_credentials_yields_no_oauth() {
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("GOOGLE_CREDENTIALS_JSON", ""),
        ]))
        .unwrap();
        assert!(config.oauth.is_none());
    }

    #[test]
    fn invalid_google_credentials_json_is_an_error() {
        let err = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("GOOGLE_CREDENTIALS_JSON", "{not json"),
        ]))
        .unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidOAuthJson(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn credentials_missing_client_id_is_an_error() {
        let err = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("GOOGLE_CREDENTIALS_JSON", r#"{"web":{"client_secret":"only-secret"}}"#),
        ]))
        .unwrap_err();
        assert_eq!(err, ConfigError::MissingOAuthCredentials);
    }

    #[test]
    fn credentials_missing_client_secret_is_an_error() {
        let err = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("GOOGLE_CREDENTIALS_JSON", r#"{"web":{"client_id":"only-id"}}"#),
        ]))
        .unwrap_err();
        assert_eq!(err, ConfigError::MissingOAuthCredentials);
    }

    #[test]
    fn web_shaped_json_is_parsed() {
        let config = OAuthConfig::from_credentials_json(WEB_JSON, DEFAULT_FRONTEND_URL).unwrap();
        assert_eq!(config.client_id, "web-client.apps.googleusercontent.com");
        assert_eq!(config.client_secret, "web-secret");
        assert_eq!(config.redirect_url, "http://localhost:5173/auth/google/callback");
    }

    #[test]
    fn top_level_json_is_parsed_when_web_absent() {
        let config = OAuthConfig::from_credentials_json(TOP_LEVEL_JSON, "https://sanctuary.example.com")
            .unwrap();
        assert_eq!(config.client_id, "top-level-client.apps.googleusercontent.com");
        assert_eq!(config.client_secret, "top-level-secret");
        assert_eq!(
            config.redirect_url,
            "https://sanctuary.example.com/auth/google/callback"
        );
    }

    #[test]
    fn web_object_wins_over_top_level_fields() {
        let json = r#"{
            "web": {"client_id": "web-id", "client_secret": "web-secret"},
            "client_id": "top-id",
            "client_secret": "top-secret"
        }"#;
        let config = OAuthConfig::from_credentials_json(json, DEFAULT_FRONTEND_URL).unwrap();
        assert_eq!(config.client_id, "web-id");
        assert_eq!(config.client_secret, "web-secret");
    }

    #[test]
    fn redirect_url_picks_uri_matching_frontend_hostname() {
        // Production URI listed first; localhost URI must win for a local frontend.
        let json = r#"{
            "web": {
                "client_id": "c",
                "client_secret": "s",
                "redirect_uris": [
                    "https://sanctuary.example.com/auth/google/callback",
                    "http://localhost:5173/auth/google/callback"
                ]
            }
        }"#;
        let config = OAuthConfig::from_credentials_json(json, DEFAULT_FRONTEND_URL).unwrap();
        assert_eq!(config.redirect_url, "http://localhost:5173/auth/google/callback");

        // And the production frontend picks the production URI (hostname match ignores port).
        let config = OAuthConfig::from_credentials_json(json, "https://sanctuary.example.com:8443").unwrap();
        assert_eq!(config.redirect_url, "https://sanctuary.example.com/auth/google/callback");
    }

    #[test]
    fn redirect_url_falls_back_to_first_uri_without_hostname_match() {
        let json = r#"{
            "web": {
                "client_id": "c",
                "client_secret": "s",
                "redirect_uris": [
                    "https://other.example.com/auth/google/callback",
                    "https://yet-another.example.com/auth/google/callback"
                ]
            }
        }"#;
        let config = OAuthConfig::from_credentials_json(json, DEFAULT_FRONTEND_URL).unwrap();
        assert_eq!(config.redirect_url, "https://other.example.com/auth/google/callback");
    }

    #[test]
    fn missing_watch_callback_url_yields_none() {
        let config = Config::from_env(lookup(&[("SESSION_SECRET", SECRET)])).unwrap();
        assert!(config.watch_callback_url.is_none());
    }

    #[test]
    fn empty_watch_callback_url_yields_none() {
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("WATCH_CALLBACK_URL", ""),
        ]))
        .unwrap();
        assert!(config.watch_callback_url.is_none());
    }

    #[test]
    fn watch_callback_url_is_stored_raw() {
        // Even a non-public URL is stored as-is: the public-HTTPS predicate is
        // applied at use time (`is_public_https_callback` in list_events), not
        // at config load.
        let raw = "http://localhost:8787/api/calendar/notifications";
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("WATCH_CALLBACK_URL", raw),
        ]))
        .unwrap();
        assert_eq!(config.watch_callback_url.as_deref(), Some(raw));
    }

    #[test]
    fn redirect_url_defaults_to_frontend_callback_without_uris() {
        let json = r#"{"web": {"client_id": "c", "client_secret": "s"}}"#;
        let config = OAuthConfig::from_credentials_json(json, DEFAULT_FRONTEND_URL).unwrap();
        assert_eq!(config.redirect_url, "http://localhost:5173/auth/google/callback");
    }

    // ── standalone gate (ADR 0012) ──────────────────────────────────────

    /// The smallest config that can enable standalone auth. `entries` come
    /// first and therefore win (the lookup helper returns the first match).
    fn standalone_config(entries: &[(&str, &str)]) -> Config {
        let mut all: Vec<(&str, &str)> = entries.to_vec();
        all.extend_from_slice(&[
            ("SESSION_SECRET", SECRET),
            ("ALLOW_STANDALONE_AUTH", "true"),
            ("SECURE_COOKIE", "false"),
            ("FRONTEND_URL", "http://localhost:5173"),
        ]);
        Config::from_env(lookup(&all)).unwrap()
    }

    #[test]
    fn standalone_is_off_by_default() {
        let config = Config::from_env(lookup(&[("SESSION_SECRET", SECRET)])).unwrap();
        assert!(!config.allow_standalone_auth);
        assert!(!config.standalone_enabled_for(Some("localhost")));
        assert!(!config.standalone_enabled_for(Some("127.0.0.1")));
    }

    #[test]
    fn standalone_flag_requires_the_exact_true_value() {
        for value in ["false", "TRUE", "True", "1", "yes", ""] {
            let config = standalone_config(&[("ALLOW_STANDALONE_AUTH", value)]);
            assert!(
                !config.allow_standalone_auth,
                "ALLOW_STANDALONE_AUTH={value:?} must be off"
            );
            assert!(!config.standalone_enabled_for(Some("localhost")));
        }
        assert!(standalone_config(&[]).standalone_enabled_for(Some("localhost")));
    }

    #[test]
    fn standalone_is_disabled_by_secure_cookie() {
        let config = standalone_config(&[("SECURE_COOKIE", "true")]);
        assert!(!config.standalone_enabled_for(Some("localhost")));
    }

    #[test]
    fn standalone_is_disabled_by_a_public_frontend_url() {
        let config = standalone_config(&[("FRONTEND_URL", "https://sanctuary.example.com")]);
        assert!(!config.standalone_enabled_for(Some("localhost")));
        // Even against a local request host: the configured origin is public.
        assert!(!config.standalone_enabled_for(Some("127.0.0.1")));
    }

    #[test]
    fn standalone_is_disabled_by_an_unparseable_frontend_url() {
        let config = standalone_config(&[("FRONTEND_URL", "not a url")]);
        assert!(!config.standalone_enabled_for(Some("localhost")));
    }

    #[test]
    fn standalone_is_disabled_by_a_public_request_host() {
        let config = standalone_config(&[]);
        for host in [
            "sanctuary.example.com",
            "my-sanctuary.fahimalizain.com",
            "8.8.8.8",
            "172.32.0.1",
            "192.169.0.1",
            "example.local.evil.com",
        ] {
            assert!(
                !config.standalone_enabled_for(Some(host)),
                "public host {host:?} must not enable standalone"
            );
        }
    }

    #[test]
    fn standalone_is_disabled_when_the_request_host_is_missing_or_garbage() {
        let config = standalone_config(&[]);
        assert!(!config.standalone_enabled_for(None));
        assert!(!config.standalone_enabled_for(Some("")));
        assert!(!config.standalone_enabled_for(Some("   ")));
        assert!(!config.standalone_enabled_for(Some("[::1")));
        assert!(!config.standalone_enabled_for(Some("[not-ipv6]")));
    }

    #[test]
    fn standalone_missing_frontend_url_cannot_enable_against_a_public_host() {
        // FRONTEND_URL defaults to localhost, so the frontend half passes…
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("ALLOW_STANDALONE_AUTH", "true"),
        ]))
        .unwrap();
        assert_eq!(config.frontend_url, DEFAULT_FRONTEND_URL);
        // …but the request host is still required to be non-public.
        assert!(!config.standalone_enabled_for(Some("sanctuary.example.com")));
        assert!(config.standalone_enabled_for(Some("localhost")));
    }

    #[test]
    fn standalone_enables_for_loopback_and_private_hosts() {
        let config = standalone_config(&[]);
        for host in [
            "localhost",
            "localhost:8787",
            "LOCALHOST",
            "127.0.0.1",
            "127.0.0.1:8787",
            "127.1.2.3",
            "::1",
            "[::1]",
            "[::1]:8787",
            "10.0.0.5",
            "10.255.255.254:5173",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.20",
            "192.168.1.20:5173",
            "169.254.10.10",
            "fe80::1",
            "[fe80::1]:8787",
            "dev.localhost",
            "sanctuary.local",
            "sanctuary.lan",
            "sanctuary.local.",
        ] {
            assert!(
                config.standalone_enabled_for(Some(host)),
                "non-public host {host:?} must enable standalone"
            );
        }
    }

    #[test]
    fn standalone_time_zone_defaults_to_utc() {
        let config = Config::from_env(lookup(&[("SESSION_SECRET", SECRET)])).unwrap();
        assert_eq!(config.standalone_default_time_zone, "UTC");
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("STANDALONE_TIME_ZONE", ""),
        ]))
        .unwrap();
        assert_eq!(config.standalone_default_time_zone, "UTC");
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("STANDALONE_TIME_ZONE", "Asia/Kolkata"),
        ]))
        .unwrap();
        assert_eq!(config.standalone_default_time_zone, "Asia/Kolkata");
    }
}
