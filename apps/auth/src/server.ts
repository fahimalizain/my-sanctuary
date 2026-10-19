import { OAuthAuthorizationServer } from '@cloudflare/workers-oauth-provider';

/** Alias for the merged global, so every module in this app names the same env type. */
export type Env = Cloudflare.Env;

/**
 * Everything this authorization server can grant, across all its resources. The resource server
 * advertises its own, smaller, up-front baseline; the consent page grants a subset of this
 * catalogue per authorization.
 */
export const SCOPES = ['mcp:read', 'mcp:write', 'offline_access'];

/** The application-owned authorization endpoint, served by `authorize.ts`. */
export const AUTHORIZE_PATH = '/oauth/authorize';

/**
 * Parse the `MCP_RESOURCES` var. Production registers a single resource; local dev registers the
 * two loopback spellings of the same one, because the library compares audiences exactly and only
 * accepts plain `http` on loopback hosts.
 *
 * An empty list is a deployment error: the constructor below refuses to build a server with no
 * resources, so the Worker fails loudly instead of minting tokens for an unintended audience.
 */
export function parseResources(value: string): string[] {
  return value
    .split(',')
    .map((resource) => resource.trim())
    .filter((resource) => resource !== '');
}

interface CachedServer {
  /** The exact configuration the server was built from. */
  configKey: string;
  server: OAuthAuthorizationServer<Env>;
}

/**
 * The server captures its issuer and resource registry at construction, so it cannot be built at
 * module scope — `issuer` only exists once `env` is bound — while rebuilding it per request would
 * throw away the library's in-process caches (client assertions, CIMD fetches, JWKS). Memoise it
 * instead, keyed on the config string: the same config reuses the instance, and a change (local
 * dev vars, a test override) transparently yields a fresh one.
 */
let cachedServer: CachedServer | undefined;

export function getAuthorizationServer(
  env: Env,
): OAuthAuthorizationServer<Env> {
  const resources = parseResources(env.MCP_RESOURCES);
  const configKey = [env.ISSUER, ...resources].join('\n');
  if (cachedServer?.configKey !== configKey) {
    cachedServer = {
      configKey,
      server: createAuthorizationServer(env.ISSUER, resources),
    };
  }
  return cachedServer.server;
}

function createAuthorizationServer(
  issuer: string,
  resources: string[],
): OAuthAuthorizationServer<Env> {
  return new OAuthAuthorizationServer<Env>({
    issuer,
    resources,
    // A request that omits RFC 8707 `resource` needs a target once more than one resource is
    // registered. The first entry is the deployment's canonical spelling — production's only
    // resource, and local dev's `localhost` URL — so it is the right compatibility default for
    // clients that predate resource indicators. Requests that do send `resource` must still
    // name one of the registered values.
    defaultResource: resources[0],
    // Paths are relative to the issuer, which is how the public Worker's proxy forwards them.
    authorizeEndpoint: AUTHORIZE_PATH,
    // The library also serves RFC 7009 revocation here and advertises this URL as
    // `revocation_endpoint`, so keep it on `/oauth/*` where the public Worker's proxy sends
    // traffic.
    tokenEndpoint: '/oauth/token',
    clientRegistrationEndpoint: '/oauth/register',
    scopesSupported: SCOPES,
    // Client ID Metadata Documents: clients use an HTTPS metadata URL as their `client_id`.
    // Requires the `global_fetch_strictly_public` compatibility flag in wrangler.toml — the
    // library only advertises the feature when it sees that flag at runtime.
    clientIdMetadataDocumentEnabled: true,
  });
}
