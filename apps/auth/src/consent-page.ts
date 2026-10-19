import type { ConsentDescription } from '@cloudflare/workers-oauth-provider';

/**
 * The consent page and the local error pages the authorization endpoint falls back to.
 *
 * Everything on these pages is either a constant of this Worker, or came from the client (its
 * name, its scopes, the redirect host) or from the session (the email): all of it is escaped
 * before it is interpolated, because an attacker chooses what a client registers.
 */

/** Human wording for the catalogue in server.ts, so the page never shows a bare scope name. */
const SCOPE_DESCRIPTIONS: Record<string, string> = {
  'mcp:read': 'Read your tasks, agenda and calendar',
  'mcp:write': 'Create and change tasks, agenda items and calendar events',
  offline_access: 'Stay connected without asking again',
};

/**
 * The policy for every page this module renders: nothing loads over the network (the stylesheet
 * is inline, there are no images), the only form target is this same origin, and nobody may
 * frame the page. The library's consent headers already carry `frame-ancestors 'none'`; the
 * policies are combined rather than one replacing the other, since a browser enforces every
 * policy it is given.
 */
const PAGE_CSP =
  "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'";

/** Escape a string for an HTML text node or a quoted attribute value. */
export function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (char) => `&#${char.charCodeAt(0)};`);
}

const STYLES = `
:root {
  color-scheme: light dark;
  --page: #f4f5f7;
  --card: #ffffff;
  --text: #16181c;
  --muted: #5c6470;
  --line: #dde1e6;
  --accent: #1a5fd0;
  --accent-ink: #ffffff;
  --quiet: #eceef1;
}
@media (prefers-color-scheme: dark) {
  :root {
    --page: #0e1013;
    --card: #171a1f;
    --text: #e7e9ec;
    --muted: #9aa3af;
    --line: #2b3038;
    --accent: #6ba1ff;
    --accent-ink: #0b0e13;
    --quiet: #23272e;
  }
}
*, *::before, *::after { box-sizing: border-box; }
body {
  margin: 0;
  padding: 2rem 1rem;
  background: var(--page);
  color: var(--text);
  font: 1rem/1.55 system-ui, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
}
main {
  margin: 0 auto;
  max-width: 27rem;
  padding: 1.75rem;
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: 14px;
}
h1 { margin: 0 0 1rem; font-size: 1.3rem; line-height: 1.3; }
p { margin: 0 0 1rem; }
a { color: var(--accent); }
.brand {
  margin: 0 0 1.25rem;
  font-size: 0.78rem;
  font-weight: 600;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--muted);
}
.origin { color: var(--muted); }
.facts { margin: 0; display: grid; gap: 0.7rem; }
.facts dt {
  font-size: 0.78rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--muted);
}
.facts dd { margin: 0.15rem 0 0; overflow-wrap: anywhere; }
.warn {
  margin: 1rem 0 0;
  padding: 0.75rem 0.9rem;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--quiet);
  font-size: 0.92rem;
}
form { margin: 1.5rem 0 0; }
fieldset { margin: 0; padding: 0; border: 0; }
legend {
  padding: 0;
  font-size: 0.78rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--muted);
}
.scope { display: flex; gap: 0.7rem; align-items: flex-start; padding: 0.7rem 0; border-bottom: 1px solid var(--line); }
.scope input { flex: none; width: 1.05rem; height: 1.05rem; margin: 0.25rem 0 0; accent-color: var(--accent); }
.scope code { display: block; margin-top: 0.1rem; font-size: 0.82rem; color: var(--muted); }
.actions { display: flex; flex-wrap: wrap; gap: 0.6rem; margin-top: 1.5rem; }
button { font: inherit; padding: 0.6rem 1.1rem; border: 1px solid transparent; border-radius: 9px; cursor: pointer; }
.allow { background: var(--accent); color: var(--accent-ink); font-weight: 600; }
.deny { background: transparent; color: var(--text); border-color: var(--line); }
`;

