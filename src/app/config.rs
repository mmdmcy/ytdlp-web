use chrono::Duration;
use std::{env, io, path::PathBuf};

use crate::security::auth::AuthConfig;

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) bind: String,
    pub(crate) db_path: PathBuf,
    pub(crate) download_dir: PathBuf,
    pub(crate) ytdlp: String,
    pub(crate) js_runtime: Option<PathBuf>,
    pub(crate) max_active: usize,
    pub(crate) job_ttl: Duration,
    pub(crate) auth: AuthConfig,
}

impl Config {
    pub(crate) fn from_env() -> io::Result<Self> {
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
        Ok(Self {
            bind,
            db_path,
            download_dir,
            ytdlp,
            js_runtime,
            max_active,
            job_ttl: Duration::hours(ttl_hours),
            auth: AuthConfig::from_env()?,
        })
    }
}
