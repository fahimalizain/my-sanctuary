# Cloudflare Workers Go WASM: "Illegal Invocation" in OAuth2 Token Exchange

## Summary

When running Go code compiled to WebAssembly on Cloudflare Workers, calling `golang.org/x/oauth2` to exchange an authorization code for a token can panic with:

```
JavaScript error: Illegal invocation: function called with incorrect `this` reference.
```

This document explains why the error occurs, how it manifests in the stack trace, and the fix applied to ensure outbound HTTP calls use the runtime's `fetch` API with correct `this` binding.

---

## The Error

### What you see in the logs

```
panic: JavaScript error: Illegal invocation: function called with incorrect `this` reference.
See https://developers.cloudflare.com/workers/observability/errors/#illegal-invocation-errors for details.

/home/runner/go/pkg/mod/golang.org/x/oauth2@v0.36.0/internal/token.go:259 +0x21
golang.org/x/oauth2/internal.doTokenRoundTrip(...)
/home/runner/go/pkg/mod/golang.org/x/oauth2@v0.36.0/token.go:175 +0x5
golang.org/x/oauth2.(*Config).Exchange(...)
/home/runner/go/pkg/mod/golang.org/x/oauth2@v0.36.0/oauth2.go:233 +0x26
my-sanctuary/apps/api/handlers.(*AuthHandler).Callback(...)
```

The panic originates inside `golang.org/x/oauth2` when it attempts to POST to `https://oauth2.googleapis.com/token`. The same error also happens on any subsequent API call (e.g. Google Calendar) that uses the default HTTP transport.

---

## Why It Happens

### 1. Go WASM uses the browser/JS `fetch` API under the hood

When Go is compiled to WebAssembly (`GOOS=js GOARCH=wasm`), the `net/http` package does not use a native TCP stack. Instead, it delegates HTTP requests to the JavaScript host's `fetch` API. Go's `syscall/js` bridge looks up `fetch` on the global object and calls it.

### 2. The Cloudflare Workers `fetch` binding requires correct `this`

In Cloudflare Workers, `fetch` is implemented by the runtime. Unlike the browser's `window.fetch`, which can be detached and called as a plain function, the Workers `fetch` is a method on an internal object and **requires its `this` binding to remain intact**.

Per [Cloudflare's documentation](https://developers.cloudflare.com/workers/observability/errors/#illegal-invocation-errors):

> This is typically caused by calling a function that calls `this`, but the value of `this` has been lost.

The equivalent JavaScript bug looks like this:

```javascript
const { fetch } = globalThis;  // loses `this`
fetch(url);                    // throws "Illegal invocation"
```

### 3. `golang.org/x/oauth2` uses `http.DefaultClient` by default

Inside `oauth2/internal/transport.go`, the library resolves the HTTP client to use for token exchange:

```go
func ContextClient(ctx context.Context) *http.Client {
    if ctx != nil {
        if hc, ok := ctx.Value(HTTPClient).(*http.Client); ok {
            return hc
        }
    }
    return http.DefaultClient
}
```

If you call `oauthConfig.Exchange(ctx, code)` with a plain context (no `oauth2.HTTPClient` value), it falls back to `http.DefaultClient`. That client uses Go's default transport, which in the WASM runtime resolves to a loose `fetch` reference and triggers the `Illegal invocation` panic.

### 4. The stack trace confirms the path

- `(*Config).Exchange(...)` → calls `retrieveToken(...)`
- `retrieveToken(...)` → calls `doTokenRoundTrip(...)`
- `doTokenRoundTrip(...)` → calls `ContextClient(ctx).Do(req)`
- `ContextClient` returns `http.DefaultClient`
- `http.DefaultClient.Do(...)` → Go WASM bridge → loose `fetch` → **panic**

---

## The Fix

### Principle

Inject a custom `*http.Client` into the OAuth2 context using `oauth2.HTTPClient` so that `golang.org/x/oauth2` uses an HTTP client whose `Transport` is backed by the Workers `fetch` API with correct `this` binding, rather than the default WASM transport.

### In `syumai/workers`

The `github.com/syumai/workers/cloudflare/fetch` package provides exactly this:

```go
package fetch

// transport is an implementation of http.RoundTripper
type transport struct {
    namespace js.Value
    redirect  RedirectMode
}

func (t *transport) RoundTrip(req *http.Request) (*http.Response, error) {
    return fetch(t.namespace, req, &RequestInit{Redirect: t.redirect})
}
```

The `RoundTrip` method does **not** detach `fetch` from its namespace. It invokes it through the JS object (`js.Value.Call`), preserving the `this` reference.

### Changes applied to the codebase

#### 1. `apps/api/handlers/auth.go`

`AuthHandler` now stores a custom `*http.Client` and provides a helper to inject it into the context:

```go
type AuthHandler struct {
    oauthConfig *oauth2.Config
    store       *sessions.CookieStore
    frontendURL string
    httpClient  *http.Client  // NEW
}

func (h *AuthHandler) oauth2Context(ctx context.Context) context.Context {
    if h.httpClient != nil {
        return context.WithValue(ctx, oauth2.HTTPClient, h.httpClient)
    }
    return ctx
}
```

All OAuth2 calls wrap the incoming context:

```go
token, err := h.oauthConfig.Exchange(h.oauth2Context(r.Context()), code)
client := h.oauthConfig.Client(h.oauth2Context(r.Context()), token)
```

#### 2. `apps/api/handlers/calendar.go`

Calendar API calls also use the wrapped context:

```go
client := h.auth.oauthConfig.Client(h.auth.oauth2Context(r.Context()), token)
```

#### 3. `apps/api/handlers/handlers.go`

`Dependencies` now carries the HTTP client:

```go
type Dependencies struct {
    Config     *config.Config
    HTTPClient *http.Client  // NEW
}
```

#### 4. Local dev entrypoint (`apps/api/main.go`)

Local development passes `http.DefaultClient`, which works fine on native builds:

```go
handlers.RegisterRoutes(router, &handlers.Dependencies{
    Config:     cfg,
    HTTPClient: http.DefaultClient,
})
```

#### 5. Cloudflare Workers entrypoint (`apps/cloudflare-deploy/main.go`)

The Workers entrypoint passes the Cloudflare-aware fetch client:

```go
import "github.com/syumai/workers/cloudflare/fetch"

handlers.RegisterRoutes(router, &handlers.Dependencies{
    Config:     cfg,
    HTTPClient: fetch.NewClient().HTTPClient(fetch.RedirectModeFollow),
})
```

This ensures every outbound HTTP request in the OAuth2 flow goes through `syumai/workers`' properly-bound `fetch` transport.

---

## Verification

After the fix, all Nx targets pass:

```bash
nx run api:tidy
nx run api:build
nx run api:lint
nx run api:test
nx run cloudflare-deploy:build
```

The Workers runtime can now exchange Google authorization codes and call Google APIs without `Illegal invocation` panics.

---

## Key Takeaways

1. **Go WASM on Workers delegates `net/http` to JS `fetch`.**
2. **The Workers `fetch` is method-bound and requires correct `this`.**
3. **The default Go WASM transport can detach `fetch`, losing `this` and causing the panic.**
4. **The fix is to provide a custom `http.Client` with a transport that preserves the `fetch` binding.**
5. **`golang.org/x/oauth2` supports this via the `oauth2.HTTPClient` context key.**
6. **`syumai/workers/cloudflare/fetch` provides a ready-made `http.Client` that does this correctly.**
