//! The `fse` binary. `fse init` (introspect an existing database into table
//! structs + snapshot) lands in build-order step 5.

use color_eyre::eyre::{Result, WrapErr};
use fse_cli::config;
use fse_cli::migrate::{self, MigrateOpts};
use fse_cli::modules;
use fse_cli::prepare;

const HELP: &str = "\
fse — schema-driven sqlx migrations, no sqlx-cli required

USAGE:
    fse migrate [--dry-run] [--yes] [--no-prepare]
    fse prepare
    fse sync

`fse migrate` is the one command for everything: it diffs the
#[derive(Table)] structs in src/tables against the committed snapshot
(.fse/schema.json), writes a plain sqlx migration, applies everything
pending to the database from DATABASE_URL (env or .env), then refreshes
the offline query cache (.sqlx/) — pass --no-prepare to skip that last
step. `fse prepare` runs just that last step on its own, e.g. after
editing a query without changing the schema. Both cover query!-family
call sites under src/ and tests/, including #[cfg(test)] code.

Modules ([orm] modules = [...] in fse.toml): their shipped schema
snapshots merge into `fse migrate` automatically; `fse sync` extracts
their frontend/ sources into .fse/modules/ for the Astro build.

OPTIONS:
    --dry-run     print the pending schema change, write nothing
    --yes, -y     skip confirmation prompts
    --no-prepare  skip refreshing the query cache after applying

Configuration (all optional) lives in fse.toml under [orm]:
tables_dir, migrations_dir, snapshot_path, database_url_env and
[orm.required_columns] for framework-required table contracts.
";

fn main() -> Result<()> {
    color_eyre::install()?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let root = std::env::current_dir().wrap_err("cannot read the current directory")?;

    match args.first().map(String::as_str) {
        Some("migrate") => {
            let opts = MigrateOpts {
                dry_run: flag("--dry-run"),
                assume_yes: flag("--yes") || flag("-y"),
                no_prepare: flag("--no-prepare"),
                database_url: None,
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .wrap_err("cannot start the tokio runtime")?;
            runtime.block_on(migrate::run(&root, &opts))?;
        }
        Some("prepare") => {
            let cfg = config::load(&root)?;
            prepare::run(&root, &cfg, None)?;
        }
        Some("sync") => {
            let cfg = config::load(&root)?;
            modules::sync(&root, &cfg)?;
        }
        None | Some("help") | Some("--help") | Some("-h") => print!("{HELP}"),
        Some(other) => {
            eprintln!("unknown command `{other}`\n\n{HELP}");
            std::process::exit(2);
        }
    }
    Ok(())
}
