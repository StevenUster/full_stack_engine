// Fixture app for the theme-layering integration test: no app pages except
// one deliberate override. Everything else must come from fse-theme-default.
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";
import fseSsr from "fse-ssr";

export default defineConfig({
  integrations: [
    fseSsr({
      locales: "./locales",
      defaultLocale: "en",
      theme: "fse-theme-default",
      modulesDir: "./.fse/modules",
    }),
  ],
  vite: {
    plugins: [tailwindcss()],
  },
});
