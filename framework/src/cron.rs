use chrono::Local;
use log::{error, info};
use std::env;
use std::fs::{OpenOptions, create_dir_all};
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

/// How many per-run log files a cron job keeps under `logs/`. Every run writes
/// `logs/<job>_<datetime>.log`; without a bound a frequent job fills the disk,
/// so registration defaults to [`LogRotation::default`] (keep the most recent
/// 30). After each run, older files for that job past the limit are pruned,
/// oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogRotation {
    /// Keep the most recent `n` run logs for the job and delete older ones.
    /// `0` is treated as `1` — a job always keeps its latest log.
    KeepLast(usize),
    /// Keep every run log forever (the original behaviour). Grows without
    /// bound, so only for jobs that run rarely.
    Unlimited,
}

impl Default for LogRotation {
    fn default() -> Self {
        LogRotation::KeepLast(30)
    }
}

impl LogRotation {
    /// The number of files to retain, or `None` for unlimited.
    fn keep_count(self) -> Option<usize> {
        match self {
            LogRotation::KeepLast(n) => Some(n.max(1)),
            LogRotation::Unlimited => None,
        }
    }
}

/// Registers a synchronous job on the scheduler. Each run is logged to a
/// timestamped file next to the executable (`logs/<job>_<datetime>.log`), with
/// the default [`LogRotation`] (keep the most recent 30). Use
/// [`add_job_with_rotation`] to choose a different policy.
///
/// # Errors
///
/// Returns [`JobSchedulerError`] if the cron `schedule` is invalid or the job
/// can't be added.
pub async fn add_job<F>(
    sched: &JobScheduler,
    job_name: &str,
    schedule: &str,
    job_action: F,
) -> Result<(), JobSchedulerError>
where
    F: Fn() -> Result<(), Box<dyn std::error::Error>> + Send + Sync + 'static,
{
    add_job_with_rotation(
        sched,
        job_name,
        schedule,
        LogRotation::default(),
        job_action,
    )
    .await
}

/// Like [`add_job`], but the caller chooses how many run logs the job keeps.
///
/// ```ignore
/// // Keep only the last 7 runs of a frequent job.
/// add_job_with_rotation(&sched, "heartbeat", "0 * * * * *",
///                       LogRotation::KeepLast(7), || Ok(())).await?;
/// ```
///
/// # Errors
///
/// Returns [`JobSchedulerError`] if the cron `schedule` is invalid or the job
/// can't be added.
pub async fn add_job_with_rotation<F>(
    sched: &JobScheduler,
    job_name: &str,
    schedule: &str,
    rotation: LogRotation,
    job_action: F,
) -> Result<(), JobSchedulerError>
where
    F: Fn() -> Result<(), Box<dyn std::error::Error>> + Send + Sync + 'static,
{
    let job_name = job_name.to_string();

    sched
        .add(Job::new(schedule, move |_uuid, _l| {
            let job_name = job_name.clone();
            if let Err(e) = execute_job(&job_name, rotation, &job_action) {
                error!("Job {job_name} failed: {e}");
            }
        })?)
        .await?;

    Ok(())
}

/// Registers an async job on the scheduler. Each run is logged to a
/// timestamped file next to the executable (`logs/<job>_<datetime>.log`), with
/// the default [`LogRotation`] (keep the most recent 30). Use
/// [`add_async_job_with_rotation`] to choose a different policy.
///
/// # Errors
///
/// Returns [`JobSchedulerError`] if the cron `schedule` is invalid or the job
/// can't be added.
pub async fn add_async_job<F, Fut>(
    sched: &JobScheduler,
    job_name: &str,
    schedule: &str,
    job_action: F,
) -> Result<(), JobSchedulerError>
where
    F: Fn() -> Fut + Send + Clone + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + Send + 'static,
{
    add_async_job_with_rotation(
        sched,
        job_name,
        schedule,
        LogRotation::default(),
        job_action,
    )
    .await
}

/// Like [`add_async_job`], but the caller chooses how many run logs the job
/// keeps.
///
/// # Errors
///
/// Returns [`JobSchedulerError`] if the cron `schedule` is invalid or the job
/// can't be added.
pub async fn add_async_job_with_rotation<F, Fut>(
    sched: &JobScheduler,
    job_name: &str,
    schedule: &str,
    rotation: LogRotation,
    job_action: F,
) -> Result<(), JobSchedulerError>
where
    F: Fn() -> Fut + Send + Clone + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + Send + 'static,
{
    let job_name = job_name.to_string();

    sched
        .add(Job::new_async(schedule, move |_uuid, _l| {
            let job_name = job_name.clone();
            let job_action = job_action.clone();
            Box::pin(async move {
                if let Err(e) = execute_job_async(&job_name, rotation, job_action).await {
                    error!("Job {job_name} failed: {e}");
                }
            })
        })?)
        .await?;

    Ok(())
}

fn execute_job<F>(
    job_name: &str,
    rotation: LogRotation,
    job_action: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn() -> Result<(), Box<dyn std::error::Error>>,
{
    info!("{job_name} started");

    let datetime = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let log_file_name = create_log_file(job_name, &datetime)?;
    apply_rotation(&log_file_name, job_name, rotation);

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_file_name)?;
    writeln!(
        file,
        "{} started at: {}",
        job_name,
        Local::now().to_rfc3339()
    )?;

    match job_action() {
        Ok(()) => {
            writeln!(
                file,
                "{} completed successfully at: {}",
                job_name,
                Local::now().to_rfc3339()
            )?;
            info!(
                "{} completed successfully at: {}",
                job_name,
                Local::now().to_rfc3339()
            );
        }
        Err(e) => {
            writeln!(file, "{job_name} failed: {e}")?;
            error!("{job_name} failed: {e}");
        }
    }

    Ok(())
}

