use rusqlite::Connection;
use std::{fs, io, path::Path};

use crate::persistence::migrations;

pub(super) fn open(path: &Path) -> io::Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path).map_err(io_other)?;
    migrations::migrate(&conn).map_err(io_other)?;
    Ok(conn)
}

fn io_other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
