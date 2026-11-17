# Sanctuary

A personal productivity sanctuary — an installable PWA for managing your life streams, tasks, and consistency.

## Apps

| App           | Description                                           | Tech                                          |
| ------------- | ----------------------------------------------------- | --------------------------------------------- |
| `apps/web`    | Installable PWA with offline support                  | React 19, TanStack Router, Vite, Tailwind CSS |
| `apps/worker` | Cloudflare Worker serving the API + PWA static assets | Rust (workers-rs), wrangler                   |

---

## Quick Start

```sh
npm install          # installs dependencies + configures git hooks
npx nx serve worker  # starts the local Worker (wrangler dev) on :8787
npx nx serve web     # starts the web dev server on :5173 (proxies /api, /auth, /health, /version to the Worker)
npx nx run worker:build # production build (worker-build, implies web build)
```

---

## Local Development

The Worker reads local configuration from `apps/worker/.dev.vars`, which is git-ignored. Start from the template:

```sh
cp apps/worker/.dev.vars.example apps/worker/.dev.vars
```

Apply migrations to the local D1 database (once, and again after pulling new migrations):

```sh
cd apps/worker
npx wrangler d1 migrations apply sanctuary-db --local
```

`SESSION_SECRET` is required in every mode (at least 32 bytes) — it signs the session cookie, including for local accounts. `SECURE_COOKIE` defaults to `false` unless it is exactly `"true"`.

There are two ways to sign in locally.

### Zero-Google (standalone) mode

Leave the Google credentials empty and enable standalone auth in `apps/worker/.dev.vars`:

```sh
SESSION_SECRET=<32+ random bytes>
GOOGLE_CREDENTIALS_JSON=
ALLOW_STANDALONE_AUTH=true
SECURE_COOKIE=false
# optional fallback; registration captures the browser's IANA time zone first
STANDALONE_TIME_ZONE=
```

With `GOOGLE_CREDENTIALS_JSON` empty no OAuth client is configured: nothing calls `accounts.google.com`, `oauth2.googleapis.com`, or `www.googleapis.com`, `/auth/google` returns 500 "oauth not configured", and the login page shows only the username/password form (with a "Create account" toggle). Register a local account from the login page with a username (`[a-z0-9._-]`, 3–32 characters) and a password (8–128 characters). The account gets a local "Personal" calendar and the full app works: calendar events, tasks, the agenda, reports/digest, and MCP.

Local accounts live in the local D1 database, so they survive restarts and only disappear if `.wrangler/state` is deleted or the local database is reset.

### Google + standalone mode

Fill in the Google OAuth client JSON and keep standalone enabled:

```sh
SESSION_SECRET=<32+ random bytes>
GOOGLE_CREDENTIALS_JSON={"web":{"client_id":"...","client_secret":"..."}}
ALLOW_STANDALONE_AUTH=true
SECURE_COOKIE=false
```

The login page then shows both "Continue with Google" and the username/password form, and the two account types stay isolated. An existing Google-only setup keeps working unchanged: leave `ALLOW_STANDALONE_AUTH` unset (or `false`) and only the Google button appears.

### Standalone auth is local-only

Standalone auth is fail-closed and off by default. It is only enabled when **all** of the following hold:

- `ALLOW_STANDALONE_AUTH=true`;
- `SECURE_COOKIE` is not `true`;
- both `FRONTEND_URL` and the incoming request host are non-public: loopback (`localhost`, `127.0.0.1`, `::1`), an RFC1918 address, a link-local address, or a `.local` / `.lan` name.

Otherwise `/auth/providers` reports `standalone: false` and `/auth/standalone/*` returns 403. Open the app on `http://localhost:5173` (the Vite dev server, which proxies `/auth`) or `http://127.0.0.1:8787` (the Worker directly). A public hostname, `SECURE_COOKIE=true`, or the flag left unset turns standalone off, so production can never expose the local login form.

### Cron locally

`wrangler dev` does not fire the `[triggers] crons` schedules on its own. To exercise the scheduled paths (digest, agenda fill, and the Google-only fallback sync / elongation), run the Worker with the test endpoint enabled and trigger a tick manually:

```sh
cd apps/worker
npx wrangler dev --test-scheduled
curl "http://127.0.0.1:8787/__scheduled?cron=*/2+*+*+*+*"
```

In zero-Google mode the digest and agenda-fill cron paths still run; the Google-dependent ones are skipped. Standalone users therefore get no cron elongation of running events, so a running chip stops growing after 15 minutes — stop/pause/complete still record the correct end.

### `wrangler dev --remote` and production D1

