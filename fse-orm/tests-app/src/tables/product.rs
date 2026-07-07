use chrono::NaiveDateTime;
use fse_orm::{DbEnum, Table};
use serde::{Deserialize, Serialize};

use crate::tables::event::Event;

#[derive(DbEnum, Debug, Clone, Copy, PartialEq)]
pub enum ProductStatus {
    Draft,
    Published,
    Archived,
}

/// An arbitrary serde type stored as TEXT via `#[orm(json)]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dimensions {
    pub width_cm: f64,
    pub height_cm: f64,
}

#[derive(Table, Debug, Clone)]
pub struct Product {
    pub id: i64,
    #[orm(unique)]
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    #[orm(default = 0.0)]
    pub price: f64,
    #[orm(default = "draft")]
    pub status: ProductStatus,
    #[orm(default = true)]
    pub active: bool,
    #[orm(references(Event, on_delete = cascade))]
    pub event_id: i64,
    /// A relation field: not a column, populated by an INNER JOIN (the FK is
    /// NOT NULL) when a query asks for it via `include: [event]`.
    #[orm(relation = event_id)]
    pub event: Option<Event>,
    #[orm(default = now)]
    pub created_at: NaiveDateTime,
    #[orm(json)]
    pub dimensions: Option<Dimensions>,
}
