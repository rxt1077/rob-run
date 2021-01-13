use tokio_postgres::NoTls;
use std::env;
use dotenv::dotenv;
use chrono::{DateTime, Utc};
use anyhow::{Result, anyhow};
use log::debug;
use tokio::time::{sleep, Duration};

// how long to wait between jobs
const JOB_PAUSE: u64 = 2;

mod vm;

// this ugly hack brought to you by a lack of enum  support in postgres
#[derive(Clone)]
enum Status {
    QUEUED,
    RUNNING,
    COMPLETE,
    ERROR,
}
impl Status {
    fn to_str(&self) -> &str {
        match self {
            Status::QUEUED => "QUEUED", 
            Status::RUNNING => "RUNNING", 
            Status::COMPLETE => "COMPLETE", 
            Status::ERROR => "ERROR", 
        }
    }
    fn from_str(input: &str) -> Status {
        match input {
            "QUEUED" => Status::QUEUED, 
            "RUNNING" => Status::RUNNING, 
            "COMPLETE" => Status::COMPLETE, 
            "ERROR" => Status::ERROR,
            _ => Status::ERROR,
        }
    }
}


#[derive(Clone)]
struct Job<> {
    id: i32,
    status: Status,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    memsnapshot: Option<String>,
    tests_image: Option<String>,
    base_image: String,
    prompt: String,
    command: String,
    command_timeout: Duration,
    output: String,
    git_url: String,
}

// pull_job connects to the DB, gets a job, sets the job status to RUNNING, and
// returns the job
async fn pull_job(database_url: &str) -> Result<Option<Job>> {
    let (client, connection) = tokio_postgres::connect(&database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection error: {}", e);
        }
    });
    let row = client.query_opt(
        r#"
        UPDATE jobs
        SET status='RUNNING', started_at=NOW()
        FROM groups, tests
        WHERE groups.id=jobs.group_id AND tests.id=jobs.test_id AND
            jobs.id IN (
                SELECT id FROM jobs
                WHERE status='QUEUED'
                ORDER BY created_at ASC
                LIMIT 1
                FOR UPDATE
           )
       RETURNING jobs.id, jobs.status, jobs.started_at, jobs.completed_at,
           tests.memsnapshot, tests.tests_image, tests.base_image,
           tests.prompt, tests.command, tests.command_timeout, jobs.output,
           groups.git_url
       "#,
       &[],
    ).await?;
    match row {
        Some(row) => Ok(Some(Job {
            id: row.try_get("id")?,
            status: Status::from_str(row.try_get("status")?),
            started_at: row.try_get("started_at")?,
            completed_at: row.try_get("completed_at")?,
            memsnapshot: row.try_get("memsnapshot")?,
            tests_image: row.try_get("tests_image")?,
            base_image: row.try_get("base_image")?,
            prompt: row.try_get("prompt")?,
            command: row.try_get("command")?,
            command_timeout: Duration::from_secs(row.try_get::<&str, i32>("command_timeout")? as u64),
            output: row.try_get("output")?,
            git_url: row.try_get("git_url")?,
        })),
        None => Ok(None),
    }
}

// runs a job on the VM and updates its parameters
async fn run_job(job: Job, memory: &str) -> Result<Job> {
    let output = vm::run(format!("{} {}", job.command, job.git_url),
                         job.command_timeout,
                         &job.base_image,
                         &job.tests_image,
                         &job.memsnapshot,
                         &job.prompt,
                         &memory).await?;
    Ok(Job {
        id: job.id,
        status: Status::COMPLETE,
        started_at: job.started_at,
        completed_at: Some(Utc::now()),
        memsnapshot: job.memsnapshot,
        tests_image: job.tests_image,
        base_image: job.base_image,
        prompt: job.prompt,
        command: job.command,
        command_timeout: job.command_timeout,
        output: output,
        git_url: job.git_url,
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
        "#, &[&job.status.to_str(), &job.completed_at, &job.started_at, &job.output,
        &job.id]).await?;
    Ok(job)
}

async fn do_job(database_url: &str, memory: &str) -> Result<Option<Job>> {
    let mut job = match pull_job(database_url).await? {
        Some(job) => job,
        None => return Ok(None),
    };
    println!("Running job id={}", job.id);
    let job = match run_job(job.clone(), memory).await {
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
            job.status = Status::ERROR;
            job
        }
    };
    Ok(Some(save_job(database_url, job).await?))
}

#[tokio::main]
async fn main() {
    env_logger::init();
    dotenv().ok();

    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL is not set in .env file");
    let memory = env::var("MEMORY").expect("MEMORY is not set in .env file");

    loop {
        match do_job(&database_url, &memory).await {
            Ok(_) => {},
            Err(e) => {
                println!("Error while running job: {}", e);
            },
        }
        debug!("Pausing for {} seconds", JOB_PAUSE);
        sleep(Duration::from_secs(JOB_PAUSE)).await;
    }
}
