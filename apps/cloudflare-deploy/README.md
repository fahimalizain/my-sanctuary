# Cloudflare Deploy

This app glues together the [API](../api/) and the [Web](../web/) frontend into a single Cloudflare Worker.

## Architecture

```
                     ┌──────────────────────────────────┐
     my-sanctuary    │  Cloudflare Worker               │
     .fahimalizain   │                                  │
     .com            │  ┌────────────────────────────┐  │
          │          │  │  Chi Router                │  │
          │          │  │                            │  │
          ▼          │  │  GET /greeting/{name}      │  │
    ┌─────────┐      │  │  GET /health               │  │
    │ Request │      │  │     ▲                      │  │
    └─────────┘      │  │     │                      │  │
          │          │  └─────┼──────────────────────┘  │
          ▼          │        │                         │
    ┌─────────┐      │  ┌─────┴──────────────────────┐  │
    │  Match  │──────┼──│  handlers.RegisterRoutes   │  │
    │ API?    │      │  │  (from apps/api/handlers)  │  │
    └─────────┘      │  └────────────────────────────┘  │
          │          │        │                         │
          no         │        │ Huma                    │
          ▼          │        │ (validation, OpenAPI)   │
    ┌─────────┐      │        ▼                         │
    │  Match  │      │  ┌────────────────────────────┐  │
    │ Static? │──────┼──│  http.FileServer           │  │
    │ (dist/) │      │  │  + SPA fallback            │  │
    └─────────┘      │  └────────────────────────────┘  │
          │          │                                  │
          ▼          └──────────────────────────────────┘
    ┌─────────┐
    │  404    │
    └─────────┘
```

- **API routes** (`/greeting/*`, `/health`) are served by the same Go handlers defined in `apps/api/handlers`, running as compiled WebAssembly.
- **Static files** (`/`, `/assets/*`) are embedded at build time from the Vite production build (`apps/web` → `dist/`) and served via `http.FileServer`.
- **SPA fallback** — any unmatched path returns `index.html`, letting React Router handle client-side routing.

## Build Pipeline

Running `nx build cloudflare-deploy` (or `nx run-many -t build --all`):

1. Builds `apps/web` → `dist/apps/web` (Vite production bundle)
2. Builds `apps/api` → `dist/apps/api` (native Go binary, used by local dev only)
3. Copies `dist/apps/web` into this app's `dist/` directory
4. Generates the JavaScript shim (`build/`) via `workers-assets-gen`
5. Compiles `main.go` to WebAssembly → `build/app.wasm`

## Deployment

```bash
cd apps/cloudflare-deploy
npx wrangler deploy
```

The worker runs on `my-sanctuary.fahimalizain.com`.

## Tech Stack

| Component      | Library / Tool                                      | Role                               |
| -------------- | --------------------------------------------------- | ---------------------------------- |
| Router         | [Chi](https://github.com/go-chi/chi)                | HTTP routing                       |
| API Framework  | [Huma](https://huma.rocks/)                         | Validation, serialization, OpenAPI |
| Worker Runtime | [syumai/workers](https://github.com/syumai/workers) | Go → Cloudflare Workers bridge     |
| Build Tool     | Wrangler (`workers-assets-gen`)                     | JS shim + WASM bundling            |
| Frontend       | [Vite](https://vitejs.dev/) + React                 | Static SPA                         |
