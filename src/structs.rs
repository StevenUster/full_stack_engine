use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub password: String,
    pub role: String,
    pub created_at: NaiveDateTime,
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
