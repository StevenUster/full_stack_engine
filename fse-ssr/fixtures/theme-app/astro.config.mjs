// Fixture child theme for the theme-inheritance build: theme.json names
// fse-theme-default as parent. Expected in dist/ after `bun run build`:
// - every parent page (inherited), with this theme's SidebarLinks override,
// - this theme's own override of fse/public-detail (APP-OVERRIDE-DETAIL),
// - the module page (MODULE-BLOG-PAGE) rendered in the parent's layout,
// - theme.json copied from the project root.
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";
import fseSsr from "fse-ssr";

export default defineConfig({
  integrations: [
    fseSsr({
      locales: "./locales",
      defaultLocale: "en",
      modulesDir: "./.fse/modules",
    }),
  ],
  vite: {
    plugins: [tailwindcss()],
  },
});
