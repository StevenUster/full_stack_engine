# fse-ssr

Compile-to-Tera SSR bindings for [full_stack_engine](https://github.com/StevenUster/full_stack_engine)'s
Astro starter. Author `.astro` templates in native TypeScript against
`ssr<T>()` placeholders; a build-time Astro/Vite integration compiles the
expressions that touch them into the equivalent Tera syntax, so the built
HTML is exactly what the Rust backend's Tera renderer expects — no template
syntax ever appears in frontend source.

This package is not generically reusable outside apps built on
full_stack_engine: it assumes the framework's `render_tpl` context shape and
the `<script type="application/json" id="__fse-props__">` placeholder emitted
by its `Layout.astro`. See the starter's [README](../starter/README.md#server-rendered-data-in-templates-fse-ssr)
for usage and the supported expression grammar.

## Entry points

- `fse-ssr` — the Astro integration (`astro.config.mjs`).
- `fse-ssr/ssr` — `ssr<T>()` and its types, for use inside `.astro` files.
- `fse-ssr/client` — `pageProps<T>()`, for browser/island code.

## Development

```bash
bun install
bun run build   # tsc, one-shot
bun run dev     # tsc --watch
```

This package is standalone — it isn't linked into the starter via a
workspace. To try local changes in the starter before publishing a new
version, `bun publish` a prerelease (or use `bun pm pack` + install the
tarball) and bump the starter's `fse-ssr` dependency to it.

## Publishing

```bash
bun run build
bun publish
```
