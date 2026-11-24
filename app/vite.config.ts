import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath, URL } from 'node:url';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  plugins: [tailwindcss(), react()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  build: { outDir: 'dist', emptyOutDir: true },
  server: {
    // scripts/dev.mjs sets ASIDE_URL from the server's session.started event.
    host: '127.0.0.1',
    port: Number(process.env.ASIDE_DEV_PORT || 5173),
    strictPort: true,
    proxy: process.env.ASIDE_URL
      ? {
          '/api': { target: process.env.ASIDE_URL, changeOrigin: true },
          '/auth': { target: process.env.ASIDE_URL, changeOrigin: true },
        }
      : undefined,
  },
});
