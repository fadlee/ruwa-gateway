//! Application database (`storages/ruwa.db`): device registry and chat
//! storage. Diesel, sharing `libsqlite3-sys` with whatsapp-rust.
//!
//! Diesel is synchronous; the connection sits behind a mutex and every call
//! runs on the blocking pool.

pub mod chat;
pub mod registry;
pub mod schedule;

use std::sync::{Arc, Mutex};

use diesel::{Connection, RunQueryDsl, SqliteConnection, sql_query};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<SqliteConnection>>,
}

impl Db {
    /// Open (creating if needed) and migrate. Accepts `file:path`, a plain
    /// path, or `:memory:`.
    pub fn open(uri: &str) -> anyhow::Result<Self> {
        let path = uri.strip_prefix("file:").unwrap_or(uri);
        let path = path.split('?').next().unwrap_or(path);
        if path != ":memory:"
            && let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = SqliteConnection::establish(path)?;
        sql_query("PRAGMA journal_mode = WAL").execute(&mut conn)?;
        sql_query("PRAGMA foreign_keys = ON").execute(&mut conn)?;
        conn.run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow::anyhow!("migration failed: {e}"))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run `f` with the connection on the blocking pool.
    pub async fn run<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut SqliteConnection) -> diesel::QueryResult<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().expect("db lock");
            f(&mut conn).map_err(anyhow::Error::from)
        })
        .await?
    }
}
