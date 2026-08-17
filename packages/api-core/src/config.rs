use thiserror::Error;

/// Minimum length (in bytes) required for `SESSION_SECRET`.
pub const MIN_SESSION_SECRET_LEN: usize = 32;

/// Frontend origin used for CORS when `FRONTEND_URL` is not configured.
/// Matches the Vite dev server used by `nx serve web`.
pub const DEFAULT_FRONTEND_URL: &str = "http://localhost:5173";

/// Runtime configuration for the Worker's API surface.
///
/// Loaded once per fetch from the Worker environment. Slice 3 will extend this
/// with Google OAuth settings; `DATABASE_DSN` is deliberately not part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Symmetric key used to seal/unseal session cookies (>= 32 bytes).
    pub session_secret: String,
    /// Allowed cross-origin frontend origin for `Access-Control-Allow-Origin`.
    pub frontend_url: String,
    /// Whether session cookies are flagged `Secure` (true only in production).
    pub secure_cookie: bool,
}

/// Errors produced while loading [`Config`] from the environment.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("SESSION_SECRET is not set")]
    MissingSecret,
    #[error("SESSION_SECRET must be at least {MIN_SESSION_SECRET_LEN} bytes")]
    SecretTooShort,
}

impl Config {
    /// Loads configuration from an environment lookup function.
    ///
    /// - `SESSION_SECRET` is required and must be at least 32 bytes.
    /// - `FRONTEND_URL` defaults to `http://localhost:5173` when missing or empty.
    /// - `SECURE_COOKIE` is true only when its value is exactly `"true"`.
    pub fn from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let session_secret = getenv("SESSION_SECRET").ok_or(ConfigError::MissingSecret)?;
        if session_secret.len() < MIN_SESSION_SECRET_LEN {
            return Err(ConfigError::SecretTooShort);
        }
        let frontend_url = getenv("FRONTEND_URL")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_FRONTEND_URL.to_string());
        let secure_cookie = getenv("SECURE_COOKIE").as_deref() == Some("true");
        Ok(Self {
            session_secret,
            frontend_url,
            secure_cookie,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef"; // 32 bytes

    fn lookup<'a>(entries: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
    }

    #[test]
    fn loads_full_config() {
        let config = Config::from_env(lookup(&[
            ("SESSION_SECRET", SECRET),
            ("FRONTEND_URL", "https://sanctuary.example.com"),
            ("SECURE_COOKIE", "true"),
        ]))
        .unwrap();
        assert_eq!(config.session_secret, SECRET);
        assert_eq!(config.frontend_url, "https://sanctuary.example.com");
        assert!(config.secure_cookie);
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
}
