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

An app keeps its themes in a **`themes/` folder, one theme per subfolder**,
and names the active one in `fse.toml`:

```
my-app/
├── fse.toml              [themes] active = "my-app"
└── themes/
    ├── my-app/           Astro project, theme.json { "name": "my-app", "parent": "fse-theme-default" }
    ├── my-app-dark/      Astro project, theme.json { "name": "my-app-dark", "parent": "my-app" }
    └── plain/            hand-written: theme.json + *.html, nothing to build
```

```toml
# fse.toml — every key optional
[themes]
active = "my-app"                      # theme.json name; default: the one nothing extends
# dir = "themes"
# dev_server = "http://localhost:4321" # where an npm theme's dev server runs in ENV=dev
```

`full_stack_engine::themes!()` embeds every folder at compile time and
returns a `ThemeSet` with that active theme. Theme crates go on top:

```rust
pub fn themes() -> ThemeSet {
    full_stack_engine::themes!().with(Theme::embedded(&fse_theme_default::DIST))
}

FrameworkApp::new().themes(themes())
```

Per folder, the macro embeds:

| Folder has | Embedded |
| --- | --- |
| `dist/theme.json` | `dist/` (the build output), with `dev_server` for `ENV=dev` |
| `package.json`, no build yet | an *unbuilt* stub: the app compiles, and boot fails with "run the build" only if the active theme needs it |
| only `theme.json` | the folder itself |
| none of these | compile error |

Editing `fse.toml` or a built file recompiles the app. Since every theme is
in the binary, `THEME=my-app-dark` switches at boot without a rebuild.
`cargo run --bin dev` installs and builds every unbuilt theme, then runs the
active theme's dev server.

Other ways to install a theme:

| Source | API |
| --- | --- |
| folder or crate, compiled into the binary | `Theme::embedded(&DIR)` |
| folder on disk, read at boot (drop-in themes) | `Theme::from_directory("themes/x")?` |
| built in code (tests, generated) | `Theme::new(manifest).with_file(path, bytes)` |

Add them with `ThemeSet::with` or `FrameworkApp::theme`.

**Active theme.** The framework picks the first match:

1. the `THEME` environment variable,
2. `[themes] active` in `fse.toml` (or `ThemeSet::active` / `.active_theme("name")`),
3. the one installed theme that no other installed theme extends.

With only a parent and its child installed, rule 3 activates the child
automatically. Themes outside the active chain are ignored, like inactive
WordPress themes. Boot fails loudly on duplicate names, a missing parent,
cycles, an ambiguous choice, or an unbuilt theme in the active chain.
Test every theme as the active one, so switching never meets a broken page:

```rust
#[test]
fn every_installed_theme_loads_as_the_active_one() {
    let themes = my_app::themes();
    for theme in themes.installed() {
        full_stack_engine::testing::load_themes(themes.clone().active(theme.name())).unwrap();
    }
}
```

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
- `nav`: `[{ table, href, label, icon, public }]` — the sidebar entries, ordered: each
  non-nested model the user may read (`#[model(nav(...))]`, `nav = false`,
  `ModelHooks::in_nav`) plus, for everyone, each `public_nav(...)` public list. `label` is
  already translated (`t.models.<table>.nav` / `.public_nav`, else `.title`, else the table
  name); `icon` names one of the default theme's `NavIcon` icons,
- `user`: `{ id, role, is_admin, can_read_users }`, only present when signed in,
- whatever the app's `global_context_injector` adds,
- on the templates it was registered for, whatever `FrameworkApp::page_context(template, ...)`
  providers return (under the handler's own keys) — how an app puts data into a page a
  module renders, e.g. its own section of the auth module's `settings` page, drawn by
  overriding the default theme's `SettingsSections.astro`.

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

The parent is looked up among the **sibling folders** first (another theme
of the same `themes/` folder, read live from disk), then as an installed npm
package. Chains of any length work: `my-app-dark → my-app →
fse-theme-default`.

```
themes/my-app/
├── theme.json            { "name": "my-app", "parent": "fse-theme-default" }
├── package.json          depends on fse-ssr + fse-theme-default (npm: the Astro sources)
├── astro.config.mjs      integrations: [fseSsr({ locales: "../../locales" })]
├── tsconfig.json         paths: { "@parent/*": ["./node_modules/fse-theme-default/src/*"] }
└── src/
    ├── pages/            the app's own pages; a same-path page overrides the parent's
    ├── components/SidebarLinks.astro   override → shows up in every inherited page
    └── styles/global.css override → recolors every inherited page

themes/my-app-dark/       a child of the theme above
├── theme.json            { "name": "my-app-dark", "parent": "my-app" }
├── package.json          same dependencies as my-app (its build needs them too)
├── astro.config.mjs      same as my-app
├── tsconfig.json         paths: { "@parent/*": ["../my-app/src/*"] }
└── src/styles/global.css the only override; add `@source "../../../my-app/src";`
                          so Tailwind sees the classes of the parent's pages
```

The default theme's extension points are listed in its
[README](../fse-theme-default/README.md).

## Child themes without Astro

Put any folder that follows the layout above into `themes/`, with a
`theme.json` that names a `parent`. Anything the folder doesn't
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
