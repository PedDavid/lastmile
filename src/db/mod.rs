//! SQLite storage: one connection behind a mutex (one replica, a handful
//! of users), used from async code through [`Db::call`].

pub mod messages;
pub mod migrations;
pub mod packages;
pub mod rules;
pub mod settings;
pub mod tokens;

use std::path::Path;
use std::sync::{Arc, Mutex};

use jiff::Timestamp;
use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database task failed: {0}")]
    Task(String),
}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Opens (or creates) the database file and brings its schema up to date.
    pub fn open(path: &Path) -> anyhow::Result<Db> {
        let conn = Connection::open(path)
            .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Db::init(conn)
    }

    /// A fresh database that lives as long as the process (`--demo`, tests).
    pub fn open_in_memory() -> anyhow::Result<Db> {
        Db::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> anyhow::Result<Db> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // The root filesystem is read-only: keep temporary tables in memory.
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrations::run(&mut conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` on the connection in the blocking thread pool.
    pub async fn call<R, F>(&self, f: F) -> Result<R, DbError>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut conn).map_err(DbError::from)
        })
        .await
        .map_err(|e| DbError::Task(e.to_string()))?
    }

    /// Runs `f` on the current thread, for callers that are not async
    /// (metrics scrapes, tests). Holds the lock for as long as `f` runs.
    pub fn blocking<R>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<R>,
    ) -> Result<R, DbError> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut conn).map_err(DbError::from)
    }

    /// True if the database answers a trivial query.
    pub async fn ping(&self) -> bool {
        self.call(|c| c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)))
            .await
            .is_ok()
    }
}

pub(crate) fn secs(t: Timestamp) -> i64 {
    t.as_second()
}

pub(crate) fn ts(secs: i64) -> rusqlite::Result<Timestamp> {
    Timestamp::from_second(secs).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, Box::new(e))
    })
}

pub(crate) fn opt_ts(secs: Option<i64>) -> rusqlite::Result<Option<Timestamp>> {
    secs.map(ts).transpose()
}

/// Parses a stored enum name, failing the row (not the process) on an
/// unknown value.
pub(crate) fn parse_col<T>(
    value: String,
    parse: impl Fn(&str) -> Option<T>,
) -> rusqlite::Result<T> {
    parse(&value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown value {value:?}").into(),
        )
    })
}
