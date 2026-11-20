import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  build: { outDir: 'dist', emptyOutDir: true },
  server: {
    // Dev: run `cargo run -- demo --no-open`, then start Vite with
    // ASIDE_URL set to the `url` from its `session.started` event.
    proxy: process.env.ASIDE_URL ? { '/api': process.env.ASIDE_URL } : undefined,
  },
});
