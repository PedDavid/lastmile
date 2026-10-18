//! Schema migrations, embedded in the binary and applied in order on start.
//! The database's `user_version` is the number of migrations applied.

use rusqlite::Connection;

/// Append only: an applied migration is never edited.
const MIGRATIONS: &[&str] = &[include_str!("sql/0001_packages.sql")];

pub fn latest() -> usize {
    MIGRATIONS.len()
}

/// Applies the migrations the database doesn't have yet, each in its own
/// transaction. Refuses a database written by a newer version of the app.
pub fn run(conn: &mut Connection) -> anyhow::Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = usize::try_from(current)?;
    if current > MIGRATIONS.len() {
        anyhow::bail!(
            "the database is at schema version {current}, newer than this build ({}): upgrade lastmile",
            MIGRATIONS.len()
        );
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i64::try_from(i + 1)?)?;
        tx.commit()?;
        tracing::info!(version = i + 1, "applied database migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        run(&mut conn).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, latest());
    }

    #[test]
    fn newer_databases_are_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", latest() as i64 + 1)
            .unwrap();
        let err = run(&mut conn).unwrap_err().to_string();
        assert!(err.contains("newer than this build"), "{err}");
    }
}
