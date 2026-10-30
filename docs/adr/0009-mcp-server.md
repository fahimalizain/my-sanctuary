# ADR 0009: Remote MCP server

Status: Accepted
Date: 2026-10-30

## Context

Sanctuary is a Cloudflare Worker (workers-rs 0.8, `wasm32-unknown-unknown`) plus
a Vite PWA. Clock-free business logic lives in `packages/api-core`; HTTP routing
is the `Router` chain in `apps/worker/src/lib.rs`; `/api/*` is already
`run_worker_first` in `apps/worker/wrangler.toml`, so API paths reach the Worker
before static assets.

Commit `c9122c1` added API keys. `auth::authenticate` in
`apps/worker/src/auth.rs` accepts `Authorization: Bearer sk_…` (SHA-256, D1,
expiry, soft-delete, throttled `last_used_at`) and falls back to the session
cookie **only when the header is absent**. `/api/keys` is deliberately
cookie-only: a bearer key must never mint or revoke keys.

Remote agents — Claude, MCP Inspector, curl scripts — need a tool surface rather
than raw REST. This Worker is not an OAuth authorization server, so the only
credential an agent can hold is an API key. This ADR records the accepted
design. Nothing is implemented yet; later slices build the protocol core in
`packages/api-core`, the endpoint in `apps/worker`, the read tools, and the
write tools against this document.

## Decision

### 1. The protocol is hand-rolled, behind a `ToolExecutor` trait

The MCP protocol lives in a new `api-core` module and talks to the rest of the
system through a `ToolExecutor` trait. That keeps it clock-free and natively
testable: `cargo test -p api-core` exercises framing, negotiation, and tool
dispatch without a Worker, a browser, or a network.

The libraries were rejected on runtime grounds, not taste. `rmcp`'s server
`tokio::spawn`s, and wasm has no tokio runtime. `rust-mcp-sdk` is built on
hyper/axum. `mcpserver` is unmaintained and speaks an old spec. A protocol this
small — four request methods, JSON only, no sessions — costs less to own than
to bend a server framework into a wasm Worker.

### 2. Transport: stateless Streamable HTTP, JSON responses only

The endpoint is `POST /api/mcp`, not `/mcp`. A `run_worker_first` entry for
`/mcp` would be a new namespace on the domain; `/api` already belongs to the
Worker. More importantly, the SPA's
`not_found_handling = "single-page-application"` answers an unknown GET with
`index.html` and 200, so a discovery probe for `/mcp` would look successful and
return HTML. Keeping the tool endpoint inside `/api/*` is the only placement
that fails honestly.

- Success and JSON-RPC error responses are `Content-Type: application/json`.
  There is no SSE stream, no `Mcp-Session-Id`, and no session store.
- A notification, or a client JSON-RPC *response*, gets HTTP 202 with an empty
  body. `notifications/initialized` is the one clients actually send;
  `notifications/cancelled` is a no-op 202; any other notification is also 202.
  With no session there is nothing to cancel and nothing to acknowledge.
- GET and DELETE are not registered. The workers-rs `Router` already answers
  405 Method Not Allowed when the path matches and the method does not (`worker`
  0.8.5 `router.rs`, around the method scan). There is deliberately no
  session-terminating DELETE.
- JSON-RPC batches (a JSON array, including an empty one) are rejected with
  HTTP 400 and `-32600`.
- Each POST stands alone. `initialize` is not required before `tools/list` or
  `tools/call`, because there is no session to gate on.

### 3. Protocol methods

**`initialize` — version negotiation.** Supported revisions are exactly
`2025-03-26`, `2025-06-18`, and `2025-11-25`. If the client asks for one of
those, the server echoes it; otherwise it answers `2025-11-25`, which it does
support. An unknown client version is not an error. `capabilities` is
`{ "tools": { "listChanged": false } }` and nothing else — no resources,
prompts, or logging. `serverInfo.name` is `sanctuary`;
`serverInfo.version` is the Worker's `env!("APP_VERSION")` (root
`package.json`, injected by `apps/worker/build.rs`) passed into the protocol
function. `api-core` does not read that environment variable itself.

**`ping`** returns `{}`.

**`tools/list`** returns one page and no `nextCursor`. A non-empty `cursor` is
`-32602`.