`wrangler dev` uses a local SQLite database under `.wrangler/state`. `wrangler dev --remote` instead runs against real Cloudflare bindings, and `[[d1_databases]]` in `apps/worker/wrangler.toml` points at the **production** database — anything you create there, including local standalone accounts, is real production data. If you need `--remote`, create a dedicated dev database, uncomment `preview_database_id` in `apps/worker/wrangler.toml`, and migrate it first:

```sh
cd apps/worker
npx wrangler d1 create sanctuary-db-dev
npx wrangler d1 migrations apply sanctuary-db-dev --remote
```

---

## PWA

The web app is a fully installable Progressive Web App:

- **Offline capable** — Service Worker precaches all assets
- **Installable** — Add to home screen on iOS Safari and Android Chrome
- **Auto-updating** — New builds prompt a reload when available

### Add to Home Screen (iOS Safari)

1. Open the deployed URL in Safari
2. Tap the **Share** button
3. Scroll down and tap **Add to Home Screen**
4. Tap **Add**

The app opens in standalone mode (no Safari chrome).

---

## Git Hooks

This repo uses tracked git hooks in `.githooks/`:

- **pre-commit** — auto-bumps the root `package.json` patch version on every commit

The `prepare` script in `package.json` auto-configures the hook path on `npm install`. If it ever needs manual setup:

```sh
git config core.hooksPath .githooks
```

---

## Nx Commands

## Generate a library

```sh
npx nx g @nx/js:lib packages/pkg1 --publishable --importPath=@my-org/pkg1
```

## Run tasks

To build the library use:

```sh
npx nx build pkg1
```

To run any task with Nx use:

```sh
npx nx <target> <project-name>
```

These targets are either [inferred automatically](https://nx.dev/concepts/inferred-tasks?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects) or defined in the `project.json` or `package.json` files.

[More about running tasks in the docs &raquo;](https://nx.dev/features/run-tasks?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)

## Versioning and releasing

To version and release the library use

```
npx nx release
```

Pass `--dry-run` to see what would happen without actually releasing the library.

[Learn more about Nx release &raquo;](https://nx.dev/features/manage-releases?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)

## Keep TypeScript project references up to date

Nx automatically updates TypeScript [project references](https://www.typescriptlang.org/docs/handbook/project-references.html) in `tsconfig.json` files to ensure they remain accurate based on your project dependencies (`import` or `require` statements). This sync is automatically done when running tasks such as `build` or `typecheck`, which require updated references to function correctly.

To manually trigger the process to sync the project graph dependencies information to the TypeScript project references, run the following command:

```sh
npx nx sync
```

You can enforce that the TypeScript project references are always in the correct state when running in CI by adding a step to your CI job configuration that runs the following command:

```sh
npx nx sync:check
```

[Learn more about nx sync](https://nx.dev/reference/nx-commands#sync)

## Nx Cloud

Nx Cloud ensures a [fast and scalable CI](https://nx.dev/ci/intro/why-nx-cloud?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects) pipeline. It includes features such as:

- [Remote caching](https://nx.dev/ci/features/remote-cache?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)
- [Task distribution across multiple machines](https://nx.dev/ci/features/distribute-task-execution?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)
- [Automated e2e test splitting](https://nx.dev/ci/features/split-e2e-tasks?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)
- [Task flakiness detection and rerunning](https://nx.dev/ci/features/flaky-tasks?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)

### Set up CI (non-Github Actions CI)

**Note:** This is only required if your CI provider is not GitHub Actions.

Use the following command to configure a CI workflow for your workspace:

```sh
npx nx g ci-workflow
```

[Learn more about Nx on CI](https://nx.dev/ci/intro/ci-with-nx#ready-get-started-with-your-provider?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)

## Install Nx Console

Nx Console is an editor extension that enriches your developer experience. It lets you run tasks, generate code, and improves code autocompletion in your IDE. It is available for VSCode and IntelliJ.

[Install Nx Console &raquo;](https://nx.dev/getting-started/editor-setup?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)

## Useful links

Learn more:

- [Learn more about this workspace setup](https://nx.dev/nx-api/js?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)
- [Learn about Nx on CI](https://nx.dev/ci/intro/ci-with-nx?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)
- [Releasing Packages with Nx release](https://nx.dev/features/manage-releases?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)
- [What are Nx plugins?](https://nx.dev/concepts/nx-plugins?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)

And join the Nx community:

- [Discord](https://go.nx.dev/community)
- [Follow us on X](https://twitter.com/nxdevtools) or [LinkedIn](https://www.linkedin.com/company/nrwl)
- [Our Youtube channel](https://www.youtube.com/@nxdevtools)
- [Our blog](https://nx.dev/blog?utm_source=nx_project&utm_medium=readme&utm_campaign=nx_projects)
