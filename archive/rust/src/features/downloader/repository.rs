//! Durable download-cache records owned by the downloader capability.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

pub(crate) fn cached_file_for_key(
    db: &Connection,
    download_dir: &Path,
    cache_key: &str,
) -> rusqlite::Result<Option<(PathBuf, String)>> {
    let row = db
        .query_row(
            "SELECT file_path, filename FROM download_cache WHERE cache_key = ?1",
            params![cache_key],
            |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get::<_, String>(1)?,
                ))
            },
        )
        .optional()?;
    let Some((path, filename)) = row else {
        return Ok(None);
    };
    let Ok(canonical_path) = path.canonicalize() else {
        return Ok(None);
    };
    let Ok(canonical_root) = download_dir.canonicalize() else {
        return Ok(None);
    };
    if canonical_path.starts_with(canonical_root) && canonical_path.is_file() {
        Ok(Some((canonical_path, filename)))
    } else {
        Ok(None)
    }
}

pub(super) fn record_completed_download(
    db: &Connection,
    cache_key: &str,
    file: &Path,
    filename: &str,
    created_at: &str,
) -> rusqlite::Result<usize> {
    db.execute(
        "INSERT OR REPLACE INTO download_cache (cache_key, file_path, filename, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![cache_key, file.to_string_lossy(), filename, created_at],
    )
}

pub(super) fn cache_entries(db: &Connection) -> Vec<(String, PathBuf, String)> {
    db.prepare("SELECT cache_key, file_path, created_at FROM download_cache")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        PathBuf::from(row.get::<_, String>(1)?),
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
}

pub(super) fn delete_cache_entry(db: &Connection, cache_key: &str) {
    let _ = db.execute(
        "DELETE FROM download_cache WHERE cache_key = ?1",
        params![cache_key],
    );
}
