//! Queries on `rules`, `rule_versions` and `rule_samples`.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{parse_col, secs, ts};
use crate::model::Channel;
use crate::rules::engine::Fields;
use crate::rules::schema::RuleDef;

/// Where a rule came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Local,
    Imported,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Local => "local",
            Origin::Imported => "imported",
        }
    }

    pub fn parse(s: &str) -> Option<Origin> {
        [Origin::Local, Origin::Imported]
            .into_iter()
            .find(|o| o.as_str() == s)
    }
}

#[derive(Clone, Debug)]
pub struct StoredRule {
    pub id: String,
    pub version: i64,
    pub enabled: bool,
    /// The YAML as written, comments included.
    pub source: String,
    pub def: RuleDef,
    pub origin: Origin,
    pub created_by: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub id: i64,
    pub rule_id: String,
    pub message_id: Option<i64>,
    pub channel: Channel,
    pub sender: Option<String>,
    pub text: String,
    pub expected: Fields,
    pub created_at: Timestamp,
}

fn rule(row: &Row) -> rusqlite::Result<StoredRule> {
    let definition: String = row.get(4)?;
    Ok(StoredRule {
        id: row.get(0)?,
        version: row.get(1)?,
        enabled: row.get(2)?,
        source: row.get(3)?,
        def: serde_json::from_str(&definition).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        origin: parse_col(row.get(5)?, Origin::parse)?,
        created_by: row.get(6)?,
        created_at: ts(row.get(7)?)?,
        updated_at: ts(row.get(8)?)?,
    })
}

const COLUMNS: &str =
    "id, version, enabled, source, definition, origin, created_by, created_at, updated_at";

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<StoredRule>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM rules ORDER BY name COLLATE NOCASE, id"
    ))?;
    stmt.query_map([], rule)?.collect()
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<StoredRule>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM rules WHERE id = ?1"),
        [id],
        rule,
    )
    .optional()
}

/// Saves a rule: a new one at version 1, or the next version of `id`.
/// Returns the version written.
#[allow(clippy::too_many_arguments)]
pub fn save(
    conn: &Connection,
    id: &str,
    source: &str,
    def: &RuleDef,
    origin: Origin,
    enabled: bool,
    by: &str,
    now: Timestamp,
) -> rusqlite::Result<i64> {
    let definition = serde_json::to_string(def).expect("rules serialise");
    let current: Option<i64> = conn
        .query_row("SELECT version FROM rules WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    let version = current.map_or(1, |v| v + 1);
    if current.is_some() {
        conn.execute(
            "UPDATE rules SET version = ?2, name = ?3, source = ?4, definition = ?5, updated_at = ?6 \
             WHERE id = ?1",
            params![id, version, def.name, source, definition, secs(now)],
        )?;
    } else {
        conn.execute(
            "INSERT INTO rules (id, version, name, enabled, source, definition, origin, created_by, \
             created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                id,
                version,
                def.name,
                enabled,
                source,
                definition,
                origin.as_str(),
                by,
                secs(now)
            ],
        )?;
    }
    conn.execute(
        "INSERT INTO rule_versions (rule_id, version, source, created_by, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, version, source, by, secs(now)],
    )?;
    Ok(version)
}

pub fn set_enabled(conn: &Connection, id: &str, enabled: bool) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE rules SET enabled = ?2 WHERE id = ?1",
        params![id, enabled],
    )? == 1)
}

fn sample(row: &Row) -> rusqlite::Result<Sample> {
    let expected: String = row.get(6)?;
    Ok(Sample {
        id: row.get(0)?,
        rule_id: row.get(1)?,
        message_id: row.get(2)?,
        channel: parse_col(row.get(3)?, Channel::parse)?,
        sender: row.get(4)?,
        text: row.get(5)?,
        expected: serde_json::from_str(&expected).unwrap_or_default(),
        created_at: ts(row.get(7)?)?,
    })
}

/// Every sample, of every rule.
pub fn all_samples(conn: &Connection) -> rusqlite::Result<Vec<Sample>> {
    let mut stmt = conn.prepare(
        "SELECT id, rule_id, message_id, channel, sender, text, expected, created_at FROM rule_samples \
         ORDER BY id",
    )?;
    stmt.query_map([], sample)?.collect()
}

#[allow(clippy::too_many_arguments)]
pub fn add_sample(
    conn: &Connection,
    rule_id: &str,
    message_id: Option<i64>,
    channel: Channel,
    sender: Option<&str>,
    text: &str,
    expected: &Fields,
    now: Timestamp,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO rule_samples (rule_id, message_id, channel, sender, text, expected, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            rule_id,
            message_id,
            channel.as_str(),
            sender,
            text,
            serde_json::to_string(expected).expect("fields serialise"),
            secs(now)
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn set_expected(conn: &Connection, sample_id: i64, expected: &Fields) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE rule_samples SET expected = ?2 WHERE id = ?1",
        params![
            sample_id,
            serde_json::to_string(expected).expect("fields serialise")
        ],
    )?;
    Ok(())
}

pub fn delete_sample(conn: &Connection, rule_id: &str, sample_id: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "DELETE FROM rule_samples WHERE id = ?1 AND rule_id = ?2",
        params![sample_id, rule_id],
    )? == 1)
}

/// Rule counts by origin and enabled state, for the metrics.
pub fn counts(conn: &Connection) -> rusqlite::Result<Vec<(String, bool, i64)>> {
    let mut stmt =
        conn.prepare("SELECT origin, enabled, COUNT(*) FROM rules GROUP BY origin, enabled")?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}
