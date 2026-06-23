use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    Json,
    body::Body,
    extract::{Form, Path as AxumPath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    env, fs,
    io::{self, Read},
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command as TokioCommand,
    sync::Semaphore,
};
use tokio_util::io::ReaderStream;
use url::Url;
use uuid::Uuid;

mod db_migrations;
mod modules;

const SESSION_COOKIE: &str = "ytdlp_web_session";
const SESSION_DAYS: i64 = 30;
const PAGE_CSS: &str = include_str!("page.css");

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str).unwrap_or("serve") {
        "serve" => serve().await,
        "hash-password" => {
            hash_password_cmd(&args[1..])?;
            Ok(())
        }
        "audit-public" => {
            if audit_public_cmd(&args[1..])? != 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        _ => {
            eprintln!("usage: ytdlp-web [serve|hash-password --stdin|audit-public]");
            Ok(())
        }
    }
}

async fn serve() -> io::Result<()> {
    let config = Config::from_env()?;
    fs::create_dir_all(&config.download_dir)?;
    let conn = open_db(&config.db_path)?;
    let state = Arc::new(AppState {
        db: Mutex::new(conn),
        config,
        jobs: Mutex::new(HashMap::new()),
        download_slots: Semaphore::new(1),
    });
    state.set_download_slots();

    let app = modules::build_router(state.clone());
    let bind = state
        .config
        .bind
        .parse::<SocketAddr>()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("YTDLP Web listening on http://{bind}");
    axum::serve(listener, app).await
}

#[derive(Clone)]
struct Config {
    bind: String,
    db_path: PathBuf,
    download_dir: PathBuf,
    ytdlp: String,
    js_runtime: Option<PathBuf>,
    max_active: usize,
    job_ttl: Duration,
    user: String,
    password_hash: Option<String>,
    cookie_secret: Vec<u8>,
    auth_disabled: bool,
}

impl Config {
    fn from_env() -> io::Result<Self> {
        let bind = env::var("YTDLP_WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8790".into());
        let db_path = env::var("YTDLP_WEB_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/ytdlp_web.sqlite"));
        let download_dir = env::var("YTDLP_WEB_DOWNLOAD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/downloads"));
        let ytdlp = env::var("YTDLP_WEB_YTDLP").unwrap_or_else(|_| "yt-dlp".into());
        let js_runtime = env::var("YTDLP_WEB_JS_RUNTIME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .filter(|path| path.exists());
        let max_active = env::var("YTDLP_WEB_MAX_ACTIVE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(1);
        let ttl_hours = env::var("YTDLP_WEB_JOB_TTL_HOURS")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(24);
        let user = env::var("YTDLP_WEB_USER").unwrap_or_else(|_| "ytdlp_web".into());
        let password_hash = env::var("YTDLP_WEB_PASSWORD_HASH")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let auth_disabled = env_flag("YTDLP_WEB_AUTH_DISABLED", false);
        if password_hash.is_none() && !auth_disabled {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "YTDLP_WEB_PASSWORD_HASH is required unless YTDLP_WEB_AUTH_DISABLED=1",
            ));
        }
        let cookie_secret = env::var("YTDLP_WEB_COOKIE_SECRET")
            .ok()
            .and_then(|value| hex::decode(value.trim()).ok())
            .filter(|bytes| bytes.len() >= 32)
            .unwrap_or_else(random_secret);

        Ok(Self {
            bind,
            db_path,
            download_dir,
            ytdlp,
            js_runtime,
            max_active,
            job_ttl: Duration::hours(ttl_hours),
            user,
            password_hash,
            cookie_secret,
            auth_disabled,
        })
    }
}

struct AppState {
    db: Mutex<Connection>,
    config: Config,
    jobs: Mutex<HashMap<String, Job>>,
    download_slots: Semaphore,
}

