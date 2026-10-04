//! Fails loudly if any template of the app's themes is broken, instead of
//! only surfacing as a request-time 500 (the framework's boot-time loader
//! logs and skips broken templates so one bad page doesn't take the app
//! down).

#[test]
fn all_templates_parse() {
    full_stack_engine::testing::load_themes(starter::themes()).expect("broken template(s) found");
}

/// Every theme of `themes/` is embedded and can be switched to with
/// `[themes] active` or `THEME` — so each must load as the active one, not
/// only the one active today.
#[test]
fn every_installed_theme_loads_as_the_active_one() {
    let themes = starter::themes();
    for theme in themes.installed() {
        let name = theme.name();
        full_stack_engine::testing::load_themes(themes.clone().active(name))
            .unwrap_or_else(|err| panic!("theme {name}: {err}"));
    }
}

#[test]
fn the_configured_theme_is_active_over_the_default_theme() {
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