/** Wrap a body in the shared document: same head, same stylesheet, no scripts, no assets. */
function page(title: string, body: string): string {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>${escapeHtml(title)}</title>
<style>${STYLES}</style>
</head>
<body>
<main>
${body}
</main>
</body>
</html>`;
}

/**
 * The consent form: who is asking, where the tokens will go, and which scopes the user grants.
 *
 * `handle` is the library's single-use transaction handle — it is the only thing the form has to
 * carry back, because the authorization request itself stayed server-side.
 */
export function renderConsentPage(
  details: ConsentDescription,
  handle: string,
  email: string,
): string {
  const clientName = escapeHtml(details.clientName);
  const origin = details.clientDomain
    ? `Published by <strong>${escapeHtml(details.clientDomain)}</strong>.`
    : 'This app registered itself; its name is not verified.';
  const scopes = details.scope
    .map(
      (scope) => `<label class="scope">
<input type="checkbox" name="scope" value="${escapeHtml(scope)}" checked>
<span><span>${escapeHtml(SCOPE_DESCRIPTIONS[scope] ?? scope)}</span><code>${escapeHtml(scope)}</code></span>
</label>`,
    )
    .join('\n');
  const loopback = details.redirectIsLoopback
    ? `<p class="warn">This sends access to an app on your computer. Continue only if you just started signing in from it.</p>`
    : '';

  return page(
    `Authorize ${details.clientName}`,
    `<p class="brand">Sanctuary</p>
<h1>${clientName} wants to access your account</h1>
<p class="origin">${origin}</p>
<dl class="facts">
<div><dt>Signed in as</dt><dd>${escapeHtml(email)}</dd></div>
<div><dt>Access will be sent to</dt><dd>${escapeHtml(details.redirectHost)}</dd></div>
</dl>
${loopback}
<form method="post">
<input type="hidden" name="handle" value="${escapeHtml(handle)}">
<fieldset>
<legend>Permissions</legend>
${scopes}
</fieldset>
<div class="actions">
<button class="allow" type="submit" name="decision" value="approve">Allow</button>
<button class="deny" type="submit" name="decision" value="deny">Deny</button>
</div>
</form>`,
  );
}

export interface ErrorPageOptions {
  status: number;
  title: string;
  message: string;
  /** This endpoint's own path and query, linked as "Start again" when a retry can succeed. */
  retryPath?: string;
  /** Extra response headers, such as `Allow` on a 405. */
  headers?: Record<string, string>;
}

/**
 * A local page for a failure that must not be sent to the client: the request was never
 * validated, the session is gone, or the public Worker did not answer. The message says what the
 * user can do about it, and is escaped like everything else.
 */
export function renderErrorPage(options: ErrorPageOptions): Response {
  const retry = options.retryPath
    ? `\n<p><a href="${escapeHtml(options.retryPath)}">Start again</a></p>`
    : '';

  return new Response(
    page(
      options.title,
      `<p class="brand">Sanctuary</p>
<h1>${escapeHtml(options.title)}</h1>
<p>${escapeHtml(options.message)}</p>${retry}`,
    ),
    {
      status: options.status,
      headers: pageHeaders(options.headers),
    },
  );
}

/**
 * Add this module's page headers to the consent response, whose other headers the library built
 * (the browser-bound cookie and its anti-framing headers). The policy is combined with the
 * library's rather than replacing it, and a header the library already set is left alone.
 */
export function applyPageHeaders(headers: Headers): Headers {
  for (const [name, value] of Object.entries(pageHeaders())) {
    if (name === 'Content-Security-Policy') continue;
    if (!headers.has(name)) headers.set(name, value);
  }
  const existing = headers.get('Content-Security-Policy');
  headers.set(
    'Content-Security-Policy',
    existing ? `${existing}, ${PAGE_CSP}` : PAGE_CSP,
  );
  return headers;
}

function pageHeaders(extra?: Record<string, string>): Record<string, string> {
  return {
    'Content-Type': 'text/html; charset=utf-8',
    'Cache-Control': 'no-store',
    'Content-Security-Policy': PAGE_CSP,
    'X-Frame-Options': 'DENY',
    'Referrer-Policy': 'no-referrer',
    ...extra,
  };
}
