//! Expansion of `themes!()`: every theme in the app's `themes/` folder,
//! embedded, plus the active one from `fse.toml`.
//!
//! One subfolder is one theme. What gets embedded is the theme's *built*
//! output, so the rule per folder is:
//!
//! - `dist/theme.json` exists → `dist/` (an Astro or other npm-built theme),
//! - a `package.json` but no built `dist/` → an *unbuilt* stub named by the
//!   source `theme.json`: the app still compiles — `cargo run --bin dev`
//!   lives in the same crate and is what builds it — and boot fails with a
//!   "run the build" message only if the active theme needs it,
//! - a `theme.json` at the folder root → the folder itself (a hand-written
//!   theme with nothing to build),
//! - none of these → compile error.
//!
//! Folders are installed in name order; which one is active never depends
//! on that order (see `ThemeStack::resolve`).

use std::path::{Path, PathBuf};

use proc_macro2::{Span, TokenStream};
use quote::quote;

/// `[themes]` in fse.toml.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Config {
    /// The themes folder, relative to the crate root.
    pub dir: String,
    /// The theme to activate, by `theme.json` name.
    pub active: Option<String>,
    /// Where an npm theme's dev server listens (`cargo run --bin dev` starts
    /// the active theme's).
    pub dev_server: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dir: "themes".to_string(),
            active: None,
            dev_server: "http://localhost:4321".to_string(),
        }
    }
}

/// One folder of `themes/`, as the macro will install it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LocalTheme {
    /// Embed this directory. `npm`: built by npm, so it has a dev server in
    /// `ENV=dev`.
    Built { embed: PathBuf, npm: bool },
    /// An npm theme without build output; `manifest` is its source
    /// `theme.json`, `location` the folder as shown to the user.
    Unbuilt { manifest: PathBuf, location: String },
}

