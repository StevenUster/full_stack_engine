// @ts-check
import { defineConfig } from 'astro/config';

import tailwindcss from '@tailwindcss/vite';

import react from '@astrojs/react';

import fseSsr from './fse-ssr/integration.mjs';

// https://astro.build/config
export default defineConfig({
  integrations: [react(), fseSsr()],
  vite: {
    plugins: [tailwindcss()],
    server: {
      hmr: {
        protocol: 'ws',
        host: 'localhost',
        clientPort: 4321,
      },
    },
  },
});
