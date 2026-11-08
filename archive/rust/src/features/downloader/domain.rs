//! Download job model, URL identity, and safe filename rules.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::PathBuf;
use url::Url;
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub(crate) struct Job {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) cache_key: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) status: String,
    pub(crate) progress: String,
    pub(crate) filename: Option<String>,
    pub(crate) file_path: Option<PathBuf>,
    pub(crate) error: Option<String>,
    pub(crate) log: Vec<String>,
}

impl Job {
    pub(crate) fn new(url: String, cache_key: String) -> Self {
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

    pub(crate) fn with_cached(
        url: String,
        cache_key: String,
        file_path: PathBuf,
        filename: String,
    ) -> Self {
        let mut job = Self::new(url, cache_key);
        job.status = "complete".into();
        job.progress = "100%".into();
        job.file_path = Some(file_path);
        job.filename = Some(filename);
        job.log.push("Using cached file".into());
        job
    }
}

pub(crate) fn normalize_youtube_url(raw: &str) -> Result<String, ()> {
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
    let accepted = host == "youtu.be"
        || host == "youtube.com"
        || host == "youtube-nocookie.com"
        || host.ends_with(".youtube.com");
    if accepted { Ok(value) } else { Err(()) }
}

pub(crate) fn cache_key_for_url(url: &str) -> String {
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

pub(crate) fn sanitize_filename(value: &str) -> String {
    let sanitized = value
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
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

#[cfg(test)]
mod tests {
    use super::{cache_key_for_url, normalize_youtube_url, sanitize_filename};

    #[test]
    fn accepts_youtube_domains() {
        assert!(normalize_youtube_url("youtu.be/dQw4w9WgXcQ").is_ok());
        assert!(normalize_youtube_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ").is_ok());
        assert!(normalize_youtube_url("https://example.com/watch?v=dQw4w9WgXcQ").is_err());
    }

    #[test]
    fn stable_download_identity_and_filename_contracts() {
        assert_eq!(
            cache_key_for_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10"),
            "youtube:dQw4w9WgXcQ"
        );
        assert_eq!(
            cache_key_for_url("https://youtu.be/dQw4w9WgXcQ"),
            "youtube:dQw4w9WgXcQ"
        );
        assert_eq!(
            sanitize_filename("  video: title?.mp4  "),
            "video__title_.mp4"
        );
    }
}
