# D1 Binding Access (Cloudflare Workers)

## The problem

`syumai/workers` bridges Workers' fetch events into Go's `http.Handler` but
does **not** expose the `env` object (where D1 bindings live) to Go. Reading
`js.Global().Get("env")` returns `undefined`.

The Workers runtime calls a JS fetch handler with
`(request, env, ctx)`, and `syumai/workers` bridges that into Go's
`http.Handler`. The `env` object is not propagated to Go by default.

## The solution — minimal JS shim

`workers-assets-gen` produces a JS entry (`build/worker.mjs`) that calls the Go
WASM handler. We replace it with a shim that captures `env.DB` on a Go-visible
global (`globalThis.__D1__`) before dispatching.

Source: [`apps/cloudflare-deploy/d1_shim.js`](../../apps/cloudflare-deploy/d1_shim.js)

```javascript
async function fetch(req, env, ctx) {
  if (env.DB) {
    globalThis.__D1__ = env.DB;
  }
  const binding = {};
  await run(createRuntimeContext({ env, ctx, binding }));
  return binding.handleRequest(req);
}
```

The build script overwrites the generated `worker.mjs` with this shim:

```bash
# apps/cloudflare-deploy/build.sh
go run github.com/syumai/workers/cmd/workers-assets-gen@latest -mode go .
cp d1_shim.js build/worker.mjs
```

This is the lightest possible bridge and avoids patching `syumai/workers`.

## Go side

`apps/cloudflare-deploy/main.go` reads the global once at startup:

```go
func getD1Binding() js.Value {
    return js.Global().Get("__D1__")
}
```

The D1 repos are wired with this function:

```go
handlers.RegisterRoutes(router, &handlers.Dependencies{
    Config:            cfg,
    HTTPClient:        fetch.NewClient().HTTPClient(fetch.RedirectModeFollow),
    Version:           version,
    UserRepo:          repository.NewD1UserRepo(getD1Binding),
    TokenRepo:         repository.NewD1TokenRepo(getD1Binding),
    CalendarRepo:      repository.NewD1CalendarRepo(getD1Binding),
    CalendarEventRepo: repository.NewD1CalendarEventRepo(getD1Binding),
})
```

Each repo calls `getD1()` at query time to retrieve the D1 binding, then uses
`d1.Call("prepare", sql).Call("bind", args...).Call("all")` (for queries) or
`d1.Call("prepare", sql).Call("bind", args...).Call("run")` (for execs).

## Risks

- The `globalThis.__D1__` bridge is unconventional. Keep the shim minimal, pin
  `syumai/workers` version, and add an integration test that hits `/health` on
  `wrangler dev`.
- If Workers ever changes the `env` lifecycle (e.g. per-request isolation),
  the global stashing approach may need to move to per-request context.
