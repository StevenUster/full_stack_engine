use full_stack_engine::prelude::*;

/// Example custom role enum — extend this with your own roles.
///
/// To add a new role:
/// 1. Add a variant here
/// 2. Add its string mapping in `as_str()` and `from_role_str()`
/// 3. Define its permissions in `has_permission()`
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AppRole {
    Admin,
    Manager,
    User,
    None,
}

impl Role for AppRole {
    fn as_str(&self) -> &str {
        match self {
            Self::Admin => "admin",
            Self::Manager => "manager",
            Self::User => "user",
            Self::None => "none",
        }
    }

    fn from_role_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "admin" => Self::Admin,
            "manager" => Self::Manager,
            "user" => Self::User,
            _ => Self::None,
        }
    }

    fn is_admin(&self) -> bool {
        matches!(self, Self::Admin)
    }

    fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    fn has_permission(&self, permission: &str) -> bool {
        match self {
            Self::Admin => true,
            Self::Manager => matches!(permission, "users.read" | "users.write"),
            Self::User => false,
            Self::None => false,
        }
    }
}

impl std::fmt::Display for AppRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
