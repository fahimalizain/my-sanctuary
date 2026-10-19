/**
 * Bindings and vars of `my-sanctuary-auth`, merged into the global `Cloudflare.Env` that
 * `@cloudflare/workers-types` (and `cloudflare:test`) already declare.
 *
 * This Worker has no public route: the public `my-sanctuary` Worker (apps/worker) reaches it
 * only over service bindings — `AUTH` for the public `/oauth/*` + discovery paths, and the same
 * binding for `POST /internal/validate` from the `/mcp` handler.
 */
declare namespace Cloudflare {
  interface Env {
    /**
     * OAuth storage: dynamically registered clients, authorization codes, grants and tokens,
     * and the cached Client ID Metadata Documents. Tokens and secrets are stored hashed only
     * (see the library's storage-schema.md). Local `wrangler dev` simulates KV, so nothing has
     * to exist in the account before deploying.
     */
    OAUTH_KV: KVNamespace;
    /**
     * The public Worker. The authorization endpoint reads the logged-in session from it
     * (`GET /auth/me`) before showing the consent page.
     */
    APP: Fetcher;
    /** Canonical issuer advertised in metadata; every endpoint URL is built from it. */
    ISSUER: string;
    /** Comma-separated canonical resource URLs this server may issue and validate tokens for. */
    MCP_RESOURCES: string;
  }
}