**`tools/call`.** An unknown tool name is JSON-RPC `-32602` (Invalid params),
not `-32601` and not a result with `isError`. `arguments` must be a JSON object
when present; anything else is `-32602`. Omitted `arguments` mean `{}`.

**Unknown request methods** return `-32601`.

**Errors and status codes.** A JSON parse failure is HTTP 400 with `-32700`. An
invalid request — not a JSON object, missing `jsonrpc: "2.0"`, or a batch — is
HTTP 400 with `-32600`. A well-formed request that fails with a JSON-RPC error
still returns HTTP 200.

**Tool failures are results, not protocol errors.** A failed `tools/call` is a
result with `isError: true` at HTTP 200, never a JSON-RPC error — including
upstream HTTP failures. Success returns
`content: [{ "type": "text", "text": "<compact json>" }]`, `structuredContent`
set to that same JSON value, and `isError: false`.

**Google re-auth is a tool error.** A sub-request that returns HTTP 401 must
not become an MCP HTTP 401. It becomes `isError: true` with exactly:

```
Google access expired - sign in to the web app
```

A hyphen, not an em dash. The upstream body is not copied into that message.
Clients must not start an OAuth flow; the message tells a human what to do.

### 4. Tool execution goes back through the existing REST router

Tools do not call `api_core` service functions directly. They issue in-process
sub-requests to the same REST routes the web app uses. The
`Router::with_data(config)…` chain in `apps/worker/src/lib.rs` (today lines
78–199) is hoisted into a single router constructor that both the fetch entry
point and the MCP endpoint use. A tool then builds a request by hand and runs
that router.

The request URL must be absolute because web_sys `Request` requires one; a
dummy origin such as `https://sanctuary.internal` plus the path and query
works, since `Request::path()` is the URL pathname. Only the caller's
`Authorization` header is copied. `Cookie` is not. The router runs against the
sub-request with a cloned `Env`; `Env` is `Clone` in worker 0.8.5.

Routing through the chain is the point: Google token gating, event defaults and
colour, reclassify queueing, `UserHub` notification, and the existing error
mapping all keep working. A tool that called the service layer directly would
silently skip them.

The sub-request helper is an allowlist of method-and-path pairs, not of path
prefixes:

| Method    | Path                               |
| --------- | ---------------------------------- |
| GET       | `/auth/me`                         |
| GET, POST | `/api/tasks`                       |
| GET       | `/api/tasks/classify`              |
| PATCH     | `/api/tasks/:id`                   |
| POST      | `/api/tasks/:id/move`              |
| GET       | `/api/agenda`                      |
| POST      | `/api/agenda/items/:id/move`       |
| POST      | `/api/agenda/items/:id/reschedule` |
| GET, POST | `/api/calendar/events`             |
| GET       | `/api/calendar/calendars`          |
| GET       | `/api/reports/status`              |
| GET       | `/api/rules/scoreboard`            |

Anything not in that table is refused — DELETE, `/api/keys`,
`/api/calendar/calendars/:id/repair`, the task timer verbs (`/start`, `/stop`,
`/complete`, …), focus, and the occurrence routes. A prefix match would let a
`POST /api/tasks` rule reach every verb hanging off a task, and a
`/api/calendar/calendars` rule reach the repair endpoint, so the table is
enumerated rather than derived. Path ids interpolated into a URL must match
`^[0-9A-Za-z_-]{1,64}$`.

### 5. `/api/mcp` is bearer-only, with an origin check

The `Authorization` header must be present. If it is absent the endpoint
returns HTTP 401 `{"error":"unauthorized"}` **without calling**
`authenticate` — that function would fall back to the session cookie. If the
header is present but malformed, unknown, expired, or revoked, `authenticate`
returns `None` and the answer is the same 401. No `WWW-Authenticate` header is
sent, so no client is invited into the MCP OAuth dance.

401 is only for this gate. A later, tool-level Google 401 is the `isError`
result described in section 3.

If an `Origin` header is present and is not exactly `FRONTEND_URL` (the
configured frontend origin, default `http://localhost:5173`), the endpoint
returns HTTP 403 `{"error":"forbidden"}` **before** authentication. An absent
`Origin` is allowed — curl, Claude, and Inspector send none. The request origin
is never reflected.

