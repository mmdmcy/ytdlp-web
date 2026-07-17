//! Process configuration, shared state, database opening, and server startup.

mod config;
mod database;
mod server;
mod state;

pub(crate) use config::Config;
pub(crate) use server::serve;
pub(crate) use state::AppState;
