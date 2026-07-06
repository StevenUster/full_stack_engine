//! The `fse` binary. `fse init` (introspect an existing database into table
//! structs + snapshot) lands in build-order step 5.

use fse_cli::migrate::{self, MigrateOpts};

const HELP: &str = "\
fse — schema-driven sqlx migrations

USAGE:
    fse migrate [--dry-run] [--yes] [--no-prepare]

Diffs the #[derive(Table)] structs in src/tables against the committed
snapshot (.fse/schema.json), writes a plain sqlx migration, and applies
everything pending to the database from DATABASE_URL (env or .env).

OPTIONS:
    --dry-run     print the pending schema change, write nothing
    --yes, -y     skip confirmation prompts
    --no-prepare  skip `cargo sqlx prepare` after applying

Configuration (all optional) lives in fse.toml under [orm]:
tables_dir, migrations_dir, snapshot_path, database_url_env and
[orm.required_columns] for framework-required table contracts.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);

    match args.first().map(String::as_str) {
        Some("migrate") => {
            let opts = MigrateOpts {
                dry_run: flag("--dry-run"),
                assume_yes: flag("--yes") || flag("-y"),
                no_prepare: flag("--no-prepare"),
                database_url: None,
            };
            let root = std::env::current_dir().expect("current dir");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            if let Err(e) = runtime.block_on(migrate::run(&root, &opts)) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        None | Some("help") | Some("--help") | Some("-h") => print!("{HELP}"),
        Some(other) => {
            eprintln!("unknown command `{other}`\n\n{HELP}");
            std::process::exit(2);
        }
    }
}
