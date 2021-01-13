use anyhow::{Result, anyhow};
use std::process::Stdio;
use tokio::process::Command;
use tokio::io::{BufReader, AsyncBufReadExt, AsyncWriteExt, AsyncReadExt};
use tokio::time::{sleep, Duration};
use log::debug;

// all times are in seconds
// how long to wait to get a bash prompt, remeber some VMs may need to boot so
// error on the high side
const START_TIMEOUT: u64 = 120;
// how long to wait after you received a bash prompt
const START_WAIT: u64 = 2;

// sends one newline a second to a stdin
async fn send_newlines(stdin: &mut tokio::process::ChildStdin) -> Result<()> {
    loop {
        debug!("Writing a newline");
        stdin.write_all("\n".as_bytes()).await?;
        sleep(Duration::from_secs(1)).await;
    }
}

// checks stdout lines for a prompt typically indicating the VM is ready or
// has finished running a command
async fn prompt_check(stdout: &mut tokio::process::ChildStdout, prompt: &str) -> Result<String> {
    let mut output = String::new();
    let mut reader = BufReader::new(stdout).lines();
    while let Some(line) = reader.next_line().await? {
        debug!("Read a line: {}", line);
        output.push_str(&format!("{}\n", line));
        if line.contains(&prompt) {
            return Ok(output);
        }
    }
    Err(anyhow!("Ran out of input from stdin"))
}

// waits for a VM to become ready by checking for a prompt in response to a \n
async fn ready(stdin: &mut tokio::process::ChildStdin,
               stdout: &mut tokio::process::ChildStdout,
               prompt: &str) -> Result<()> {
    debug!("Giving the VM {} seconds to start", START_TIMEOUT);
    loop {
        tokio::select! {
            _ = sleep(Duration::from_secs(START_TIMEOUT)) => {
                return Err(anyhow!("Timed out waiting for prompt"));
            }
            _ = send_newlines(stdin) => {
                return Err(anyhow!("Sender finished"));
            }
            _ = prompt_check(stdout, prompt) => {
                debug!("Reader finished");
                break;
            }
        }
    }
    debug!("Waiting {} seconds to read unfinished prompts", START_WAIT);
    sleep(Duration::from_secs(START_WAIT)).await;
    let mut buffer = [0; 512];
    let bytes_read = stdout.read(&mut buffer).await?;
    debug!("Read an extra {} bytes from STDIN", bytes_read);
    Ok(())
}

// writes a command to stdin, adding '\r\n' to the end
async fn run_command(stdin: &mut tokio::process::ChildStdin,
                     stdout: &mut tokio::process::ChildStdout,
                     prompt: &str, command: String, command_timeout: Duration) -> Result<String> {
    debug!("Writing: {}\\r\\n", command);
    stdin.write_all(format!("{}\r\n", command).as_bytes()).await?;
    loop {
        tokio::select! {
            _ = sleep(command_timeout) => {
                return Err(anyhow!("Timed out waiting for command to finish"));
            }
            output_result = prompt_check(stdout, prompt) => {
                return output_result;
            }
        }
    }
}

// runs a single command in a VM, returning the output and killing the VM when
// done
pub async fn run(command: String,
                 command_timeout: Duration,
                 image: &str,
                 scripts_image: &Option<String>,
                 snapshot: &Option<String>,
                 prompt: &str) -> Result<String>
{
    let mut args = vec!["-hda".to_string(), image.to_string(),
                        "-m".to_string(), "1G".to_string(),
                        "-netdev".to_string(), "user,id=n1".to_string(),
                        "-device".to_string(), "virtio-net-pci,netdev=n1".to_string(),
                        "-nographic".to_string(), "-snapshot".to_string()];
    match snapshot {
        Some(snapshot) => {
            args.push("-incoming".to_string());
            args.push(format!("exec: gzip -c -d {}", snapshot));
        },
        None => {}
    };
    match scripts_image {
        Some(scripts_image) => {
            args.push("-cdrom".to_string());
            args.push(scripts_image.to_string());
        }
        None => ()
    };
    debug!("qemu-system-x86_64 args: {:#?}", args);
    let mut child = Command::new("qemu-system-x86_64")
                .args(&args)
                .stdout(Stdio::piped())
                .stdin(Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
    let mut stdin = child.stdin
        .take()
        .ok_or(anyhow!("Couldn't take stdin from child"))?;
    let mut stdout = child.stdout
        .take()
        .ok_or(anyhow!("Couldn't take stdout from child"))?;
    // run the child in its own task
    tokio::spawn(async move {
        let status = child.wait().await
            .expect("Child process encountered an error");
        debug!("child status was: {}", status);
    });
    ready(&mut stdin, &mut stdout, prompt).await?;
    return run_command(&mut stdin, &mut stdout, prompt, command,
                       command_timeout).await;
}
