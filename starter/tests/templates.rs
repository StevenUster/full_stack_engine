//! Fails loudly if any embedded template is broken, instead of only
//! surfacing as a request-time 500 (the framework's boot-time loader logs
//! and skips broken templates so one bad page doesn't take the app down).

#[test]
fn all_templates_parse() {
    full_stack_engine::testing::load_templates(&starter::DIST_DIR).expect("broken template(s) found");
}
