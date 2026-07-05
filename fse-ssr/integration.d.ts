import type { AstroIntegration } from "astro";

export interface FseSsrOptions {
  /** Path to the locale directory, relative to the Astro project root. Default: `"../../locales"`. */
  locales?: string;
  /** Locale file (without extension) read to type `t.*`. Default: `"en"`. */
  defaultLocale?: string;
}

export default function fseSsr(options?: FseSsrOptions): AstroIntegration;
