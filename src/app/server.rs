use std::{collections::HashMap, fs, io, net::SocketAddr, sync::Arc};
use tokio::sync::Semaphore;

use super::{AppState, Config, database};

pub(crate) async fn serve() -> io::Result<()> {
    let config = Config::from_env()?;
    fs::create_dir_all(&config.download_dir)?;
    let conn = database::open(&config.db_path)?;
    let state = Arc::new(AppState {
        db: std::sync::Mutex::new(conn),
        config,
        jobs: std::sync::Mutex::new(HashMap::new()),
        download_slots: Semaphore::new(1),
    });
    state.set_download_slots();

    let app = crate::interfaces::http::build_router(state.clone());
    let bind = state
        .config
        .bind
        .parse::<SocketAddr>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("YTDLP Web listening on http://{bind}");
    axum::serve(listener, app).await
}
