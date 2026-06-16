import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { VitePWA } from 'vite-plugin-pwa';
import * as path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Read root package.json for version injection
import { createRequire } from 'module';
const require = createRequire(import.meta.url);
const rootPkg = require('../../package.json');

export default defineConfig(({ mode }) => ({
  plugins: [
    react(),
    VitePWA({
      registerType: 'autoUpdate',
      manifest: {
        name: 'Sanctuary',
        short_name: 'Sanctuary',
        description: 'Your personal productivity sanctuary',
        theme_color: '#204f0a',
        background_color: '#f5f5ef',
        display: 'standalone',
        scope: '/',
        start_url: '/',
        icons: [
          {
            src: '/pwa-96x96.png',
            sizes: '96x96',
            type: 'image/png',
          },
          {
            src: '/pwa-192x192.png',
            sizes: '192x192',
            type: 'image/png',
          },
          {
            src: '/pwa-512x512.png',
            sizes: '512x512',
            type: 'image/png',
          },
          {
            src: '/pwa-512x512.png',
            sizes: '512x512',
            type: 'image/png',
            purpose: 'any maskable',
          },
        ],
      },
      workbox: {
        globPatterns: ['**/*.{js,css,html,ico,png,svg}'],
        navigateFallbackDenylist: [/^\/auth\//],
      },
    }),
  ],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './'),
    },
  },
  define: {
    __API_BASE_URL__: JSON.stringify(
      mode === 'production' ? '' : (process.env.VITE_API_BASE_URL || 'http://localhost:8080')
    ),
    __APP_VERSION__: JSON.stringify(rootPkg.version),
  },
  build: {
    outDir: '../../dist/apps/web',
    emptyOutDir: true,
  },
}));
