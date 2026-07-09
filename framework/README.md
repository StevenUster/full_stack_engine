<div align="center">
  <h1>🛑 UNDER DEVELOPMENT 🛑</h1>
  <p><strong>USE IT AT YOUR OWN RISK</strong></p>
  <img src="https://img.shields.io/badge/STATUS-UNDER_DEVELOPMENT-red?style=for-the-badge&logo=rust" alt="Development Status">
</div>

<br/>

# My Rust Framework

A lightweight, opinionated Rust web framework built on top of Actix-web, SQLx, and Tera.

## Repository Structure

This repository contains two separate Cargo projects:

- **[`/framework`](/framework)**: The core framework code.
- **[`/starter`](/starter)**: A complete template application. Use this to start your own project!

## Features

- **Integrated Auth**: Built-in JWT and Argon2 password hashing. Is also pre-configured in the starter app.
- **Template Engine**: Server-side rendering via Tera, authored as plain TypeScript — the starter's fse-ssr integration compiles typed `ssr<T>()` expressions in Astro templates to Tera at build time (no template syntax in frontend code), and the server injects each page's context as JSON for client-side code. Includes an Astro dev server proxy for rapid frontend development.
- **Cron Scheduler**: Easy async job scheduling.
- **Rate Limiting**: proxy-aware, per-client-IP rate limiting via Actix-governor — a generous site-wide limiter is applied to every request automatically (DDoS guard, tunable via `GLOBAL_RATE_LIMIT_*` env vars), plus stricter presets for auth/custom endpoints.
- **Database**: [`fse-orm`](/fse-orm), a compile-time-checked ORM on top of SQLx — schema defined as plain structs, migrations generated (never hand-written), checked query macros plus a dynamic builder for runtime-shaped queries, Prisma-style relation eager-loading. See [Using the ORM](#using-the-orm).

## Design Principles

These are the core rules the framework is built around. They apply both to changes in `/framework` and to apps built on top of it.

1. **Priorities, in order: Security → Reliability → Speed → Readability.** When two goals conflict, the earlier one wins. A faster or cleaner solution never justifies a weaker security or correctness guarantee.

2. **Secure and stable by default.** Every default the framework ships must be the safest and most robust option available — never the most convenient. If a setting can be insecure, its default is the secure value and loosening it is an explicit, opt-in decision by the app. This includes: autoescaped templates, `HttpOnly` + `SameSite=Strict` + `Secure` (prod) cookies, hardened response headers (CSP, `X-Content-Type-Options`, `X-Frame-Options`), and Argon2 password hashing.

3. **Everything bundles into one executable.** The frontend (`dist/`) and locales are embedded via `include_dir!`, and migrations via `sqlx::migrate!()` (passed to `FrameworkApp::migrator`) — a built binary has no external `migrations/` or `locales/` directory to ship. The framework must not introduce runtime dependencies on external services or sidecar processes. Prefer simple, predictable behavior over configurability for its own sake.

4. **Batteries included — where it makes sense.** Common needs (auth, mail, uploads, i18n, rate limiting, cron, error pages) live in the framework so apps don't re-implement them. A helper earns its place only if most apps want it and it can carry the secure default with it; niche or opinion-heavy concerns stay in the app.

5. **Cross-cutting safety belongs in the framework, not the app.** Security and reliability guarantees that every app needs — token expiry, session/JWT invalidation, proxy-aware rate-limit keying, safe template loading that logs and skips a bad template instead of crashing at boot — should be solved once here so every downstream app inherits them.

6. **Untrusted input stays untrusted.** Parameterize all SQL, validate every upload (size + type), keep public files (`uploads/`) separate from private ones (`data/`), and treat any user-supplied HTML that reaches a renderer as hostile.

7. **Migrations are forward-only.** Schema changes are new, timestamped migrations run automatically at startup; applied migrations are never edited. Apps regenerate the SQLx offline cache (`fse prepare` — no `sqlx-cli` needed, see [Installing the CLI](#installing-the-cli)) after query changes.

## Using the ORM

The framework ships an ORM (`fse-orm`, re-exported from the prelude) built directly on `sqlx` — it generates real `sqlx::query!`-based code, so every query stays compile-time checked against your actual database schema. There is no runtime-built SQL in the primary API.

### 1. Define tables as structs

Each `#[derive(Table)]` struct in `src/tables/` **is** a table:

```rust
#[derive(Table, Debug, Clone)]
#[orm(unique(user_id, run_id))]      // composite UNIQUE INDEX
pub struct Registration {
    pub id: i64,
    #[orm(references(User, on_delete = cascade))]
    pub user_id: i64,
    #[orm(relation = user_id)]        // joinable via include:, not a DB column
    pub user: Option<User>,
    #[orm(default = false)]
    pub completed: bool,
    #[orm(default = now)]
    pub created_at: chrono::NaiveDateTime,
}
```

Common field attributes: `primary_key`, `references(Target, on_delete = cascade|set_null|restrict)`, `unique`, `index`, `text` (open-ended string enum, no CHECK), `json`, `default = ...`. Struct-level `#[orm(unique(col_a, col_b))]` / `#[orm(index(col_a, col_b))]` cover multi-column constraints — both always compile to `CREATE [UNIQUE] INDEX`, never an inline table constraint, so adding or dropping one is a plain index change, never a table rebuild.

### 2. Generate migrations — never write them by hand

```bash
fse migrate              # diff src/tables against the committed snapshot, generate + apply, refresh .sqlx/
fse migrate --dry-run     # preview the pending change without writing anything
fse migrate --no-prepare  # skip the .sqlx/ refresh step
fse prepare               # just refresh .sqlx/, e.g. after editing a query without touching the schema
```

`fse` (from `fse-cli`) parses your structs, diffs them against `.fse/schema.json`, and writes a plain timestamped `sqlx` migration, then refreshes the offline query cache (`.sqlx/`) itself — no `sqlx-cli` install required, `fse` is the only tool you need. If a schema shape isn't representable by a struct/field attribute yet, the fix is to extend the ORM — never to hand-author a migration file as a workaround.

#### Installing the CLI

```bash
cargo install fse-cli
```

This installs the `fse` binary. It's the only tool needed for schema/migration/query-cache workflows — there's nothing else to install.

### 3. Query

Compile-time checked, for the common cases:

```rust
let reg = find_one!(Registration, &db, Registration::ID.eq(id))?;
let page = find_page!(Registration, &db, Registration::COMPLETED.eq(true), page = 1, per_page = 20)?;
let reg = insert!(Registration, &db, user_id = user_id, run_id = run_id)?;
update!(Registration, &db, Registration::ID.eq(id), completed = true)?;
```

`insert!` uses the same `Table, executor, ...` shape as every other macro. Columns you leave out that are nullable or carry `#[orm(default = ...)]` are simply omitted from the `INSERT` — the column's own SQL default (or implicit `NULL`) fills them in, and the returned row reflects the real value. Omitting a required column, or assigning the auto-increment `id`, is a compile error.

A dynamic, unchecked builder (same operator names) for query shapes decided at runtime:

```rust
let ids = Registration::find().filter(Registration::RUN_ID.in_(run_ids)).fetch_all(&db).await?;
```

Relations declared with `#[orm(relation = fk_column)]` can be eager-loaded with a real SQL `JOIN`/`LEFT JOIN` — still checked:

```rust
let reg = find_one!(Registration, &db, Registration::ID.eq(id), include: [user, run])?;
```

## Getting Started

The fastest way to get started is to explore the **[Starter App](/starter)**. It comes with a preconfigured frontend (Astro), auth services, and database migrations.

### 1. Copy the Starter
Copy the `starter` folder to your own repository or work directly inside it.

### 2. Configure Environment
Navigate to the starter directory and copy the example environment file:
```bash
cd starter
cp .example.env .env
```

### 3. Run Development Mode
The starter includes a `dev` binary that launches both the Rust backend and the Astro frontend concurrently:
```bash
cargo run --bin dev
```

## License

MIT OR Apache-2.0