impl AppState {
    fn set_download_slots(&self) {
        let current = self.download_slots.available_permits();
        if self.config.max_active > current {
            self.download_slots
                .add_permits(self.config.max_active.saturating_sub(current));
        }
    }
}

#[derive(Clone, Serialize)]
struct Job {
    id: String,
    url: String,
    cache_key: String,
    created_at: DateTime<Utc>,
    status: String,
    progress: String,
    filename: Option<String>,
    file_path: Option<PathBuf>,
    error: Option<String>,
    log: Vec<String>,
}

impl Job {
    fn new(url: String, cache_key: String) -> Self {
        Self {
            id: Uuid::new_v4().simple().to_string()[..12].to_string(),
            url,
            cache_key,
            created_at: Utc::now(),
            status: "queued".into(),
            progress: String::new(),
            filename: None,
            file_path: None,
            error: None,
            log: Vec::new(),
        }
    }

    fn with_cached(url: String, cache_key: String, file_path: PathBuf, filename: String) -> Self {
        let mut job = Self::new(url, cache_key);
        job.status = "complete".into();
        job.progress = "100%".into();
        job.file_path = Some(file_path);
        job.filename = Some(filename);
        job.log.push("Using cached file".into());
        job
    }
}

#[derive(Debug)]
struct AuditFinding {
    path: String,
    line: Option<usize>,
    message: String,
}

fn open_db(path: &Path) -> io::Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path).map_err(io_other)?;
    db_migrations::migrate(&conn).map_err(io_other)?;
    Ok(conn)
}

async fn login_page(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if authorized(&state.config, &headers) {
        return Redirect::to("/").into_response();
    }
    page(
        "YTDLP Web Login",
        r#"
<main class="login">
  <h1>YTDLP Web</h1>
  <form action="/login" method="post">
    <label>Password</label>
    <input name="password" type="password" autocomplete="current-password" autofocus required>
    <button type="submit">Log in</button>
  </form>
</main>
"#,
    )
}

#[derive(Deserialize)]
struct LoginForm {
    password: String,
}

async fn login_post(State(state): State<Arc<AppState>>, Form(form): Form<LoginForm>) -> Response {
    if !verify_password(&state.config, &form.password) {
        return page(
            "YTDLP Web Login",
            r#"
<main class="login">
  <h1>YTDLP Web</h1>
  <p class="error">Wrong password.</p>
  <form action="/login" method="post">
    <label>Password</label>
    <input name="password" type="password" autocomplete="current-password" autofocus required>
    <button type="submit">Log in</button>
  </form>
</main>
"#,
        );
    }
    let cookie = make_session_cookie(&state.config);
    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, HeaderValue::from_static("/")),
            (header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap()),
        ],
    )
        .into_response()
}

async fn logout_post() -> Response {
    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, HeaderValue::from_static("/login")),
            (
                header::SET_COOKIE,
                HeaderValue::from_static(
                    "ytdlp_web_session=; Max-Age=0; Path=/; HttpOnly; SameSite=Lax",
                ),
            ),
        ],
    )
        .into_response()
}

async fn downloads_page(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
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
struct DownloadForm {
    url: String,
}

async fn download_create(
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
        tokio::spawn(run_download_job(state.clone(), id.clone()));
    }
    Redirect::to(&format!("/downloads/jobs/{id}")).into_response()
}

