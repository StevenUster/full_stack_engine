use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    User,
    None,
}

impl std::fmt::Display for UserRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserRole::Admin => write!(f, "admin"),
            UserRole::User => write!(f, "user"),
            UserRole::None => write!(f, "none"),
        }
    }
}

impl std::str::FromStr for UserRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "admin" => Ok(UserRole::Admin),
            "user" => Ok(UserRole::User),
            "none" => Ok(UserRole::None),
            _ => Err(format!("Unknown role: {}", s)),
        }
    }
}

impl From<String> for UserRole {
    fn from(s: String) -> Self {
        s.parse().unwrap_or(UserRole::None)
    }
}

impl From<&str> for UserRole {
    fn from(s: &str) -> Self {
        s.parse().unwrap_or(UserRole::None)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub password: String,
    pub role: UserRole,
    pub created_at: NaiveDateTime,
    #[serde(default = "default_true")]
    pub is_verified: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
pub struct TableHeader {
    pub label: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Serialize)]
pub struct TableAction {
    pub label: String,
    pub action: String,
    pub method: String,
}

#[derive(Serialize)]
pub struct Table<T: Serialize> {
    pub headers: Vec<TableHeader>,
    pub rows: Vec<T>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TableAction>,
}
