//! `cargo run --bin dev` — the theme dev server and the backend together.
//! The runner itself lives in the framework; this only names the binary.
fn main() -> std::io::Result<()> {
    full_stack_engine::dev::run(env!("CARGO_PKG_NAME"))
}
