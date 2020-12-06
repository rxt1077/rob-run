use tokio_postgres::NoTls;
use std::env;
use dotenv::dotenv;
use chrono::{NaiveDateTime, Utc};
use anyhow::Result;
use log::debug;
use tokio::time::{sleep, Duration};

// how long to wait before trying again after we can't pull a job
const JOB_PAUSE: u64 = 2;

mod vm;

struct Job<> {
    id: i32,
    status: String,
    completed_at: Option<NaiveDateTime>,
    command: String,
    output: String,
    git_url: String,
}

// pull_job connects to the DB, gets a job, and sets the job status to RUNNING
// in the DB
async fn pull_job(database_url: String) -> Result<Job> {
    let (client, connection) = tokio_postgres::connect(&database_url, NoTls).await?;
    // run the connection in it's own thread
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {}", e);
        }
    });
    let job_row = client.query_one(
        r#"
        SELECT id, group_id, script FROM jobs
        WHERE status='QUEUED'
        ORDER BY created_at ASC
        LIMIT 1
       "#,
       &[],
    ).await?;
    let group_id: i32 = job_row.try_get("group_id")?;
    let group_row = client.query_one(
        r#"
        SELECT git_url FROM groups
        WHERE id=$1
        "#,
        &[&group_id],
    ).await?;
    let job = Job {
        id: job_row.try_get("id")?,
        status: "RUNNING".to_string(),
        completed_at: None,
        git_url: group_row.try_get("git_url")?,
        command: job_row.try_get("script")?,
        output: "".to_string(),
    };
    client.execute("UPDATE jobs SET status=$1 WHERE id=$2", &[&job.status, &job.id]).await?;
    Ok(job)
}

async fn run_job(job: Job) -> Result<Job> {
    let output = vm::run("ls /",
                         "images/debian-10.qcow",
                         "images/memsnapshot.gz",
                         "root@debian:~#").await?;
    Ok(Job {
        id: job.id,
        status: "COMPLETE".to_string(),
        completed_at: Some(Utc::now().naive_utc()),
        git_url: job.git_url,
        command: job.command,
        output: output,
    })
}

#[tokio::main]
async fn main() {
    env_logger::init();
    dotenv().ok();

    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL is not set in .env file");

    match pull_job(database_url).await {
        Ok(job) => {
            println!("Running job id={}", job.id);
            match run_job(job).await {
                Ok(job) => {
                    println!("Job finished.");
                    println!("{}", job.output); },
                Err(e) => println!("Failure! {:?}", e),
            }
        }
        Err(e) => {
            debug!("Failed to pull a job: {:?}", e);
            debug!("Pausing for {} seconds", JOB_PAUSE);
            sleep(Duration::from_secs(JOB_PAUSE)).await;
        }
    }
}
