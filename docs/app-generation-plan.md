# Plan: Struct-Defined Apps (Frappe-style generation)

## Context

Today a dev writes ~30 lines of table struct and then hand-writes ~350 lines of service
handlers plus several hundred lines of Astro pages per resource. The goal is to invert
this: the structs in (renamed) `src/models/` define the whole app — DB, endpoints, and
frontend are generated from their metadata at runtime. `services/` and `frontend/` become
override/extension layers only. On top of that: full locale support with three switching
modes, themeable frontend, and reusable app modules (think Frappe/ERPNext).

## Decisions made (with Steven, 2026-07-19)

| Topic | Decision |
|---|---|
| Generation mechanism | **Runtime metadata-driven** — derive macro registers `TableDef` + UI config in a runtime registry; framework ships generic handlers + generic templates. No generated files in the app. |
| App/UI config | **Separate `#[ui]`/`#[model]` attributes** on the same struct; `#[orm]` stays pure DB. |
| Folder rename | `src/tables/` → **`src/models/`** |
| Auth flows | Move into the framework as a **built-in overridable module** (first consumer of the module system). |
| Frontend build | **Source-merged layered build**: app > theme > modules > framework defaults, resolved by the fse-ssr integration into one Astro build. |
| Themes | **npm package or local folder** of Astro sources; default theme published as `fse-theme-default`. |
| Modules | **Single cargo crate**: Rust compiles in normally; frontend sources + locales + schema snapshot embedded via `include_dir`, extracted by the fse CLI before the frontend build. |
| Path locales | **Default language unprefixed** (`/products`, `/de/products`). |
| Locale switching | Dev must pick exactly one mode: `Hardcoded(lang)`, `Domain(map)`, or `Path { default }`. |

## Target end state (starter)

```
starter/
  src/
    models/            # THE APP: user.rs, product.rs, order.rs (~40 lines each)
    services/          # only override examples (e.g. custom order-placement flow)
    frontend/          # only: astro config, global.css tweak, 1-2 page overrides
    lib.rs             # roles, locale mode, modules, builder config
  locales/en.json, de.json   # app-specific keys only; framework provides CRUD chrome
```

A model looks like (one annotation defines DB + endpoints + UI; `#[model]` is
an attribute macro that expands to `#[derive(Table, Debug, Clone)]` plus the
registry submission, so no separate ORM derive is written):

```rust
#[model(public_read = slug, api)]
pub struct Product {
    pub id: i64,
    #[ui(list, search)]
    pub name: String,
    #[orm(unique)] #[ui(list)]
    pub slug: String,
    #[ui(textarea)]
    pub description: Option<String>,
    #[orm(default = 0.0)] #[ui(list)]
    pub price: f64,
    #[orm(default = "draft")] #[ui(list, filter)]
    pub status: ProductStatus,
    #[orm(default = now)] #[ui(list)]
    pub created_at: NaiveDateTime,
}
```

Conventions (overridable via attrs): permissions `{table}.read`/`{table}.write`; admin UI at
`/{table}` (list/create/detail/delete); `public_read = <unique col>` adds public catalog
pages; `api` adds `/api/{table}` JSON endpoints; labels come from locale keys
`models.{table}.fields.{col}` with humanized-name fallback.

## Architecture

### 1. Runtime registry + `#[model]` — DONE (2026-07-19)

- New proc-macro crate `framework/macros` (`full_stack_engine_macros`).
- `#[model(...)]` is an **attribute macro** (not a derive): it parses the struct with
  the existing `fse_schema::parse` code (same source of truth as the ORM derive — no
  drift), validates its own args + field `#[ui]` attrs at compile time, and emits the
  struct with `#[derive(Table, Debug, Clone)]` attached (skipping ones already present,
  stripping `#[ui]`) plus:
  - a `ModelRegistration` (TableDef as JSON + const-constructed `UiModel`),
  - submitted via `inventory` so the framework discovers all models (including ones
    from module crates) at boot.
- `fse_schema::parse::parse_sources` treats `#[model]` structs as tables, so `fse
  migrate`/tests-app-style build scripts see them without a textual `derive(Table)`.
- framework/build.rs builds a scratch sqlite DB from the `#[model]` structs in
  `tests/` (tests-app pattern) so the emitted Table codegen is compile-checked.
