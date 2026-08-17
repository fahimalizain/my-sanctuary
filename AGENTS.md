<!-- nx configuration start-->
<!-- Leave the start & end comments to automatically receive updates. -->

# General Guidelines for working with Nx

- For navigating/exploring the workspace, invoke the `nx-workspace` skill first - it has patterns for querying projects, targets, and dependencies
- When running tasks (for example build, lint, test, e2e, etc.), always prefer running the task through `nx` (i.e. `nx run`, `nx run-many`, `nx affected`) instead of using the underlying tooling directly
- Prefix nx commands with the workspace's package manager (e.g., `pnpm nx build`, `npm exec nx test`) - avoids using globally installed CLI
- You have access to the Nx MCP server and its tools, use them to help the user
- For Nx plugin best practices, check `node_modules/@nx/<plugin>/PLUGIN.md`. Not all plugins have this file - proceed without it if unavailable.
- NEVER guess CLI flags - always check nx_docs or `--help` first when unsure

## Scaffolding & Generators

- For scaffolding tasks (creating apps, libs, project structure, setup), ALWAYS invoke the `nx-generate` skill FIRST before exploring or calling MCP tools

## When to use nx_docs

- USE for: advanced config options, unfamiliar flags, migration guides, plugin configuration, edge cases
- DON'T USE for: basic generator syntax (`nx g @nx/react:app`), standard commands, things you already know
- The `nx-generate` skill handles generator discovery internally - don't call nx_docs just to look up generator syntax

<!-- nx configuration end-->

> All commits in this repository follow [Conventional Commits](https://www.conventionalcommits.org/). Load the `semantic-commits` skill (`.agents/skills/semantic-commits/SKILL.md`) for the full specification.

---

## @apps/web

The web app is an installable PWA built with Vite + React 19 + TanStack Router + Tailwind CSS.

- **Version** — root `package.json` is the single source of truth. The pre-commit hook auto-bumps the patch version on every commit. The web app reads this at build time via `__APP_VERSION__` (injected by Vite `define`).
- **PWA** — `vite-plugin-pwa` handles the manifest, service worker (Workbox), and precaching. A `<ReloadPrompt />` component notifies users when updates are available or offline mode is ready.
- **Icons** — `public/logo.svg` is the source of truth. PNG exports (`pwa-*.png`, `favicon.png`, `apple-touch-icon.png`) are generated from it using `resvg`.
- **Build** — `npx nx run web:build` outputs to `dist/apps/web`.
- **Dev** — `npx nx serve web` or `npm run dev` inside `apps/web`.

## @packages/api-core

Shared Rust library (`api-core` crate) holding the API response types. Pure Rust, no `worker` dependency, so it can be unit-tested natively (`cargo test -p api-core`). Consumed by `apps/worker`.

## @apps/worker

The Cloudflare Worker is a workers-rs (Rust → WASM) app that serves `GET /health`, `GET /version`, `GET/POST /api/calendar/events`, and the Vite PWA via Workers Static Assets. Local runtime is `wrangler dev` + local D1; there is no native API server.

- **Version** — root `package.json` is the single source of truth. The pre-commit hook auto-bumps the patch version. `apps/worker/build.rs` reads it at build time and injects it as `APP_VERSION` (consumed via `env!("APP_VERSION")`), replacing the old Go `-ldflags` mechanism.
- **Build** — `npx nx run worker:build` runs `worker-build --release` (implies `web:build` via `dependsOn`). Output lands in `apps/worker/build` (`build/index.js` is `main` in `wrangler.toml`).
- **Dev** — `npx nx serve worker` (`wrangler dev` in `apps/worker`), frontend on `http://localhost:5173` with Vite proxying `/api`, `/auth`, `/health`, `/version` to `http://127.0.0.1:8787`.
- **Static Assets** — `[assets]` serves `dist/apps/web` with `not_found_handling = "single-page-application"`; `run_worker_first = ["/api/*", "/auth/*", "/health", "/version"]` keeps API paths on the Worker.
- **Deploy** — `npx nx deploy worker` pushes via wrangler (custom domain `my-sanctuary.fahimalizain.com`).
- **Toolchain** — pinned by the root `rust-toolchain.toml` (stable + `wasm32-unknown-unknown`); `worker-build` is installed via `cargo install worker-build --version 0.8.4` (pinned: 0.8.5 regressed on `strip = true` with "externref table required for catch wrappers").