async fn download_job_page(
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

async fn download_job_status(
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

async fn download_file(
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
        .filter(|ch| ch.is_ascii() && !ch.is_ascii_control() && *ch != '"')
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

async fn run_download_job(state: Arc<AppState>, id: String) {
    {
        let mut jobs = state.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(&id) {
            job.status = "queued".into();
            job.progress.clear();
        }
    }
    let _permit = match state.download_slots.acquire().await {
        Ok(permit) => permit,
        Err(_) => return,
    };
    update_job(&state, &id, |job| {
        job.status = "downloading".into();
        job.progress = "starting".into();
        push_log(job, "Starting download");
    });

    let job = state.jobs.lock().unwrap().get(&id).cloned();
    let Some(job) = job else {
        return;
    };
    let job_dir = state.config.download_dir.join(&job.id);
    if let Err(err) = tokio::fs::create_dir_all(&job_dir).await {
        fail_job(
            &state,
            &id,
            format!("Could not create download directory: {err}"),
        );
        return;
    }

    let output_template = job_dir.join("%(title).180B [%(id)s].%(ext)s");
    let mut command = TokioCommand::new(&state.config.ytdlp);
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
    if let Some(runtime) = &state.config.js_runtime {
        command
            .arg("--js-runtimes")
            .arg(format!("node:{}", runtime.display()));
    }
    command.arg("-o").arg(output_template).arg(&job.url);
    command
        .current_dir(&job_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            fail_job(&state, &id, format!("Could not start yt-dlp: {err}"));
            return;
        }
    };
    if let Some(stdout) = child.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let progress = progress_from_line(&line);
            update_job(&state, &id, |job| {
                if let Some(progress) = progress {
                    job.progress = progress;
                }
                push_log(job, &line);
            });
        }
    }
    let status = match child.wait().await {
        Ok(status) => status,
        Err(err) => {
            fail_job(&state, &id, format!("yt-dlp wait failed: {err}"));
            return;
        }
    };
    if !status.success() {
        fail_job(
            &state,
            &id,
            format!("yt-dlp exited with code {}", status.code().unwrap_or(-1)),
        );
        return;
    }
    let Some(file) = find_downloaded_file(&job_dir) else {
        fail_job(
            &state,
            &id,
            "Download finished but no file was found.".into(),
        );
        return;
    };
    let filename = file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    {
        let db = state.db.lock().unwrap();
        let _ = db.execute(
            "INSERT OR REPLACE INTO download_cache (cache_key, file_path, filename, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![job.cache_key, file.to_string_lossy(), filename, Utc::now().to_rfc3339()],
        );
    }
    update_job(&state, &id, |job| {
        job.status = "complete".into();
        job.progress = "100%".into();
        job.file_path = Some(file);
        job.filename = Some(filename);
        push_log(job, "Download ready");
    });
}

fn update_job(state: &AppState, id: &str, edit: impl FnOnce(&mut Job)) {
    let mut jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get_mut(id) {
        edit(job);
    }
}

fn fail_job(state: &AppState, id: &str, error: String) {
    update_job(state, id, |job| {
        job.status = "error".into();
        job.error = Some(error);
    });
}

fn push_log(job: &mut Job, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    job.log.push(line.to_string());
    if job.log.len() > 80 {
        let extra = job.log.len() - 80;
        job.log.drain(0..extra);
    }
}

fn progress_from_line(line: &str) -> Option<String> {
    let marker = "[download]";
    let index = line.find(marker)?;
    let tail = &line[index + marker.len()..];
    let pct = tail.find('%')?;
    let number = tail[..pct]
        .split_whitespace()
        .last()
        .filter(|value| value.chars().all(|ch| ch.is_ascii_digit() || ch == '.'))?;
    Some(format!("{number}%"))
}

fn normalize_youtube_url(raw: &str) -> Result<String, ()> {
    let mut value = raw.trim().to_string();
    if value.is_empty() {
        return Err(());
    }
    if !value.contains("://") {
        value = format!("https://{value}");
    }
    let url = Url::parse(&value).map_err(|_| ())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(());
    }
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_lowercase();
    let ok = host == "youtu.be"
        || host == "youtube.com"
        || host == "youtube-nocookie.com"
        || host.ends_with(".youtube.com");
    if ok { Ok(value) } else { Err(()) }
}

