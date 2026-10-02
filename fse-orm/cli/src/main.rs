//! The `fse` binary: scaffold an app (`fse new`), turn model changes into
//! migrations (`fse migrate`), and list the app's routes (`fse routes`).

use color_eyre::eyre::{Result, WrapErr};
use fse_cli::config;
use fse_cli::migrate::{self, MigrateOpts};
use fse_cli::modules;
use fse_cli::new::{self, NewOpts};
use fse_cli::prepare;

const HELP: &str = "\
fse — the full_stack_engine tool

USAGE:
    fse new <name> [--framework-path <repo>]
    fse migrate [--dry-run] [--yes] [--no-prepare]
    fse prepare
    fse routes
    fse sync

`fse new <name>` creates a ready-to-run app in ./<name> (a .env with a
fresh secret included): then `cd <name> && fse migrate && cargo test`.
--framework-path depends on a local framework checkout instead of the
released crates.

`fse routes` prints every generated route with its required permission,
from the app's own binary (`cargo run -- --routes`), so it always matches
the framework version the app uses.

`fse migrate` is the one command for everything: it diffs the
#[model]/#[derive(Table)] structs in src/models against the committed snapshot
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

/// `cargo run -q -- <flag>` in the app: a flag the framework handles itself
/// before booting (`--routes`).
fn run_app(root: &std::path::Path, flag: &str) -> Result<()> {
    let status = std::process::Command::new("cargo")
        .args(["run", "--quiet", "--", flag])
        .current_dir(root)
        .status()
        .wrap_err("cannot run cargo")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

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
        Some("new") => {
            let Some(name) = args.get(1).filter(|a| !a.starts_with('-')) else {
                eprintln!("usage: fse new <name> [--framework-path <repo>]");
                std::process::exit(2);
            };
            let framework_path = args
                .iter()
                .position(|a| a == "--framework-path")
                .and_then(|i| args.get(i + 1))
                .map(std::path::PathBuf::from);
            let dir = new::run(
                &root,
                &NewOpts {
                    name: name.clone(),
                    framework_path,
                },
            )?;
            println!(
                "Created {}\n\nNext:\n    cd {name}\n    fse migrate      # create the database + .sqlx cache\n    \
                 cargo test       # the example model's tests\n    cargo run        # http://localhost:8080\n\n\
                 Agents: start with AGENTS.md.",
                dir.display()
            );
        }
        Some("routes") => run_app(&root, "--routes")?,
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
