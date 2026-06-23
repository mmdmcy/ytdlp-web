use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use std::sync::Arc;

use crate::AppState;

pub(crate) fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(crate::downloads_page).post(crate::download_create))
        .route("/login", get(crate::login_page).post(crate::login_post))
        .route("/logout", post(crate::logout_post))
        .route(
            "/downloads",
            get(crate::downloads_page).post(crate::download_create),
        )
        .route("/downloads/jobs/{id}", get(crate::download_job_page))
        .route(
            "/downloads/jobs/{id}/status",
            get(crate::download_job_status),
        )
        .route("/downloads/jobs/{id}/file", get(crate::download_file))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}