fn cache_key_for_url(url: &str) -> String {
    if let Ok(parsed) = Url::parse(url) {
        let host = parsed.host_str().unwrap_or("").to_lowercase();
        if host == "youtu.be"
            && let Some(id) = parsed.path_segments().and_then(|mut parts| parts.next())
        {
            return format!("youtube:{id}");
        }
        if (host == "youtube.com" || host.ends_with(".youtube.com"))
            && let Some((_, value)) = parsed.query_pairs().find(|(key, _)| key == "v")
        {
            return format!("youtube:{value}");
        }
    }
    format!("url:{url}")
}

fn find_downloaded_file(dir: &Path) -> Option<PathBuf> {
    let ignored = ["json", "part", "ytdl", "temp", "tmp"];
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_none_or(|ext| !ignored.contains(&ext))
                && !path.to_string_lossy().ends_with(".part")
        })
        .max_by_key(|path| {
            path.metadata()
                .map(|meta| (meta.len(), meta.modified().ok()))
                .unwrap_or((0, None))
        })
}

fn cached_file_for_key(
    db: &Connection,
    download_dir: &Path,
    cache_key: &str,
) -> rusqlite::Result<Option<(PathBuf, String)>> {
    let row = db
        .query_row(
            "SELECT file_path, filename FROM download_cache WHERE cache_key = ?1",
            params![cache_key],
            |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get::<_, String>(1)?,
                ))
            },
        )
        .optional()?;
    let Some((path, filename)) = row else {
        return Ok(None);
    };
    let Ok(canon) = path.canonicalize() else {
        return Ok(None);
    };
    let Ok(root) = download_dir.canonicalize() else {
        return Ok(None);
    };
    if canon.starts_with(root) && canon.is_file() {
        Ok(Some((canon, filename)))
    } else {
        Ok(None)
    }
}

fn cleanup_downloads(state: &AppState) {
    let cutoff = Utc::now() - state.config.job_ttl;
    state.jobs.lock().unwrap().retain(|_, job| {
        job.created_at >= cutoff || !matches!(job.status.as_str(), "complete" | "error")
    });
    let db = state.db.lock().unwrap();
    let stale = db
        .prepare("SELECT cache_key, file_path, created_at FROM download_cache")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    PathBuf::from(row.get::<_, String>(1)?),
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    for (key, path, created) in stale {
        let parsed = DateTime::parse_from_rfc3339(&created)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        if parsed < cutoff {
            let _ = db.execute(
                "DELETE FROM download_cache WHERE cache_key = ?1",
                params![key],
            );
            if let Some(parent) = path.parent() {
                let _ = fs::remove_dir_all(parent);
            }
        }
    }
}

fn sanitize_filename(value: &str) -> String {
    let sanitized = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if sanitized.is_empty() {
        "download".into()
    } else {
        sanitized.chars().take(120).collect()
    }
}

fn page(title: &str, body: &str) -> Response {
    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, viewport-fit=cover, interactive-widget=resizes-content">
<title>{}</title>
<style>
{PAGE_CSS}
</style>
</head>
<body>{}</body>
</html>"#,
        html_escape(title),
        body
    ))
    .into_response()
}

fn page_guard(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if authorized(&state.config, headers) {
        None
    } else {
        Some(Redirect::to("/login").into_response())
    }
}

fn raw_guard(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if authorized(&state.config, headers) {
        None
    } else {
        Some((StatusCode::UNAUTHORIZED, "authentication required").into_response())
    }
}

fn authorized(config: &Config, headers: &HeaderMap) -> bool {
    if config.auth_disabled {
        return true;
    }
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        && let Some(raw) = value.strip_prefix("Basic ")
        && let Ok(decoded) = BASE64.decode(raw.trim())
        && let Ok(pair) = String::from_utf8(decoded)
        && let Some((user, password)) = pair.split_once(':')
    {
        return user == config.user && verify_password(config, password);
    }
    let Some(cookie) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .is_some_and(|(_, value)| verify_session_cookie(config, value))
}