pub(crate) fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    if !input.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "themes!() takes no arguments — configure it under [themes] in fse.toml",
        ));
    }
    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .map_err(|_| error("CARGO_MANIFEST_DIR is not set — themes!() runs under cargo"))?,
    );
    let toml_path = manifest.join("fse.toml");
    let raw = match std::fs::read_to_string(&toml_path) {
        Ok(raw) => Some(raw),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(error(&format!("reading {}: {err}", toml_path.display()))),
    };
    let config = match &raw {
        Some(raw) => parse_config(raw).map_err(|msg| error(&format!("fse.toml: {msg}")))?,
        None => Config::default(),
    };
    let themes = discover(&manifest.join(&config.dir), &config.dir).map_err(|msg| error(&msg))?;

    // Recompile when fse.toml changes (e.g. a different active theme): a
    // proc macro's own file reads aren't tracked by cargo, `include_bytes!`
    // is.
    let track = raw.is_some().then(|| {
        let path = path_literal(&toml_path);
        quote! { const _: &[u8] = include_bytes!(#path); }
    });
    let dev_server = &config.dev_server;
    let installs = themes.iter().map(|theme| match theme {
        LocalTheme::Built { embed, npm } => {
            let path = path_literal(embed);
            let dev = npm.then(|| quote! { .dev_server(#dev_server) });
            quote! {
                .with({
                    static DIR: include_dir::Dir<'static> = include_dir::include_dir!(#path);
                    ::full_stack_engine::themes::Theme::embedded(&DIR) #dev
                })
            }
        }
        // `include_bytes!` makes the manifest a tracked file: the dev runner
        // touches it after building, so the next build embeds `dist/`.
        LocalTheme::Unbuilt { manifest, location } => {
            let path = path_literal(manifest);
            quote! {
                .with(::full_stack_engine::themes::Theme::unbuilt(include_bytes!(#path), #location))
            }
        }
    });
    let active = config.active.as_ref().map(|name| quote! { .active(#name) });

    Ok(quote! {
        {
            use ::full_stack_engine::prelude::include_dir;
            #track
            ::full_stack_engine::themes::ThemeSet::new()
                #(#installs)*
                #active
        }
    })
}

fn error(msg: &str) -> syn::Error {
    syn::Error::new(Span::call_site(), msg)
}

fn path_literal(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Reads `[themes]`; every key is optional, unknown keys are errors (a typo
/// like `activ` would otherwise silently do nothing).
pub(crate) fn parse_config(raw: &str) -> Result<Config, String> {
    let value: toml::Value = raw.parse().map_err(|e| format!("not valid TOML: {e}"))?;
    let mut config = Config::default();
    let Some(section) = value.get("themes") else {
        return Ok(config);
    };
    let table = section
        .as_table()
        .ok_or("[themes] must be a table".to_string())?;
    for (key, value) in table {
        let text = value
            .as_str()
            .ok_or_else(|| format!("themes.{key} must be a string"))?
            .trim()
            .to_string();
        match key.as_str() {
            "dir" => config.dir = text,
            "active" => config.active = (!text.is_empty()).then_some(text),
            "dev_server" => config.dev_server = text.trim_end_matches('/').to_string(),
            other => {
                return Err(format!(
                    "unknown key themes.{other} (known: dir, active, dev_server)"
                ));
            }
        }
    }
    Ok(config)
}

/// The themes of `dir`, in folder-name order. A missing folder is no error:
/// the app then only has the themes it installs itself.
pub(crate) fn discover(dir: &Path, shown: &str) -> Result<Vec<LocalTheme>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("reading {}: {err}", dir.display())),
    };
    let mut folders: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    folders.sort();

    let mut themes = Vec::new();
    for (name, path) in folders {
        let dist = path.join("dist");
        let manifest = path.join("theme.json");
        if dist.join("theme.json").is_file() {
            themes.push(LocalTheme::Built {
                embed: dist,
                npm: path.join("package.json").is_file(),
            });
        } else if path.join("package.json").is_file() && manifest.is_file() {
            check_manifest(&manifest, &format!("{shown}/{name}"))?;
            themes.push(LocalTheme::Unbuilt {
                manifest,
                location: format!("{shown}/{name}"),
            });
        } else if manifest.is_file() {
            themes.push(LocalTheme::Built {
                embed: path,
                npm: false,
            });
        } else {
            return Err(format!(
                "{shown}/{name} is not a theme: it needs a theme.json at its root (next to the \
                 package.json of a built theme)"
            ));
        }
    }
    Ok(themes)
}

/// What `Theme::unbuilt` will parse at runtime must parse now.
fn check_manifest(path: &Path, location: &str) -> Result<(), String> {
    let raw = std::fs::read(path).map_err(|e| format!("reading {location}/theme.json: {e}"))?;
    let value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| format!("{location}/theme.json is not valid JSON: {e}"))?;
    if value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .is_none()
    {
        return Err(format!("{location}/theme.json has no \"name\""));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_overrides() {
        assert_eq!(parse_config("[orm]\n").unwrap(), Config::default());
        let config = parse_config(
            "[themes]\nactive = \"dark\"\ndir = \"looks\"\ndev_server = \"http://localhost:5000/\"\n",
        )
        .unwrap();
        assert_eq!(config.active.as_deref(), Some("dark"));
        assert_eq!(config.dir, "looks");
        assert_eq!(config.dev_server, "http://localhost:5000");
        assert_eq!(
            parse_config("[themes]\nactive = \"\"\n").unwrap().active,
            None
        );
        assert!(
            parse_config("[themes]\nactiv = \"x\"\n")
                .unwrap_err()
                .contains("activ")
        );
        assert!(parse_config("[themes]\nactive = 1\n").is_err());
    }

    #[test]
    fn folders_are_built_npm_themes_or_hand_written_ones() {
        let root = std::env::temp_dir().join(format!("fse-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |rel: &str| {
            let file = root.join(rel);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "{}").unwrap();
        };
        assert_eq!(discover(&root, "themes").unwrap(), []);

        write("b-astro/package.json");
        write("b-astro/dist/theme.json");
        write("a-plain/theme.json");
        write(".hidden/whatever");
        write("c-unbuilt/package.json");
        std::fs::write(root.join("c-unbuilt/theme.json"), r#"{ "name": "c" }"#).unwrap();
        let found = discover(&root, "themes").unwrap();
        assert_eq!(
            found,
            [
                LocalTheme::Built {
                    embed: root.join("a-plain"),
                    npm: false
                },
                LocalTheme::Built {
                    embed: root.join("b-astro/dist"),
                    npm: true
                },
                LocalTheme::Unbuilt {
                    manifest: root.join("c-unbuilt/theme.json"),
                    location: "themes/c-unbuilt".to_string(),
                },
            ]
        );

        // An unbuilt theme's manifest is checked now, not at boot.
        write("c-unbuilt/theme.json");
        let err = discover(&root, "themes").unwrap_err();
        assert!(err.contains("has no \"name\""), "{err}");
        std::fs::remove_dir_all(root.join("c-unbuilt")).unwrap();

        std::fs::create_dir_all(root.join("d-empty")).unwrap();
        assert!(
            discover(&root, "themes")
                .unwrap_err()
                .contains("not a theme")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
