# ADR 0009: Remote MCP and OAuth

Status: Accepted
Date: 2026-10-19
Implemented: branch `feat/mcp-oauth-auth-worker`.

## Context

The remote MCP server will live at `https://my-sanctuary.fahimalizain.com/mcp`.
MCP clients (Claude, ChatGPT, others) expect the MCP authorization profile:
OAuth 2.1 with RFC 8414 authorization-server metadata, RFC 9728 protected-resource
metadata, dynamic client registration (RFC 7591) or Client ID Metadata Documents,
PKCE S256, RFC 8707 resource indicators, refresh tokens, and RFC 7009 revocation.

The app's only sign-in is Google OAuth, and it ends in a sealed session cookie
read by the Rust Worker (`GET /auth/me`). There is no password, no token
endpoint, and no identity the app owns besides the D1 user row. An MCP client has
no cookie: it needs a bearer token the resource server can verify without a
browser present.

Three constraints shaped the design:

- The issuer must be the app origin, because that is what the client discovers
  from the protected-resource metadata and what its credentials are scoped to.
- The Rust Worker already owns `/auth/*`, the session cookie, and D1. It is the
  only component that can say who the visitor is.
- The authorization server is security-sensitive protocol code. It should be
  small, and its contract with the rest of the app should be one endpoint.

## Decision

### 1. Two Workers, one public origin

`my-sanctuary-auth` (`apps/auth`) is a TypeScript Worker and the OAuth 2.1
authorization server, built on `@cloudflare/workers-oauth-provider` 1.2.3's
split-role `OAuthAuthorizationServer`. It has no public route:
`workers_dev = false`, `preview_urls = false`, no `routes`.

The public Rust Worker is the only origin. It re-issues `/oauth/*` and
`/.well-known/oauth-authorization-server*` to `my-sanctuary-auth` over the `AUTH`
service binding, keeping the original URL, method, headers, and streamed body, so
the AS builds every endpoint URL from the public issuer. Redirects are fetched
manually (`RequestRedirect::Manual`): the AS's 302s — to `/auth/google` for
sign-in, and to the client's redirect URI after consent — belong to the browser.
The proxy stamps `x-sanctuary-proxy: oauth` on what it forwards and never forwards
`/internal/*`. A missing binding or a failed upstream fetch is a 502, not a
protocol error, so an outage is never mistaken for a rejected request.

| Endpoint                                  | Served by      | Purpose                                       |
| ----------------------------------------- | -------------- | --------------------------------------------- |
| `/.well-known/oauth-authorization-server` | library        | RFC 8414 metadata (exact well-known location) |
| `GET /oauth/authorize`                    | `authorize.ts` | validate the request, sign in, render consent |
| `POST /oauth/authorize`                   | `authorize.ts` | approve or deny the stored request            |
| `POST /oauth/token`                       | library        | code exchange, refresh, RFC 7009 revocation   |
| `POST /oauth/register`                    | library        | RFC 7591 dynamic client registration          |
| `POST /internal/validate`                 | `validate.ts`  | resource-server token check (binding only)    |

Two deviations are worth naming. The library serves revocation at the token
endpoint — there is no `/oauth/revoke` — and advertises that URL as
`revocation_endpoint`; keeping it on `/oauth/token` is deliberate, because
`/oauth/*` is what the proxy forwards. It serves RFC 8414 metadata, not OpenID
Connect discovery, so `/.well-known/openid-configuration` stays on the Rust
Worker and 404s. The proxy forwards path-suffixed probes under the metadata
prefix, but the library matches only the exact location above; anything else
404s.

`/.well-known/oauth-protected-resource*` and `/mcp` are the resource side and are
served by the Rust Worker, not here. They are separate work; this branch does not
implement them.

### 2. Identity comes from the existing session

The AS has no identity store and no Google client. `GET /oauth/authorize`:

1. validates the request with `parseAuthRequest()`, so a bad request never
   bounces through Google;
2. calls the Rust Worker's `GET /auth/me` over the `APP` binding, forwarding the
   browser's `Cookie` header untouched;
3. sends a logged-out visitor to `302 /auth/google?next=<this path and query>`;
4. renders consent for a signed-in visitor.

A non-2xx response, unparsable JSON, or a user object without `id` and `email`
is `unavailable` — a 502 page, never a silent sign-out mid-flow.

`next` is the Rust Worker's contract, not the AS's: a same-origin relative path
starting with a single `/`, at most 2048 bytes, with no backslash, whitespace, or
control character, and not `//host` or `/\host`. `api_core::safe_next_path` is
the validator. `/auth/google` carries it through Google in the `oauth_state`
cookie as `<state>.<base64url(next)>`, and `/auth/google/callback` re-validates it
before redirecting to `FRONTEND_URL` plus the path. A tampered cookie can only
lose the return path; it cannot leave the origin. The AS sends the exact authorize
URL it received, so a user who signs in mid-flow returns to that request rather
than the app home.

