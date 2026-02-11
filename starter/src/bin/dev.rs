use std::env;
use std::io;
use std::process::{Command, Stdio};

fn main() -> io::Result<()> {
    dotenv::dotenv().ok();

    let env_mode = env::var("ENV").unwrap_or_else(|_| "prod".to_string());
    if env_mode != "dev" {
        eprintln!("⚠️  ENV is not set to 'dev'. Please set ENV=dev in your .env file.");
        std::process::exit(1);
    }

    println!("🚀 Starting development servers...");

    println!("📦 Starting Astro dev server on port 4321...");
    let mut astro_child = Command::new("bun")
        .args(["dev"])
        .current_dir("src/frontend")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    std::thread::sleep(std::time::Duration::from_secs(2));

    println!("🦀 Starting Rust backend with hot reload on port 8080...");
    let mut cargo_child = Command::new("cargo")
        .args(["watch", "-x", "run --bin starter"])
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
