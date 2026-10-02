<div align="center">
  <h1>🛑 UNDER DEVELOPMENT 🛑</h1>
  <p><strong>USE IT AT YOUR OWN RISK</strong></p>
  <img src="https://img.shields.io/badge/STATUS-UNDER_DEVELOPMENT-red?style=for-the-badge&logo=rust" alt="Development Status">
</div>

<br/>

# full_stack_engine

A full-stack Rust web framework (actix-web, SQLite, server-rendered pages)
designed for **AI agents to build secure apps with few tokens**: an app is
mostly a set of annotated structs, and the safe thing is the default thing.

```rust
#[model(owner = author_id, public_read = slug, api, hooks)]
pub struct Article {
    pub id: i64,
    #[orm(references(User, on_delete = cascade))]
    pub author_id: i64,
    #[ui(list, search, max = 120)]
    pub title: String,
    #[orm(unique)]
    #[ui(slug_from = title)]
    pub slug: String,
    #[ui(textarea)]
    pub body: Option<String>,
    #[orm(default = "draft")]
    #[ui(list, filter)]
    pub status: Status,
}

impl ModelHooks for Article {
    fn public_scope() -> Option<Cond> {
        Some(Article::STATUS.eq(Status::Published))
    }
}
```

That is a complete feature: the table and its migrations, compile-time
checked queries, an admin UI (list, search, filter, create, edit, delete)
where authors only ever see their own articles, public pages and a JSON API
that only show published ones, validation, permissions
(`articles.read`/`articles.write`) and translations.

## Quick start

```bash
cargo install fse-cli
fse new my-app && cd my-app
fse migrate        # database + query cache
cargo test         # the example model, tested over the real stack
cargo run          # http://localhost:8080
```

Every app contains an
**[`AGENTS.md`](https://github.com/StevenUster/full_stack_engine/blob/main/starter/AGENTS.md)**
— the complete guide for agents and people: the workflow, every model option
and hook, the generated routes and page contexts, how to write the rare
custom handler, testing, and the security rules. It is the documentation to
read.

## What a model can declare

- **Pages and API** — admin CRUD, public list/detail pages (`public_read`),
  JSON API (`api`), list search, filters (enum, text, ranges), sorting,
  paging, default order.
- **Who sees which rows** — `owner = col` (users see their own), the
  `scope`/`public_scope` hooks (any rule, e.g. via a join table), row locks
  (`can_edit`, `can_delete`, `can_create`).
- **Validation** — required, email, http(s) URL, min/max, unique, slugs,
  plus `before_save` for anything cross-field.
- **Structure** — relations shown by title, foreign-key selects limited to
  what the user may read, nested models (`parent`), many-to-many links
  (`link`), row actions (`actions(...)` → buttons), lifecycle hooks.
- **Display** — locale-aware formatting (`format = date|currency|...`),
  computed fields (`decorate`), `private` columns for signed-in users only.

Cross-model mistakes (a missing parent, two routes on one URL) stop the boot
with one message listing every problem; `cargo test` catches them first.
`cargo run -- --routes` prints the app's whole generated surface.

## Secure by default

- One code path for every generated endpoint: role permission → owner /
  scope / parent → row hooks. Out-of-scope rows and denied pages are 404s.
- Secret-looking columns (`password`, `*_token`, `*_hash`, ...) never reach
  a page or an API response; anonymous pages get neither `private` columns
  nor admin metadata; foreign keys can't point at rows the user can't see.
- Parameterized SQL only (the ORM); autoescaped templates; per-request CSP
  nonce; hardened headers; `HttpOnly` + `SameSite=Strict` + `Secure` session
  cookies (the CSRF defence); Argon2; per-IP rate limits; revocable
  sessions; configuration validated at boot; secrets unloggable.
- Tests run the production stack (`testing::TestApp`), so what a test sees
  is what a user gets.

## Batteries

Auth module (login, registration, email verification, password reset,
settings, user admin, first-admin bootstrap) · i18n with three language
modes · mail with templates · cron · uploads (validated, sandboxed) · QR
codes · HTML→PDF (feature) · locale-aware formatting · OpenAPI +
`/api/docs` · CORS · health probes · structured tracing with optional
OTLP/Sentry · WordPress-style parent/child themes (Astro compiled to Tera by
`fse-ssr`, default theme included) · one self-contained binary. Deep dives:
[batteries](https://github.com/StevenUster/full_stack_engine/blob/main/docs/batteries.md),
[hardening](https://github.com/StevenUster/full_stack_engine/blob/main/docs/hardening.md),
[observability](https://github.com/StevenUster/full_stack_engine/blob/main/docs/observability.md),
[themes](https://github.com/StevenUster/full_stack_engine/blob/main/docs/themes.md).

## Repository

| path | what |
|---|---|
| `framework/` | the `full_stack_engine` crate (+ `macros/`: `#[model]`) |
| `fse-orm/` | the ORM: schema parser, query macros, runtime, the `fse` CLI (`new`, `migrate`, `prepare`, `routes`, `sync`) |
| `fse-ssr/` | Astro integration compiling pages to Tera |
| `fse-theme-default/` | the default theme (npm package + crate embedding its build) |
| `starter/` | the reference app and the canonical `AGENTS.md` |
| `docs/` | design notes and deep dives |

## Design principles

1. **Priorities, in order: Security → Reliability → Speed → Readability.**
   When two goals conflict, the earlier one wins.
2. **Secure and stable by default.** Every default is the safest option;
   loosening one is an explicit, visible decision in the app.
3. **Declare, don't hand-write.** If many apps need it, it becomes a model
   option or hook — so it is written once, tested once, and every app gets
   the security that comes with it. `services/` is for genuinely unique
   flows.
4. **Built for agents.** One guide, compile-time errors that say how to fix
   them, boot checks that list every problem, a test harness that is the
   real app, and no step that needs a human to remember a convention.
5. **Everything bundles into one executable.** Themes, locales and
   migrations are embedded; no sidecar processes.
6. **Cross-cutting safety belongs in the framework.** Token expiry, session
   revocation, rate-limit keying, escaping and access checks are solved here
   so every app inherits them.
7. **Untrusted input stays untrusted.** Parameterized SQL, validated
   uploads, public (`uploads/`) and private (`data/`) files kept apart,
   user HTML treated as hostile.
8. **Telemetry is observation, never a dependency and never a leak.**
9. **Migrations are generated and forward-only** (`fse migrate`); applied
   migrations are never edited.

## License

MIT OR Apache-2.0
