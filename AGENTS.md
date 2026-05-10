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

## @apps/api

The API is a Go HTTP server using chi router + huma for OpenAPI documentation.

- **Version** — root `package.json` is the single source of truth. The pre-commit hook auto-bumps the patch version. The Go binary reads this at build time via `-ldflags -X main.version=...` from `build.sh`.
- **Build** — `npx nx run api:build` outputs to `dist/apps/api`.
- **Dev** — `npx nx serve api` or `go run .` inside `apps/api`.
- **Endpoints** — `GET /version` returns the deployed app version.

## @apps/cloudflare-deploy

The Cloudflare Workers deploy app bundles the web frontend and API into a single WASM Workers deployment.

- **Version** — same root `package.json` single source of truth. Injected via `-ldflags -X main.version=...` from `build.sh`.
- **Build** — `npx nx run cloudflare-deploy:build` bundles web dist + Go WASM binary.
- **Deploy** — `npx nx run cloudflare-deploy:deploy` pushes to Cloudflare via wrangler.
- **Implied deps** — `api` and `web` must be built first.
