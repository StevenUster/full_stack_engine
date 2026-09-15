//! Fails loudly if any template of the app's theme stack (the child theme
//! over fse-theme-default) is broken, instead of only surfacing as a
//! request-time 500 (the framework's boot-time loader logs and skips broken
//! templates so one bad page doesn't take the app down).

#[test]
fn all_templates_parse() {
    full_stack_engine::testing::load_themes(starter::themes()).expect("broken template(s) found");
}

#[test]
fn the_child_theme_is_active_over_the_default_theme() {
    let stack = full_stack_engine::testing::theme_stack(starter::themes()).unwrap();
    let names: Vec<&str> = stack.chain().iter().map(|t| t.name()).collect();
    assert_eq!(names, ["starter", "fse-theme-default"]);
    // App pages come from the child; the parent stays reachable by name.
    assert_eq!(stack.template_owner("my-orders").unwrap().name(), "starter");
    assert!(
        stack
            .template_sources()
            .contains_key("@fse-theme-default/login")
    );
}
