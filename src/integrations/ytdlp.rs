use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

pub(crate) async fn download(
    program: &str,
    js_runtime: Option<&Path>,
    job_dir: &Path,
    url: &str,
    mut on_line: impl FnMut(&str, Option<String>) + Send,
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
    command
        .current_dir(job_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start yt-dlp: {error}"))?;
    if let Some(stdout) = child.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let progress = progress_from_line(&line);
            on_line(&line, progress);
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("yt-dlp wait failed: {error}"))?;
    if !status.success() {
        return Err(format!(
            "yt-dlp exited with code {}",
            status.code().unwrap_or(-1)
        ));
    }
    find_downloaded_file(job_dir)
        .ok_or_else(|| "Download finished but no file was found.".to_string())
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
    use super::progress_from_line;

    #[test]
    fn progress_parser_extracts_download_percent() {
        assert_eq!(
            progress_from_line("[download]  42.7% of 12.34MiB at 1.2MiB/s"),
            Some("42.7%".into())
        );
        assert_eq!(progress_from_line("not progress"), None);
    }
}
