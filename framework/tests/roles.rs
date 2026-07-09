//! `define_roles!` permission semantics: a permission check must only pass
//! for permissions a role actually declares (or an admin role). No implicit
//! grants — in particular the empty string must never act as a wildcard that
//! every role holds.

use full_stack_engine::define_roles;
use full_stack_engine::prelude::Role;

define_roles! {
    (Admin,   "admin",   ["all"]),
    (Manager, "manager", ["users.read", "users.write"]),
    (User,    "user",    []),
    (None,    "none",    ["none"]),
}

#[test]
fn declared_permissions_gate_correctly() {
    assert!(AppRole::Admin.has_permission("anything"));
    assert!(AppRole::Manager.has_permission("users.read"));
    assert!(!AppRole::Manager.has_permission("products.write"));
    assert!(!AppRole::User.has_permission("users.read"));
}

#[test]
fn empty_permission_string_is_not_a_wildcard() {
    // A `require_permission("")` call (e.g. from a bug or an empty config
    // value) must not silently grant access to every logged-in role.
    assert!(!AppRole::User.has_permission(""));
    assert!(!AppRole::Manager.has_permission(""));
    assert!(!AppRole::None.has_permission(""));
    // Admin passes everything by definition.
    assert!(AppRole::Admin.has_permission(""));
}

#[test]
fn unknown_role_strings_map_to_the_none_role() {
    assert!(AppRole::from_role_str("nonsense").is_none());
    assert!(AppRole::from_role_str("ADMIN").is_admin());
}