- Phase 2 adds the `ModelResource` trait with **typed** ORM calls generated per table
  (list via `SelectBuilder`, `fetch`, `delete`, and macro-generated
  `insert_from_form`/`update_from_form` that validate + coerce form fields per column —
  keeps sqlx compile-time checking, no stringly runtime SQL).

### 2. Generic CRUD backend (framework) — DONE (2026-07-19)

- `#[model]` also emits a typed `ModelResource` impl per struct (framework/macros/src/
  resource.rs): checked `insert!`/`update!` for writes (all form fields parsed into the
  column's Rust type first via `models::form` helpers, every error collected, unique
  columns pre-checked → `not_unique` field errors), `Col`-token dynamic builder for the
  runtime-shaped list query (search OR over `#[ui(search)]` cols, enum/bool filters,
  sort by real column names, default `created_at DESC`). Rows cross as JSON with enums
  as their stored strings.
- `FrameworkApp::models::<AppRole>()` mounts per model (framework/src/models/routes.rs):
  - admin UI at `base_path()` = explicit `path` verbatim, else **`/admin/{table}`**
    (so `public_read` pages own bare `/{table}`): list, create GET/POST,
    `{id}` GET/POST/DELETE — read pages need `<base>.read`, mutations `<base>.write`.
  - `public_read`: `/{table}` + `/{table}/{key}` (no auth).
  - `api`: `/api/{table}` + detail — public when `public_read`, else JWT + read perm.
- **Override story (backend):** app `configure` routes register first; actix matches in
  registration order, so a same-path app route shadows the generated one (tested). Plus
  `no_create/no_edit/no_delete/disabled` attrs.
- **Override story (templates):** handlers prefer a model-specific template
  (`admin/posts`, `admin/posts/form`, `posts`, `posts/detail`) over the generic
  `_model/list|form|public-list|public-detail` (tested) — one page overridable with
  zero Rust. NOTE: `render_template`'s old `_`→`/` name rewriting was removed
  (breaking, 5.0): names are used verbatim now.

### 3. Generic frontend + theming (fse-ssr) — CORE DONE (2026-07-19)

- New npm package `fse-theme-default/` (repo root): Layout, the starter's design system
  (styles/global.css with `@source` so Tailwind scans the package), `types.ts` (context
  shapes), and the metadata-driven generic pages `pages/fse/{list,form,public-list,
  public-detail}.astro`. Namespace is `fse/*` NOT `_model/*` — Astro treats
  underscore-prefixed page paths as private, which would make app-side overrides
  impossible ("fse" is a reserved path segment, documented).
- fse-ssr `0.2`: `fseSsr({ theme: "fse-theme-default" | "./local-dir" })` — theme pages
  the app doesn't define are added via `injectRoute` (same-path app file wins), theme
  files importable as `@theme/...`, and the integration pins `fse-ssr/ssr`/`client` to
  absolute paths so sources outside the app tree resolve.
- Verified: fixture app at `fse-ssr/fixtures/theme-app` builds (3 injected + 1 app
  override), and all four built templates PARSE AND RENDER through real Tera with
  handler-shaped contexts (scratch check; permanent coverage lands with the starter
  rewrite). Key proxy features used: computed keys (`row[col.name]`), `??` →
  `default(value=...)`, boolean attrs.
- Context contract hardening in routes.rs (Tera errors on missing keys): `filters` map
  is total over filter columns, `search`/`sort` are `""` not null, create forms get a
  `default_row` (declared column defaults pre-filled), `form_values` re-render rows are
  total over form columns.
- Still open in phase 3: porting the starter's full component set (Sidebar/nav etc.)
  into the theme, `@theme`-based module-layer resolution, dev-server template fallback.

### 4. Locales — DONE (2026-07-19)

- Framework ships base translations for generated UI chrome (`framework/locales/en.json`
  + `de.json`: `model_ui.*`, `form_errors.*`), embedded via `include_str!`. Layering:
  framework < app files (deep merge, app wins — `i18n::build_locales`), then every
  non-default language is resolved over the default (`resolve_locales`) so partial
  translations fall back per key.
