use fse_orm::Table;

/// Column (and struct) names that are SQL keywords — everything the ORM
/// generates quotes its identifiers, so these must work end to end through
/// DDL, the checked macros and the dynamic builder alike.
#[derive(Table, Debug, Clone, PartialEq)]
pub struct SortItem {
    pub id: i64,
    pub order: i64,
    pub group: String,
}