fn verify_password(config: &Config, password: &str) -> bool {
    if config.auth_disabled {
        return true;
    }
    let Some(hash) = &config.password_hash else {
        return false;
    };
    if hash.starts_with("$argon2") {
        let Ok(parsed) = PasswordHash::new(hash) else {
            return false;
        };
        return Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
    }
    let Some((prefix, rest)) = hash.split_once(':') else {
        return false;
    };
    if prefix != "sha256" {
        return false;
    }
    let Some((salt_hex, expected_hex)) = rest.split_once(':') else {
        return false;
    };
    let Ok(salt) = hex::decode(salt_hex) else {
        return false;
    };
    let Ok(expected) = hex::decode(expected_hex) else {
        return false;
    };
    let actual = password_digest(&salt, password);
    actual.as_slice().ct_eq(expected.as_slice()).into()
}

fn password_digest(salt: &[u8], password: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(password.as_bytes());
    hasher.finalize().to_vec()
}

fn make_session_cookie(config: &Config) -> String {
    let expires = (Utc::now() + Duration::days(SESSION_DAYS)).timestamp();
    let signature = session_signature(config, expires);
    format!(
        "{SESSION_COOKIE}={expires}:{signature}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax",
        SESSION_DAYS * 24 * 60 * 60
    )
}

fn verify_session_cookie(config: &Config, value: &str) -> bool {
    let Some((raw_expires, signature)) = value.split_once(':') else {
        return false;
    };
    let Ok(expires) = raw_expires.parse::<i64>() else {
        return false;
    };
    if expires < Utc::now().timestamp() {
        return false;
    }
    let expected = session_signature(config, expires);
    expected.as_bytes().ct_eq(signature.as_bytes()).into()
}

fn session_signature(config: &Config, expires: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(&config.cookie_secret);
    hasher.update(expires.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

fn audit_public_cmd(args: &[String]) -> io::Result<u8> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!(
            r#"Usage:
  ytdlp-web audit-public
  ytdlp-web audit-public --install-hook

Checks tracked files for local/private paths, common secret markers,
private-network IP leaks, and host-specific denylist terms.
"#
        );
        return Ok(0);
    }

    let root = git_root()?;
    if args.iter().any(|arg| arg == "--install-hook") {
        install_audit_hooks(&root)?;
        println!("installed .git/hooks/pre-commit and .git/hooks/pre-push");
    }

    let findings = audit_public(&root)?;
    if findings.is_empty() {
        println!("audit-public: ok");
        return Ok(0);
    }

    eprintln!("audit-public: found {} issue(s)", findings.len());
    for finding in &findings {
        match finding.line {
            Some(line) => eprintln!("{}:{}: {}", finding.path, line, finding.message),
            None => eprintln!("{}: {}", finding.path, finding.message),
        }
    }
    Ok(1)
}

fn git_root() -> io::Result<PathBuf> {
    let output = StdCommand::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("not inside a Git repository"));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

fn install_audit_hooks(root: &Path) -> io::Result<()> {
    let hooks = root.join(".git/hooks");
    fs::create_dir_all(&hooks)?;
    for name in ["pre-commit", "pre-push"] {
        let hook = hooks.join(name);
        fs::write(
            &hook,
            r#"#!/bin/sh
set -eu
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"
cargo run --quiet -- audit-public
"#,
        )?;
        let mut permissions = fs::metadata(&hook)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions)?;
    }
    Ok(())
}

fn audit_public(root: &Path) -> io::Result<Vec<AuditFinding>> {
    let files = git_tracked_files(root)?;
    let private_terms = load_audit_denylist(root);
    let mut findings = Vec::new();

    for path in files {
        if let Some(message) = audit_path(&path) {
            findings.push(AuditFinding {
                path,
                line: None,
                message,
            });
            continue;
        }

        let full_path = root.join(&path);
        if fs::metadata(&full_path)
            .map(|metadata| metadata.len() > 1_000_000)
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(&full_path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            for message in audit_line(line, &private_terms) {
                findings.push(AuditFinding {
                    path: path.clone(),
                    line: Some(index + 1),
                    message,
                });
            }
        }
    }

    Ok(findings)
}

