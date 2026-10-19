use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

/// How much of the end of yt-dlp's stderr is kept for the error message.
const STDERR_TAIL_BYTES: usize = 4 * 1024;

pub(crate) async fn download(
    program: &str,
    js_runtime: Option<&Path>,
    job_dir: &Path,
    url: &str,
    timeout: Duration,
    on_line: impl FnMut(&str, Option<String>) + Send,
) -> Result<PathBuf, String> {
    let output_template = job_dir.join("%(title).180B [%(id)s].%(ext)s");
    let mut command = Command::new(program);
    command
        .arg("--no-playlist")
        .arg("--newline")
        .arg("--no-part")
        .arg("--restrict-filenames")
        .arg("--windows-filenames")
        .arg("--no-mtime")
        .arg("--socket-timeout")
        .arg("30")
        .arg("--retries")
        .arg("3")
        .arg("--fragment-retries")
        .arg("3")
        .arg("-f")
        .arg("bv*[ext=mp4][vcodec^=avc1][height<=1080]+ba[ext=m4a]/bv*[ext=mp4][height<=1080]+ba[ext=m4a]/b[ext=mp4][vcodec^=avc1][height<=720]/b[ext=mp4][height<=720]/b[ext=mp4]")
        .arg("--merge-output-format")
        .arg("mp4");
    if let Some(runtime) = js_runtime {
        command
            .arg("--js-runtimes")
            .arg(format!("node:{}", runtime.display()));
    }
    command.arg("-o").arg(output_template).arg(url);
    command.current_dir(job_dir);

    run(command, timeout, on_line).await?;
    find_downloaded_file(job_dir)
        .ok_or_else(|| "Download finished but no file was found.".to_string())
}

/// Runs the prepared command: stdout lines go to `on_line`, stderr is drained
/// concurrently (only its tail is kept), and the process is killed once
/// `timeout` elapses or the job is dropped.
async fn run(
    mut command: Command,
    timeout: Duration,
    mut on_line: impl FnMut(&str, Option<String>) + Send,
) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start yt-dlp: {error}"))?;
    let stderr = child.stderr.take();
    let stderr_tail = tokio::spawn(async move {
        let mut tail = String::new();
        let Some(stderr) = stderr else {
            return tail;
        };
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tail.push_str(line.trim_end());
            tail.push('\n');
            if tail.len() > STDERR_TAIL_BYTES {
                let cut = tail.len() - STDERR_TAIL_BYTES;
                let cut = tail
                    .char_indices()
                    .map(|(index, _)| index)
                    .find(|index| *index >= cut)
                    .unwrap_or(tail.len());
                tail.drain(..cut);
            }
        }
        tail
    });
    let stdout = child.stdout.take();
    let finished = tokio::time::timeout(timeout, async {
        if let Some(stdout) = stdout {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let progress = progress_from_line(&line);
                on_line(&line, progress);
            }
        }
        child.wait().await
    })
    .await;
    let status = match finished {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return Err(format!("yt-dlp wait failed: {error}")),
        Err(_) => {
            let _ = child.kill().await;
            return Err(format!(
                "yt-dlp was stopped after {} seconds without finishing.",
                timeout.as_secs()
            ));
        }
    };
    let tail = stderr_tail.await.unwrap_or_default();
    if !status.success() {
        let tail = tail.trim();
        return Err(if tail.is_empty() {
            format!("yt-dlp exited with code {}", status.code().unwrap_or(-1))
        } else {
            format!(
                "yt-dlp exited with code {}: {tail}",
                status.code().unwrap_or(-1)
            )
        });
    }
    Ok(())
}

fn progress_from_line(line: &str) -> Option<String> {
    let marker = "[download]";
    let index = line.find(marker)?;
    let tail = &line[index + marker.len()..];
    let percent = tail.find('%')?;
    let number = tail[..percent].split_whitespace().last().filter(|value| {
        value
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
    })?;
    Some(format!("{number}%"))
}

fn find_downloaded_file(directory: &Path) -> Option<PathBuf> {
    let ignored = ["json", "part", "ytdl", "temp", "tmp"];
    std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_none_or(|extension| !ignored.contains(&extension))
                && !path.to_string_lossy().ends_with(".part")
        })
        .max_by_key(|path| {
            path.metadata()
                .map(|metadata| (metadata.len(), metadata.modified().ok()))
                .unwrap_or((0, None))
        })
}

#[cfg(test)]
mod tests {
    use super::{Command, Duration, STDERR_TAIL_BYTES, progress_from_line, run};

    #[test]
    fn progress_parser_extracts_download_percent() {
        assert_eq!(
            progress_from_line("[download]  42.7% of 12.34MiB at 1.2MiB/s"),
            Some("42.7%".into())
        );
        assert_eq!(progress_from_line("not progress"), None);
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    #[tokio::test]
    async fn chatty_stderr_does_not_block_and_its_tail_is_reported() {
        // 200 KiB on stderr exceeds the pipe buffer; an unread pipe would
        // block the child before it ever prints to stdout or exits.
        let script = "i=0; while [ $i -lt 20000 ]; do echo \"stderr-line-$i\" >&2; i=$((i+1)); done; \
                      echo '[download]  50.0% of 1MiB'; echo 'ERROR: final' >&2; exit 3";
        let mut lines = Vec::new();
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            run(shell(script), Duration::from_secs(20), |line, progress| {
                lines.push((line.to_string(), progress));
            }),
        )
        .await
        .expect("run must not hang on a chatty stderr");
        let error = result.unwrap_err();
        assert!(error.starts_with("yt-dlp exited with code 3: "), "{error}");
        assert!(error.ends_with("ERROR: final"), "{error}");
        assert!(error.len() <= STDERR_TAIL_BYTES + 64, "{}", error.len());
        assert_eq!(
            lines,
            vec![(
                "[download]  50.0% of 1MiB".to_string(),
                Some("50.0%".into())
            )]
        );
    }

    #[tokio::test]
    async fn wall_clock_timeout_kills_the_process() {
        let started = std::time::Instant::now();
        let result = run(shell("sleep 30"), Duration::from_millis(300), |_, _| {}).await;
        assert!(result.unwrap_err().contains("stopped after 0 seconds"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn successful_run_reports_nothing() {
        assert_eq!(
            run(shell("echo ok; exit 0"), Duration::from_secs(5), |_, _| {}).await,
            Ok(())
        );
    }
}
