use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub trait Role:
    Send + Sync + Clone + std::fmt::Debug + Serialize + DeserializeOwned + 'static
{
    fn as_str(&self) -> &str;
    fn from_role_str(s: &str) -> Self;
    fn is_admin(&self) -> bool;
    fn is_none(&self) -> bool;
    fn has_permission(&self, permission: &str) -> bool;
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum DefaultRole {
    Admin,
    User,
    None,
}

impl Role for DefaultRole {
    fn as_str(&self) -> &str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
            Self::None => "none",
        }
    }

    fn from_role_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "admin" => Self::Admin,
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

    fn has_permission(&self, _permission: &str) -> bool {
        self.is_admin()
    }
}

impl std::fmt::Display for DefaultRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for DefaultRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_role_str(s))
    }
}

impl From<String> for DefaultRole {
    fn from(s: String) -> Self {
        Self::from_role_str(&s)
    }
}

impl From<&str> for DefaultRole {
    fn from(s: &str) -> Self {
        Self::from_role_str(s)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound(deserialize = "R: Role"))]
pub struct User<R: Role> {
    pub id: i64,
    pub email: String,
    pub password: String,
    pub role: R,
    pub created_at: NaiveDateTime,
    #[serde(default = "default_true")]
    pub is_verified: bool,
    pub verification_token: Option<String>,
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
