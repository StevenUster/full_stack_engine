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
| App/UI config | **Separate `#[ui]`/`#[crud]` attributes** on the same struct; `#[orm]` stays pure DB. |
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

A model looks like:

```rust
#[derive(Table, Crud, Debug, Clone)]
#[crud(public_read = slug, api)]              // struct-level app config
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

### 1. Runtime registry + `#[derive(Crud)]`

- New proc-macro crate `framework/macros` (published as part of full_stack_engine).
- `#[derive(Crud)]` re-parses the struct with the existing `fse_schema::parse` code
  (same source of truth as the ORM derive — no drift), parses `#[ui]`/`#[crud]` attrs
  into a new `UiMeta`, and emits:
  - a `CrudMeta` static (TableDef + UiMeta, serialized or const-constructed),
  - an impl of a new `CrudResource` trait with **typed** ORM calls generated per table
    (list via `SelectBuilder`, `fetch`, `delete`, and macro-generated
    `insert_from_form`/`update_from_form` that validate + coerce form fields per column —
    keeps sqlx compile-time checking, no stringly runtime SQL),
  - `inventory::submit!` registration so the framework discovers all models (including
    ones from module crates) at boot.

### 2. Generic CRUD backend (framework)

- At boot, `FrameworkApp` iterates the registry and mounts generic handlers:
  list (search/filter/paginate), create GET/POST, detail GET/POST, DELETE, optional
  public pages, optional JSON API (+ OpenAPI entries).
- Permission checks use the existing `AuthUser::require_permission` with the
  convention strings.
- **Override story (backend):** app routes are registered *first*; actix matches in
  registration order, so a same-path app route shadows the generic one automatically.
  Plus attrs to switch endpoints off (`#[crud(no_delete)]`, `#[crud(disabled)]`, …).
  Generic handlers are also exported as plain functions so an override can wrap/extend
  rather than reimplement.
- **Override story (templates):** generic handler renders `"{table}/index"` if that
  template exists in Tera, else falls back to the theme's generic `_crud/list` (same for
  form/detail). So dropping `pages/products/index.astro` in the app overrides just that
  page with zero Rust.

### 3. Generic frontend + theming (fse-ssr)

- New npm package `fse-theme-default`: the current starter layouts/components/global.css
  moved out, plus new metadata-driven generic pages `_crud/list.astro`, `_crud/form.astro`,
  `_crud/detail.astro` (render columns/widgets from the injected `meta` context).
- fse-ssr integration gains layered resolution: for each page path, pick
  app → theme → module → framework-default; non-app pages are added via Astro
  `injectRoute`; shared components/layouts import through an alias (`@theme/...`) that
  resolves down the layers. One `astro build`, one dist, embedded as today.
- Theme selection in the frontend config: `fseSsr({ theme: "fse-theme-default" })` or a
  local path.

### 4. Locales

- Framework ships base translations for all generated UI chrome (en + de to start);
  merge order: framework < modules < theme < app (deep merge, app wins).
- `LocaleConfig` on the builder — dev picks exactly one:
  - `Hardcoded("en")`
  - `Domain([("example.de", "de"), ("de.example.com", "de")], default)` — matched on Host
  - `Path { default: "en" }` — default lang unprefixed; middleware strips a known
    `/{lang}` prefix, stores lang in request extensions; templates get `lang_prefix`
    and a URL helper so generated links are lang-aware.
- `inject_locale_context` reads the per-request lang instead of a constant; server-side
  `load_locale` (emails) uses the request/user lang.

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

1. **Registry + derive** — `framework/macros`, `CrudResource`, `UiMeta`, inventory
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
