//! `fse new <name>`: scaffold a ready-to-run full_stack_engine app.
//!
//! The template (`cli/template/`) is embedded in the binary and is a *blank*
//! app — roles, the builder, the auth `users` table, one example model, a
//! test driving the real stack — not a showcase an agent would first have to
//! delete. Placeholders: `__NAME__` (package/binary), `__CRATE__` (Rust
//! crate name) and `__DEPS__` (framework dependencies, registry or path).

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};

/// The framework release this CLI scaffolds against.
pub const FRAMEWORK_VERSION: &str = "10.0.0";
pub const ORM_VERSION: &str = "0.4.0";
pub const THEME_VERSION: &str = "0.3.0";

/// `(path in the new app, template contents)`.
const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../template/Cargo.toml.tpl")),
    ("fse.toml", include_str!("../template/fse.toml")),
    (".gitignore", include_str!("../template/gitignore")),
    (".example.env", include_str!("../template/example.env")),
    ("Dockerfile", include_str!("../template/Dockerfile.tpl")),
    ("AGENTS.md", include_str!("../template/AGENTS.md")),
    ("CLAUDE.md", include_str!("../template/CLAUDE.md")),
    ("src/main.rs", include_str!("../template/src/main.rs.tpl")),
    ("src/lib.rs", include_str!("../template/src/lib.rs")),
    (
        "src/models/mod.rs",
        include_str!("../template/src/models/mod.rs"),
    ),
    (
        "src/models/note.rs",
        include_str!("../template/src/models/note.rs"),
    ),
    (
        "src/models/user.rs",
        include_str!("../template/src/models/user.rs"),
    ),
    (
        "src/services/mod.rs",
        include_str!("../template/src/services/mod.rs"),
    ),
    (
        "locales/en.json",
        include_str!("../template/locales/en.json"),
    ),
    ("tests/app.rs", include_str!("../template/tests/app.rs.tpl")),
    ("migrations/.gitkeep", ""),
];

pub struct NewOpts {
    pub name: String,
    /// A local checkout of the framework repository: depend on it by path
    /// instead of the registry (to try unreleased framework changes).
    pub framework_path: Option<PathBuf>,
}

/// Creates `<cwd>/<name>` and returns its path.
///
/// # Errors
///
/// An invalid name, an existing target directory, or I/O failures.
pub fn run(cwd: &Path, opts: &NewOpts) -> Result<PathBuf> {
    let name = opts.name.as_str();
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid {
        bail!(
            "app name `{name}`: use lowercase letters, digits, `-` and `_`, starting with a letter"
        );
    }
    let dir = cwd.join(name);
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    let crate_name = name.replace('-', "_");
    let deps = dependencies(opts.framework_path.as_deref())?;

    for (path, contents) in FILES {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("cannot create {}", parent.display()))?;
        }
        let contents = contents
            .replace("__NAME__", name)
            .replace("__CRATE__", &crate_name)
            .replace("__DEPS__", &deps);
        std::fs::write(&target, contents)
            .wrap_err_with(|| format!("cannot write {}", target.display()))?;
    }

    // A ready .env with a fresh secret, so `fse migrate` and `cargo run`
    // work immediately. Never committed (.gitignore).
    let env = include_str!("../template/example.env").replace(
        "JWT_SECRET=replace-me",
        &format!("JWT_SECRET={}", random_secret()?),
    );
    std::fs::write(dir.join(".env"), env).wrap_err("cannot write .env")?;
    Ok(dir)
}

fn dependencies(framework_path: Option<&Path>) -> Result<String> {
    Ok(match framework_path {
        None => format!(
            "full_stack_engine = \"{FRAMEWORK_VERSION}\"\n\
             fse-theme-default = \"{THEME_VERSION}\"\n\
             fse-orm = \"{ORM_VERSION}\""
        ),
        Some(path) => {
            let root = path
                .canonicalize()
                .wrap_err_with(|| format!("framework path {}", path.display()))?;
            if !root.join("framework/Cargo.toml").exists() {
                bail!(
                    "{} is not a full_stack_engine checkout (no framework/Cargo.toml)",
                    root.display()
                );
            }
            let p = |sub: &str| root.join(sub).display().to_string();
            format!(
                "full_stack_engine = {{ version = \"{FRAMEWORK_VERSION}\", path = \"{}\" }}\n\
                 fse-theme-default = {{ version = \"{THEME_VERSION}\", path = \"{}\" }}\n\
                 fse-orm = {{ version = \"{ORM_VERSION}\", path = \"{}\" }}",
                p("framework"),
                p("fse-theme-default"),
                p("fse-orm/runtime")
            )
        }
    })
}

/// 32 random bytes from the OS, hex-encoded (64 chars).
fn random_secret() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .wrap_err("cannot read /dev/urandom for JWT_SECRET")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
