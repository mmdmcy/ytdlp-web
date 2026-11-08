//! Versioned SQLite schema migrations.

use rusqlite::{Connection, params};

pub const LATEST_SCHEMA_VERSION: i64 = 2;

const BASE_SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS download_cache (
    cache_key TEXT PRIMARY KEY,
    file_path TEXT NOT NULL,
    filename TEXT NOT NULL,
    created_at TEXT NOT NULL
);
"#;

const AUTH_SESSION_MIGRATION: &str = r#"
CREATE TABLE IF NOT EXISTS auth_sessions (
    token_hash TEXT PRIMARY KEY,
    auth_method TEXT NOT NULL CHECK (auth_method IN ('local', 'oidc')),
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_auth_sessions_expires_at ON auth_sessions(expires_at);
CREATE TABLE IF NOT EXISTS oidc_pending_flows (
    state_hash TEXT PRIMARY KEY,
    flow_hash TEXT NOT NULL,
    nonce TEXT NOT NULL,
    pkce_verifier TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_oidc_pending_flows_expires_at ON oidc_pending_flows(expires_at);
"#;

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        "#,
    )?;

    let current = current_version(conn)?;
    if current < 1 {
        apply_migration(conn, 1, BASE_SCHEMA)?;
    }
    if current < 2 {
        apply_migration(conn, 2, AUTH_SESSION_MIGRATION)?;
    }
    debug_assert!(current_version(conn)? >= LATEST_SCHEMA_VERSION);
    Ok(())
}

fn apply_migration(conn: &Connection, version: i64, sql: &str) -> rusqlite::Result<()> {
    let transaction = conn.unchecked_transaction()?;
    transaction.execute_batch(sql)?;
    record_migration(&transaction, version)?;
    transaction.commit()
}

fn current_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
        row.get::<_, Option<i64>>(0)
    })
    .map(|version| version.unwrap_or(0))
}

fn record_migration(conn: &Connection, version: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version) VALUES (?1)",
        params![version],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_records_latest_schema_version() {
        let db = Connection::open_in_memory().unwrap();
        migrate(&db).unwrap();
        assert_eq!(current_version(&db).unwrap(), LATEST_SCHEMA_VERSION);
    }

    #[test]
    fn migrate_creates_download_cache_and_auth_tables() {
        let db = Connection::open_in_memory().unwrap();
        migrate(&db).unwrap();

        let tables = db
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"download_cache".into()));
        assert!(tables.contains(&"auth_sessions".into()));
        assert!(tables.contains(&"oidc_pending_flows".into()));
        assert!(!tables.contains(&"channels".into()));
        assert!(!tables.contains(&"agent_slots".into()));
    }

    #[test]
    fn migration_two_preserves_download_cache() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(BASE_SCHEMA).unwrap();
        db.execute_batch(
            r#"
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO schema_migrations (version) VALUES (1);
            INSERT INTO download_cache (cache_key, file_path, filename, created_at)
                VALUES ('keep', 'file.mp4', 'file.mp4', 'now');
            "#,
        )
        .unwrap();

        migrate(&db).unwrap();

        let filename: String = db
            .query_row("SELECT filename FROM download_cache", [], |row| row.get(0))
            .unwrap();
        assert_eq!(filename, "file.mp4");
        assert_eq!(current_version(&db).unwrap(), LATEST_SCHEMA_VERSION);
    }

    #[test]
    fn auth_migration_rolls_back_if_version_record_fails() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            r#"
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TRIGGER reject_auth_migration
            BEFORE INSERT ON schema_migrations
            WHEN NEW.version = 2
            BEGIN
                SELECT RAISE(ABORT, 'simulated version-record failure');
            END;
            "#,
        )
        .unwrap();

        assert!(apply_migration(&db, 2, AUTH_SESSION_MIGRATION).is_err());
        assert!(!table_exists(&db, "auth_sessions"));
        assert!(!table_exists(&db, "oidc_pending_flows"));
        assert_eq!(current_version(&db).unwrap(), 0);
    }

    #[test]
    fn auth_migration_recovers_legacy_partial_schema() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(BASE_SCHEMA).unwrap();
        db.execute_batch(
            r#"
            CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO schema_migrations (version) VALUES (1);
            CREATE TABLE auth_sessions (
                token_hash TEXT PRIMARY KEY,
                auth_method TEXT NOT NULL CHECK (auth_method IN ('local', 'oidc')),
                expires_at INTEGER NOT NULL
            );
            "#,
        )
        .unwrap();

        migrate(&db).unwrap();

        assert!(table_exists(&db, "auth_sessions"));
        assert!(table_exists(&db, "oidc_pending_flows"));
        assert_eq!(current_version(&db).unwrap(), LATEST_SCHEMA_VERSION);
    }

    fn table_exists(db: &Connection, name: &str) -> bool {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [name],
            |row| row.get(0),
        )
        .unwrap()
    }
}
