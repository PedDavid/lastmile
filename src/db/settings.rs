//! Admin-edited settings, stored as JSON by key.

use rusqlite::{Connection, OptionalExtension, params};

use crate::redact::Household;

const HOUSEHOLD: &str = "household";

pub fn household(conn: &Connection) -> rusqlite::Result<Household> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [HOUSEHOLD],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}

pub fn set_household(conn: &Connection, household: &Household) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![
            HOUSEHOLD,
            serde_json::to_string(household).expect("household serialises")
        ],
    )?;
    Ok(())
}

/// Per-account preference; the subject comes from the authenticated session.
pub fn redact_by_default(conn: &Connection, sub: &str) -> rusqlite::Result<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [format!("user:{sub}:redact_by_default")],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value.as_deref() != Some("false"))
}

pub fn set_redact_by_default(conn: &Connection, sub: &str, enabled: bool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![format!("user:{sub}:redact_by_default"), if enabled { "true" } else { "false" }],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn the_household_round_trips() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.blocking(|c| household(c)).unwrap().is_empty());
        let h = Household {
            names: vec!["Ana".into()],
            ..Default::default()
        };
        db.blocking(|c| set_household(c, &h)).unwrap();
        db.blocking(|c| set_household(c, &h)).unwrap();
        assert_eq!(db.blocking(|c| household(c)).unwrap(), h);
    }
}
