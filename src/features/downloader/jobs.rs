//! In-memory queue orchestration and retention.

use chrono::{DateTime, Utc};
use std::{fs, sync::Arc};

use super::{Job, repository};
use crate::{app::AppState, integrations::ytdlp};

pub(crate) fn start_download_job(state: Arc<AppState>, id: String) {
    tokio::spawn(run_download_job(state, id));
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
    if let Err(error) = tokio::fs::create_dir_all(&job_dir).await {
        fail_job(
            &state,
            &id,
            format!("Could not create download directory: {error}"),
        );
        return;
    }

    let result = ytdlp::download(
        &state.config.ytdlp,
        state.config.js_runtime.as_deref(),
        &job_dir,
        &job.url,
        |line, progress| {
            update_job(&state, &id, |job| {
                if let Some(progress) = progress {
                    job.progress = progress;
                }
                push_log(job, line);
            });
        },
    )
    .await;
    let file = match result {
        Ok(file) => file,
        Err(error) => {
            fail_job(&state, &id, error);
            return;
        }
    };
    let filename = file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    {
        let db = state.db.lock().unwrap();
        let _ = repository::record_completed_download(
            &db,
            &job.cache_key,
            &file,
            &filename,
            &Utc::now().to_rfc3339(),
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

pub(crate) fn cleanup_downloads(state: &AppState) {
    let cutoff = Utc::now() - state.config.job_ttl;
    state.jobs.lock().unwrap().retain(|_, job| {
        job.created_at >= cutoff || !matches!(job.status.as_str(), "complete" | "error")
    });
    let db = state.db.lock().unwrap();
    for (key, path, created) in repository::cache_entries(&db) {
        let parsed = DateTime::parse_from_rfc3339(&created)
            .map(|date| date.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        if parsed < cutoff {
            repository::delete_cache_entry(&db, &key);
            if let Some(parent) = path.parent() {
                let _ = fs::remove_dir_all(parent);
            }
        }
    }
}
