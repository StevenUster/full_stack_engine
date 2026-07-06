// @ts-check
import { defineConfig, fontProviders } from 'astro/config';

import tailwindcss from '@tailwindcss/vite';

import react from '@astrojs/react';

import fseSsr from 'fse-ssr';

// https://astro.build/config
export default defineConfig({
  integrations: [react(), fseSsr({ defaultLocale: 'en' })],
  // Self-hosted, build-time optimized fonts (no runtime Google Fonts request).
  // The Fonts API graduated to stable in Astro 6, so it now lives at top level.
  fonts: [
    {
      provider: fontProviders.google(),
      name: 'Montserrat',
      cssVariable: '--font-montserrat',
      weights: [300, 400, 500, 600, 700, 800],
      styles: ['normal'],
      subsets: ['latin', 'latin-ext'],
      fallbacks: ['ui-sans-serif', 'system-ui', 'sans-serif'],
    },
  ],
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
