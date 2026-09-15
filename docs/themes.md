# Themes

full_stack_engine themes work like WordPress parent and child themes. A theme
is built by **any HTML-generating tool**. The framework only sees the build
output and layers it at runtime.

## What a theme is

A built theme is one folder:

```
dist/
├── theme.json              { "name": "my-theme", "parent": "fse-theme-default" }
├── index.html              → template "index"
├── login/index.html        → template "login"   (login.html works too)
├── emails/verify/index.html→ template "emails/verify"
├── _astro/app.3f1c.css     → served at /_astro/app.3f1c.css
└── favicon.svg             → served at /favicon.svg
```

- **`theme.json`**: `name` (unique) and an optional `parent`, plus optional `version` and `description`.
- **`*.html`**: [Tera](https://keats.github.io/tera/) templates, named by their path. Autoescaping is always on.
- **Everything else** is a static asset, served at its path. Templates and `theme.json` are never served raw.

How the theme was produced doesn't matter: Astro with `fse-ssr`, Eleventy,
Hugo, a hand-written folder. Any of them works if the output follows this
layout.

## Installing and activating

```rust
static THEME: Dir = include_dir!("$CARGO_MANIFEST_DIR/theme/dist");

FrameworkApp::new()
    // cargo crate with the built default theme
    .theme(Theme::embedded(&fse_theme_default::DIST))
    // this app's child theme; in ENV=dev, pages/assets come from astro dev first
    .theme(Theme::embedded(&THEME).dev_server("http://localhost:4321"))
```

| Source | API |
| --- | --- |
| folder or crate, compiled into the binary | `Theme::embedded(&DIR)` |
| folder on disk, read at boot (drop-in themes) | `Theme::from_directory("themes/x")?` |
| built in code (tests, generated) | `Theme::new(manifest).with_file(path, bytes)` |

**Active theme.** The framework picks the first match:

1. the `THEME` environment variable,
2. `.active_theme("name")`,
3. the one installed theme that no other installed theme extends.

With only a parent and its child installed, rule 3 activates the child
automatically. Themes outside the active chain are ignored, like inactive
WordPress themes. Boot fails loudly on duplicate names, a missing parent,
cycles, or an ambiguous choice.

## How layering works

The active theme and its ancestors form the chain: `child → parent → grandparent`.

- **Templates.** `render_tpl("login", …)` uses the most specific theme that has
  `login`. Every theme's own copy is also registered as `@{name}/{template}`,
  so a child template can extend or include what it overrides:
  `{% extends "@fse-theme-default/login" %}`.
- **Assets.** `/favicon.svg` comes from the most specific theme that has the file.
- **Dev mode** (`ENV=dev`). For each page or asset, the framework tries the
  chain's dev servers (child first) and falls back to the built files.
- **Broken templates** are logged and skipped at boot.
  `full_stack_engine::testing::load_themes(themes)` makes them fail `cargo test`.

A child theme therefore only has to contain what it changes.

## The template contract

The framework renders these names; the default theme provides all of them.

| Template | Rendered by | Context (besides the defaults below) |
| --- | --- | --- |
| `index` | app (`services/index.rs` in the starter) | app-defined |
| `error`, `public/error` | error handler (signed in / signed out) | `status`, `error` |
| `login`, `register`, `register-success`, `forgot-password`, `reset-password`, `settings`, `users`, `user` | auth module | see `fse-theme-default/src/types/pages.ts` |
| `emails/verify`, `emails/verify-email-change`, `emails/password-reset` | auth module (emails) | `verify_url`/`reset_url`, `base_url`, `t` |
| `fse/list`, `fse/form`, `fse/public-list`, `fse/public-detail` | generated model CRUD | see `fse-theme-default/src/types.ts` |

A model-specific template beats the generic one: `admin/products` overrides
`fse/list` for that model.

Every page context also contains:

- `t`: translations for the request's language,
- `lang`, `lang_prefix` and `i18n`,
- `nav`: `[{ table, href }]` for each model the user may read (empty when signed out),
- `user`: `{ id, role, is_admin, can_read_users }`, only present when signed in,
- whatever the app's `global_context_injector` adds.

## Child themes with Astro

`fse-ssr` reads the project's `theme.json`. With a `parent`, it does two things:

1. **Builds the parent's pages into the child** (`inheritPages`, default
   `true`), with the child's overrides applied. `dist/` then contains every
   page. Pass `inheritPages: false` to build only the child's own pages and
   let the runtime fall back to the parent.
2. **Resolves parent parts child-first**, like WordPress template parts. When
   a parent file imports another parent file (`../components/Card.astro`,
   `../styles/global.css`, `../assets/logo.svg`), a file at the same `src/`
   path in the child wins. `@parent/...` always means the parent's original,
   so an override can wrap it.

```
theme/
├── theme.json            { "name": "starter", "parent": "fse-theme-default" }
├── package.json          depends on fse-ssr + fse-theme-default (npm: the Astro sources)
├── astro.config.mjs      integrations: [fseSsr({ locales: "../locales" })]
├── tsconfig.json         paths: { "@parent/*": ["./node_modules/fse-theme-default/src/*"] }
└── src/
    ├── pages/            the app's own pages; a same-path page overrides the parent's
    ├── components/SidebarLinks.astro   override → shows up in every inherited page
    └── styles/global.css override → recolors every inherited page
```

The default theme's extension points are listed in its
[README](../fse-theme-default/README.md).

## Child themes without Astro

Build any folder that follows the layout above, give it a `theme.json` with
a `parent`, and install it after its parent. Anything the folder doesn't
contain falls back to the parent at runtime. A minimal plain-HTML child that
restyles only the login page:

```
my-theme/
├── theme.json          { "name": "my-theme", "parent": "fse-theme-default" }
└── login/index.html    {% extends "@fse-theme-default/login" %} … or a full page
```

## Publishing a theme

Ship the **built folder** so apps can install it, typically as a crate that
embeds `dist/` (see `fse-theme-default/lib.rs`). If other Astro themes should
extend it, also publish its **sources** (with `theme.json` at the root) to npm
under the same name as `name`.