- `FrameworkApp::locales(&LOCALES_DIR, LocaleSelector::…)` — exactly one of
  `Hardcoded(lang)`, `Domain { map, default }` (Host header), `Path { default }`
  (default unprefixed; `/en/...` is a 404 by design). Path mode strips the prefix
  before routing via `i18n::apply_request_locale` (rewrites BOTH `head.uri` and
  `match_info` — actix routing matches the latter, same trick as NormalizePath).
- Every `render_tpl` automatically injects `t` (request language), `lang`,
  `lang_prefix` ("/de" in path mode, else "") and `i18n` — apps no longer call
  `inject_locale_context` themselves (old fns kept for compat). App injector runs
  after and can override.
- Generated redirects and all theme links carry `{lang_prefix}`; `lang_prefix` is in
  fse-ssr's `GlobalSsrContext`.
- Tested: framework/tests/locales_http.rs drives all three modes over HTTP, layering
  and per-key fallback included. Email language (server-side `load_locale` by request
  lang) still TODO for the auth-module phase.

### 5. Modules

- `ModuleDef` (name, `routes: fn(&mut ServiceConfig)`, cronjobs, `locales: &'static Dir`,
  `frontend: &'static Dir`, `schema_json: &'static str`, contributed permissions) +
  `FrameworkApp::module(...)`. Model structs in the module crate register themselves via
  the same derive/inventory path.
- Frontend/locales: `fse sync` (run automatically by `fse dev`/build) extracts embedded
  module sources to `.fse/modules/<name>/` where the layered build picks them up.
- Migrations: module author ships their `schema.json` snapshot in the crate; the app's
  `fse migrate` merges app-parsed tables + module snapshots (modules discovered via
  `cargo metadata`), so migrations stay app-local and ordered.
- **Auth becomes the first module**: login/register/email-verify/forgot/reset/settings/
  admin-users (~1,200 starter lines) move into a framework-internal auth module with a
  default `User` model; apps can extend/replace it (existing `[orm.required_columns]`
  contract stays the enforcement mechanism).

## Phases

1. **Registry + derive** — `framework/macros`, `ModelResource`, `UiMeta`, inventory
   registration; unit tests against tests-app-style fixtures.
2. **Generic backend CRUD** — handlers, route mounting with app-first shadowing,
   permission conventions, template-name fallback logic. Starter keeps its old code;
   prove generated routes match hand-written behavior via the existing
   `starter/tests/products.rs` suite pointed at generated endpoints.
3. **Default theme + generic pages** — extract `fse-theme-default`, build `_crud/*`
   metadata-driven pages, implement layered resolution in fse-ssr (app + theme layers).
4. **Locales** — framework base translations, deep-merge layering, `LocaleConfig` with the
   three modes, path middleware + Host matching, lang-aware URLs.
5. **Modules** — `ModuleDef`, `fse sync`, schema.json merge in `fse migrate`.
6. **Auth module** — port auth flows into the framework module; default User model.
7. **Starter rewrite** — delete generated-equivalent code; keep only override examples
   (custom order flow, one page override, one wrapped handler); rename `tables/`→`models/`;
   update fse.toml (`tables_dir = "src/models"` default), CLAUDE.md, README, tests.
8. **Release** — framework 5.0, fse-orm minor (schema crate additions), fse-ssr major,
   fse-cli, `fse-theme-default` 1.0; migration notes for existing apps (RFJ).

## Known risks / watch items

- Tera has no grouping parens (fse-ssr inverts conditions) — generic `_crud` templates
  must keep conditions simple.
- Astro `injectRoute` vs app-page conflicts: integration must skip injecting when the app
  layer has the same route.
- `inventory` registration across crates requires the module crate to be linked — the
  `.module(...)` call guarantees that.
- Dynamic form → typed insert boundary: generated `insert_from_form` must handle enum
  parsing, Option, defaults, and reject unknown fields.
- Actix registration-order shadowing must be verified with a test (app route vs generic
  route on the same path).

## Verification

- Each phase: `cargo test` across the workspace (framework, fse-orm, starter) + fse-ssr
  build of the starter frontend (`bun run build` catches template/type errors).
- End state: starter integration tests (products/orders/auth flows) pass against fully
  generated endpoints; `fse migrate --dry-run` clean; run the starter and click through
  list/create/edit/delete, language switching in all three modes, and a page override.
