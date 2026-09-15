use std::env;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

fn main() -> io::Result<()> {
    dotenv::dotenv().ok();

    let env_mode = env::var("ENV").unwrap_or_else(|_| "prod".to_string());
    if env_mode != "dev" {
        eprintln!("⚠️  ENV is not set to 'dev'. Please set ENV=dev in your .env file.");
        std::process::exit(1);
    }

    println!("🚀 Starting development servers...");

    if !Path::new("theme/node_modules").exists() {
        println!("📦 Installing theme dependencies...");
        run(Command::new("bun").arg("install").current_dir("theme"))?;
    }
    // The binary embeds theme/dist (the fallback for anything the dev server
    // doesn't serve), so it must exist before the first `cargo run`.
    if !Path::new("theme/dist").exists() {
        println!("🎨 Building the theme once...");
        run(Command::new("bun")
            .args(["run", "build"])
            .current_dir("theme"))?;
    }

    println!("🎨 Starting the theme dev server (Astro) on port 4321...");
    let mut astro_child = Command::new("bun")
        .args(["dev"])
        .current_dir("theme")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    std::thread::sleep(std::time::Duration::from_secs(2));

    println!("🦀 Starting Rust backend with hot reload on port 8080...");
    let mut cargo_child = Command::new("cargo")
        .args(["watch", "-i", "theme/", "-x", "run --bin starter"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    ctrlc::set_handler(move || {
        println!("\n🛑 Stopping development servers...");
        std::process::exit(0);
    })
    .expect("Error setting Ctrl-C handler");

    let cargo_status = cargo_child.wait()?;

    astro_child.kill()?;
    astro_child.wait()?;

    if !cargo_status.success() {
        std::process::exit(cargo_status.code().unwrap_or(1));
    }

    Ok(())
}

fn run(cmd: &mut Command) -> io::Result<()> {
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{cmd:?} failed with {status}")))
    }
}