### 6. CORS belongs to `/api/mcp`, not to the shared helper

`/api/mcp` has its own OPTIONS handler: `Allow-Headers` exactly
`Authorization, Content-Type, Accept, Mcp-Protocol-Version`, `Allow-Methods`
`POST, OPTIONS`, `Allow-Origin` set to `FRONTEND_URL`, credentials true, status
204. The same `Allow-Origin` is put on the POST, 401, and 403 responses so a
preflighted browser call can read them.

`auth::cors_headers` is not widened. It deliberately does not advertise
`Authorization`, and the web app stays on the cookie; changing the shared
allow-list to suit the MCP endpoint would loosen every other route.

The Streamable HTTP `Accept: application/json, text/event-stream` requirement
is not enforced; the server always answers `application/json`. The request
`Content-Type` must be `application/json`, optionally with
`; charset=utf-8`; anything else is 415.

If `MCP-Protocol-Version` is present and is not one of the three supported
revisions, the request is rejected with HTTP 400. If it is absent, the request
proceeds: this is stateless, and the spec's fallback revision `2025-03-26` is
supported. `Mcp-Method` and `Mcp-Name` are not required; they belong to
revision `2026-07-28`, which this server does not speak.

### 7. OAuth discovery must not hit the SPA

`/.well-known/oauth-*` is added to `run_worker_first` in
`apps/worker/wrangler.toml`. Every request whose path starts with
`/.well-known/oauth-` is answered by the Worker with HTTP 404
`{"error":"not found"}` and `Content-Type: application/json`, for every method,
before the router runs.

That covers `/.well-known/oauth-protected-resource`,
`/.well-known/oauth-authorization-server`, and the path-inserted forms such as
`/.well-known/oauth-protected-resource/api/mcp`. The point is a JSON 404 rather
than `index.html` with a 200, which would look like a discovery document that
happens to be HTML. No `WWW-Authenticate` is sent anywhere.

### 8. Tools v1: read-mostly, no deletes

Every tool sets all three annotation hints. "Read-only" here means **no
user-facing mutation** — see the note after the table.

| Tool                     | readOnlyHint | destructiveHint | idempotentHint | Route and purpose                                                    |
| ------------------------ | ------------ | --------------- | -------------- | -------------------------------------------------------------------- |
| `now`                    | true         | false           | true           | `GET /auth/me` + `GET /api/agenda`; today and time zone              |
| `list_tasks`             | true         | false           | true           | `GET /api/tasks`; cap 100                                            |
| `get_agenda`             | true         | false           | true           | `GET /api/agenda`; cap 80 items, 40 backlog rows                     |
| `list_events`            | true         | false           | true           | `GET /api/calendar/events`; cap 100                                  |
| `list_calendars`         | true         | false           | true           | `GET /api/calendar/calendars`; cap 50                                |
| `reports_status`         | true         | false           | true           | `GET /api/reports/status`                                            |
| `rules_scoreboard`       | true         | false           | true           | `GET /api/rules/scoreboard`                                          |
| `classify_title`         | true         | false           | true           | `GET /api/tasks/classify`                                            |
| `create_task`            | false        | false           | false          | `POST /api/tasks`                                                    |
| `update_task`            | false        | false           | true           | `PATCH /api/tasks/:id`; no status field                              |
| `move_task`              | false        | true            | true           | `POST /api/tasks/:id/move`; `status` only                            |
| `move_agenda_item`       | false        | false           | true           | `POST /api/agenda/items/:id/move`; `sort_order`                      |
| `reschedule_agenda_item` | false        | false           | true           | `POST /api/agenda/items/:id/reschedule`; date and/or `backlog: true` |
| `create_event`           | false        | false           | false          | `POST /api/calendar/events`                                          |

Per tool:

- **`now`** returns the user from `GET /auth/me`, and `today` plus `time_zone`
  from `GET /api/agenda` (the primary calendar's zone, else UTC). It does not
  return agenda items.
- **`list_tasks`**, **`get_agenda`**, **`list_events`**, **`list_calendars`**
  return slim rows, not the REST envelopes. `get_agenda` takes `from` and `to`
  together or not at all. `list_events` requires `time_min` and `time_max` as
  RFC3339 and checks the span before the sub-request: at most 31 days. The REST
  route would otherwise default to now − 1 month … now + 2 months and has no
  span cap. It drops the `sync` envelope.
- **`reports_status`** may flip the backfill job to `running`. That is an
  idempotent internal flag, not a user mutation.
- **`rules_scoreboard`** may seed default rules and queue history the same way
  the page does. Also not a user-facing mutation.
- **`create_task`** creates a new row on every call; it is not idempotent.
- **`update_task`** never sets status; status changes go through `move_task`.
- **`move_task`** sends `status` only — one of `OPEN`, `PLANNED`, `IN_PROGRESS`,
  `COMPLETED`, `DISCARDED` — and omits `sort_order` so the server applies the
  column default. Sending the same status twice with no rank is a no-op.
  `destructiveHint` is true because the tool can discard or complete a task,
  and entering or leaving `IN_PROGRESS` writes Google.
- **`create_event`** requires `summary`, `start`, and `end`; it accepts
  `description`, `is_all_day`, `start_time_zone`, and `calendar_id`. When
  `calendar_id` is omitted it resolves the primary calendar's local id and
  sends `calendar_id_explicit: false`, so title classification can still
  retarget and colour the event. When `calendar_id` is set it sends
  `calendar_id_explicit: true`. `task_id`, `routine_id`, `occurrence_id`,
  focus, and `color_hex` are not exposed.

**Why the read hints are true even though reads write.** `list_events` and
`list_calendars` can fill the replica cache or import a calendar list, and
`reports_status` and `rules_scoreboard` can touch internal job state. The hint
means "an agent may call this without asking the user to confirm", which is
exactly the intended contract. Do not "fix" these hints to false later.

**Result size.** A tool result whose compact JSON exceeds 64 KiB becomes
`isError` with a short "result too large" message, rather than a truncated
blob that is no longer valid JSON. The per-tool caps should make that
unreachable.

**Validation boundaries.** An argument validation failure inside a known tool
is `isError`, not `-32602`. `-32602` is reserved for protocol-level invalid
params: an unknown tool, a non-object `arguments`, a `tools/list` cursor, or
bad `initialize` params.

### 9. Deliberately deferred

| Deferred                                                  | Why                                                                                   |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| Protocol revision `2026-07-28`                            | No `initialize`, adds `server/discover`, requires `Mcp-Method` / `Mcp-Name` headers.  |
| SSE, sessions, `Mcp-Session-Id`                           | JSON request/response is enough; a session store is state the Worker does not need.   |
| Resources, prompts, sampling, elicitation, logging        | Tools only; the capability surface stays `{ "tools": { "listChanged": false } }`.     |
| Deletes, focus, occurrence timer verbs, rule CRUD         | Not needed for a first agent surface, and deletes are the easiest thing to get wrong. |
| Key management                                            | `/api/keys` stays cookie-only; a bearer key must never mint or revoke keys.           |
| Event patch/delete, `move-to-today`                       | Write scope is create and move; editing and deleting events wait.                     |
| Calling `api_core` services directly from tools           | Bypasses Google gating, defaults, reclassify queueing, and notify.                    |
| Advertising `Authorization` on the shared CORS allow-list | The web app stays on the cookie; `/api/mcp` owns its own allow-list.                  |

## Consequences

- Agents authenticate with `Authorization: Bearer sk_…` against `POST /api/mcp`
  and nothing else. There is no OAuth flow to fall into and no session to
  maintain.
- A Google refresh failure on a write stays HTTP 200 with `isError: true`, so
  the client reports the problem instead of opening an OAuth flow it cannot
  complete.
- OAuth discovery probes get a JSON 404 instead of `index.html`, so a client
  cannot mistake the SPA for an authorization server.
- `/api/keys` stays cookie-only. The MCP endpoint is one more caller of the
  existing REST routes, not a second implementation of them.
- The endpoint is stateless, so it scales with the Worker and needs no Durable
  Object, KV, or D1 table of its own.
- Later slices add: the protocol module and `ToolExecutor` trait, the endpoint
  and its CORS/auth gates, the read tools, and the write tools.
