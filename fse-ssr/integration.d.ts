import type { AstroIntegration } from "astro";

export interface FseSsrOptions {
  /**
   * Path to the locale directory, relative to the project root. Default: the
   * app's `locales/` — `"../locales"` for a `theme/` folder, `"../../locales"`
   * for a `themes/<name>/` folder (whichever directory holds the app's
   * `fse.toml` or `Cargo.toml`).
   */
  locales?: string;
  /** Locale file (without extension) read to type `t.*`. Default: `"en"`. */
  defaultLocale?: string;
  /**
   * Where `fse sync` extracts module frontends, relative to the project
   * root. Default: the app's `.fse/modules`, found like `locales`. Each `<name>/frontend/pages` layers
   * below every theme's pages.
   */
  modulesDir?: string;
  /**
   * Build the parent themes' pages (named by `parent` in `theme.json`) into
   * this theme, with this theme's overridden components, layouts, styles and
   * assets applied. Default: `true`. With `false` only the project's own
   * pages are built and the framework serves the rest from the parent's
   * built templates at runtime.
   */
  inheritPages?: boolean;
}

/**
 * The fse-ssr Astro integration: compile-to-Tera SSR expressions plus
 * WordPress-style theme inheritance driven by the project's `theme.json`.
 */
export default function fseSsr(options?: FseSsrOptions): AstroIntegration;