fn git_tracked_files(root: &Path) -> io::Result<Vec<String>> {
    let output = StdCommand::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

fn audit_path(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let name = normalized.rsplit('/').next().unwrap_or(&normalized);
    let denied_exact = [
        "AGENTS.md",
        "ytdlp_web.local.toml",
        ".env",
        ".env.local",
        "id_rsa",
        "id_ed25519",
    ];
    if denied_exact.contains(&normalized.as_str()) || denied_exact.contains(&name) {
        return Some("private file path is tracked".into());
    }
    if name.starts_with(".env.") && name != ".env.example" {
        return Some("private env file is tracked".into());
    }
    if normalized.starts_with("docs/private/")
        || normalized.starts_with(".ytdlp_web/")
        || normalized.starts_with("backups/")
        || normalized.starts_with("data/")
        || normalized.starts_with("downloads/")
    {
        return Some("ignored private/runtime path is tracked".into());
    }
    if normalized.contains("/data/")
        || normalized.contains("/cache/")
        || normalized.contains("/config/")
        || normalized.contains("/downloads/")
        || normalized.contains("/secrets/")
    {
        return Some("runtime or secret data path is tracked".into());
    }
    if matches!(
        Path::new(name).extension().and_then(|ext| ext.to_str()),
        Some("db" | "sqlite" | "sqlite3" | "log" | "pid" | "pem" | "key" | "p12" | "pfx")
    ) {
        return Some("private state or key-like file is tracked".into());
    }
    None
}

fn load_audit_denylist(root: &Path) -> Vec<String> {
    let mut paths = vec![
        root.join("docs/private/audit-denylist.txt"),
        root.join(".ytdlp_web/audit-denylist.txt"),
    ];
    if let Ok(path) = env::var("YTDLP_WEB_AUDIT_DENYLIST") {
        paths.push(PathBuf::from(path));
    }

    let mut terms = Vec::new();
    for path in paths {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            let term = line.trim();
            if term.is_empty() || term.starts_with('#') {
                continue;
            }
            terms.push(term.to_ascii_lowercase());
        }
    }
    terms
}

fn audit_line(line: &str, private_terms: &[String]) -> Vec<String> {
    let mut findings = Vec::new();
    let lower = line.to_ascii_lowercase();
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return findings;
    }

    if line.contains("-----BEGIN ") && line.contains(&["PRIVATE", " KEY"].concat()) {
        findings.push("private key material".into());
    }
    for marker in token_markers() {
        if line.contains(&marker) {
            findings.push(format!("token marker `{marker}`"));
        }
    }
    if contains_tailscale_ipv4(line) {
        findings.push("Tailscale/CGNAT private IP address".into());
    }
    if suspicious_secret_assignment(line) {
        findings.push("non-placeholder secret-looking assignment".into());
    }
    for term in private_terms {
        if !term.is_empty() && lower.contains(term) {
            findings.push("local denylist term".into());
        }
    }

    findings
}

fn token_markers() -> Vec<String> {
    vec![
        ["github", "_pat_"].concat(),
        ["gh", "p_"].concat(),
        ["gh", "o_"].concat(),
        ["gh", "s_"].concat(),
        ["gh", "u_"].concat(),
        ["s", "k-"].concat(),
        ["xo", "xb-"].concat(),
        ["xo", "xp-"].concat(),
    ]
}