async fn execute_job_async<F, Fut>(
    job_name: &str,
    rotation: LogRotation,
    job_action: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + Send,
{
    info!("{job_name} started");

    let datetime = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let log_file_name = create_log_file(job_name, &datetime)?;
    apply_rotation(&log_file_name, job_name, rotation);

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_file_name)?;
    writeln!(
        file,
        "{} started at: {}",
        job_name,
        Local::now().to_rfc3339()
    )?;

    match job_action().await {
        Ok(()) => {
            writeln!(
                file,
                "{} completed successfully at: {}",
                job_name,
                Local::now().to_rfc3339()
            )?;
            info!(
                "{} completed successfully at: {}",
                job_name,
                Local::now().to_rfc3339()
            );
        }
        Err(e) => {
            writeln!(file, "{job_name} failed: {e}")?;
            error!("{job_name} failed: {e}");
        }
    }

    Ok(())
}

fn create_log_file(job_name: &str, datetime: &str) -> std::io::Result<PathBuf> {
    let exe_path = env::current_exe()?;
    let exe_dir = exe_path
        .parent()
        .expect("Failed to get executable directory");

    let logs_dir = exe_dir.join("logs");

    create_dir_all(&logs_dir)?;

    let log_file_name = format!("{job_name}_{datetime}.log");
    let log_file_path = logs_dir.join(log_file_name);

    Ok(log_file_path)
}

/// Prunes this job's old run logs down to the rotation limit. Called before
/// the new run's file is created, so `keep` counts the file about to be
/// written: keeping `n` leaves `n - 1` existing logs plus the new one. A
/// pruning failure (permission, race) is logged and ignored — it must never
/// abort the job run itself.
fn apply_rotation(new_log_path: &Path, job_name: &str, rotation: LogRotation) {
    let Some(keep) = rotation.keep_count() else {
        return;
    };
    let Some(logs_dir) = new_log_path.parent() else {
        return;
    };
    // Reserve one slot for the run that is about to be written.
    prune_old_logs(logs_dir, job_name, keep.saturating_sub(1));
}

/// Deletes this job's log files in `logs_dir` beyond the newest `keep`, oldest
/// first. Only files belonging to exactly `job_name` are touched (see
/// [`is_job_log`]); the datetime in the name sorts chronologically, so a
/// lexical sort is a chronological one.
fn prune_old_logs(logs_dir: &Path, job_name: &str, keep: usize) {
    let mut matching: Vec<PathBuf> = match std::fs::read_dir(logs_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| is_job_log(n, job_name))
            })
            .collect(),
        Err(_) => return,
    };

    if matching.len() <= keep {
        return;
    }
    matching.sort();
    let remove_count = matching.len() - keep;
    for path in matching.into_iter().take(remove_count) {
        if let Err(e) = std::fs::remove_file(&path) {
            error!("Failed to rotate log {}: {e}", path.display());
        }
    }
}

/// True when `file_name` is a run log produced for exactly `job_name`, i.e.
/// `<job_name>_<datetime>.log` where the datetime part starts with a digit
/// (the year). The digit check keeps job `report` from matching another job
/// `report_daily`'s logs — the latter's suffix starts with `d`, not a digit.
fn is_job_log(file_name: &str, job_name: &str) -> bool {
    let prefix = format!("{job_name}_");
    file_name
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(".log"))
        .is_some_and(|dt| dt.starts_with(|c: char| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rotation_keeps_a_bounded_history() {
        assert_eq!(LogRotation::default().keep_count(), Some(30));
        assert_eq!(LogRotation::KeepLast(7).keep_count(), Some(7));
        // Zero is clamped up to one — a job always keeps its latest log.
        assert_eq!(LogRotation::KeepLast(0).keep_count(), Some(1));
        assert_eq!(LogRotation::Unlimited.keep_count(), None);
    }

    #[test]
    fn is_job_log_matches_only_the_named_job() {
        assert!(is_job_log("report_2026-07-19_10-00-00.log", "report"));
        // Different job with a shared prefix is not matched (its suffix
        // starts with a letter, not the year digit).
        assert!(!is_job_log(
            "report_daily_2026-07-19_10-00-00.log",
            "report"
        ));
        assert!(is_job_log(
            "report_daily_2026-07-19_10-00-00.log",
            "report_daily"
        ));
        // Wrong extension / unrelated names.
        assert!(!is_job_log("report_2026-07-19.txt", "report"));
        assert!(!is_job_log("other_2026-07-19_10-00-00.log", "report"));
    }

    #[test]
    fn prune_keeps_newest_and_respects_job_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str| std::fs::write(dir.path().join(name), b"x").unwrap();

        // Five runs of `report`, chronologically ordered by name.
        write("report_2026-07-19_10-00-00.log");
        write("report_2026-07-19_11-00-00.log");
        write("report_2026-07-19_12-00-00.log");
        write("report_2026-07-19_13-00-00.log");
        write("report_2026-07-19_14-00-00.log");
        // A different job that shares the `report` prefix must be untouched.
        write("report_daily_2026-07-19_09-00-00.log");

        prune_old_logs(dir.path(), "report", 2);

        let mut remaining: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();

        assert_eq!(
            remaining,
            vec![
                "report_2026-07-19_13-00-00.log".to_string(),
                "report_2026-07-19_14-00-00.log".to_string(),
                "report_daily_2026-07-19_09-00-00.log".to_string(),
            ],
            "keeps the two newest `report` logs and leaves `report_daily` alone"
        );
    }

    #[test]
    fn prune_is_a_noop_when_under_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("job_2026-07-19_10-00-00.log"), b"x").unwrap();
        prune_old_logs(dir.path(), "job", 30);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
