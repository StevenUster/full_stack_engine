/**
 * fse-ssr browser runtime.
 *
 * The Rust server serializes each page's full render context into
 * `<script type="application/json" id="__fse-props__">` (see the framework's
 * `render_template`). Client code — islands, inline scripts — reads it here,
 * so no extra request is needed for the data the page was rendered with.
 */
import type { GlobalSsrContext } from "./types";

let cached: unknown;

/** Typed page props, parsed once from the embedded JSON blob. */
export function pageProps<T = Record<string, never>>(): T & GlobalSsrContext {
  if (cached === undefined) {
    const el = document.getElementById("__fse-props__");
    cached = el?.textContent ? JSON.parse(el.textContent) : {};
  }
  return cached as T & GlobalSsrContext;
}
