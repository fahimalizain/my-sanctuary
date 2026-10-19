import { cloudflareTest } from '@cloudflare/vitest-pool-workers';
import { defineConfig } from 'vitest/config';

/**
 * Tests run inside workerd against the real Worker, wired the way production is: the wrangler
 * config's bindings, and the production issuer and resource so every assertion is about the
 * contract clients will actually see.
 *
 * `ISSUER`/`MCP_RESOURCES` are overridden back to the production values because `wrangler.toml`
 * already carries them — the override exists so a `.dev.vars` written for local `wrangler dev`
 * cannot leak into a test run.
 */
export default defineConfig({
  plugins: [
    cloudflareTest({
      wrangler: { configPath: './wrangler.toml' },
      miniflare: {
        bindings: {
          ISSUER: 'https://my-sanctuary.fahimalizain.com',
          MCP_RESOURCES: 'https://my-sanctuary.fahimalizain.com/mcp',
        },
        // Miniflare resolves a function-valued service binding to an in-process fetch handler,
        // which is the test double the `APP` binding needs; its option types only describe the
        // declarative forms, hence the cast. Slice 2b replaces this with the session lookup.
        serviceBindings: {
          APP: (() => Response.json({ user: null })) as unknown as string,
        },
      },
    }),
  ],
  test: {
    include: ['test/**/*.test.ts'],
  },
});
