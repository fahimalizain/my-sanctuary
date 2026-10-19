//! Which public paths belong to the OAuth authorization-server Worker.
//!
//! The authorization server runs as a separate, private Worker because discovery and
//! endpoint URLs must keep the public issuer origin. The public Worker forwards exactly
//! these paths to it verbatim (method, URL, headers, body), so this predicate is that
//! forwarding contract — natively tested here rather than inferred in the Worker's router.

/// RFC 8414 metadata path for this authorization server.
const METADATA_PATH: &str = "/.well-known/oauth-authorization-server";

/// Whether `path` is served by the private authorization-server Worker.
///
/// `/oauth/*` covers authorize, token, RFC 7009 revocation and dynamic client
/// registration. `/.well-known/oauth-authorization-server` is the metadata document,
/// including the path-suffixed forms clients probe by appending the issuer path.
///
/// Everything else stays on this Worker: `/oauth` (no trailing slash), `/mcp`, the
/// protected-resource metadata (`/.well-known/oauth-protected-resource*`), `/internal/*`,
/// and OIDC discovery, which the library does not serve.
pub fn is_auth_server_path(path: &str) -> bool {
    path.starts_with("/oauth/")
        || path == METADATA_PATH
        || path
            .strip_prefix(METADATA_PATH)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_paths_forward() {
        for path in [
            "/oauth/",
            "/oauth/authorize",
            "/oauth/token",
            "/oauth/register",
            "/oauth/authorize/extra",
        ] {
            assert!(is_auth_server_path(path), "{path} must forward");
        }
    }

    #[test]
    fn authorization_server_metadata_forwards() {
        for path in [
            "/.well-known/oauth-authorization-server",
            "/.well-known/oauth-authorization-server/",
            "/.well-known/oauth-authorization-server/tenant",
        ] {
            assert!(is_auth_server_path(path), "{path} must forward");
        }
    }

    #[test]
    fn other_paths_stay_on_this_worker() {
        for path in [
            "/",
            "/oauth",
            "/oauthx",
            "/.well-known/oauth-authorization-serverx",
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
            "/.well-known/openid-configuration",
            "/mcp",
            "/internal/validate",
            "/internal",
            "/auth/me",
        ] {
            assert!(!is_auth_server_path(path), "{path} must not forward");
        }
    }
}
