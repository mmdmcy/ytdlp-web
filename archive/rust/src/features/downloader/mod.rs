//! Downloader lifecycle capability.
//!
//! Owns job state, accepted YouTube URLs, cache identity, retention, and cache
//! records. HTTP handlers are its callers; `integrations::ytdlp` is its only
//! external-process dependency.

mod domain;
mod jobs;
mod repository;

pub(crate) use domain::{Job, cache_key_for_url, normalize_youtube_url, sanitize_filename};
pub(crate) use jobs::{cleanup_downloads, start_download_job};
pub(crate) use repository::cached_file_for_key;