The consent page shows the client name and, for a Client ID Metadata Document
client, its published domain; for a dynamically registered client it says the name
is not verified. It also shows the signed-in email, the redirect host, a warning
when the redirect is loopback, and one checkbox per offered scope with human
wording plus the raw scope name. Every interpolated value is escaped, including
the handle.

CSRF has two layers. `beginConsent()` stores the request server-side and returns a
single-use handle plus a `__Host-` binding cookie; the form carries only the
handle, and `approveConsent()` / `denyConsent()` reject a handle that is missing,
unbound, expired, or already used. The POST separately rejects an `Origin` that is
not this origin with 403. The page is unframeable — the library sends
`frame-ancestors 'none'` and `X-Frame-Options: DENY`, and the page adds a CSP —
and loads nothing: `default-src 'none'; style-src 'unsafe-inline'; form-action
'self'`, no scripts, no images.

The grant's props are the four `SessionUser` fields
`{id, email, name, picture}`, and `userId` is `id` — the D1 user id, not the
Google id. Google tokens never enter the AS; it only asks the public Worker who is
signed in. `completeAuthorization()` keeps the library default and revokes the
user's previous grant for the same client and resource, so re-authorizing replaces
the grant instead of leaving two live. For a CIMD client, whose `client_id` is
shared by every installation, that revocation is additionally scoped to the same
redirect URI.

### 3. Scopes

The catalogue is `mcp:read`, `mcp:write`, `offline_access`.

- `mcp:read` — read tasks, agenda, and calendar.
- `mcp:write` — create and change tasks, agenda items, and calendar events.
- `offline_access` — stay connected without asking again. It is a capability, not
  a resource requirement; the library strips it from a resource's advertised
  scopes and challenges automatically.

`scopesSupported` is the whole catalogue, and the consent page offers the
requested subset in catalogue order. When a client asks for nothing this server
knows — no scopes, or only names never advertised — the grant is `mcp:read`. Least
privilege is the safe default: the resource server answers `403
insufficient_scope` naming what an operation needs, and the client re-authorizes
through a second consent page. The checkboxes let the user narrow the subset;
`approveConsent()` rejects any scope outside the catalogue with 400.

Keep the resource's advertised baseline smaller than this catalogue. MCP clients
pick scopes from the `401` challenge, so a resource that advertises everything
makes every client request everything on first sign-in.

### 4. The validate contract

The resource server never reads the AS's KV. It calls one endpoint over the
`AUTH` binding:

```http
POST /internal/validate
{ "token": "<bearer>", "resource": "<canonical resource URL>" }
```

| Status | Body                                                                         | When                                                                                                  |
| ------ | ---------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| 200    | `{ userId, scope: string[], props }`; `props` = `{id, email, name, picture}` | the token is valid for `resource`                                                                     |
| 401    | `{ "error": "invalid_token" }`                                               | unknown, expired, or revoked token; an unregistered resource; a token minted for a different resource |
| 400    | `{ "error": "invalid_request" }`                                             | the body is not a JSON object, or `token` / `resource` is missing or empty                            |
| 405    | `{ "error": "invalid_request" }`, `Allow: POST`                              | any other method                                                                                      |

Audience binding is the library's: `validateToken(resource, token)` only accepts a
token minted for that resource, so two registered resources cannot share tokens.
An unknown resource is rejected as `invalid_token` before the library is called,
because `validateToken()` throws for it and a bad audience must never read as a
server error.

The internal surface has no public route. `is_auth_server_path` keeps
`/internal/*` off the proxy, and the AS answers 404 to any `/internal/*` request
that still carries `x-sanctuary-proxy`, so exposing the route by mistake would not
expose validation. Any other internal path is also 404.

Lifetimes are the library defaults and are not configured per grant: access token
1 hour, refresh token 30 days absolute from the code exchange (refreshing does not
slide it), authorization code and consent handle 10 minutes.

### 5. Configuration

| Binding / var   | Production                                  | Local (`apps/auth/.dev.vars`)                         |
| --------------- | ------------------------------------------- | ----------------------------------------------------- |
| `OAUTH_KV`      | KV namespace, id in `wrangler.toml`         | simulated in-process by `wrangler dev`                |
| `APP`           | service `my-sanctuary`                      | the same Worker in the multi-config dev               |
| `ISSUER`        | `https://my-sanctuary.fahimalizain.com`     | `http://localhost:8787`                               |
| `MCP_RESOURCES` | `https://my-sanctuary.fahimalizain.com/mcp` | `http://localhost:8787/mcp,http://127.0.0.1:8787/mcp` |

