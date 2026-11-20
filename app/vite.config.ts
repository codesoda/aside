import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
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
