import js from '@eslint/js';
import tseslint from 'typescript-eslint';

/**
 * Minimal flat config, scoped to this app: the rest of the workspace has no ESLint yet, so
 * nothing here is shared. Type-aware rules are deliberately off — linting must not need a
 * tsconfig program — and Workers globals come from @cloudflare/workers-types, not ESLint.
 */
export default tseslint.config(
  { ignores: ['dist/**', '.wrangler/**'] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ['**/*.{ts,mjs,js}'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
    },
  },
);
