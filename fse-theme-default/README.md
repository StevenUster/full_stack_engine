# fse-theme-default

The default theme for [full_stack_engine](https://github.com/StevenUster/full_stack_engine)
apps: the app shell (sidebar navigation built from your models, dark mode),
the design system, every auth page and email, and the generic
metadata-driven CRUD pages. An app with nothing but `#[model]` structs gets
a complete UI from it.

It ships in two forms, built from this one folder:

| Package | Contains | Used by |
| --- | --- | --- |
| crate `fse-theme-default` | the **built** theme (`dist/`) | every app: `.theme(Theme::embedded(&fse_theme_default::DIST))` |
| npm `fse-theme-default` | the **Astro sources** | child themes built with Astro |

## Theme contract

A built theme is a folder with `theme.json` (`name`, optional `parent`),
`**/*.html` Tera templates (named by path: `login/index.html` → `login`) and
static assets. The framework layers the active theme over its parents at
runtime, so a child theme only contains what it changes. See
[docs/themes.md](../docs/themes.md).

Template names the framework renders: `index`, `error`, `public/error`,
`login`, `register`, `register-success`, `forgot-password`,
`reset-password`, `settings`, `users`, `user`, `emails/verify`,
`emails/verify-email-change`, `emails/password-reset`, `fse/list`,
`fse/form`, `fse/public-list`, `fse/public-detail`.

## Extension points for Astro child themes

A child theme overrides any file under `src/` by creating the same path in
its own `src/` — the parent's pages then use the child's version:

- `src/layouts/Layout.astro` — the whole shell,
- `src/components/SidebarLinks.astro` — app-specific sidebar entries (empty here),
- `src/assets/logo.svg` — the logo,
- `src/styles/global.css` — the Tailwind root (import `src/styles/theme.css`
  and override tokens after it).

`@parent/...` imports the parent's original file (e.g. to wrap it).

## Development

```bash
bun install
bun run dev     # astro dev
bun run build   # astro check + build → dist/ (what the crate embeds)
```

The pages are typed against the framework's base translations
(`../framework/locales`).
