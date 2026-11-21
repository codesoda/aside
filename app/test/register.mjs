// Lets `node --test` import .svelte components and extensionless .ts modules
// the way Vite does, so components can be mounted in jsdom without a bundler.
import { register } from 'node:module';

register('./svelte-hooks.mjs', import.meta.url);
