use chrono::Local;
use log::{error, info};
use std::env;
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

pub async fn add_job<F>(
    sched: &JobScheduler,
    job_name: &str,
    schedule: &str,
    job_action: F,
) -> Result<(), JobSchedulerError>
where
    F: Fn() -> Result<(), Box<dyn std::error::Error>> + Send + Sync + 'static,
{
    let job_name = job_name.to_string();

    sched
        .add(Job::new(schedule, move |_uuid, _l| {
            let job_name = job_name.clone();
            if let Err(e) = execute_job(&job_name, &job_action) {
                error!("Job {} failed: {}", job_name, e);
            }
        })?)
        .await?;

    Ok(())
}

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
    let job_name = job_name.to_string();

    sched
        .add(Job::new_async(schedule, move |_uuid, _l| {
            let job_name = job_name.clone();
            let job_action = job_action.clone();
            Box::pin(async move {
                if let Err(e) = execute_job_async(&job_name, job_action).await {
                    error!("Job {} failed: {}", job_name, e);
                }
            })
        })?)
        .await?;

    Ok(())
}

fn execute_job<F>(job_name: &str, job_action: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn() -> Result<(), Box<dyn std::error::Error>>,
{
    info!("{} started", job_name);

    let datetime = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let log_file_name = create_log_file(job_name, &datetime)?;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&log_file_name)?;
    writeln!(
        file,
        "{} started at: {}",
        job_name,
        Local::now().to_rfc3339()
    )?;

    match job_action() {
        Ok(_) => {
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
            writeln!(file, "{} failed: {}", job_name, e)?;
            error!("{} failed: {}", job_name, e);
        }
    }

    Ok(())
}

async fn execute_job_async<F, Fut>(
    job_name: &str,
    job_action: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + Send,
{
    info!("{} started", job_name);

    let datetime = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let log_file_name = create_log_file(job_name, &datetime)?;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&log_file_name)?;
    writeln!(
        file,
        "{} started at: {}",
        job_name,
        Local::now().to_rfc3339()
    )?;

    match job_action().await {
        Ok(_) => {
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
            writeln!(file, "{} failed: {}", job_name, e)?;
            error!("{} failed: {}", job_name, e);
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

    let log_file_name = format!("{}_{}.log", job_name, datetime);
    let log_file_path = logs_dir.join(log_file_name);

    Ok(log_file_path)
}
