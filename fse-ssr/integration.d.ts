import type { AstroIntegration } from "astro";

export interface FseSsrOptions {
  /** Path to the locale directory, relative to the Astro project root. Default: `"../../locales"`. */
  locales?: string;
  /** Locale file (without extension) read to type `t.*`. Default: `"en"`. */
  defaultLocale?: string;
  /**
   * Theme providing default pages and shared UI: an installed package name
   * (e.g. `"fse-theme-default"`) or a local folder relative to the project
   * root (`"./themes/custom"`). Every page under the theme's `pages/` that
   * the app does not define itself is added to the build; theme files are
   * importable as `@theme/...`.
   */
  theme?: string;
}

export default function fseSsr(options?: FseSsrOptions): AstroIntegration;
