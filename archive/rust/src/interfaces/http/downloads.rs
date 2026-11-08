use axum::{
    Json,
    body::Body,
    extract::{Form, Path as AxumPath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use super::presentation::{html_escape, page};
use crate::{
    app::AppState,
    features::downloader::{
        Job, cache_key_for_url, cached_file_for_key, cleanup_downloads, normalize_youtube_url,
        sanitize_filename, start_download_job,
    },
    security::auth::{page_guard, raw_guard},
};

pub(super) async fn downloads_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = page_guard(&state, &headers) {
        return response;
    }
    let jobs = state
        .jobs
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let jobs_html = jobs
        .iter()
        .rev()
        .take(12)
        .map(|job| {
            format!(
                r#"<a class="job" href="/downloads/jobs/{}"><strong>{}</strong><span>{}</span></a>"#,
                html_escape(&job.id),
                html_escape(job.filename.as_deref().unwrap_or(&job.status)),
                html_escape(&job.progress)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    page(
        "YTP Downloader",
        &format!(
            r#"
<nav><a href="/">YTDLP Web</a><strong>YTP Downloader</strong><form action="/logout" method="post"><button class="ghost" type="submit">Log out</button></form></nav>
<main class="download">
  <form action="/downloads" method="post" class="download-form">
    <input name="url" type="url" inputmode="url" placeholder="https://youtu.be/..." required autofocus>
    <button type="submit">Download Video</button>
  </form>
  <section class="jobs">{jobs_html}</section>
</main>
"#
        ),
    )
}

#[derive(Deserialize)]
pub(super) struct DownloadForm {
    url: String,
}

pub(super) async fn download_create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<DownloadForm>,
) -> Response {
    if let Some(response) = page_guard(&state, &headers) {
        return response;
    }
    let Ok(url) = normalize_youtube_url(&form.url) else {
        return page(
            "YTP Downloader",
            r#"<nav><a href="/">YTDLP Web</a><strong>YTP Downloader</strong></nav><main class="download"><p class="error">Only YouTube links are accepted.</p><form action="/downloads" method="post" class="download-form"><input name="url" type="url" inputmode="url" required autofocus><button type="submit">Download Video</button></form></main>"#,
        );
    };
    cleanup_downloads(&state);
    let cache_key = cache_key_for_url(&url);
    let cached = {
        let db = state.db.lock().unwrap();
        cached_file_for_key(&db, &state.config.download_dir, &cache_key).unwrap_or(None)
    };
    let job = if let Some((path, filename)) = cached {
        Job::with_cached(url, cache_key, path, filename)
    } else {
        Job::new(url, cache_key)
    };
    let id = job.id.clone();
    let complete = job.status == "complete";
    state.jobs.lock().unwrap().insert(id.clone(), job);
    if !complete {
        start_download_job(state.clone(), id.clone());
    }
    Redirect::to(&format!("/downloads/jobs/{id}")).into_response()
}

pub(super) async fn download_job_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Some(response) = page_guard(&state, &headers) {
        return response;
    }
    if !state.jobs.lock().unwrap().contains_key(&id) {
        return (StatusCode::NOT_FOUND, "unknown job").into_response();
    }
    page(
        "YTP Downloader",
        &format!(
            r#"
<nav><a href="/">YTDLP Web</a><a href="/downloads">YTP Downloader</a></nav>
<main class="download">
  <h1 id="state">Preparing</h1>
  <div class="bar"><div id="fill"></div></div>
  <p id="error" class="error"></p>
  <p id="ready" hidden><a class="button" id="file" href="/downloads/jobs/{}/file">Save</a></p>
  <pre id="log"></pre>
</main>
<script>
const statusUrl = "/downloads/jobs/{}/status";
function width(progress) {{
  const match = String(progress || "").match(/([0-9.]+)%/);
  if (!match) return "8%";
  return Math.max(4, Math.min(100, Number(match[1]))) + "%";
}}
async function poll() {{
  const response = await fetch(statusUrl, {{cache: "no-store"}});
  if (!response.ok) return;
  const job = await response.json();
  document.getElementById("state").textContent = job.status + (job.progress ? " · " + job.progress : "");
  document.getElementById("fill").style.width = width(job.progress);
  document.getElementById("log").textContent = (job.log || []).join("\n");
  if (job.status === "error") {{
    document.getElementById("error").textContent = job.error || "Download failed.";
    return;
  }}
  if (job.status === "complete") {{
    document.getElementById("ready").hidden = false;
    document.getElementById("fill").style.width = "100%";
    return;
  }}
  setTimeout(poll, 1500);
}}
poll();
</script>
"#,
            html_escape(&id),
            html_escape(&id)
        ),
    )
}

pub(super) async fn download_job_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Some(response) = raw_guard(&state, &headers) {
        return response;
    }
    let job = state.jobs.lock().unwrap().get(&id).cloned();
    match job {
        Some(job) => Json(job).into_response(),
        None => (StatusCode::NOT_FOUND, "unknown job").into_response(),
    }
}

pub(super) async fn download_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Some(response) = raw_guard(&state, &headers) {
        return response;
    }
    let job = state.jobs.lock().unwrap().get(&id).cloned();
    let Some(job) = job else {
        return (StatusCode::NOT_FOUND, "unknown job").into_response();
    };
    if job.status != "complete" {
        return (StatusCode::CONFLICT, "not ready").into_response();
    }
    let Some(path) = job.file_path else {
        return (StatusCode::NOT_FOUND, "missing file").into_response();
    };
    let Ok(path) = path.canonicalize() else {
        return (StatusCode::NOT_FOUND, "missing file").into_response();
    };
    let Ok(root) = state.config.download_dir.canonicalize() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "bad download dir").into_response();
    };
    if !path.starts_with(root) || !path.is_file() {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return (StatusCode::NOT_FOUND, "missing file").into_response();
    };
    let filename = job.filename.unwrap_or_else(|| {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into()
    });
    let ascii_name = sanitize_filename(&filename)
        .chars()
        .filter(|character| {
            character.is_ascii() && !character.is_ascii_control() && *character != '"'
        })
        .collect::<String>();
    let disposition = format!(
        "attachment; filename=\"{}\"",
        if ascii_name.is_empty() {
            "download.mp4"
        } else {
            &ascii_name
        }
    );
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_str(
                    mime_guess::from_path(&path)
                        .first_or_octet_stream()
                        .essence_str(),
                )
                .unwrap(),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&disposition).unwrap(),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response()
}
