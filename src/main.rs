use tokio_postgres::NoTls;
use std::env;
use dotenv::dotenv;
use chrono::{NaiveDateTime, Utc};
use anyhow::{Result, anyhow};
use log::debug;
use tokio::time::{sleep, Duration};

// how long to wait between jobs
const JOB_PAUSE: u64 = 2;

mod vm;

#[derive(Clone)]
struct Job<> {
    id: i32,
    status: String,
    started_at: Option<NaiveDateTime>,
    completed_at: Option<NaiveDateTime>,
    command: String,
    output: String,
    git_url: String,
}

// pull_job connects to the DB, gets a job, sets the job status to STARTED, and
// returns the job
async fn pull_job(database_url: &str) -> Result<Job> {
    let (client, connection) = tokio_postgres::connect(&database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {}", e);
        }
    });
    let row = client.query_one(
        r#"
        UPDATE jobs
        SET status='STARTED', started_at=NOW()
        FROM groups
        WHERE groups.id=jobs.group_id AND
            jobs.id IN (
                SELECT id FROM jobs
                WHERE status='QUEUED'
                ORDER BY created_at ASC
                LIMIT 1
                FOR UPDATE
           )
       RETURNING jobs.id, jobs.status, jobs.started_at, jobs.completed_at,
           jobs.command, jobs.output, groups.git_url
       "#,
       &[],
    ).await?;
    Ok(Job {
        id: row.try_get("id")?,
        status: row.try_get("status")?,
        started_at: row.try_get("started_at")?,
        completed_at: row.try_get("completed_at")?,
        command: row.try_get("command")?,
        output: row.try_get("output")?,
        git_url: row.try_get("git_url")?,
    })
}

// runs a job on the VM and updates its parameters
async fn run_job(job: Job) -> Result<Job> {
    let output = vm::run(&job.command,
                         "images/debian-10.qcow",
                         "images/memsnapshot.gz",
                         "root@debian:~#").await?;
    Ok(Job {
        id: job.id,
        status: "COMPLETE".to_string(),
        completed_at: Some(Utc::now().naive_utc()),
        started_at: job.started_at,
        git_url: job.git_url,
        command: job.command,
        output: output,
    })
}

// saves a job to the DB
// *this only saves the fields that should be changed by this process*
async fn save_job(database_url: &str, job: Job) -> Result<Job> {
    let (client, connection) = tokio_postgres::connect(&database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {}", e);
        }
    });
    client.execute(
        r#"
        UPDATE jobs
        SET status=$1, completed_at=$2, started_at=$3, output=$4
        WHERE id=$5
        "#, &[&job.status, &job.completed_at, &job.started_at, &job.output,
        &job.id]).await?;
    Ok(job)
}

async fn do_job(database_url: &str) -> Result<Job> {
    let mut job = pull_job(database_url).await?;
    println!("Running job id={}", job.id);
    let job = match run_job(job.clone()).await {
        Ok(job) => {
            if job.started_at == None || job.completed_at == None {
                return Err(anyhow!("Job fields not set correctly in run_job()"));
            }
            let duration = job.completed_at.unwrap() - job.started_at.unwrap();
            println!("Job successfully finished in {}s", duration.num_seconds());
            job
        }
        Err(e) => {
            println!("Job failed: {:?}", e);
            job.status = "ERROR".to_string();
            job
        }
    };
    save_job(database_url, job).await
}

#[tokio::main]
async fn main() {
    env_logger::init();
    dotenv().ok();

    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL is not set in .env file");

    loop {
        match do_job(&database_url).await {
            Ok(_) => {},
            Err(e) => {
                println!("Error while running job: {}", e);
            },
        }
        debug!("Pausing for {} seconds", JOB_PAUSE);
        sleep(Duration::from_secs(JOB_PAUSE)).await;
    }
}
