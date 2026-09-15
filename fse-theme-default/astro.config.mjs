// @ts-check
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";
import fseSsr from "fse-ssr";

// The default theme is a standalone Astro project: `bun run build` produces
// dist/ (templates + assets + theme.json), which the Rust crate in this
// folder embeds. Child themes don't reuse this config — they have their own
// and name this theme as `parent` in their theme.json.
export default defineConfig({
  integrations: [
    fseSsr({
      // The theme's pages use the framework's built-in translations.
      locales: "../framework/locales",
      defaultLocale: "en",
      modulesDir: "./.fse/modules",
    }),
  ],
  vite: {
    plugins: [tailwindcss()],
  },
});
