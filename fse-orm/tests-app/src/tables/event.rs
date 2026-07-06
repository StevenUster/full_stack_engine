use fse_orm::Table;

#[derive(Table, Debug, Clone, PartialEq)]
pub struct Event {
    pub id: i64,
    pub name: String,
}
