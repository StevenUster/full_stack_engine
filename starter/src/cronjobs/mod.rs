use crate::tokio_cron_scheduler::JobScheduler;
use sqlx::{Pool, Sqlite};
use std::error::Error;

pub async fn add_cronjobs(
    _sched: JobScheduler,
    _db_pool: Pool<Sqlite>,
) -> Result<(), Box<dyn Error>> {
    // Add your cronjobs here

    Ok(())
}
