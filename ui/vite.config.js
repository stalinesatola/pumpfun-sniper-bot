import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';

// In dev (`npm run dev`) the bot's API is proxied, so the page behaves exactly like production.
const bot = process.env.BOT_URL || 'http://127.0.0.1:8787';

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  server: {
    proxy: {
      '/api': bot,
      '/ws': { target: bot, ws: true },
    },
  },
});
