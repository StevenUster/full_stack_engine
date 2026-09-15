// @ts-check
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";
import fseSsr from "fse-ssr";

// A child theme: theme.json names `fse-theme-default` as its parent, so the
// fse-ssr integration builds every parent page into dist/ with this theme's
// overrides applied (see src/components/SidebarLinks.astro and
// src/styles/global.css). Pages under src/pages/ are the app's own.
export default defineConfig({
  integrations: [fseSsr({ locales: "../locales", defaultLocale: "en" })],
  vite: {
    plugins: [tailwindcss()],
    server: {
      hmr: { protocol: "ws", host: "localhost", clientPort: 4321 },
    },
  },
});
