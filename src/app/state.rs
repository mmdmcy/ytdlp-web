use rusqlite::Connection;
use std::{collections::HashMap, sync::Mutex};
use tokio::sync::Semaphore;

use super::Config;
use crate::features::downloader::Job;

pub(crate) struct AppState {
    pub(crate) db: Mutex<Connection>,
    pub(crate) config: Config,
    pub(crate) jobs: Mutex<HashMap<String, Job>>,
    pub(crate) download_slots: Semaphore,
}

impl AppState {
    pub(super) fn set_download_slots(&self) {
        let current = self.download_slots.available_permits();
        if self.config.max_active > current {
            self.download_slots
                .add_permits(self.config.max_active.saturating_sub(current));
        }
    }
}
