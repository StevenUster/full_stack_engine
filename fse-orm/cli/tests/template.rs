//! The agent guide an `fse new` app gets must be the starter's guide: the CLI
//! has to embed its own copy (a published crate can't reach outside its
//! directory), so this test keeps the two from drifting. Skipped when the
//! starter isn't next to the CLI (e.g. building from crates.io).

#[test]
fn template_agents_md_matches_the_starter() {
    let starter = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../starter/AGENTS.md");
    let Ok(guide) = std::fs::read_to_string(&starter) else {
        return;
    };
    assert!(
        guide == include_str!("../template/AGENTS.md"),
        "cli/template/AGENTS.md differs from starter/AGENTS.md — copy the starter's over"
    );
}