fn suspicious_secret_assignment(line: &str) -> bool {
    if line.contains("::") {
        return false;
    }
    let Some((key, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
        return false;
    };
    let key = key.trim().to_ascii_lowercase();
    if key.is_empty()
        || key.len() > 80
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return false;
    }
    let secret_keys = [
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "access_key",
        "client_secret",
        "private_key",
    ];
    if !secret_keys.iter().any(|needle| key.contains(needle)) {
        return false;
    }

    let value = value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches(',')
        .trim();
    let allowed = [
        "",
        "example",
        "placeholder",
        "changeme",
        "change-me",
        "redacted",
        "dummy",
        "none",
        "null",
        "false",
        "true",
    ];
    if allowed.contains(&value.to_ascii_lowercase().as_str()) {
        return false;
    }
    if value == "String" || value.starts_with("Option<") || value.starts_with("Vec<") {
        return false;
    }
    if value.starts_with("${")
        || value.starts_with('<')
        || value.starts_with("your-")
        || value.contains("...")
        || value.starts_with("Some(")
        || value.starts_with("vec!")
    {
        return false;
    }
    true
}

fn contains_tailscale_ipv4(line: &str) -> bool {
    line.split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter(|token| token.matches('.').count() == 3)
        .any(|token| {
            let octets: Vec<u16> = token
                .split('.')
                .filter_map(|part| part.parse::<u16>().ok())
                .collect();
            octets.len() == 4
                && octets[0] == 100
                && (64..=127).contains(&octets[1])
                && octets.iter().all(|octet| *octet <= 255)
        })
}

fn hash_password_cmd(args: &[String]) -> io::Result<()> {
    if !args.iter().any(|arg| arg == "--stdin") {
        eprintln!("usage: ytdlp-web hash-password --stdin");
        return Ok(());
    }
    let mut password = String::new();
    io::stdin().read_to_string(&mut password)?;
    let password = password.trim_end_matches(['\r', '\n']);
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let salt = SaltString::encode_b64(&salt).map_err(io_other)?;
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(io_other)?;
    println!("{hash}");
    Ok(())
}

fn random_secret() -> Vec<u8> {
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    secret.to_vec()
}

fn env_flag(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn io_other(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_youtube_domains() {
        assert!(normalize_youtube_url("youtu.be/dQw4w9WgXcQ").is_ok());
        assert!(normalize_youtube_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ").is_ok());
        assert!(normalize_youtube_url("https://example.com/watch?v=dQw4w9WgXcQ").is_err());
    }

    #[test]
    fn progress_parser_extracts_download_percent() {
        assert_eq!(
            progress_from_line("[download]  42.7% of 12.34MiB at 1.2MiB/s"),
            Some("42.7%".into())
        );
        assert_eq!(progress_from_line("not progress"), None);
    }

    #[test]
    fn password_hash_round_trips() {
        let salt = b"0123456789abcdef";
        let digest = password_digest(salt, "secret");
        let hash = format!("sha256:{}:{}", hex::encode(salt), hex::encode(digest));
        let config = Config {
            bind: "127.0.0.1:0".into(),
            db_path: PathBuf::new(),
            download_dir: PathBuf::new(),
            ytdlp: "yt-dlp".into(),
            js_runtime: None,
            max_active: 1,
            job_ttl: Duration::hours(1),
            user: "ytdlp_web".into(),
            password_hash: Some(hash),
            cookie_secret: vec![1; 32],
            auth_disabled: false,
        };
        assert!(verify_password(&config, "secret"));
        assert!(!verify_password(&config, "wrong"));
    }

    #[test]
    fn audit_secret_assignment_allows_placeholders() {
        assert!(!suspicious_secret_assignment(&format!("{}=", "TOKEN")));
        assert!(!suspicious_secret_assignment(&format!(
            "{}=<value>",
            "TOKEN"
        )));
        assert!(!suspicious_secret_assignment(&format!(
            "{}=${{VALUE}}",
            "TOKEN"
        )));
        assert!(suspicious_secret_assignment(&format!(
            "{}={}",
            "TOKEN", "abc123"
        )));
    }

    #[test]
    fn audit_detects_cgnat_private_address() {
        let line = format!("bind = 100.{}.0.1", 64);
        assert!(contains_tailscale_ipv4(&line));
        assert!(!contains_tailscale_ipv4("bind = 127.0.0.1"));
    }
}
