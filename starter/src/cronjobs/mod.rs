use crate::tokio_cron_scheduler::JobScheduler;
use sqlx::{Pool, Sqlite};
use std::error::Error;

pub async fn add_cronjobs(
    _sched: JobScheduler,
    _db_pool: Pool<Sqlite>,
) -> Result<(), Box<dyn Error>> {
    // Add your cronjobs here. Each run writes logs/<job>_<datetime>.log;
    // `add_job`/`add_async_job` keep the most recent 30 runs. To choose the
    // history size (or opt out of rotation), use the `_with_rotation` variants:
    //
    //   use full_stack_engine::cron::{add_async_job_with_rotation, LogRotation};
    //   add_async_job_with_rotation(
    //       &_sched,
    //       "cleanup",
    //       "0 0 3 * * *",               // every day at 03:00
    //       LogRotation::KeepLast(7),    // or LogRotation::Unlimited
    //       move || async { Ok(()) },
    //   )
    //   .await?;

    Ok(())
}
