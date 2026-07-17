use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use std::sync::Arc;

use crate::app::AppState;

pub(crate) fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/",
            get(super::downloads::downloads_page).post(super::downloads::download_create),
        )
        .route(
            "/login",
            get(crate::security::auth::login_page).post(crate::security::auth::login_post),
        )
        .route("/logout", post(crate::security::auth::logout_post))
        .route("/auth/oidc/start", get(crate::security::oidc::oidc_start))
        .route(
            "/auth/oidc/callback",
            get(crate::security::oidc::oidc_callback),
        )
        .route(
            "/.well-known/linuxmice/component",
            get(super::component_status::component_status),
        )
        .route(
            "/downloads",
            get(super::downloads::downloads_page).post(super::downloads::download_create),
        )
        .route(
            "/downloads/jobs/{id}",
            get(super::downloads::download_job_page),
        )
        .route(
            "/downloads/jobs/{id}/status",
            get(super::downloads::download_job_status),
        )
        .route(
            "/downloads/jobs/{id}/file",
            get(super::downloads::download_file),
        )
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use chrono::Duration;
    use rusqlite::Connection;
    use std::{collections::HashMap, path::PathBuf, sync::Mutex};
    use tokio::sync::Semaphore;
    use tower::ServiceExt;

    fn oidc_state() -> Arc<AppState> {
        let db = Connection::open_in_memory().unwrap();
        crate::persistence::migrations::migrate(&db).unwrap();
        Arc::new(AppState {
            db: Mutex::new(db),
            config: crate::app::Config {
                bind: "127.0.0.1:0".into(),
                db_path: PathBuf::new(),
                download_dir: PathBuf::new(),
                ytdlp: "yt-dlp".into(),
                js_runtime: None,
                max_active: 1,
                job_ttl: Duration::hours(1),
                auth: crate::security::auth::test_auth_config(true),
            },
            jobs: Mutex::new(HashMap::new()),
            download_slots: Semaphore::new(1),
        })
    }

    #[tokio::test]
    async fn login_route_offers_organization_and_local_break_glass() {
        let response = build_router(oidc_state())
            .oneshot(Request::get("/login").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Organization login"));
        assert!(body.contains("Local login"));
    }

    #[tokio::test]
    async fn download_page_preserves_the_form_contract() {
        let response = build_router(oidc_state())
            .oneshot(
                Request::get("/")
                    .header(header::AUTHORIZATION, "Basic eXRkbHBfd2ViOnNlY3JldA==")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains(r#"action="/downloads""#));
        assert!(body.contains("YTP Downloader"));
        assert!(body.contains("Download Video"));
        assert!(body.contains("https://youtu.be/..."));
    }

    #[tokio::test]
    async fn local_break_glass_login_never_contacts_oidc() {
        let response = build_router(oidc_state())
            .oneshot(
                Request::post("/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("password=secret"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .any(|value| value.starts_with("ytdlp_web_session="))
        );
    }

    #[tokio::test]
    async fn logout_invalidates_the_opaque_session() {
        let app = build_router(oidc_state());
        let login = app
            .clone()
            .oneshot(
                Request::post("/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("password=secret"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookie = login
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with("ytdlp_web_session="))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let logout = app
            .clone()
            .oneshot(
                Request::post("/logout")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::SEE_OTHER);
        let protected = app
            .oneshot(
                Request::get("/")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(protected.status(), StatusCode::SEE_OTHER);
        assert_eq!(protected.headers()[header::LOCATION], "/login");
    }

    #[tokio::test]
    async fn callback_route_rejects_missing_flow_and_clears_flow_cookie() {
        let response = build_router(oidc_state())
            .oneshot(
                Request::get("/auth/oidc/callback")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with("ytdlp_web_oidc_flow="))
            .unwrap();
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("Secure"));
    }
}
