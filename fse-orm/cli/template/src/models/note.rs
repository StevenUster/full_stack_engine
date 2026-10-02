//! Example model — replace it with your own. A note belongs to the user who
//! wrote it (`owner`): users see and edit only their own notes, admins all
//! of them. Generated at `/admin/notes`.

use crate::{chrono::NaiveDateTime, model};

#[model(owner = user_id, order_by = "-created_at")]
pub struct Note {
    pub id: i64,
    #[orm(index, references(User, on_delete = cascade))]
    pub user_id: i64,
    #[ui(list, search, max = 200)]
    pub title: String,
    #[ui(textarea)]
    pub body: Option<String>,
    #[orm(default = now)]
    #[ui(list, format = datetime)]
    pub created_at: NaiveDateTime,
}
