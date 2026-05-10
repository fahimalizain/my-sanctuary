import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import * as path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig(({ mode }) => ({
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './'),
    },
  },
  define: {
    __API_BASE_URL__: JSON.stringify(
      mode === 'production' ? '' : (process.env.VITE_API_BASE_URL || 'http://localhost:8080')
    ),
  },
  build: {
    outDir: '../../dist/apps/web',
    emptyOutDir: true,
  },
}));