`ISSUER` is used verbatim in metadata and to build every endpoint URL.
`MCP_RESOURCES` is comma-separated and compared exactly; the library accepts plain
`http` only on loopback, which is why local dev registers both `localhost` and
`127.0.0.1`. The first entry is `defaultResource`, used when a client omits the
RFC 8707 `resource` parameter — the only resource in production, `localhost`
locally.

CIMD requires `compatibility_flags = ["global_fetch_strictly_public"]`. Without
it, the library refuses to accept or advertise Client ID Metadata Documents,
because a same-zone fetch could be routed to an internal host.

### 6. Why not a Rust port

`workers-rs` could implement this, but the library is the better owner:

- Cloudflare maintains it and tracks the MCP authorization spec: RFC 8414
  metadata, CIMD, RFC 9207 `iss`, RFC 8707 resource indicators, PKCE S256,
  refresh, hashed-at-rest token storage, consent helpers, and conformance tests.
  That is a moving target and a large security-sensitive surface to own.
- A port would have to get the subtle parts exactly right: client metadata
  resolution, exact redirect-URI matching, PKCE verification, audience binding,
  and KV transaction semantics.
- Keeping it in TypeScript makes the AS small and isolated. It owns no user data,
  reaches the app through one binding, and its contract with Rust is one HTTP
  endpoint. The Rust Worker keeps the session, D1, and Google.

Alternatives considered:

| Alternative                                 | Why not                                                                                                                                                 |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| One TypeScript Worker using `OAuthProvider` | It would also have to serve the app, so the whole Rust Worker would be ported or fronted by a TypeScript Worker proxying everything.                    |
| A separate auth subdomain                   | The issuer would differ from the app origin; discovery would name a host the app does not own, and cookies, routes, and CORS would grow for no benefit. |
| Port the library to Rust                    | Same security and maintenance cost, no conformance suite, and it would grow the Worker that already has the most responsibilities.                      |

## Consequences

- The public origin is unchanged. Discovery, endpoints, and tokens all read
  `https://my-sanctuary.fahimalizain.com`, so an MCP client treats the app as one
  authorization server.
- The AS is unreachable from the internet, including `/internal`. Its only public
  exposure is the paths the Rust Worker forwards.
- Auth and app depend on each other — circular service bindings: auth → `APP` to
  read the session, app → `AUTH` to proxy and validate. The bindings resolve only
  once both Workers exist, so auth deploys first.
- A Google login is now reusable as the MCP sign-in: one `next` path carries the
  authorize request through it, and the session cookie stays the only credential.
- Tokens are bearer credentials with no browser behind them. Revocation is local
  to this issuance, because the AS holds no upstream Google token to revoke.
- KV is a hard dependency. Grants, codes, and tokens are keyed by hash and do not
  survive a namespace change.

## Operations

**One-time setup.** Create the namespace and put the returned id in
`apps/auth/wrangler.toml`:

```sh
npx wrangler kv namespace create OAUTH_KV
```

The checked-in id is a placeholder. Do not reuse another Worker's namespace id:
grants and tokens are keyed only by hash, so pointing this at the wrong namespace
silently invalidates them. `wrangler dev` simulates KV in-process, so tests and
local dev need nothing.

**Deploy order.** `npx nx deploy auth`, then `npx nx deploy worker`. CI already
does this order. `npx nx deploy auth` implies `auth:build`
(`wrangler deploy --dry-run`).

**Local dev.** `npx nx serve worker` starts both configs in one `wrangler dev`
(`-c wrangler.toml -c ../auth/wrangler.toml`). Copy
`apps/auth/.dev.vars.example` to `apps/auth/.dev.vars`; it replaces the production
`ISSUER` and `MCP_RESOURCES` declared in `wrangler.toml`. The issuer must be
spelled exactly `http://localhost:8787`; `--local-upstream localhost:8787` makes
that the dev origin instead of the custom-domain route, and the library compares
issuer spelling exactly. `wrangler dev` rewrites `localhost` inside `Location`
headers, so the logged-out `/oauth/authorize` redirect cannot be followed straight
from the dev server: sign in through the app first — the session cookie is shared
across localhost ports — then open the authorize URL and consent.

**KV cleanup.** Nothing calls `purgeExpiredData()` on a schedule yet. KV TTLs
expire consent transactions and codes (10 minutes), access tokens (1 hour), and
grants (30 days), so the ordinary path is clean; a cron sweep for orphaned records
is a follow-up.

## Known gaps

- `/mcp` and `/.well-known/oauth-protected-resource*` are not implemented in this
  branch; only the authorization server and its plumbing are.
- No scheduled KV purge; expiry, not garbage collection, is the only cleanup.
- No grant list or "revoke this app" UI. Revocation is a token-endpoint call, or
  implicit when the user re-authorizes.
- Consent choices are not remembered (the library supports it), so a new grant
  always shows the page.
- No OIDC discovery. MCP does not need it.
