import { AUTHORIZE_PATH, getAuthorizationServer, type Env } from './server';
import {
  handleValidateRequest,
  INTERNAL_PREFIX,
  PROXY_HEADER,
  VALIDATE_PATH,
} from './validate';

/**
 * The OAuth 2.1 authorization server for the remote MCP server at
 * `https://my-sanctuary.fahimalizain.com/mcp`.
 *
 * Nothing here is public: the `my-sanctuary` Worker forwards the public `/oauth/*` and
 * `/.well-known/oauth-authorization-server*` paths verbatim over the `AUTH` service binding, and
 * its `/mcp` handler calls `POST /internal/validate` on the same binding. Requests therefore
 * arrive with the original public URL, so metadata and endpoint URLs keep the public issuer.
 */
export default {
  async fetch(
    request: Request,
    env: Env,
    ctx: ExecutionContext,
  ): Promise<Response> {
    const { pathname } = new URL(request.url);

    if (
      pathname === INTERNAL_PREFIX ||
      pathname.startsWith(`${INTERNAL_PREFIX}/`)
    ) {
      return handleInternal(request, env, pathname);
    }

    // Slice 2b replaces this placeholder with the session lookup and consent page. It must be
    // routed before the library: `parseAuthRequest()` rejects requests to the real endpoint.
    if (pathname === AUTHORIZE_PATH) {
      return new Response('not implemented', {
        status: 501,
        headers: { 'Content-Type': 'text/plain; charset=utf-8' },
      });
    }

    // Discovery, token exchange, refresh and RFC 7009 revocation are the library's.
    return getAuthorizationServer(env).fetch(request, env, ctx);
  },
} satisfies ExportedHandler<Env>;

function handleInternal(
  request: Request,
  env: Env,
  pathname: string,
): Response | Promise<Response> {
  // See PROXY_HEADER: a request from the public internet must not reach the internal surface.
  if (request.headers.has(PROXY_HEADER)) return notFound();
  if (pathname !== VALIDATE_PATH) return notFound();
  return handleValidateRequest(request, env);
}

function notFound(): Response {
  return Response.json({ error: 'not_found' }, { status: 404 });
}
