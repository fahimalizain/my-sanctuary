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

## Environment Variables

All environment variables are managed in **GitHub** — the single source of truth.

### Non-secrets (via GitHub Repository Variables)

Set these at **Settings → Secrets and variables → Actions → Variables**:

| Variable        | Example Value                                       | Purpose                                              |
| --------------- | --------------------------------------------------- | ---------------------------------------------------- |
| `FRONTEND_URL`  | `https://my-sanctuary.fahimalizain.com`             | The public URL of the app (used for post-login redirect). |
| `SECURE_COOKIE` | `true`                                              | Set to `true` in production to mark cookies as Secure. |

On every deploy, the CI workflow writes these into `wrangler.toml`:

```yaml
- run: |
    cat >> apps/cloudflare-deploy/wrangler.toml << 'EOF'
    [vars]
    FRONTEND_URL = "${{ vars.FRONTEND_URL }}"
    SECURE_COOKIE = "${{ vars.SECURE_COOKIE }}"
    EOF
```

### Secrets (via GitHub Secrets)

Set these at **Settings → Secrets and variables → Actions → Secrets**:

| Secret                    | Purpose                                                              |
| ------------------------- | -------------------------------------------------------------------- |
| `SESSION_SECRET`          | 32+ byte key for signing session cookies.                            |
| `GOOGLE_CREDENTIALS_JSON` | Raw JSON of your Google OAuth 2.0 client credentials (web app type). |
| `CLOUDFLARE_API_TOKEN`    | Cloudflare API token for deployment.                                 |
| `CLOUDFLARE_ACCOUNT_ID`   | Cloudflare account ID.                                               |

The deploy job pushes secrets to Cloudflare before deploying:

```bash
echo "$SESSION_SECRET" | npx wrangler secret put SESSION_SECRET --cwd apps/cloudflare-deploy
echo "$GOOGLE_CREDENTIALS_JSON" | npx wrangler secret put GOOGLE_CREDENTIALS_JSON --cwd apps/cloudflare-deploy
```

### Local Development vs Production

| Environment | Config source |
|-------------|---------------|
| **Local dev** (`nx serve api`) | `.env` file at repo root. See `.env.example`. |
| **Production** (Cloudflare Worker) | GitHub Repository Variables + GitHub Secrets, injected by CI. |

> ⚠️ Never commit secrets. `wrangler.toml` is kept free of any credentials.

### Why `wrangler secret put` / Dashboard Variables Don't Work Out-of-the-Box for Go

If you set a secret in the Wrangler dashboard or via `wrangler secret put`, it **is** available in the Cloudflare Workers runtime — but Go code cannot read it with `os.Getenv()`.

**The problem:** Cloudflare Workers expose env vars (including secrets) through a JavaScript runtime object, not POSIX environment variables. When Go is compiled to WebAssembly for Workers, `os.Getenv` has no access to the JS runtime context and returns empty strings.

**The solution:** Use `cloudflare.Getenv` from `github.com/syumai/workers/cloudflare`, which bridges to the JS runtime context where Wrangler secrets and variables live.

| Method | Works in JS Worker? | Works in Go WASM Worker? |
|--------|--------------------|-------------------------|
| `wrangler secret put` then `env.SECRET` in JS | ✅ Yes | N/A (JS only) |
| `wrangler secret put` then `os.Getenv("SECRET")` in Go | ❌ No | ❌ Returns empty |
| `wrangler secret put` then `cloudflare.Getenv("SECRET")` in Go | N/A | ✅ Yes |

This is why `main.go` uses `config.LoadWithEnv(cloudflare.Getenv)` instead of the standard `config.Load()` (which uses `os.Getenv` for local dev).

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
