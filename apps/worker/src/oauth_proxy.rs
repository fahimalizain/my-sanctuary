//! Verbatim proxy to the private OAuth authorization-server Worker.
//!
//! The authorization server (`apps/auth`, Worker `my-sanctuary-auth`) owns the OAuth 2.1
//! protocol but not the public origin: its discovery document and every endpoint URL must
//! read `https://my-sanctuary.fahimalizain.com`. It is therefore deployed with no route of
//! its own, and this Worker re-issues `/oauth/*` and `/.well-known/oauth-authorization-server*`
//! to it over the `AUTH` service binding with the original URL, method, headers and body.
//!
//! Redirects are fetched with `RequestRedirect::Manual`: the authorization server's 302s
//! (to `/auth/google` for sign-in, and to the client's redirect URI after consent) belong
//! to the browser. Following them here would consume the `Set-Cookie` across the wrong
//! origin and hand the browser the wrong `Location`.
//!
//! `/internal/*` is never forwarded: `api_core::is_auth_server_path` excludes it, and the
//! stamped header makes the authorization server answer 404 even if it were.

use worker::*;

/// Marks a request as having come through the public proxy. It is set here, overwriting
/// anything a client sent, because the authorization server 404s `/internal/*` requests
/// that carry it (see `apps/auth/src/validate.ts`).
const PROXY_HEADER: &str = "x-sanctuary-proxy";

/// Forward `req` to the authorization-server Worker, unchanged apart from the proxy header.
pub async fn forward(req: Request, env: &Env) -> Result<Response> {
    let service = match env.service("AUTH") {
        Ok(service) => service,
        Err(err) => {
            console_log!("oauth proxy: AUTH binding missing: {err}");
            return unavailable();
        }
    };

    // `new Request(input, init)` keeps the input's URL and body when the init supplies
    // neither, so the consent form POST, token form and DCR JSON stream through without
    // being buffered. Headers are copied because the incoming set is immutable, and `set`
    // replaces any client-supplied proxy header.
    let headers = Headers::new();
    for (name, value) in req.headers().entries() {
        headers.append(&name, &value)?;
    }
    headers.set(PROXY_HEADER, "oauth")?;

    let mut init = RequestInit::new();
    init.with_method(req.method())
        .with_headers(headers)
        .with_redirect(RequestRedirect::Manual);

    let web_init: web_sys::RequestInit = (&init).into();
    let forwarded = web_sys::Request::new_with_request_and_init(req.inner(), &web_init).map_err(
        |err| {
            Error::JsError(
                err.as_string()
                    .unwrap_or_else(|| "failed to build the oauth proxy request".to_string()),
            )
        },
    )?;

    match service.fetch_request(Request::from(forwarded)).await {
        Ok(response) => Ok(response),
        Err(err) => {
            console_log!("oauth proxy: AUTH fetch failed: {err}");
            unavailable()
        }
    }
}

/// A missing binding or a failed fetch is this Worker's failure, not the AS's: answer 502
/// rather than letting the caller mistake an outage for a protocol error.
fn unavailable() -> Result<Response> {
    Response::error("oauth unavailable", 502)
}
